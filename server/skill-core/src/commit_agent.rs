//! Commit-message generation backend selector.
//!
//! The default path shells out to a coding-agent CLI the user already installed
//! and logged into (Claude Code, Codex, Gemini) — keyless, because their
//! subscription OAuth login does the auth, so we ship nothing and ask for no API
//! key. opencode rides along too, but it's BYO-API-key (no subscription concept),
//! so it's the exception to the scrub rule below. Same shell-out philosophy as
//! `gitops`→`git` and `engine`→`llama-server`. The on-device `engine` (llama.cpp)
//! is still here but DEMOTED to an opt-in offline backend
//! (`VIBESTUDIO_COMMIT_AGENT=llama`); it is no longer bundled.
//!
//! Precedence (first that's installed AND logged in): explicit
//! `VIBESTUDIO_COMMIT_AGENT` → claude → codex → gemini → opencode → manual.
//! opencode sits last because it's the only one that may bill a metered API key
//! rather than a flat subscription. `llama` is never auto-selected — it must be
//! opted in.
//!
//! Two load-bearing rules, both enforced below:
//!   - For the SUBSCRIPTION CLIs we REMOVE the provider's API-key env vars from
//!     the child, because they prefer an API key over the subscription when one
//!     is present (the desktop injects secrets into spawned envs), which would
//!     silently bill the API org instead of using the keyless subscription.
//!     opencode is exempt: it has no subscription, so scrubbing would leave it
//!     with no auth at all — it keeps whatever provider key it has configured.
//!   - We run from a NEUTRAL cwd and (where the CLI supports it) forbid the agent
//!     tool-loop so the call is a single inference turn that summarizes the diff
//!     instead of poking the repo.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{mpsc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::engine::{self, ChatMessage};
use crate::process::hidden_command;

/// The instruction handed to a cloud CLI; the diff arrives on the child's stdin.
/// Kept lean on purpose (see the commit-prompt philosophy) — format, no prefix,
/// ~10 words, output-only.
const INSTRUCTION: &str = "Summarize the git diff provided on standard input in about 10 words \
describing what changed. Do not use a \"feat:\"/\"fix:\"/type prefix and do not prefix a filename — \
just the description. Output only that, nothing else.";

/// Hard ceiling on a single generation. Cloud calls are ~3–12s; this only fires
/// on a hung/blocked child, after which we kill it and surface a clean error.
const GEN_TIMEOUT: Duration = Duration::from_secs(60);
/// Login/availability probes are local credential checks (no network); cap them
/// so a wedged CLI can't hang the Save dialog.
const PROBE_TIMEOUT: Duration = Duration::from_secs(8);
/// How long a backend selection is reused before we re-probe — long enough that
/// a status check and the generate that follows it don't probe twice, short
/// enough that logging in mid-session is picked up promptly.
const SELECT_TTL: Duration = Duration::from_secs(15);

/// Which generator produced (or will produce) the message.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Backend {
    Claude,
    Codex,
    Gemini,
    /// BYO-API-key coding agent (provider keys, not a subscription).
    Opencode,
    /// Opt-in on-device llama.cpp (`engine`).
    Llama,
    /// Nothing usable — the UI falls back to letting the user type.
    None,
}

impl Backend {
    fn id(self) -> &'static str {
        match self {
            Backend::Claude => "claude",
            Backend::Codex => "codex",
            Backend::Gemini => "gemini",
            Backend::Opencode => "opencode",
            Backend::Llama => "llama",
            Backend::None => "none",
        }
    }
}

/// One generated draft plus provenance, so the caller can debug-log "why did it
/// say that?" with the backend and its raw output preserved.
pub struct Generated {
    /// The message text (pre-`post_process`).
    pub text: String,
    /// Which backend produced it.
    pub backend: &'static str,
    /// The backend's raw output (CLI JSON / output file / engine reply).
    pub raw: String,
}

// ─────────────────────────────── public API ───────────────────────────────

/// Is the opt-in on-device offline backend selected? Gates the model
/// download/engine lifecycle so the DEFAULT install never fetches the ~1.4 GB
/// GGUF or spawns llama-server. Read directly from the env (no probing) so
/// `engine` can call it cheaply at startup.
pub fn offline_opted_in() -> bool {
    matches!(env_choice().as_deref(), Some("llama") | Some("local") | Some("offline"))
}

/// Generate a one-line message from an already-truncated `diff`, using the
/// first available backend. `seed`/`temperature` steer the llama backend; cloud
/// CLIs ignore them (they're non-deterministic, which is fine — `regenerate`
/// just wants a fresh phrasing).
pub fn generate(diff: &str, seed: i64, temperature: f32) -> Result<Generated, String> {
    match selected_backend() {
        Backend::Claude => run_claude(&require_bin(Backend::Claude, CLAUDE_BINS)?, diff),
        Backend::Codex => run_codex(&require_bin(Backend::Codex, CODEX_BINS)?, diff),
        Backend::Gemini => run_gemini(&require_bin(Backend::Gemini, GEMINI_BINS)?, diff),
        Backend::Opencode => run_opencode(&require_bin(Backend::Opencode, OPENCODE_BINS)?, diff),
        Backend::Llama => run_llama(diff, seed, temperature),
        Backend::None => Err(no_backend_message()),
    }
}

/// Readiness of the commit-message generator, for the Save dialog. Keeps the
/// llama-era fields (`model`/`downloaded`/`sizeMb`/`path`) populated so the
/// existing UI consumer keeps working, and adds `backend`/`ready`/`needsLogin`/
/// `detail` for the new CLI-backed states.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitStatus {
    /// Active backend id: `claude` | `codex` | `gemini` | `opencode` | `llama` | `none`.
    backend: String,
    /// A draft can be produced right now (logged-in CLI, or downloaded model).
    ready: bool,
    /// A supported CLI is installed but not logged in — hint the user to log in.
    needs_login: bool,
    /// One-line human hint for the dialog.
    detail: String,
    /// Model id (CLI model, or the llama GGUF id). Empty for `none`.
    model: String,
    /// llama only: the GGUF is on disk. For a ready cloud backend this mirrors
    /// `ready` so older checks of `downloaded` still behave.
    downloaded: bool,
    /// llama only: on-disk model size in MB.
    size_mb: Option<u64>,
    /// llama only: where the GGUF lives / would be cached.
    path: String,
}

pub fn status() -> CommitStatus {
    let cloud = |backend: Backend, model: &str, detail: &str| CommitStatus {
        backend: backend.id().into(),
        ready: true,
        needs_login: false,
        detail: detail.into(),
        model: model.into(),
        downloaded: true, // back-compat: the dialog's "needs download" note keys off this
        size_mb: None,
        path: String::new(),
    };
    match selected_backend() {
        Backend::Claude => cloud(Backend::Claude, "claude-haiku-4-5", "Drafting with your Claude login"),
        Backend::Codex => cloud(Backend::Codex, "", "Drafting with your ChatGPT (Codex) login"),
        Backend::Gemini => cloud(Backend::Gemini, "gemini-2.5-flash", "Drafting with your Google (Gemini) login"),
        Backend::Opencode => cloud(Backend::Opencode, "", "Drafting with your opencode CLI"),
        Backend::Llama => {
            let m = engine::model_status();
            CommitStatus {
                backend: Backend::Llama.id().into(),
                ready: m.downloaded,
                needs_login: false,
                detail: if m.downloaded {
                    "Generating on-device".into()
                } else {
                    "First use downloads the on-device model (~1–1.5 GB), one time".into()
                },
                model: m.model,
                downloaded: m.downloaded,
                size_mb: m.size_mb,
                path: m.path,
            }
        }
        Backend::None => {
            // Distinguish an explicit `off` (the user disabled drafting) from "a
            // CLI is installed but not logged in" from "nothing installed".
            let off = matches!(env_choice().as_deref(), Some("off") | Some("none") | Some("manual") | Some("disabled"));
            let installed = !off && any_installed();
            CommitStatus {
                backend: Backend::None.id().into(),
                ready: false,
                needs_login: installed,
                detail: if off {
                    "AI drafting is turned off — type your message".into()
                } else if installed {
                    "Log in to your coding-agent CLI (claude, codex, gemini, or opencode) to draft messages".into()
                } else {
                    "No AI generator found — type a message, or install and log in to claude, codex, gemini, or opencode".into()
                },
                model: String::new(),
                downloaded: false,
                size_mb: None,
                path: String::new(),
            }
        }
    }
}

fn no_backend_message() -> String {
    if any_installed() {
        "No logged-in coding-agent CLI. Run `claude` (or `codex` / `gemini` / `opencode`) once to log in — \
         or set VIBESTUDIO_COMMIT_AGENT=llama to use the on-device model."
            .into()
    } else {
        "No commit-message generator available. Install and log in to claude, codex, gemini, or opencode — \
         or set VIBESTUDIO_COMMIT_AGENT=llama to use the on-device model."
            .into()
    }
}

// ─────────────────────────── backend selection ───────────────────────────

const CLAUDE_BINS: &[&str] = &["claude"];
const CODEX_BINS: &[&str] = &["codex"];
const GEMINI_BINS: &[&str] = &["gemini"];
const OPENCODE_BINS: &[&str] = &["opencode"];

/// Raw lowercased `VIBESTUDIO_COMMIT_AGENT`, if set and non-empty.
fn env_choice() -> Option<String> {
    let v = std::env::var("VIBESTUDIO_COMMIT_AGENT").ok()?;
    let v = v.trim().to_ascii_lowercase();
    (!v.is_empty()).then_some(v)
}

/// TTL-cached backend selection (probes are the expensive part).
fn selected_backend() -> Backend {
    static CACHE: OnceLock<Mutex<Option<(Instant, Backend)>>> = OnceLock::new();
    let cell = CACHE.get_or_init(|| Mutex::new(None));
    if let Ok(guard) = cell.lock() {
        if let Some((at, b)) = *guard {
            if at.elapsed() < SELECT_TTL {
                return b;
            }
        }
    }
    let b = detect_backend();
    if let Ok(mut guard) = cell.lock() {
        *guard = Some((Instant::now(), b));
    }
    b
}

/// Resolve the active backend without caching.
fn detect_backend() -> Backend {
    // Explicit override wins. A named CLI is used even if its login probe fails
    // (generation surfaces the login error) — the user asked for it by name.
    if let Some(choice) = env_choice() {
        match choice.as_str() {
            "off" | "none" | "manual" | "disabled" => return Backend::None,
            "llama" | "local" | "offline" => return Backend::Llama,
            "claude" => return Backend::Claude,
            "codex" => return Backend::Codex,
            "gemini" => return Backend::Gemini,
            "opencode" => return Backend::Opencode,
            _ => {} // unknown value → fall through to auto-detect
        }
    }
    // Auto chain: first installed AND logged in. llama is opt-in only, never here.
    if let Some(bin) = resolve(CLAUDE_BINS) {
        if claude_logged_in(&bin) {
            return Backend::Claude;
        }
    }
    if let Some(bin) = resolve(CODEX_BINS) {
        if codex_logged_in(&bin) {
            return Backend::Codex;
        }
    }
    if resolve(GEMINI_BINS).is_some() && gemini_logged_in() {
        return Backend::Gemini;
    }
    // opencode last: it may bill a metered provider key rather than a flat subscription.
    if resolve(OPENCODE_BINS).is_some() && opencode_logged_in() {
        return Backend::Opencode;
    }
    Backend::None
}

/// True if any supported cloud CLI binary is present (regardless of login).
fn any_installed() -> bool {
    resolve(CLAUDE_BINS).is_some()
        || resolve(CODEX_BINS).is_some()
        || resolve(GEMINI_BINS).is_some()
        || resolve(OPENCODE_BINS).is_some()
}

fn require_bin(backend: Backend, names: &[&str]) -> Result<PathBuf, String> {
    resolve(names).ok_or_else(|| format!("The {} CLI isn't installed (not on PATH).", backend.id()))
}

// ───────────────────────── binary resolution ─────────────────────────

/// Find a CLI on PATH, then in well-known install dirs. The fallback dirs matter
/// because a macOS `.app` launched from Finder (and the Tauri-spawned server)
/// inherit a stripped PATH that hides `~/.local/bin`, Homebrew, and npm/bun
/// global bins — the same lesson as `gpu.rs` discovering CUDA dirs. Cached: a
/// binary's location doesn't move within a session. (`connections` reuses this
/// to find `claude` for MCP config writes.)
pub(crate) fn resolve(names: &[&str]) -> Option<PathBuf> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<PathBuf>>>> = OnceLock::new();
    let cell = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = names.first().copied().unwrap_or_default().to_string();
    if let Ok(guard) = cell.lock() {
        if let Some(hit) = guard.get(&key) {
            return hit.clone();
        }
    }
    let found = resolve_uncached(names);
    if let Ok(mut guard) = cell.lock() {
        guard.insert(key, found.clone());
    }
    found
}

fn resolve_uncached(names: &[&str]) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    dirs.extend(extra_bin_dirs());
    for dir in &dirs {
        for name in names {
            for variant in name_variants(name) {
                let cand = dir.join(&variant);
                if cand.is_file() {
                    return Some(cand);
                }
            }
        }
    }
    None
}

/// On Windows a CLI is usually a `.cmd` (npm shim) or `.exe`; on unix the bare
/// name is the executable.
fn name_variants(name: &str) -> Vec<String> {
    #[cfg(windows)]
    {
        vec![format!("{name}.cmd"), format!("{name}.exe"), format!("{name}.bat"), name.to_string()]
    }
    #[cfg(not(windows))]
    {
        vec![name.to_string()]
    }
}

/// Well-known install locations that a stripped PATH may omit.
fn extra_bin_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let home = dirs::home_dir();
    #[cfg(not(windows))]
    {
        if let Some(h) = &home {
            dirs.push(h.join(".local/bin"));
            dirs.push(h.join(".bun/bin"));
            dirs.push(h.join(".deno/bin"));
            dirs.push(h.join(".npm-global/bin"));
            dirs.push(h.join(".volta/bin"));
        }
        dirs.push(PathBuf::from("/opt/homebrew/bin"));
        dirs.push(PathBuf::from("/usr/local/bin"));
        dirs.push(PathBuf::from("/usr/bin"));
    }
    #[cfg(windows)]
    {
        if let Some(h) = &home {
            dirs.push(h.join(".local").join("bin"));
        }
        if let Some(appdata) = std::env::var_os("APPDATA") {
            dirs.push(PathBuf::from(appdata).join("npm"));
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(PathBuf::from(local).join("Programs"));
        }
    }
    dirs
}

// ─────────────────────────── login probes ───────────────────────────

/// `claude auth status` prints JSON with `"loggedIn": true` (exit 0) when a
/// subscription/OAuth login is active.
fn claude_logged_in(bin: &PathBuf) -> bool {
    match run_proc(probe_cmd(bin, &["auth", "status"]), None, PROBE_TIMEOUT) {
        Ok((ok, out)) => ok && out.replace(' ', "").contains("\"loggedIn\":true"),
        Err(_) => false,
    }
}

/// `codex login status` exits 0 when logged in (ChatGPT OAuth or API key).
fn codex_logged_in(bin: &PathBuf) -> bool {
    matches!(run_proc(probe_cmd(bin, &["login", "status"]), None, PROBE_TIMEOUT), Ok((true, _)))
}

/// Gemini CLI has no scriptable status command; its OAuth ("Login with Google")
/// caches credentials on disk, so presence of that file is our signal.
fn gemini_logged_in() -> bool {
    dirs::home_dir().map(|h| h.join(".gemini").join("oauth_creds.json").is_file()).unwrap_or(false)
}

/// opencode keeps provider credentials in ~/.local/share/opencode/auth.json (a
/// JSON object keyed by provider). A non-empty object = at least one provider is
/// configured — the same disk-probe approach as gemini, and cheaper than spawning
/// `opencode auth list`.
fn opencode_logged_in() -> bool {
    let Some(home) = dirs::home_dir() else { return false };
    match std::fs::read_to_string(home.join(".local/share/opencode/auth.json")) {
        // contains a quote ⇒ has at least one key; rules out "", "{}", whitespace.
        Ok(s) => s.contains('"'),
        Err(_) => false,
    }
}

fn probe_cmd(bin: &PathBuf, args: &[&str]) -> Command {
    let mut cmd = hidden_command(bin);
    cmd.args(args);
    cmd
}

// ─────────────────────────── backends ───────────────────────────

fn run_claude(bin: &PathBuf, diff: &str) -> Result<Generated, String> {
    let mut cmd = hidden_command(bin);
    scrub(&mut cmd, &["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN"]);
    neutral_cwd(&mut cmd);
    cmd.args([
        "-p",
        INSTRUCTION,
        "--output-format",
        "json",
        "--model",
        "claude-haiku-4-5", // full id, NOT the `haiku` alias (which resolves to Sonnet)
        "--allowed-tools",
        "", // empty allowlist → one inference turn, no tool loop, no repo poking
    ]);
    let (ok, out) = run_proc(cmd, Some(diff), GEN_TIMEOUT)?;
    let json: serde_json::Value =
        serde_json::from_str(&out).map_err(|_| "Claude returned an unexpected (non-JSON) response.".to_string())?;
    if !ok || json.get("is_error").and_then(|b| b.as_bool()).unwrap_or(false) {
        return Err("Claude couldn't generate a message — check `claude auth status`.".into());
    }
    let text = json
        .get("result")
        .and_then(|r| r.as_str())
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "Claude returned an empty response.".to_string())?
        .to_string();
    Ok(Generated { text, backend: "claude", raw: out })
}

fn run_codex(bin: &PathBuf, diff: &str) -> Result<Generated, String> {
    // codex's clean answer is only in the --output-last-message file; stdout is a
    // noisy session banner. Use a per-call temp file (unique across concurrent
    // generations) and remove it after reading.
    let out_path = std::env::temp_dir().join(format!("vibestudio-codex-{}.txt", unique_suffix()));
    let mut cmd = hidden_command(bin);
    scrub(&mut cmd, &["OPENAI_API_KEY", "CODEX_API_KEY"]);
    neutral_cwd(&mut cmd);
    cmd.args([
        "exec",
        "--sandbox",
        "read-only",
        "--skip-git-repo-check",
        "--color",
        "never",
        "-o",
        &out_path.to_string_lossy(),
        INSTRUCTION,
    ]);
    let result = run_proc(cmd, Some(diff), GEN_TIMEOUT);
    let answer = std::fs::read_to_string(&out_path).ok();
    let _ = std::fs::remove_file(&out_path);
    let (ok, _stdout) = result?;
    let text = answer.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    match (ok, text) {
        (true, Some(text)) => Ok(Generated { text: text.clone(), backend: "codex", raw: text }),
        _ => Err("Codex couldn't generate a message — check `codex login status`.".into()),
    }
}

fn run_gemini(bin: &PathBuf, diff: &str) -> Result<Generated, String> {
    let mut cmd = hidden_command(bin);
    scrub(&mut cmd, &["GEMINI_API_KEY", "GOOGLE_API_KEY"]);
    neutral_cwd(&mut cmd);
    cmd.args(["--output-format", "json", "-m", "gemini-2.5-flash", "-p", INSTRUCTION]);
    let (ok, out) = run_proc(cmd, Some(diff), GEN_TIMEOUT)?;
    if !ok {
        return Err("Gemini couldn't generate a message — check your `gemini` login.".into());
    }
    // Gemini's JSON wraps the text in `.response`; tolerate plain text too.
    let text = serde_json::from_str::<serde_json::Value>(&out)
        .ok()
        .and_then(|v| v.get("response").and_then(|r| r.as_str()).map(|s| s.to_string()))
        .unwrap_or_else(|| out.clone());
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err("Gemini returned an empty response.".into());
    }
    Ok(Generated { text, backend: "gemini", raw: out })
}

/// opencode is BYO-API-key, so — unlike the subscription CLIs — we do NOT scrub
/// provider keys (scrubbing would leave it unauthenticated). `run --format
/// default` prints just the assistant's text to stdout (the session banner goes
/// to stderr, which `run_proc` discards). `run` reads no stdin, so the diff rides
/// inline in the message, after a `--` guard so a diff line beginning with `-`
/// can't be parsed as a flag. The model is opencode's own configured default.
fn run_opencode(bin: &PathBuf, diff: &str) -> Result<Generated, String> {
    let mut cmd = hidden_command(bin);
    neutral_cwd(&mut cmd);
    cmd.args(["run", "--format", "default", "--", &format!("{INSTRUCTION}\n\nGit diff:\n{diff}")]);
    let (ok, out) = run_proc(cmd, None, GEN_TIMEOUT)?;
    let text = out.trim().to_string();
    if !ok || text.is_empty() {
        return Err("opencode couldn't generate a message — check `opencode auth list`.".into());
    }
    Ok(Generated { text: text.clone(), backend: "opencode", raw: out })
}

/// The opt-in on-device path: the existing `engine` (llama.cpp) with the
/// analyse-then-message structured schema that makes a small model read the diff.
fn run_llama(diff: &str, seed: i64, temperature: f32) -> Result<Generated, String> {
    let prompt = format!(
        "Analyze the following git diff in no more than 100 words, then write a short commit message. \
The message must be about 10 words describing what changed — with no \
\"feat:\"/\"fix:\"/type prefix and no filename prefix, just the description.\n\nDiff:\n{diff}"
    );
    let messages = vec![ChatMessage::new("user", prompt)];
    let raw = engine::chat(&messages, temperature, seed, Some(commit_schema()))?;
    Ok(Generated { text: extract_commit_message(&raw), backend: "llama", raw })
}

/// The JSON-schema `response_format` for the llama backend: an `analysis` (the
/// model's reasoning, generated first because it's listed first) before the
/// `commit_message`. Constraining to this is what makes the small model think
/// before it writes — and lets us parse the result.
fn commit_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "json_schema",
        "json_schema": {
            "name": "commit",
            "schema": {
                "type": "object",
                "properties": {
                    "analysis": { "type": "string", "maxLength": 700 },
                    "commit_message": { "type": "string" }
                },
                "required": ["analysis", "commit_message"],
                "additionalProperties": false
            }
        }
    })
}

/// Pull `commit_message` out of the llama backend's structured JSON reply. Falls
/// back to the raw text if it somehow isn't the expected JSON.
fn extract_commit_message(raw: &str) -> String {
    serde_json::from_str::<serde_json::Value>(raw)
        .ok()
        .and_then(|v| v.get("commit_message").and_then(|m| m.as_str()).map(|s| s.to_string()))
        .unwrap_or_else(|| raw.to_string())
}

// ─────────────────────────── process plumbing ───────────────────────────

/// Remove env vars from the child so an injected API key can't override the
/// keyless subscription login.
fn scrub(cmd: &mut Command, vars: &[&str]) {
    for v in vars {
        cmd.env_remove(v);
    }
}

/// Run from a neutral directory so a coding-agent CLI doesn't ingest the current
/// repo's `CLAUDE.md` / git status into the prompt.
fn neutral_cwd(cmd: &mut Command) {
    cmd.current_dir(std::env::temp_dir());
}

fn unique_suffix() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    format!("{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed))
}

/// Spawn `cmd`, optionally write `stdin_data`, capture stdout, and enforce
/// `timeout` (kill the child if it overruns). Returns `(exit_success, stdout)`.
/// stdin is written on its own thread so a child that fills its stdout pipe
/// before draining stdin can't deadlock us.
pub(crate) fn run_proc(mut cmd: Command, stdin_data: Option<&str>, timeout: Duration) -> Result<(bool, String), String> {
    cmd.stdin(if stdin_data.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = cmd.spawn().map_err(|e| format!("Couldn't start the generator: {e}"))?;

    if let (Some(data), Some(mut si)) = (stdin_data, child.stdin.take()) {
        let data = data.to_string();
        std::thread::spawn(move || {
            let _ = si.write_all(data.as_bytes()); // si drops here → stdin closes
        });
    }

    let (tx, rx) = mpsc::channel();
    if let Some(mut so) = child.stdout.take() {
        std::thread::spawn(move || {
            let mut s = String::new();
            let _ = so.read_to_string(&mut s);
            let _ = tx.send(s);
        });
    }

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let out = rx.recv_timeout(Duration::from_secs(5)).unwrap_or_default();
                return Ok((status.success(), out));
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("The generator took too long and was stopped.".into());
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(format!("The generator failed: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One test owns the process-global VIBESTUDIO_COMMIT_AGENT (a second test
    /// touching it would race under cargo's parallel runner). Covers both the
    /// offline opt-in and the named-backend override; the override branch of
    /// `detect_backend` returns directly with no filesystem probe, so the backend
    /// assertions are deterministic.
    #[test]
    fn env_override_drives_offline_and_backend() {
        // No env override → not offline (default is the cloud CLI chain).
        std::env::remove_var("VIBESTUDIO_COMMIT_AGENT");
        assert!(!offline_opted_in());

        for (val, want, offline) in [
            ("llama", Backend::Llama, true),
            ("claude", Backend::Claude, false),
            ("codex", Backend::Codex, false),
            ("gemini", Backend::Gemini, false),
            ("opencode", Backend::Opencode, false),
            ("off", Backend::None, false),
        ] {
            std::env::set_var("VIBESTUDIO_COMMIT_AGENT", val);
            assert_eq!(detect_backend(), want, "backend for env={val}");
            assert_eq!(offline_opted_in(), offline, "offline for env={val}");
        }
        std::env::remove_var("VIBESTUDIO_COMMIT_AGENT");
        assert_eq!(Backend::Opencode.id(), "opencode");
    }

    #[test]
    fn extract_commit_message_reads_structured_field() {
        let raw = r#"{"analysis":"removed the body","commit_message":"docs: trim SKILL.md"}"#;
        assert_eq!(extract_commit_message(raw), "docs: trim SKILL.md");
        // Falls back to the raw text when it isn't the expected JSON object.
        assert_eq!(extract_commit_message("docs: plain text"), "docs: plain text");
    }

    #[test]
    fn name_variants_are_platform_shaped() {
        #[cfg(windows)]
        assert_eq!(name_variants("claude"), ["claude.cmd", "claude.exe", "claude.bat", "claude"]);
        #[cfg(not(windows))]
        assert_eq!(name_variants("claude"), ["claude"]);
    }
}
