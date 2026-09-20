//! App-managed agent terminals, backed by tmux for true detach / nohup.
//!
//! The durable session is a tmux session named `ass-<id>` that holds the agent
//! process. The Rust app is a *bridge*: per connected client it spawns
//! `tmux attach` inside a PTY (portable-pty) and streams bytes to/from the UI.
//! Dropping the attach client leaves the session running (nohup w.r.t. the
//! frontend); reattaching spawns a fresh `tmux attach`, so full-screen TUIs
//! (claude/codex) redraw correctly.
//!
//! Lifetime model — terminals are PERSISTENT:
//!   * attachment ↔ frontend — decoupled: a global registry of `Weak`
//!     attachments; the strong `Arc` is held by the streaming owner (the SSE
//!     reader, or the desktop's managed state). When a client disconnects the
//!     strong ref drops → the attach PTY dies → tmux detaches → session lives.
//!   * session ↔ backend — ALSO decoupled: sessions outlive the backend that
//!     created them. Quit the desktop app, drop an SSH connection, upgrade or
//!     restart the server — the agent keeps running, and any later client of
//!     any backend can list/attach it (the `ass-*` tmux namespace is shared
//!     machine-wide, deliberately unfiltered by creator). A session ends only
//!     when the user kills it explicitly, or when [`sweep_stale`] garbage-
//!     collects one whose agent has EXITED (every pane back at a plain shell)
//!     after sitting unattached and idle for [`GC_IDLE_SECS`] — finished runs
//!     stay reviewable for a week, but can't pile up forever. A session with a
//!     live agent (or any non-shell foreground process) is never reaped.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::PathBuf;
use std::process::Command;

use skill_core::process::hidden_command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use serde::Serialize;

/// Bounded, attachment-independent tmux observations for agent state detection.
pub mod detection_snapshot;
mod terminal_links;
pub use terminal_links::resolve_file_link;

/// Prefix that marks every tmux session this app owns (so we never touch the
/// user's own tmux sessions).
const PREFIX: &str = "ass-";
/// Field separator in our `tmux list-sessions -F` output. A tab: tmux passes it
/// through literally (it escapes non-printable control bytes), and none of our
/// fields — name, label, agent, cwd, created — realistically contains one.
const SEP: char = '\t';

/// Floor for client-reported terminal sizes — below it is a browser layout
/// glitch, never a real pane. Honoring one is destructive: tmux
/// clamps the whole window to the geometry owner's PTY, a TUI repaints
/// at that width, and the repaint is baked into scrollback for every viewer.
/// Resizes below the floor are rejected (the window keeps its last good
/// size); create/attach are clamped up (a wrong-sized viewer beats none).
const MIN_COLS: u16 = 20;
const MIN_ROWS: u16 = 5;

fn size_floor(what: &str, id: &str, cols: u16, rows: u16) -> (u16, u16) {
    if cols < MIN_COLS || rows < MIN_ROWS {
        log::warn!("implausible {what} size {cols}x{rows} (id={id}) — clamping to {MIN_COLS}x{MIN_ROWS} floor");
    }
    (cols.max(MIN_COLS), rows.max(MIN_ROWS))
}

static SEQ: AtomicU64 = AtomicU64::new(0);
static UUID_SEQ: AtomicU64 = AtomicU64::new(0);

// ───────────────────────────── public types ─────────────────────────────

/// A launchable agent option surfaced in the "New terminal" picker. The same
/// agent can appear multiple times (e.g. the CLI on PATH *and* the version
/// bundled inside a VS Code / Cursor extension).
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AgentOption {
    /// Stable id passed back to `create_session` (e.g. `claude:cli`, `codex:ext:vs-code`, `shell`).
    pub id: String,
    /// Agent family: `claude` | `codex` | `shell`.
    pub agent: String,
    /// Display name, e.g. "Claude Code".
    pub label: String,
    /// `cli` | `extension` | `shell`.
    pub flavor: String,
    /// Human flavor, e.g. "CLI" or "VS Code extension".
    pub flavor_label: String,
    /// Absolute path to the executable.
    pub bin: String,
    /// Best-effort version string (from `<bin> --version`).
    pub version: Option<String>,
    /// Whether this agent supports `--ide` (attach to a running editor extension).
    pub supports_ide: bool,
    /// Whether the agent registry has an interactive launch line (TUI with an
    /// initial prompt) for this family — the gate for app-driven runs like
    /// skill mining.
    pub can_mine: bool,
}

/// Metadata for one live tmux-backed terminal session.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub id: String,
    pub label: String,
    pub agent: String,
    pub cwd: String,
    /// Unix seconds (as a string) when the session was created.
    pub created: String,
    /// Unix seconds (as a string) of the session's most recent pane output (tmux
    /// `window_activity`). Informational / ordering aid — NOT the unread signal
    /// anymore: an idle agent TUI keeps repainting its pane, so output ≠ "new"
    /// (see `bell_at`).
    pub activity: String,
    /// Unix seconds (as a string) of the last terminal BELL the agent rang. tmux
    /// `monitor-bell` + an `alert-bell` hook stamp it into `@ass_bell_at`; agents
    /// we launch are configured to bell on turn-completion, so this means
    /// "finished a turn / waiting for you" — what the rail's unread dot keys off.
    /// "0" until the first bell.
    pub bell_at: String,
    /// The agent session id, forced at launch (`claude --session-id`) or resolved
    /// from a live Codex process's open rollout, including resumed sessions.
    /// This keeps terminals mapped to their own transcript even when several
    /// share a cwd. Empty when unknown.
    pub session_id: String,
}

// ─────────────────────────── base64 (single home) ───────────────────────────

/// Encode raw PTY bytes for the (text-only) JSON / SSE transports.
pub fn b64_encode(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}
/// Decode keystroke bytes from the transport (lenient: bad input → empty).
pub fn b64_decode(s: &str) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(s.trim())
        .unwrap_or_default()
}

// ────────────────────────────── tmux helpers ──────────────────────────────

/// The user's shell PATH, resolved once. A GUI-launched desktop app inherits a
/// minimal PATH that omits ~/.local/bin, the nvm/pyenv/rbenv shims, Homebrew,
/// etc. — exactly where the agent CLIs live — so the raw process PATH
/// under-reports installed agents. We recover it by asking an *interactive*
/// login shell (`-l -i`) for its exported PATH: version managers (nvm, conda, …)
/// and Homebrew's shellenv extend PATH from ~/.bashrc / ~/.zshrc, which a
/// non-interactive shell never sources — so a plain login shell would miss, say,
/// an nvm-installed `codex`. We read the *exported* PATH via `printenv` (it is
/// colon-joined in every shell, incl. fish, whose own $PATH is a space-joined
/// list), fenced by sentinels so an rc-file banner can't corrupt it. stdout is
/// drained on a side thread so a chatty rc can't fill the pipe and deadlock the
/// shell before it prints; stdin is /dev/null and a deadline kills an rc that
/// blocks. Empty on failure (we fall back to the process PATH).
fn login_path() -> std::ffi::OsString {
    static PATH: OnceLock<std::ffi::OsString> = OnceLock::new();
    PATH.get_or_init(|| {
        use std::process::Stdio;
        let shell = std::env::var("SHELL")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "bash".into());
        let Ok(mut child) = hidden_command(shell)
            .args(["-l", "-i", "-c", "printf '<<SSP:'; printenv PATH; printf ':SSP>>'"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        else {
            return std::ffi::OsString::new();
        };
        // Drain stdout concurrently: an interactive rc may print a banner before
        // our markers, and >64KB of unread output would block the shell on write
        // before it reaches printenv. The reader sees EOF once the child exits or
        // is killed below, so the join always returns.
        let reader = child.stdout.take().map(|mut out| {
            thread::spawn(move || {
                let mut buf = String::new();
                let _ = out.read_to_string(&mut buf);
                buf
            })
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if std::time::Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                Ok(None) => thread::sleep(std::time::Duration::from_millis(20)),
                Err(_) => break,
            }
        }
        let text = reader.and_then(|r| r.join().ok()).unwrap_or_default();
        // Our markers print last, after any rc banner — match the final pair.
        let val = text
            .rsplit_once("<<SSP:")
            .and_then(|(_, rest)| rest.split_once(":SSP>>"))
            .map(|(v, _)| v.trim())
            .unwrap_or("");
        std::ffi::OsString::from(val)
    })
    .clone()
}

/// Find `bin` on PATH, searching the process PATH first and then the
/// login-shell PATH ([`login_path`]) — the latter recovers CLIs that a GUI
/// launch's stripped-down PATH would otherwise hide.
fn which(bin: &str) -> Option<String> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    let login = login_path();
    if !login.is_empty() {
        dirs.extend(std::env::split_paths(&login));
    }
    dirs.into_iter()
        .map(|d| d.join(bin))
        .find(|p| p.is_file())
        .map(|p| p.to_string_lossy().into_owned())
}

/// Expand a leading `~`/`~/` to the user's home directory; pass other paths through
/// unchanged. `None` only when `~` is used but no home dir is resolvable.
fn expand_tilde(path: &str) -> Option<PathBuf> {
    match path.strip_prefix("~/") {
        Some(rest) => dirs::home_dir().map(|h| h.join(rest)),
        None if path == "~" => dirs::home_dir(),
        None => Some(PathBuf::from(path)),
    }
}

/// Resolve an agent's CLI binary: the shell PATH first, then the agent-specific
/// off-PATH install locations from its `Spec` — the fixed `cli_paths` (leading `~`
/// expanded) and `<install_dir_env>/<path_name>`. Returns the first existing file,
/// so a native/standalone install that isn't on PATH (e.g. claude's
/// `~/.claude/local/claude`) is still detected.
fn resolve_cli(spec: &Spec) -> Option<String> {
    if let Some(bin) = which(spec.path_name) {
        return Some(bin);
    }
    for cand in spec.cli_paths {
        if let Some(path) = expand_tilde(cand) {
            if path.is_file() {
                return Some(path.to_string_lossy().into_owned());
            }
        }
    }
    if !spec.install_dir_env.is_empty() {
        if let Some(dir) = std::env::var_os(spec.install_dir_env) {
            if !dir.is_empty() {
                let path = PathBuf::from(dir).join(spec.path_name);
                if path.is_file() {
                    return Some(path.to_string_lossy().into_owned());
                }
            }
        }
    }
    None
}

fn tmux_bin() -> String {
    static TMUX: OnceLock<String> = OnceLock::new();
    TMUX.get_or_init(|| {
        which("tmux")
            .or_else(|| bundled_tmux_beside(&std::env::current_exe().ok()?))
            .unwrap_or_else(|| "tmux".to_string())
    })
    .clone()
}

/// The sidecar tmux shipped in the same directory as `exe`, if present. The
/// macOS .app carries one (Tauri `externalBin` lands next to the app binary in
/// `Contents/MacOS`) because fresh Macs have no tmux; a standalone
/// `skill-server` dropped on a remote host can carry one the same way. The
/// host's own tmux (PATH, then login-shell PATH) always wins — see [`tmux_bin`].
fn bundled_tmux_beside(exe: &std::path::Path) -> Option<String> {
    let cand = exe.parent()?.join("tmux");
    cand.is_file().then(|| cand.to_string_lossy().into_owned())
}

/// A failed tmux spawn is almost always "not installed" — say so, with the
/// fix, instead of leaking a raw OS error into the UI.
fn tmux_spawn_err(e: std::io::Error) -> String {
    if e.kind() == std::io::ErrorKind::NotFound {
        "tmux isn't installed on this machine — install it (brew install tmux on macOS, \
apt install tmux on Debian/Ubuntu), then try again."
            .into()
    } else {
        format!("Couldn't start tmux: {e}")
    }
}

/// A `tmux` command that works even when the backend itself runs inside a tmux
/// pane (`-d` creates are fine within tmux; we only must not inherit `$TMUX`).
fn tmux() -> Command {
    let mut c = hidden_command(tmux_bin());
    // A daemon/accessor may have outlived the directory it was launched from.
    // Never pass that stale cwd descriptor to tmux or its startup shell.
    #[cfg(unix)]
    c.current_dir("/");
    #[cfg(target_os = "macos")]
    macos_fds::isolate(&mut c);
    // `-u` forces UTF-8 regardless of locale: a GUI-launched app has no LANG/
    // LC_*, and a locale-less tmux ASCII-sanitizes client output — the literal
    // tabs in our list-sessions format came back as `_`, corrupting every
    // parsed session id (macOS installed builds; dev boxes always have LANG).
    // See tests::tmux_u_keeps_output_raw_without_locale.
    c.arg("-u");
    c.env_remove("TMUX");
    c
}

#[cfg(target_os = "macos")]
mod macos_fds;
mod session_start;

/// Strip characters that would corrupt our tab-separated `list-sessions` parse.
/// Tabs/newlines are legal in Unix paths but must never leak into metadata.
fn sanitize_meta(s: &str) -> String {
    s.replace(['\t', '\r', '\n'], " ")
}

/// POSIX single-quote a string for embedding in a `bash -lc` script.
fn shell_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// A new pane inherits the long-lived tmux server's limits, even if the host
/// service was restarted with a higher allowance. Raise the pane's soft limit
/// after login rc files run, before the agent starts, without changing its hard
/// cap. The function keeps its temporary variables out of the agent environment.
fn shell_open_file_limit() -> String {
    format!(
        r#"_vibestudio_open_file_limit() {{
    local soft hard target={target};
    soft=$(builtin ulimit -Sn) || return;
    hard=$(builtin ulimit -Hn) || return;
    [ "$soft" = unlimited ] && return;
    [ "$soft" -ge "$target" ] && return;
    if [ "$hard" != unlimited ] && [ "$hard" -lt "$target" ]; then target=$hard; fi;
    if [ "$soft" -lt "$target" ]; then
        builtin ulimit -Sn "$target" 2>/dev/null || builtin printf 'VibeStudio: could not raise the open-file soft limit from %s to %s (hard: %s).\n' "$soft" "$target" "$hard" >&2;
    fi;
}};
_vibestudio_open_file_limit || :; unset -f _vibestudio_open_file_limit;
"#,
        target = skill_core::process::open_file_limit_target(),
    )
}

fn basename(p: &str) -> String {
    p.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(p)
        .to_string()
}

// ──────────────────────────── session registry ─────────────────────────────

/// Session metadata lives HERE, not in tmux. Free text round-tripped through
/// tmux's stdout is what broke on locale-less hosts (output sanitization — see
/// [`tmux`]); after that, the tmux wire carries only safe alphabets (a minted
/// name and numeric fields) and everything human-readable lives in one JSON
/// file per session, in a directory every backend on this machine shares —
/// the same machine-wide invariant as the tmux server itself. The legacy
/// `@ass_*` options are still WRITTEN so older backends keep working during
/// the auto-update window, and sessions THEY create are lazily backfilled
/// into the registry by [`backfill_legacy`].
mod registry {
    use serde::{Deserialize, Serialize};
    use std::path::PathBuf;

    #[derive(Serialize, Deserialize, Clone, Default)]
    pub(crate) struct Meta {
        pub label: String,
        pub agent: String,
        pub cwd: String,
        pub created: String,
        #[serde(default)]
        pub session_id: String,
    }

    fn dir() -> Option<PathBuf> {
        skill_core::paths::config_dir().ok().map(|d| d.join("terminals"))
    }

    /// `None` for anything that isn't a name we minted — the id can arrive
    /// from the HTTP API, so the filename must be shape-validated, never
    /// trusted (a crafted "ass-../…" id must not become a path).
    fn file(name: &str) -> Option<PathBuf> {
        super::valid_session_name(name).then_some(())?;
        dir().map(|d| d.join(format!("{name}.json")))
    }

    pub(crate) fn write(name: &str, meta: &Meta) -> Result<(), String> {
        let path = file(name).ok_or("invalid session name or no config dir")?;
        let parent = path.parent().ok_or("no registry dir")?;
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        // tmp+rename: readers never see a torn file. The tmp name is
        // pid-suffixed because backfill means a session can have more than one
        // concurrent writer (two backends' pollers); the renames then race, but
        // both carry identical settled content, and rename is atomic.
        let tmp = path.with_extension(format!("json.tmp{}", std::process::id()));
        std::fs::write(&tmp, serde_json::to_vec(meta).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
    }

    pub(crate) fn read(name: &str) -> Option<Meta> {
        serde_json::from_slice(&std::fs::read(file(name)?).ok()?).ok()
    }

    pub(crate) fn remove(name: &str) {
        if let Some(p) = file(name) {
            let _ = std::fs::remove_file(p);
        }
    }

    /// Grace before an entry with no matching session may be reaped: protects
    /// an in-flight create (registry written moments before `new-session`).
    const ORPHAN_GRACE_SECS: u64 = 600;

    /// Drop entries whose session no longer exists (killed outside the app, or
    /// every session died with a reboot — tmux sessions don't survive one).
    pub(crate) fn sweep_orphans(live: &std::collections::HashSet<String>) {
        if let Some(d) = dir() {
            sweep_orphans_in(&d, live);
        }
    }

    pub(crate) fn sweep_orphans_in(d: &std::path::Path, live: &std::collections::HashSet<String>) {
        let Ok(entries) = std::fs::read_dir(d) else { return };
        for e in entries.flatten() {
            let p = e.path();
            let name = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            if live.contains(&name) {
                continue;
            }
            let age_ok = e
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .map(|el| el.as_secs() > ORPHAN_GRACE_SECS)
                .unwrap_or(false);
            if age_ok {
                let _ = std::fs::remove_file(&p);
            }
        }
    }
}

/// Strict shape of a name WE minted (`ass-<pid>-<secs>-<seq>`). The
/// list-sessions parse trusts nothing else on the wire — a user's own session
/// name can contain spaces or any bytes, and must simply be skipped.
fn valid_session_name(n: &str) -> bool {
    n.strip_prefix(PREFIX)
        .map(|rest| {
            let parts: Vec<&str> = rest.split('-').collect();
            parts.len() == 3
                && parts.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        })
        .unwrap_or(false)
}

// ──────────────────────────── session lifecycle ────────────────────────────

/// List the app's live sessions (queries tmux, so this is correct within the
/// current backend lifetime regardless of in-process attachment state).
pub fn list_sessions() -> Result<Vec<SessionInfo>, String> {
    let mut sessions = list_sessions_checked().unwrap_or_default();
    resolve_codex_session_ids(&mut sessions);
    Ok(sessions)
}

/// Codex chooses its own UUID. Its native process keeps the current rollout
/// open, including on resume, so process ancestry provides an exact mapping
/// without guessing from cwd or activity. Do this only for the HTTP inventory;
/// the attention watcher's more frequent `list_sessions_checked` needs no titles.
fn resolve_codex_session_ids(sessions: &mut [SessionInfo]) {
    if !sessions.iter().any(|s| s.agent == "codex") {
        return;
    }
    let Ok(panes) = tmux().args([
        "list-panes", "-a", "-F", "#{session_name} #{pane_pid}",
    ]).output() else { return };
    let Ok(processes) = hidden_command("ps").args(["-eo", "pid=,ppid=,comm="]).output()
        else { return };
    if !panes.status.success() || !processes.status.success() {
        return;
    }
    let by_session = codex_pids_by_session(
        &String::from_utf8_lossy(&panes.stdout),
        &String::from_utf8_lossy(&processes.stdout),
    );
    let pids: HashSet<u32> = sessions.iter().filter(|s| s.agent == "codex")
        .filter_map(|s| by_session.get(&s.id)).flatten().copied().collect();
    let paths = open_rollout_paths(&pids);
    for session in sessions.iter_mut().filter(|s| s.agent == "codex") {
        let candidates = by_session.get(&session.id).into_iter().flatten()
            .filter_map(|pid| paths.get(pid)).flatten();
        if let Some(id) = unique_codex_rollout_session_id(candidates) {
            session.session_id = id;
        }
    }
}

fn codex_pids_by_session(panes: &str, processes: &str) -> HashMap<String, Vec<u32>> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut codex = HashSet::new();
    for line in processes.lines() {
        let mut fields = line.split_whitespace();
        let (Some(pid), Some(parent)) = (
            fields.next().and_then(|n| n.parse::<u32>().ok()),
            fields.next().and_then(|n| n.parse::<u32>().ok()),
        ) else { continue };
        let name = fields.collect::<Vec<_>>().join(" ");
        children.entry(parent).or_default().push(pid);
        if skill_core::agent_detection::matches_process("codex", &name, &name) {
            codex.insert(pid);
        }
    }
    let mut result: HashMap<String, Vec<u32>> = HashMap::new();
    for line in panes.lines() {
        let mut fields = line.split_whitespace();
        let (Some(session), Some(pane)) = (
            fields.next(), fields.next().and_then(|n| n.parse::<u32>().ok()),
        ) else { continue };
        if !valid_session_name(session) { continue; }
        let mut queue = vec![pane];
        let mut seen = HashSet::new();
        while let Some(pid) = queue.pop() {
            if !seen.insert(pid) { continue; }
            if codex.contains(&pid) {
                result.entry(session.to_string()).or_default().push(pid);
                // A nested `codex exec` belongs to a tool call, not the pane's
                // interactive conversation. Its parent owns the wanted rollout.
                continue;
            }
            queue.extend(children.get(&pid).into_iter().flatten().copied());
        }
    }
    result
}

fn open_rollout_paths(pids: &HashSet<u32>) -> HashMap<u32, Vec<PathBuf>> {
    let mut result: HashMap<u32, Vec<PathBuf>> = HashMap::new();
    #[cfg(target_os = "linux")]
    for &pid in pids {
        let Ok(entries) = std::fs::read_dir(format!("/proc/{pid}/fd")) else { continue };
        for path in entries.flatten().filter_map(|entry| std::fs::read_link(entry.path()).ok()) {
            if is_codex_rollout_path(&path) {
                result.entry(pid).or_default().push(path);
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    if !pids.is_empty() {
        // macOS has no /proc. One lsof for the whole inventory, restricted to
        // the relevant native processes. Missing tools/permissions degrade to
        // an unknown ID; they never attach another terminal's conversation.
        let ids = pids.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
        if let Ok(out) = hidden_command("lsof").args(["-b", "-n", "-P", "-Fpn", "-p", &ids]).output() {
            let mut pid = None;
            for line in String::from_utf8_lossy(&out.stdout).lines() {
                if let Some(value) = line.strip_prefix('p') {
                    pid = value.parse::<u32>().ok().filter(|p| pids.contains(p));
                } else if let (Some(pid), Some(value)) = (pid, line.strip_prefix('n')) {
                    let path = PathBuf::from(value);
                    if is_codex_rollout_path(&path) {
                        result.entry(pid).or_default().push(path);
                    }
                }
            }
        }
    }
    result
}

fn is_codex_rollout_path(path: &std::path::Path) -> bool {
    path.file_name().and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("rollout-") && n.ends_with(".jsonl"))
}

fn unique_codex_rollout_session_id<'a>(paths: impl Iterator<Item = &'a PathBuf>) -> Option<String> {
    use std::io::BufRead;
    let mut ids = HashSet::new();
    for path in paths {
        if !is_codex_rollout_path(path) { continue; }
        let Ok(file) = std::fs::File::open(path) else { continue };
        // Only session metadata: never scan conversation bodies or unbounded
        // files. Codex subagent rollouts share the process but have source
        // {subagent: ...}; they must not become the terminal's title. Top-level
        // sources are strings; a resumed session keeps its original source
        // (e.g. vscode or exec) even when the current process is the CLI.
        let mut line = String::new();
        if std::io::BufReader::new(file.take(64 * 1024)).read_line(&mut line).is_err() {
            continue;
        }
        let Ok(record) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        let top_level = record["payload"]["source"].as_str()
            .is_some_and(|source| !source.is_empty() && !source.eq_ignore_ascii_case("subagent"));
        if record["type"] != "session_meta" || !top_level {
            continue;
        }
        let Some(id) = record["payload"]["id"].as_str() else { continue };
        let valid_id = id.len() == 36 && id.bytes().enumerate().all(|(i, c)| {
            if [8, 13, 18, 23].contains(&i) { c == b'-' } else { c.is_ascii_hexdigit() }
        });
        if valid_id && path.file_name().and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with(&format!("-{id}.jsonl"))) {
            ids.insert(id.to_string());
        }
    }
    (ids.len() == 1).then(|| ids.into_iter().next()).flatten()
}

/// Like [`list_sessions`], but distinguishes "tmux answered" from "couldn't
/// even run tmux" (`None`) — [`sweep_stale`] must not treat a transient spawn
/// failure as "every session is gone" and reap the whole registry.
pub fn list_sessions_checked() -> Option<Vec<SessionInfo>> {
    // The wire carries ONLY safe alphabets — a minted [a-z0-9-] name and two
    // numeric fields, space-separated — so no locale, sanitizer, tmux version,
    // or user session name can corrupt the parse. Free text used to ride this
    // line and got ASCII-mangled on locale-less hosts; it lives in the session
    // registry now (see `mod registry`).
    //
    // `window_activity`, not `session_activity`: the latter is frozen unless a
    // client is attached, but the rail's unread dot is for *background*
    // (unattached) sessions — so it must reflect raw pane output. Our sessions
    // are single-window by construction, so the current window's activity is the
    // session's activity.
    let out = tmux()
        .args(["list-sessions", "-F", "#{session_name} #{window_activity} #{@ass_bell_at}"])
        .output()
        .ok()?;
    // Non-zero exit just means "no tmux server running yet" → no sessions.
    if !out.status.success() {
        return Some(vec![]);
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut sessions = Vec::new();
    for line in text.lines() {
        let mut f = line.split_whitespace();
        let Some(name) = f.next() else { continue };
        if !valid_session_name(name) {
            continue; // not ours (user sessions can be named anything)
        }
        let activity = f.next().unwrap_or("").to_string();
        // Empty (→ "0" on the client) for sessions created before the bell
        // hook existed, or any that haven't belled yet.
        let bell_at = f.next().unwrap_or("").to_string();
        let meta = registry::read(name).or_else(|| backfill_legacy(name)).unwrap_or_default();
        sessions.push(SessionInfo {
            id: name.to_string(),
            label: if meta.label.is_empty() { name.to_string() } else { meta.label },
            agent: meta.agent,
            cwd: meta.cwd,
            created: meta.created,
            activity,
            bell_at,
            // Empty for shells / resumed / pre-existing sessions (no forced id).
            session_id: meta.session_id,
        });
    }
    Some(sessions)
}

/// One-time migration: a session created by an older backend has its metadata
/// only in `@ass_*` options. Read them once (tab-framed; the `-u` on [`tmux`]
/// keeps the bytes faithful), persist to the registry, and never ask tmux for
/// free text again.
fn backfill_legacy(name: &str) -> Option<registry::Meta> {
    let fmt = format!(
        "#{{@ass_label}}{SEP}#{{@ass_agent}}{SEP}#{{@ass_cwd}}{SEP}#{{@ass_created}}{SEP}#{{@ass_session_id}}"
    );
    let out = tmux().args(["display-message", "-p", "-t", name, &fmt]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let f: Vec<&str> = text.lines().next().unwrap_or("").split(SEP).collect();
    let g = |i: usize| f.get(i).copied().unwrap_or("").to_string();
    let meta = registry::Meta { label: g(0), agent: g(1), cwd: g(2), created: g(3), session_id: g(4) };
    if meta.label.is_empty() && meta.agent.is_empty() && meta.cwd.is_empty() {
        return None; // nothing stored — nothing worth persisting
    }
    // Persist only a provably SETTLED snapshot: an old backend writes the
    // options in several tmux calls right after new-session, and a poll can
    // land mid-create — persisting that partial read would freeze it forever
    // (the registry is write-once; a successful read ends backfilling). All
    // options land within milliseconds of `created`, so a stamp ≥10s old means
    // the set is complete. Until then the partial meta is display-only and the
    // next poll re-reads.
    let settled = meta
        .created
        .parse::<u64>()
        .ok()
        .zip(SystemTime::now().duration_since(UNIX_EPOCH).ok())
        .map(|(c, now)| now.as_secs().saturating_sub(c) >= 10)
        .unwrap_or(false);
    if settled {
        if let Err(e) = registry::write(name, &meta) {
            log::warn!("couldn't backfill terminal registry for {name}: {e}");
        }
    }
    Some(meta)
}

fn session_exists(id: &str) -> bool {
    tmux()
        .args(["has-session", "-t", id])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Kill a session (idempotent — a missing session is treated as success).
/// Loud on any real failure: a swallowed error here once left a ✕ that did
/// nothing (the locale bug made ids unmatchable). The goal state is "no such
/// session", so killing an already-gone session stays Ok.
pub fn kill_session(id: &str) -> Result<(), String> {
    if !id.starts_with(PREFIX) {
        return Err("Invalid terminal id.".into());
    }
    let out = tmux().args(["kill-session", "-t", id]).output();
    match out {
        Ok(o) if o.status.success() => {
            registry::remove(id);
            Ok(())
        }
        _ if !session_exists(id) => {
            registry::remove(id);
            Ok(())
        }
        Ok(o) => Err(format!(
            "Couldn't close the session: {}",
            String::from_utf8_lossy(&o.stderr).trim()
        )),
        Err(e) => Err(format!("Couldn't close the session: {e}")),
    }
}

/// How long a finished (agent-exited), unattached session sticks around before
/// the GC may reap it: a week, so a run that finishes Friday night is still
/// reviewable well past the weekend. Live agents are never reaped regardless.
const GC_IDLE_SECS: u64 = 7 * 24 * 3600;

/// Garbage-collect stale terminals. Run at backend startup. This is the ONLY
/// automatic reaping — sessions deliberately outlive their creating backend
/// (see the module docs), so the GC's bar is high: a session is stale only if
/// it is unattached, every pane is back at a plain shell (the agent exited),
/// and nothing has touched it for [`GC_IDLE_SECS`].
pub fn sweep_stale() {
    // `checked`: if tmux couldn't even run, skip — an empty list here must mean
    // "the sessions are really gone", or the orphan sweep would eat live
    // sessions' metadata on a transient spawn failure.
    let Some(sessions) = list_sessions_checked() else { return };
    let mut live = HashSet::new();
    for s in sessions {
        if !gc_session_if_stale(&s.id, GC_IDLE_SECS) {
            live.insert(s.id);
        }
    }
    registry::sweep_orphans(&live);
}

/// Reap `id` iff stale (see [`sweep_stale`]); returns whether it was reaped.
/// Per-session so tests can target their own sessions with a zero cutoff
/// without sweeping a developer's real terminals.
fn gc_session_if_stale(id: &str, idle_secs: u64) -> bool {
    // Space-separated numeric fields: nothing on this line a sanitizer can touch.
    let fmt = "#{session_attached} #{session_activity}";
    let Ok(out) = tmux().args(["display-message", "-p", "-t", id, fmt]).output() else {
        return false;
    };
    if !out.status.success() {
        return false; // session gone (or tmux unhappy) — nothing to do
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut f = text.split_whitespace();
    let attached = f.next().unwrap_or("1");
    let activity: u64 = f.next().and_then(|s| s.parse().ok()).unwrap_or(u64::MAX);
    if attached != "0" {
        return false; // someone is looking at it
    }
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    if now.saturating_sub(activity) < idle_secs {
        return false; // touched too recently
    }
    if !all_panes_are_shells(id) {
        return false; // the agent (or some process) is still running
    }
    log::info!("reaping stale terminal {id} (agent exited, idle)");
    let _ = kill_session(id);
    true
}

/// True when the session's agent has exited and only shell prompts remain.
/// Used by mining to tell "the run's TUI is gone" apart from "still open"
/// (the launch line ends in `; exec bash -l`, so the session itself lives on).
pub fn agent_exited(id: &str) -> bool {
    all_panes_are_shells(id)
}

/// True when every pane is a plain shell at rest — i.e. the agent (and
/// anything the user ran) has exited and only prompts remain.
///
/// tmux's `#{pane_current_command}` is not enough on its own: panes are
/// spawned as `bash -lc "<agent>; exec bash -l"`, and a non-interactive bash
/// has no job control, so the tty's foreground process group — which is what
/// tmux reports — stays "bash" for the agent's whole lifetime. A pane only
/// counts as at-rest when its process is a shell AND that shell has no
/// non-shell descendants (the agent pipeline, a command the user typed, …).
fn all_panes_are_shells(id: &str) -> bool {
    let Ok(out) = tmux().args(["list-panes", "-s", "-t", id, "-F", "#{pane_pid}"]).output() else {
        return false;
    };
    if !out.status.success() {
        return false;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let pane_pids: Vec<u32> = text.split_whitespace().filter_map(|p| p.parse().ok()).collect();
    if pane_pids.is_empty() {
        return false;
    }

    // One snapshot of the process table (Linux and macOS both take -eo with
    // empty headers). comm is a bare name on Linux and may be a full path on
    // macOS; is_shell() compares the basename.
    let Ok(ps) = hidden_command("ps").args(["-eo", "pid=,ppid=,comm="]).output() else {
        return false;
    };
    if !ps.status.success() {
        return false;
    }
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut comm: HashMap<u32, String> = HashMap::new();
    for line in String::from_utf8_lossy(&ps.stdout).lines() {
        let mut f = line.split_whitespace();
        let (Some(pid), Some(ppid)) = (
            f.next().and_then(|s| s.parse::<u32>().ok()),
            f.next().and_then(|s| s.parse::<u32>().ok()),
        ) else {
            continue;
        };
        children.entry(ppid).or_default().push(pid);
        comm.insert(pid, f.collect::<Vec<_>>().join(" "));
    }

    for pane in pane_pids {
        let Some(name) = comm.get(&pane) else {
            return false; // pane process vanished mid-probe; try again later
        };
        if !is_shell(name) {
            return false; // the pane itself was exec'd into something else
        }
        let mut queue: Vec<u32> = children.get(&pane).cloned().unwrap_or_default();
        while let Some(pid) = queue.pop() {
            if !comm.get(&pid).map(|n| is_shell(n)).unwrap_or(true) {
                return false; // a live non-shell descendant: the agent is working
            }
            queue.extend(children.get(&pid).cloned().unwrap_or_default());
        }
    }
    true
}

/// Shell-name check for process `comm` values: basename'd (macOS reports full
/// paths) and stripped of the login-shell `-` prefix.
fn is_shell(comm: &str) -> bool {
    const SHELLS: [&str; 8] = ["bash", "sh", "zsh", "fish", "dash", "ash", "ksh", "tcsh"];
    let name = comm.trim().rsplit('/').next().unwrap_or("").trim_start_matches('-');
    SHELLS.contains(&name)
}

/// A random-enough UUIDv4 string, std-only (no extra deps). Uniqueness — not
/// unpredictability — is the requirement: it becomes the agent's transcript
/// filename via `--session-id`, so it only has to be well-formed and not collide
/// with an existing one. SipHash over (nanos, seq, pid) gives that.
fn new_uuid() -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let seq = UUID_SEQ.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let mk = |salt: u8| {
        let mut h = DefaultHasher::new();
        (nanos, seq, pid, salt).hash(&mut h);
        h.finish()
    };
    let mut b = [0u8; 16];
    b[..8].copy_from_slice(&mk(0xA5).to_le_bytes());
    b[8..].copy_from_slice(&mk(0x5A).to_le_bytes());
    b[6] = (b[6] & 0x0F) | 0x40; // version 4
    b[8] = (b[8] & 0x3F) | 0x80; // variant 10xx
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15],
    )
}

/// Current Claude rejects a forced --session-id with resume/continue unless the
/// session is being forked. Respect explicit identities and record exact resume
/// UUIDs without changing the user's launch semantics.
fn configure_claude_session(argv: &mut Vec<String>, extra: &[String]) -> Option<String> {
    let options = &extra[..extra.iter().position(|arg| arg == "--").unwrap_or(extra.len())];
    let mut explicit = None;
    let mut resume = None;
    let mut continuing = false;
    let mut fork = false;
    for (index, arg) in options.iter().enumerate() {
        let (flag, inline) = if let Some(value) = arg.strip_prefix("-r").filter(|value| !value.is_empty()) {
            ("-r", Some(value.strip_prefix('=').unwrap_or(value)))
        } else {
            arg.split_once('=').map_or((arg.as_str(), None), |(flag, value)| (flag, Some(value)))
        };
        let value = || inline.or_else(|| options.get(index + 1).map(String::as_str));
        match flag {
            "--session-id" => explicit = Some(value()),
            "--resume" | "-r" => resume = Some(value()),
            "--continue" | "-c" => continuing = true,
            "--fork-session" => fork = true,
            _ => {}
        }
    }
    let uuid = |value: &str| {
        (value.len() == 36 && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) { byte == b'-' } else { byte.is_ascii_hexdigit() }
        })).then(|| value.to_string())
    };
    if let Some(id) = explicit {
        return id.and_then(uuid);
    }
    if !fork && (continuing || resume.is_some()) {
        return if continuing { None } else { resume.flatten().and_then(uuid) };
    }
    let id = new_uuid();
    argv.extend(["--session-id".into(), id.clone()]);
    Some(id)
}

/// Create a detached tmux session running the chosen agent in `cwd`, tagged so
/// it can be listed from any backend. The session is persistent: nothing about
/// it dies with this process (see the module docs for the lifetime model).
#[allow(clippy::too_many_arguments)]
pub fn create_session(
    agent_id: &str,
    cwd: &str,
    cols: u16,
    rows: u16,
    ide: bool,
    skip_permissions: bool,
    auto_mode: bool,
    extra_args: &[String],
) -> Result<SessionInfo, String> {
    let opt = detect_agents()
        .into_iter()
        .find(|a| a.id == agent_id)
        .ok_or_else(|| format!("Unknown agent option: {agent_id}"))?;

    // Build the agent argv (empty for a plain shell).
    let mut argv: Vec<String> = Vec::new();
    // The agent session id we force at launch, recorded on the terminal so title
    // extraction can map it to its OWN transcript (see create_session_inner).
    let mut session_id: Option<String> = None;
    if opt.agent != "shell" {
        argv.push(opt.bin.clone());
        if opt.agent == "claude" {
            if ide {
                argv.push("--ide".into());
            }
            // Auto mode and skip-permissions are mutually exclusive (the flags
            // conflict); the UI enforces this, but prefer auto if both arrive.
            if auto_mode {
                argv.push("--permission-mode".into());
                argv.push("auto".into());
            } else if skip_permissions {
                argv.push("--dangerously-skip-permissions".into());
            }
            // Ring the terminal bell when a turn finishes, so the rail's unread
            // dot lights only when Claude is done and waiting (the alert-bell hook
            // in create_session_inner turns that bell into the dot). `--settings`
            // layers this for the launch only — the user's settings.json is left
            // untouched.
            argv.push("--settings".into());
            argv.push(r#"{"preferredNotifChannel":"terminal_bell"}"#.into());
            // Force a known session id so THIS terminal resolves to exactly its own
            // ~/.claude/projects/<enc-cwd>/<uuid>.jsonl. Without it, several Claude
            // sessions in one cwd all resolve to the newest transcript and show a
            // single shared title. Claude names the file after the id we pass.
            session_id = configure_claude_session(&mut argv, extra_args);
        } else if opt.agent == "codex" {
            // Same goal for Codex: force a real BEL on turn-completion. Its default
            // `auto` method prefers an OSC-9 desktop notification tmux's bell
            // monitor can't see, so pin `method=bel`; `condition=always` drops the
            // focus gate (xterm has no audible bell, and the active terminal never
            // dots anyway). Override syntax mirrors the existing effort override.
            argv.push("-c".into());
            argv.push("tui.notifications=true".into());
            argv.push("-c".into());
            argv.push(r#"tui.notification_method="bel""#.into());
            argv.push("-c".into());
            argv.push(r#"tui.notification_condition="always""#.into());
        }
        for a in extra_args {
            if !a.trim().is_empty() {
                argv.push(a.clone());
            }
        }
    }
    let agent_cmd = argv
        .iter()
        .map(|a| shell_quote(a))
        .collect::<Vec<_>>()
        .join(" ");
    create_session_inner(&opt, cwd, cols, rows, agent_cmd, session_id.as_deref())
}

/// Create a session that RESUMES the agent's most recent conversation in
/// `cwd`: the agent registry's resume line is cwd-scoped (claude
/// `--continue`, codex `resume --last`), so spawning it in the run dir
/// reopens that run's conversation. The programmatic counterpart of
/// `create_session` — exposed on the terminal API as a `resume` flag with
/// deliberately no dialog UI; skill mining's "continue the conversation" is
/// the caller today.
pub fn create_session_resume(
    agent_id: &str,
    cwd: &str,
    cols: u16,
    rows: u16,
    model: Option<&str>,
    effort: Option<&str>,
) -> Result<SessionInfo, String> {
    let opt = detect_agents()
        .into_iter()
        .find(|a| a.id == agent_id)
        .ok_or_else(|| format!("Unknown agent option: {agent_id}"))?;
    let resume = skill_core::agents::by_family(&opt.agent)
        .and_then(|d| d.resume)
        .ok_or_else(|| format!("{} can't resume a recorded session yet.", opt.label))?;
    let cmd = resume(&skill_core::agents::ResumeCtx { bin: &opt.bin, model, effort });
    // No forced session id on resume: `--continue` reopens the existing transcript
    // (which is also the newest, so the fallback resolves it correctly).
    create_session_inner(&opt, cwd, cols, rows, cmd, None)
}

/// Create a session whose agent command is a caller-built shell LINE — e.g.
/// an agent TUI launched with an initial prompt (skill mining's run). The
/// caller is responsible for quoting; the secrets env sourcing and the
/// keep-alive shell wrapper still apply.
pub fn create_session_cmd(
    agent_id: &str,
    cwd: &str,
    cols: u16,
    rows: u16,
    cmd: &str,
) -> Result<SessionInfo, String> {
    let opt = detect_agents()
        .into_iter()
        .find(|a| a.id == agent_id)
        .ok_or_else(|| format!("Unknown agent option: {agent_id}"))?;
    create_session_inner(&opt, cwd, cols, rows, cmd.to_string(), None)
}

fn create_session_inner(
    opt: &AgentOption,
    cwd: &str,
    cols: u16,
    rows: u16,
    agent_cmd: String,
    session_id: Option<&str>,
) -> Result<SessionInfo, String> {
    let cwd_resolved = session_start::resolve_directory(cwd)?;
    let startup = session_start::Startup::new()?;

    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let owner = std::process::id();
    // The pid is part of the name because the seq counter is per-process: two
    // backends creating a terminal in the same second would otherwise both
    // mint `ass-<secs>-0` and the second create would fail.
    let name = format!("{PREFIX}{owner}-{secs}-{seq}");

    // Source the managed-secrets env file (the same one the `load-secrets`
    // activation skill reads) before the agent starts, so skills that need
    // credentials find them in the environment without an activation step.
    // The `[ -f ]` guard makes a missing/empty store a silent no-op.
    let env_source = skill_core::secrets::env_path()
        .map(|p| {
            let q = shell_quote(&p.to_string_lossy());
            format!("[ -f {q} ] && . {q}; ")
        })
        .unwrap_or_default();

    // `; exec bash -l` keeps the pane (and the agent's scrollback) alive after
    // the agent exits, so a finished run stays reviewable from any client —
    // the GC only collects it once it's been idle for a week (see sweep_stale).
    let line = if agent_cmd.is_empty() {
        format!("{env_source}exec bash -l")
    } else {
        format!("{env_source}{agent_cmd}; exec bash -l")
    };
    let line = format!("{}{}{line}", startup.shell_prefix(&cwd_resolved), shell_open_file_limit());
    let bootstrap = startup.bootstrap(&cwd_resolved, &line);

    let (cols, rows) = size_floor("create", &name, cols, rows);
    let cols_s = cols.to_string();
    let rows_s = rows.to_string();
    // Create the session around a short-lived stub window first: `history-limit`
    // is captured per-window at creation time, so the session options must be in
    // place *before* the real agent window exists. `-P -F` prints the stub's
    // global window id (`@N`) so exactly that window can be dropped afterwards
    // (immune to `base-index` / `renumber-windows` in the user's tmux config).
    let out = tmux()
        .args([
            "new-session", "-d", "-s", &name, "-x", &cols_s, "-y", &rows_s, "-c", "/",
            "-P", "-F", "#{window_id}", "sleep", "60",
        ])
        .output()
        .map_err(tmux_spawn_err)?;
    if !out.status.success() {
        log::error!("tmux new-session failed (agent={})", opt.agent);
        return Err("tmux couldn't create the session.".into());
    }
    let stub = String::from_utf8_lossy(&out.stdout).trim().to_string();

    // Server-scoped, set while the server is guaranteed alive: several backends
    // (desktop, daemon, dev) share one tmux server, and the default `exit-empty
    // on` kills it — everyone's sessions — when ONE backend's last session
    // exits (a daemon legitimately holds zero between agent runs). The cost is
    // an idle tmux server lingering after the user kills their last terminal.
    let _ = tmux().args(["set-option", "-s", "exit-empty", "off"]).output();

    let label = format!("{} · {}", opt.label, basename(&cwd_resolved));
    // The registry is the metadata source of truth (see `mod registry`); a
    // failed write degrades to a bare-name rail row, never a failed create.
    let meta = registry::Meta {
        label: label.clone(),
        agent: opt.agent.clone(),
        cwd: cwd_resolved.clone(),
        created: secs.to_string(),
        session_id: session_id.unwrap_or_default().to_string(),
    };
    if let Err(e) = registry::write(&name, &meta) {
        log::warn!("terminal registry write failed for {name}: {e}");
    }
    // Legacy `@ass_*` copies: only OLDER backends read these now (dev and
    // installed builds share the machine's tmux server across an auto-update
    // window) — drop after a few releases. Sanitized because the old readers
    // parse tab-separated list-sessions output.
    let set = |k: &str, v: &str| {
        let _ = tmux().args(["set-option", "-t", &name, k, &sanitize_meta(v)]).output();
    };
    set("@ass_label", &label);
    set("@ass_agent", &opt.agent);
    set("@ass_cwd", &cwd_resolved);
    set("@ass_created", &secs.to_string());
    // The forced agent session id (claude), so title extraction maps this terminal
    // to its own transcript; absent for shells / resumes / pre-existing sessions.
    if let Some(sid) = session_id {
        set("@ass_session_id", sid);
    }
    // Informational only (provenance for debugging) — sessions deliberately
    // outlive their creator, so nothing keys lifecycle off this anymore.
    set("@ass_owner_pid", &owner.to_string());
    set("status", "off"); // clean embed — no tmux status bar
    // Scrolling happens in *tmux's* history (the UI's terminal only ever sees the
    // alternate screen): `mouse on` turns wheel events into copy-mode scrolling.
    // Session-scoped, so the user's own tmux sessions keep their settings.
    set("mouse", "on");
    set("history-limit", "10000");

    let launched = tmux()
        .args(["new-window", "-t", &name, "-c", "/", "/bin/sh", "-c", &bootstrap])
        .output();
    let started = match launched {
        Ok(output) if output.status.success() => startup.wait(&cwd_resolved),
        Ok(output) => Err(format!("tmux couldn't create the session: {}", String::from_utf8_lossy(&output.stderr).trim())),
        Err(error) => Err(tmux_spawn_err(error)),
    };
    if let Err(error) = started {
        let _ = kill_session(&name);
        log::error!("tmux new-window failed (agent={}, cwd={cwd_resolved})", opt.agent);
        return Err(error);
    }
    let _ = tmux().args(["kill-window", "-t", &stub]).output();

    // Unread-dot signal: light the rail dot only when the agent FINISHES a turn,
    // not on every line an idle TUI repaints. The agent is launched to ring the
    // terminal BELL on turn-completion (see create_session); tmux `monitor-bell`
    // catches it and this `alert-bell` hook stamps the bell's timestamp into
    // `@ass_bell_at`, which list_sessions reports and the rail compares against
    // "last viewed". monitor-bell is a WINDOW option, so it must be set now — on
    // the real agent window (the session-option set()s above ran while only the
    // throwaway stub window existed). set-option can't expand #{...} in a value,
    // so run-shell does the expansion at bell time.
    let _ = tmux().args(["set-option", "-t", &name, "monitor-bell", "on"]).output();
    let _ = tmux().args(["set-option", "-t", &name, "@ass_bell_at", "0"]).output();
    // The path is double-quoted inside the single-quoted run-shell body: the
    // bundled sidecar lives under the .app, whose install path may contain spaces.
    let bell_hook = format!(
        "run-shell '\"{tmux}\" set-option -t {name} @ass_bell_at \"#{{window_activity}}\"'",
        tmux = tmux_bin(),
    );
    let _ = tmux().args(["set-hook", "-t", &name, "alert-bell", &bell_hook]).output();

    Ok(SessionInfo {
        id: name,
        label,
        agent: opt.agent.clone(),
        cwd: cwd_resolved,
        created: secs.to_string(),
        // A just-created session's last activity is its creation time.
        activity: secs.to_string(),
        // No bell yet — the dot stays dark until the agent finishes a turn.
        bell_at: "0".into(),
        session_id: session_id.unwrap_or_default().to_string(),
    })
}

// ──────────────────────────── attach / stream I/O ────────────────────────────

mod attachments;
pub use attachments::{attach, detach, resize, resize_attachment, write, write_attachment, Attachment};

// ───────────────────────────── pasted images ─────────────────────────────

static PASTE_SEQ: AtomicU64 = AtomicU64::new(0);

/// Where pasted clipboard images land: a per-user dir on the machine the agents
/// run on, so the returned path is readable from inside any session. The user's
/// cache dir, NOT the shared /tmp — a predictable name in a world-writable dir
/// invites symlink games on multi-user hosts (and [`sweep_old_pastes`] deletes
/// in here, which must never be redirectable by another user).
fn paste_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("vibestudio")
        .join("pastes")
}

/// Best-effort: drop pasted images older than a day so the dir can't grow
/// without bound under a long-lived backend.
fn sweep_old_pastes(dir: &std::path::Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let cutoff = SystemTime::now() - std::time::Duration::from_secs(24 * 3600);
    for e in rd.flatten() {
        let stale = e
            .metadata()
            .and_then(|m| m.modified())
            .map(|t| t < cutoff)
            .unwrap_or(false);
        if stale {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Save a pasted clipboard image (base64) to a temp file and return its absolute
/// path. This is how images cross the client/server boundary: the agent may run
/// on a different machine than the user's clipboard (remote SSH), so "paste"
/// must materialize the bytes server-side and hand back a path — the same shape
/// drag-and-drop produces in a native terminal.
pub fn save_pasted_image(data_b64: &str, mime: &str) -> Result<String, String> {
    let ext = match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/bmp" => "bmp",
        _ => return Err(format!("Unsupported image type: {mime}")),
    };
    let bytes = b64_decode(data_b64);
    if bytes.is_empty() {
        return Err("The pasted image was empty.".into());
    }
    const MAX_BYTES: usize = 32 * 1024 * 1024;
    if bytes.len() > MAX_BYTES {
        return Err("The pasted image is too large (max 32 MB).".into());
    }
    let dir = paste_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("Couldn't create the paste dir: {e}"))?;
    sweep_old_pastes(&dir);
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let seq = PASTE_SEQ.fetch_add(1, Ordering::Relaxed);
    // pid in the name: temp dirs are shared, and two backends could both be at
    // seq 0 in the same second.
    let path = dir.join(format!("image-{}-{secs}-{seq}.{ext}", std::process::id()));
    std::fs::write(&path, &bytes).map_err(|e| format!("Couldn't save the image: {e}"))?;
    Ok(path.to_string_lossy().into_owned())
}

// ───────────────────────────── agent detection ─────────────────────────────

enum ExtRel {
    /// A fixed relative file inside the extension dir.
    File(&'static str),
    /// A `<dir>/<arch>/<file>` layout — glob the single arch subdir.
    GlobDir { dir: &'static str, file: &'static str },
}

struct Spec {
    agent: &'static str,
    label: &'static str,
    path_name: &'static str,
    supports_ide: bool,
    ext_prefix: &'static str,
    ext_rel: ExtRel,
    /// Fixed, non-PATH install locations to also probe (agent-specific; `~` ok).
    /// Catches installs that aren't on the current shell's PATH — native
    /// standalone, the curl-installer dir, a different node manager, etc.
    cli_paths: &'static [&'static str],
    /// Env var naming an install dir to also probe (`<dir>/<name>`); "" if none.
    install_dir_env: &'static str,
}

fn agent_specs() -> Vec<Spec> {
    vec![
        Spec {
            agent: "claude",
            label: "Claude Code",
            path_name: "claude",
            supports_ide: true,
            ext_prefix: "anthropic.claude-code-",
            ext_rel: ExtRel::File("resources/native-binary/claude"),
            // The native installer's current target (~/.local/bin/claude) and
            // its older location; covers shells whose login PATH we can't read.
            cli_paths: &["~/.local/bin/claude", "~/.claude/local/claude"],
            install_dir_env: "",
        },
        Spec {
            agent: "codex",
            label: "Codex",
            path_name: "codex",
            supports_ide: false,
            ext_prefix: "openai.chatgpt-",
            ext_rel: ExtRel::GlobDir {
                dir: "bin",
                file: "codex",
            },
            // The native/standalone managed install (curl installer / IDE-managed).
            cli_paths: &["~/.codex/packages/standalone/current/codex"],
            install_dir_env: "CODEX_INSTALL_DIR",
        },
        Spec {
            agent: "opencode",
            label: "opencode",
            path_name: "opencode",
            supports_ide: false,
            // No CLI-bundling editor extension to probe; an empty prefix is the
            // signal `ext_finds` uses to skip the editor-roots scan entirely.
            ext_prefix: "",
            ext_rel: ExtRel::File(""),
            // The install script's default target ($HOME/.opencode/bin); other
            // install paths land on PATH or in the dirs `resolve_cli` already probes.
            cli_paths: &["~/.opencode/bin/opencode"],
            install_dir_env: "OPENCODE_INSTALL_DIR",
        },
    ]
}

fn editor_roots() -> Vec<(&'static str, &'static str)> {
    vec![
        ("VS Code", "~/.vscode-server/extensions"),
        ("VS Code", "~/.vscode/extensions"),
        ("Cursor", "~/.cursor/extensions"),
        ("Cursor", "~/.cursor-server/extensions"),
        ("VS Code Insiders", "~/.vscode-insiders/extensions"),
    ]
}

fn slug(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Best-effort version from `<bin> --version` (first dotted, digit-leading token),
/// bounded by a timeout so a hung/misbehaving binary can't freeze agent detection.
fn bin_version(bin: &str) -> Option<String> {
    use std::process::Stdio;
    let mut child = hidden_command(bin)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(_) => return None,
        }
    }
    // `--version` output is tiny, so the pipe never blocked; read what's buffered.
    let mut text = String::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_string(&mut text);
    }
    if text.trim().is_empty() {
        if let Some(mut err) = child.stderr.take() {
            let _ = err.read_to_string(&mut text);
        }
    }
    text.split_whitespace()
        .find(|t| {
            t.contains('.') && t.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false)
        })
        .map(|s| {
            s.trim_matches(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '-'))
                .to_string()
        })
}

/// Locate an agent's extension-bundled binary across known editor roots.
/// Returns `(editor_label, abs_path)` pairs, deduped by path, latest version first.
fn ext_finds(spec: &Spec) -> Vec<(&'static str, String)> {
    // An empty prefix would `starts_with`-match every extension dir; agents with
    // no CLI-bundling editor extension opt out of the scan this way.
    if spec.ext_prefix.is_empty() {
        return Vec::new();
    }
    let mut found = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (label, root) in editor_roots() {
        let dir = skill_core::pathsafe::resolve_root(root);
        let rd = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let mut names: Vec<String> = rd
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(spec.ext_prefix))
            .collect();
        names.sort();
        names.reverse(); // lexicographically-latest version dir first
        for n in names {
            let base = dir.join(&n);
            let bin = match &spec.ext_rel {
                ExtRel::File(rel) => {
                    let p = base.join(rel);
                    p.is_file().then_some(p)
                }
                ExtRel::GlobDir { dir: sub, file } => std::fs::read_dir(base.join(sub))
                    .ok()
                    .and_then(|rd| {
                        rd.filter_map(|e| e.ok())
                            .map(|e| e.path().join(file))
                            .find(|p| p.is_file())
                    }),
            };
            if let Some(p) = bin {
                let ps = p.to_string_lossy().into_owned();
                if seen.insert(ps.clone()) {
                    found.push((label, ps));
                }
                break; // first (latest) hit in this root
            }
        }
    }
    found
}

fn compute_agents() -> Vec<AgentOption> {
    let mut out = Vec::new();

    // A plain login shell is always offered.
    out.push(AgentOption {
        id: "shell".into(),
        agent: "shell".into(),
        label: "Shell".into(),
        flavor: "shell".into(),
        flavor_label: "bash".into(),
        bin: which("bash").unwrap_or_else(|| "/bin/bash".to_string()),
        version: None,
        supports_ide: false,
        can_mine: false,
    });

    for spec in agent_specs() {
        if let Some(bin) = resolve_cli(&spec) {
            out.push(AgentOption {
                id: format!("{}:cli", spec.agent),
                agent: spec.agent.into(),
                label: spec.label.into(),
                flavor: "cli".into(),
                flavor_label: "CLI".into(),
                version: bin_version(&bin),
                bin,
                supports_ide: spec.supports_ide,
                can_mine: skill_core::agents::can_launch(spec.agent),
            });
        }
        for (editor, bin) in ext_finds(&spec) {
            out.push(AgentOption {
                id: format!("{}:ext:{}", spec.agent, slug(editor)),
                agent: spec.agent.into(),
                label: spec.label.into(),
                flavor: "extension".into(),
                flavor_label: format!("{editor} extension"),
                version: bin_version(&bin),
                bin,
                supports_ide: spec.supports_ide,
                can_mine: skill_core::agents::can_launch(spec.agent),
            });
        }
    }
    out
}

/// Detected launchable agents (computed once per process — agents rarely change
/// mid-session, and `<bin> --version` probes are relatively slow).
pub fn detect_agents() -> Vec<AgentOption> {
    static CACHE: OnceLock<Vec<AgentOption>> = OnceLock::new();
    CACHE.get_or_init(compute_agents).clone()
}

// ─────────────────────────────────── tests ───────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_session_ids_do_not_conflict_with_resume_or_explicit_ids() {
        let id = "12345678-1234-4234-8234-123456789ABC";
        let configure = |extra: &[&str]| {
            let mut argv = vec![];
            let extra: Vec<_> = extra.iter().map(|arg| arg.to_string()).collect();
            let recorded = configure_claude_session(&mut argv, &extra);
            (argv, recorded)
        };
        for extra in [&["--continue"][..], &["-c"], &["--resume"], &["-r", "Task name"]] {
            assert_eq!(configure(extra), (vec![], None));
        }
        for extra in [vec!["--resume", id], vec!["--session-id", id]] {
            assert_eq!(configure(&extra), (vec![], Some(id.to_string())));
        }
        assert_eq!(configure(&[&format!("--resume={id}")]), (vec![], Some(id.to_string())));
        assert_eq!(configure(&[&format!("-r{id}")]), (vec![], Some(id.to_string())));
        assert_eq!(configure(&[&format!("--session-id={id}")]), (vec![], Some(id.to_string())));
        for extra in [&[][..], &["--continue", "--fork-session"], &["--", "--resume"]] {
            let (argv, recorded) = configure(extra);
            assert_eq!(argv, vec!["--session-id".into(), recorded.unwrap()]);
        }
        assert_eq!(configure(&["--session-id", "../invalid"]), (vec![], None));
    }

    #[test]
    fn codex_processes_are_scoped_to_their_own_terminal() {
        let sessions = codex_pids_by_session(
            "ass-1-1-1 10\nass-1-1-2 20\nass-1-1-3 30\nnot-ours 40\n",
            "10 1 bash\n11 10 node\n12 11 codex\n13 12 codex\n\
             20 1 bash\n21 20 /opt/codex\n30 1 bash\n31 30 sleep\n\
             40 1 codex\n99 1 codex\n",
        );
        assert_eq!(sessions.get("ass-1-1-1"), Some(&vec![12]));
        assert_eq!(sessions.get("ass-1-1-2"), Some(&vec![21]));
        assert!(!sessions.contains_key("ass-1-1-3"));
        assert_eq!(sessions.len(), 2, "ignore other panes and nested tool-call agents");
    }

    #[test]
    fn codex_rollout_identity_ignores_subagents_and_rejects_ambiguity() {
        let dir = std::env::temp_dir().join(format!("vs-rollout-{}", new_uuid()));
        std::fs::create_dir(&dir).unwrap();
        let write = |id: &str, source: serde_json::Value| {
            let path = dir.join(format!("rollout-2026-09-19T10-00-00-{id}.jsonl"));
            std::fs::write(&path, format!("{}\n", serde_json::json!({
                "type": "session_meta",
                "payload": {"id": id, "source": source, "cwd": "/same/project"},
            }))).unwrap();
            path
        };
        let first = write("01a0bd55-10d4-7470-94f1-4c98dd5af803", serde_json::json!("cli"));
        // A CLI resume preserves the source of the original VS Code session.
        let second = write("01a0bd56-01bc-7e30-9c3c-352c4ffdd834", serde_json::json!("vscode"));
        let child = write("01a0bd56-c1f5-72d2-8b90-4faffd31e733", serde_json::json!({
            "subagent": {"thread_spawn": {"parent_thread_id": "01a0bd55-10d4-7470-94f1-4c98dd5af803"}},
        }));
        assert_eq!(
            unique_codex_rollout_session_id([&first, &child, &first].into_iter()),
            Some("01a0bd55-10d4-7470-94f1-4c98dd5af803".into()),
        );
        assert_eq!(
            unique_codex_rollout_session_id([&second, &child].into_iter()),
            Some("01a0bd56-01bc-7e30-9c3c-352c4ffdd834".into()),
        );
        assert_eq!(unique_codex_rollout_session_id([&first, &second, &child].into_iter()), None);
        assert_eq!(unique_codex_rollout_session_id([&child].into_iter()), None);
        let legacy_child = write(&new_uuid(), serde_json::json!("subagent"));
        assert_eq!(unique_codex_rollout_session_id([&legacy_child].into_iter()), None);
        std::fs::write(&first, "malformed\n").unwrap();
        assert_eq!(unique_codex_rollout_session_id([&first].into_iter()), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Re-run stateful cases alone so their config, paste cache and tmux server
    /// belong to the test even when the surrounding suite runs in parallel.
    fn in_isolated_process(name: &str, needs_tmux: bool) -> bool {
        const CHILD: &str = "VIBESTUDIO_TERM_TEST_CHILD";
        if std::env::var(CHILD).as_deref() == Ok(name) {
            return false;
        }

        struct Fixture {
            root: PathBuf,
            tmux: Option<PathBuf>,
        }
        impl Drop for Fixture {
            fn drop(&mut self) {
                if let Some(binary) = &self.tmux {
                    let _ = hidden_command(binary)
                        .env("TMUX_TMPDIR", self.root.join("tmux"))
                        .env_remove("TMUX")
                        .args(["kill-server"])
                        .output();
                }
                let _ = std::fs::remove_dir_all(&self.root);
            }
        }

        let tmux = needs_tmux.then(|| {
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                .map(|dir| dir.join("tmux"))
                .find(|path| path.is_file())
                .expect("terminal integration tests require tmux on PATH")
                .canonicalize().unwrap()
        });
        // Keep Unix socket paths short on macOS too.
        let base = if cfg!(unix) { PathBuf::from("/tmp") } else { std::env::temp_dir() };
        let fixture = Fixture {
            root: base.join(format!("vs-test-{}-{}", std::process::id(), &new_uuid()[..8])),
            tmux,
        };
        std::fs::create_dir(&fixture.root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fixture.root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        for dir in ["home", "config", "cache", "tmux", "bin", "tmp"] {
            std::fs::create_dir(fixture.root.join(dir)).unwrap();
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/bin/bash", fixture.root.join("bin/bash")).unwrap();
            if let Some(binary) = &fixture.tmux {
                std::os::unix::fs::symlink(binary, fixture.root.join("bin/tmux")).unwrap();
            }
        }
        let mut search_path = vec![fixture.root.join("bin")];
        if needs_tmux {
            search_path.extend([PathBuf::from("/usr/bin"), PathBuf::from("/bin")]);
        }
        let search_path = std::env::join_paths(search_path).unwrap();
        std::fs::write(
            fixture.root.join("home/.bash_profile"),
            format!("export PATH={}\n", shell_quote(&search_path.to_string_lossy())),
        ).unwrap();

        let log_path = fixture.root.join("output.log");
        let log = std::fs::File::create(&log_path).unwrap();
        let mut command = hidden_command(std::env::current_exe().unwrap());
        command.env_clear();
        // Windows needs these to load its system libraries in the child.
        for key in ["SystemRoot", "WINDIR"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        let mut child = command
            .args(["--exact", &format!("tests::{name}"), "--include-ignored", "--nocapture"])
            .env(CHILD, name)
            .env("HOME", fixture.root.join("home"))
            .env("USERPROFILE", fixture.root.join("home"))
            .env("XDG_CONFIG_HOME", fixture.root.join("config"))
            .env("APPDATA", fixture.root.join("config"))
            .env("XDG_CACHE_HOME", fixture.root.join("cache"))
            .env("LOCALAPPDATA", fixture.root.join("cache"))
            .env("TMUX_TMPDIR", fixture.root.join("tmux"))
            .env("TMPDIR", fixture.root.join("tmp"))
            .env("PATH", search_path)
            .env("SHELL", "/bin/bash")
            .env("TERM", "xterm-256color")
            .current_dir(&fixture.root)
            .stdin(std::process::Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() { break status; }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("{name} exceeded 30 seconds:\n{}", std::fs::read_to_string(&log_path).unwrap());
            }
            thread::sleep(std::time::Duration::from_millis(20));
        };
        assert!(status.success(), "{name} failed:\n{}", std::fs::read_to_string(log_path).unwrap());
        true
    }

    #[test]
    fn shell_quote_handles_quotes() {
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(shell_quote("plain"), "'plain'");
    }

    #[cfg(unix)]
    #[test]
    fn shell_open_file_limit_preserves_the_hard_cap_and_higher_allowances() {
        // Each shell changes only its own limits, never the test runner's.
        for (soft, hard, expected) in [(256, 8192, 8192), (256, 512, 512), (256, 256, 256), (9000, 9000, 9000)] {
            let script = format!(
                "set -eu; builtin ulimit -Sn {soft} && builtin ulimit -Hn {hard} || exit 77; {}builtin ulimit -Sn; builtin ulimit -Hn",
                shell_open_file_limit()
            );
            let out = hidden_command("bash")
                .args(["--noprofile", "--norc", "-c", &script])
                .env_remove("BASH_ENV")
                .output().unwrap();
            if out.status.code() == Some(77) {
                continue; // This host's inherited hard limit is more restrictive.
            }
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
            let wanted = soft.max(expected.min(skill_core::process::open_file_limit_target()));
            assert_eq!(String::from_utf8_lossy(&out.stdout), format!("{wanted}\n{hard}\n"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn shell_open_file_limit_is_best_effort_with_custom_shell_settings() {
        for setup in [
            // A login rc can define a ulimit wrapper. Query/change the actual
            // process limit, without invoking user wrappers or their output.
            "ulimit() { printf 'custom wrapper\\n'; return 1; };",
            // Even a shell that disables the builtin must still start its agent.
            "enable -n ulimit;",
        ] {
            let script = format!("set -eu; {setup} {}printf 'agent started\\n'", shell_open_file_limit());
            let out = hidden_command("bash")
                .args(["--noprofile", "--norc", "-c", &script])
                .env_remove("BASH_ENV")
                .output().unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
            assert_eq!(out.stdout, b"agent started\n");
        }
    }

    #[cfg(unix)]
    #[test]
    fn new_pane_raises_limit_in_an_existing_low_limit_tmux_server() {
        if hidden_command("tmux").arg("-V").output().is_err() {
            return;
        }
        struct PrivateTmux(std::path::PathBuf);
        impl Drop for PrivateTmux {
            fn drop(&mut self) {
                let _ = hidden_command("tmux").arg("-S").arg(self.0.join("socket"))
                    .arg("kill-server").output();
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        // macOS's default temp directory is already long; keep the socket path
        // comfortably below sockaddr_un's 104-byte limit there.
        let dir = std::env::temp_dir().join(format!("vsfd-{}-{}", std::process::id(), &new_uuid()[..8]));
        std::fs::create_dir(&dir).unwrap();
        let _guard = PrivateTmux(dir.clone());
        let socket = dir.join("socket");
        let output_path = dir.join("limits");
        // The first command starts a separate tmux server with the problematic
        // inherited allowance. A later, unrestricted client cannot repair it.
        let start = hidden_command("bash")
            .args(["--noprofile", "--norc", "-c", "ulimit -Sn 256 && exec \"$@\"", "bash", "tmux", "-S"])
            .arg(&socket)
            .args(["-f", "/dev/null", "new-session", "-d", "-s", "probe", "sleep", "60"])
            .env_remove("BASH_ENV").env_remove("TMUX")
            .output().unwrap();
        assert!(start.status.success(), "{}", String::from_utf8_lossy(&start.stderr));
        let script = format!(
            "before=$(ulimit -Sn); {}printf '%s\\n' \"$before\" \"$(ulimit -Sn)\" \"$(ulimit -Hn)\" > \"$1\"",
            shell_open_file_limit()
        );
        let pane = hidden_command("tmux")
            .arg("-S").arg(&socket)
            .args(["new-window", "-d", "-t", "probe", "bash", "--noprofile", "--norc", "-c", &script, "bash"])
            .arg(&output_path).env_remove("TMUX").output().unwrap();
        assert!(pane.status.success(), "{}", String::from_utf8_lossy(&pane.stderr));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let fields = loop {
            let text = std::fs::read_to_string(&output_path).unwrap_or_default();
            let fields: Vec<_> = text.lines().map(str::to_string).collect();
            if fields.len() == 3 { break fields; }
            assert!(std::time::Instant::now() < deadline, "pane did not report its limits");
            thread::sleep(std::time::Duration::from_millis(10));
        };
        assert_eq!(fields[0], "256");
        let hard = fields[2].parse::<u64>().unwrap_or(u64::MAX);
        assert_eq!(fields[1].parse::<u64>().unwrap(), skill_core::process::open_file_limit_target().min(hard).max(256));
    }

    #[test]
    #[cfg_attr(windows, ignore = "Windows known-folder profile paths cannot be isolated with child environment variables")]
    fn detect_includes_shell() {
        if in_isolated_process("detect_includes_shell", false) { return; }
        assert_eq!(dirs::home_dir(), Some(PathBuf::from(std::env::var_os("HOME").unwrap())));
        assert!(detect_agents().iter().any(|o| o.agent == "shell"));
    }

    #[test]
    #[cfg_attr(windows, ignore = "Windows known-folder profile paths cannot be isolated with child environment variables")]
    fn resume_requires_a_registry_resume_capability() {
        if in_isolated_process("resume_requires_a_registry_resume_capability", false) { return; }
        assert_eq!(dirs::home_dir(), Some(PathBuf::from(std::env::var_os("HOME").unwrap())));
        // "shell" has no agent-registry entry, so the error comes before any
        // tmux work — no session may be spawned for an unresumable agent.
        let err = match create_session_resume("shell", "/tmp", 80, 24, None, None) {
            Err(e) => e,
            Ok(s) => panic!("expected an error, spawned {}", s.id),
        };
        assert!(err.contains("can't resume"), "got: {err}");
    }

    #[test]
    fn basename_trims() {
        assert_eq!(basename("/home/x/proj/"), "proj");
        assert_eq!(basename("/home/x/proj"), "proj");
    }

    #[test]
    fn bundled_tmux_beside_finds_a_sidecar_next_to_the_exe() {
        let dir = std::env::temp_dir().join(format!("ass-sidecar-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("skill-server");
        assert!(bundled_tmux_beside(&exe).is_none(), "no sidecar → None");
        std::fs::write(dir.join("tmux"), b"").unwrap();
        let found = bundled_tmux_beside(&exe).expect("sidecar next to the exe is found");
        assert_eq!(std::path::Path::new(&found).file_name().unwrap(), "tmux");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn tmux_spawn_err_explains_a_missing_binary() {
        let missing = std::io::Error::from(std::io::ErrorKind::NotFound);
        assert!(tmux_spawn_err(missing).contains("isn't installed"));
        let other = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        assert!(tmux_spawn_err(other).starts_with("Couldn't start tmux:"));
    }

    #[test]
    fn sanitize_meta_strips_separators() {
        assert_eq!(sanitize_meta("a\tb\nc\rd"), "a b c d");
        assert_eq!(sanitize_meta("plain"), "plain");
    }

    #[test]
    fn valid_session_name_accepts_only_minted_shapes() {
        assert!(valid_session_name("ass-13800-1784338461-0"));
        assert!(!valid_session_name("ass-13800-1784338461")); // missing seq
        assert!(!valid_session_name("ass-1-2-x")); // non-digit
        assert!(!valid_session_name("assistant-1-2-3")); // wrong prefix world
        assert!(!valid_session_name("my session")); // user session, spaces
        // The v1.1.1 sanitizer garble must never validate again:
        assert!(!valid_session_name("ass-13800-1784338461-0_Claude Code _ x_/Users/h"));
        // And an API-supplied id must never become a path:
        assert!(!valid_session_name("ass-../1-2-3"));
    }

    #[test]
    fn registry_roundtrip_and_traversal_guard() {
        if in_isolated_process("registry_roundtrip_and_traversal_guard", false) { return; }
        let name = format!("ass-{}-1-1", std::process::id());
        let meta = registry::Meta {
            label: "Claude Code · skillviewer".into(),
            agent: "claude".into(),
            cwd: "/tmp/x".into(),
            created: "123".into(),
            session_id: "".into(),
        };
        registry::write(&name, &meta).expect("write");
        let got = registry::read(&name).expect("read back");
        assert_eq!(got.label, meta.label);
        assert_eq!(got.cwd, meta.cwd);
        registry::remove(&name);
        assert!(registry::read(&name).is_none(), "removed");
        // Invalid names are rejected before they can become paths.
        assert!(registry::write("ass-../1-2-3", &meta).is_err());
        assert!(registry::read("ass-../1-2-3").is_none());
    }

    #[test]
    fn sweep_orphans_respects_live_and_grace() {
        let d = std::env::temp_dir().join(format!("ass-sweep-test-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let old = |p: &std::path::Path| {
            let f = std::fs::OpenOptions::new().write(true).open(p).unwrap();
            let t = SystemTime::now() - std::time::Duration::from_secs(3600);
            f.set_times(std::fs::FileTimes::new().set_modified(t)).unwrap();
        };
        std::fs::write(d.join("ass-1-1-1.json"), b"{}").unwrap(); // dead but old
        old(&d.join("ass-1-1-1.json"));
        std::fs::write(d.join("ass-2-2-2.json"), b"{}").unwrap(); // live and old
        old(&d.join("ass-2-2-2.json"));
        std::fs::write(d.join("ass-3-3-3.json"), b"{}").unwrap(); // dead but young
        let live = HashSet::from(["ass-2-2-2".to_string()]);
        registry::sweep_orphans_in(&d, &live);
        assert!(!d.join("ass-1-1-1.json").exists(), "dead+old reaped");
        assert!(d.join("ass-2-2-2.json").exists(), "live kept");
        assert!(d.join("ass-3-3-3.json").exists(), "young kept (in-flight create)");
        std::fs::remove_dir_all(&d).unwrap();
    }

    // Registry is the metadata source of truth; a session whose registry file
    // vanished (older-backend create, wiped dir) must be backfilled from the
    // legacy @ass_* options and keep its label.
    #[test]
    #[cfg_attr(windows, ignore = "requires a Unix tmux host")]
    fn tmux_list_backfills_registry_from_legacy_options() {
        if in_isolated_process("tmux_list_backfills_registry_from_legacy_options", true) { return; }
        let cwd = std::env::temp_dir().to_string_lossy().into_owned();
        let s = create_session("shell", &cwd, 80, 24, false, false, false, &[]).expect("create");
        let _guard = SessionGuard(s.id.clone());
        assert!(registry::read(&s.id).is_some(), "create writes the registry");
        registry::remove(&s.id); // simulate an old-backend session
        let listed = list_sessions().unwrap();
        let found = listed.iter().find(|x| x.id == s.id).expect("still listed");
        assert!(found.label.contains('·'), "label backfilled from @ass_*: {}", found.label);
        // Fresh `created` stamp → the snapshot may still be mid-write on an old
        // backend, so backfill must show it but NOT persist it yet.
        assert!(registry::read(&s.id).is_none(), "unsettled snapshot is display-only");
        // Age the session past the settle window → next list persists it.
        let old = (SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() - 60).to_string();
        let _ = tmux().args(["set-option", "-t", &s.id, "@ass_created", &old]).output();
        let listed = list_sessions().unwrap();
        let found = listed.iter().find(|x| x.id == s.id).expect("still listed");
        assert!(found.label.contains('·'), "label survives: {}", found.label);
        assert!(registry::read(&s.id).is_some(), "settled snapshot is persisted");
        let _ = kill_session(&s.id);
        assert!(registry::read(&s.id).is_none(), "kill removes the registry entry");
    }

    // Pins the load-bearing `-u` semantics: with NO locale in the environment
    // (a GUI-launched macOS app has none) a tmux client ASCII-sanitizes its
    // output — tabs and non-ASCII become `_` — which corrupted every parsed
    // session id. `-u` must keep list-sessions output raw with locale scrubbed.
    // Isolated TMUX_TMPDIR: own tmux server, never the shared one.
    #[test]
    fn tmux_u_keeps_output_raw_without_locale() {
        if which("tmux").is_none() {
            eprintln!("tmux not installed — skipping");
            return;
        }
        let tmpdir = std::env::temp_dir().join(format!("ass-u-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmpdir).unwrap();
        let run = |args: &[&str]| {
            let mut c = tmux();
            c.args(args);
            c.env_remove("TMUX").env_remove("LANG").env_remove("LC_ALL").env_remove("LC_CTYPE");
            c.env("TMUX_TMPDIR", &tmpdir);
            c.output().expect("tmux runs")
        };
        run(&["new-session", "-d", "-s", "rt-u", "-x", "80", "-y", "24", "sleep", "30"]);
        run(&["set-option", "-t", "rt-u", "@l", "a · b"]);
        let out = run(&["list-sessions", "-F", "#{session_name}\t#{@l}\tEND"]);
        let s = String::from_utf8_lossy(&out.stdout).into_owned();
        run(&["kill-server"]);
        let _ = std::fs::remove_dir_all(&tmpdir);
        assert!(
            s.contains("rt-u\ta · b\tEND"),
            "raw tabs + UTF-8 must survive -u without locale; got {s:?}"
        );
    }

    #[test]
    #[cfg_attr(windows, ignore = "Windows known-folder cache paths cannot be isolated with child environment variables")]
    fn save_pasted_image_roundtrip() {
        if in_isolated_process("save_pasted_image_roundtrip", false) { return; }
        let config = PathBuf::from(std::env::var_os("XDG_CONFIG_HOME").unwrap());
        assert!(paste_dir().starts_with(config.parent().unwrap()), "paste cleanup must stay inside the fixture");
        let bytes = b"\x89PNG\r\n\x1a\nfakepng";
        let path = save_pasted_image(&b64_encode(bytes), "image/png").expect("save");
        assert!(path.ends_with(".png"));
        assert_eq!(std::fs::read(&path).expect("file exists"), bytes);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn save_pasted_image_rejects_bad_input() {
        assert!(save_pasted_image("aGVsbG8=", "text/plain").is_err(), "mime allowlist");
        assert!(save_pasted_image("", "image/png").is_err(), "empty payload");
        assert!(save_pasted_image("!!!not-base64!!!", "image/png").is_err(), "undecodable payload");
    }

    /// Kills its tmux session on drop, so a panicking assertion in a tmux-gated
    /// test can't strand a detached `ass-*` session (a stray `bash -l`) in the dev
    /// box's / CI runner's tmux server — the app's GC only reaps those after a
    /// week. Backstops the explicit kill_session calls (a second kill is a no-op).
    struct SessionGuard(String);
    impl Drop for SessionGuard {
        fn drop(&mut self) {
            let _ = kill_session(&self.0);
        }
    }

    // tmux-gated end-to-end of session creation + the persistence/GC policy.
    #[test]
    #[cfg_attr(windows, ignore = "requires a Unix tmux host")]
    fn tmux_session_lifecycle_and_stale_gc() {
        if in_isolated_process("tmux_session_lifecycle_and_stale_gc", true) { return; }
        let cwd = std::env::temp_dir().to_string_lossy().into_owned();
        let s = create_session("shell", &cwd, 80, 24, false, false, false, &[]).expect("create");
        let _guard = SessionGuard(s.id.clone());
        assert!(
            s.id.starts_with(&format!("{PREFIX}{}-", std::process::id())),
            "names are pid-namespaced so two backends can't collide: {}",
            s.id
        );
        assert!(session_exists(&s.id), "session should exist after create");
        let listed = list_sessions().unwrap();
        let found = listed.iter().find(|x| x.id == s.id).expect("list_sessions should include it");
        assert_eq!(found.agent, "shell");
        assert!(found.label.contains('·'), "label carries the agent + cwd basename");

        // The stub-window dance must leave exactly one window, with mouse
        // scrolling on and the deeper (window-creation-time) history limit.
        let opt = |k: &str| {
            let out = tmux().args(["show-options", "-t", &s.id, "-v", k]).output().unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        assert_eq!(opt("mouse"), "on", "wheel scrolling needs tmux mouse mode");
        assert_eq!(opt("history-limit"), "10000");
        let panes = tmux()
            .args(["list-windows", "-t", &s.id, "-F", "#{history_limit}"])
            .output()
            .unwrap();
        let lines: Vec<String> = String::from_utf8_lossy(&panes.stdout)
            .lines()
            .map(str::to_string)
            .collect();
        assert_eq!(lines, vec!["10000"], "one window, created after history-limit was set");

        // The week-long idle cutoff spares a fresh session (the startup sweep
        // never touches recent work)…
        sweep_stale();
        assert!(session_exists(&s.id), "a fresh session survives the real sweep");
        // …and with a zero cutoff a detached, shell-only session is collected.
        // (Targeted per-id so this test can't reap a developer's real terminals.)
        // Poll: right after creation the login shell's rc files spawn transient
        // children, which make the all-panes-are-shells probe flap (see below).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut reaped = gc_session_if_stale(&s.id, 0);
        while !reaped && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(200));
            reaped = gc_session_if_stale(&s.id, 0);
        }
        assert!(reaped, "detached + agent-exited + past cutoff ⇒ reaped");
        assert!(!session_exists(&s.id));
    }

    // tmux-gated: the GC must never take a session with a live process or a
    // watching client — only explicit kills end those.
    #[test]
    #[cfg_attr(windows, ignore = "requires a Unix tmux host")]
    fn tmux_gc_spares_live_and_attached_sessions() {
        if in_isolated_process("tmux_gc_spares_live_and_attached_sessions", true) { return; }
        let cwd = std::env::temp_dir().to_string_lossy().into_owned();

        // A non-shell foreground process (stand-in for a running agent).
        let live = create_session("shell", &cwd, 80, 24, false, false, false, &[]).expect("create");
        let _live_guard = SessionGuard(live.id.clone());
        let _ = tmux().args(["send-keys", "-t", &live.id, "exec sleep 300", "Enter"]).output();
        // Wait for the exec to land: the pane PROCESS becomes `sleep`. (Don't
        // sample all_panes_are_shells for this — the keep-alive login shell's
        // rc files spawn transient children right after creation, which make
        // that probe flap during startup.)
        let sleeping = |id: &str| {
            let Ok(out) = tmux().args(["list-panes", "-s", "-t", id, "-F", "#{pane_pid}"]).output()
            else {
                return false;
            };
            let pid = String::from_utf8_lossy(&out.stdout).trim().to_string();
            !pid.is_empty()
                && hidden_command("ps")
                    .args(["-p", &pid, "-o", "comm="])
                    .output()
                    .map(|o| String::from_utf8_lossy(&o.stdout).trim().ends_with("sleep"))
                    .unwrap_or(false)
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline && !sleeping(&live.id) {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(sleeping(&live.id), "exec sleep should replace the pane shell");
        assert!(!all_panes_are_shells(&live.id), "sleep should be the foreground command");
        assert!(!gc_session_if_stale(&live.id, 0), "a live agent process is never GC'd");
        assert!(session_exists(&live.id));
        let _ = kill_session(&live.id);

        // An attached client protects even a plain idle shell.
        let watched = create_session("shell", &cwd, 80, 24, false, false, false, &[]).expect("create");
        let _watched_guard = SessionGuard(watched.id.clone());
        let (att, _rx) = attach(&watched.id, 80, 24).expect("attach");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let attached = |id: &str| {
            let out = tmux()
                .args(["display-message", "-p", "-t", id, "#{session_attached}"])
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).trim() != "0"
        };
        while std::time::Instant::now() < deadline && !attached(&watched.id) {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(attached(&watched.id), "the tmux client should register as attached");
        assert!(!gc_session_if_stale(&watched.id, 0), "an attached session is never GC'd");
        assert!(session_exists(&watched.id));
        drop(att);
        let _ = kill_session(&watched.id);
    }
}
