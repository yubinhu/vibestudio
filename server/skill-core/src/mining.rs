// Skill mining: run the bundled `skill-miner` skill as an agent terminal
// session and land its outputs in the app's existing review loops (new skills
// → the `generated-skills/` Proposed staging area; improvements → ordinary
// uncommitted changes reviewed with the worktree diff).
//
// A run IS an ordinary interactive agent session: the registry's launch line
// starts the agent's TUI in the run dir with the (user-previewed) prompt
// pre-submitted, and the client navigates the user to that terminal — they
// watch the run live, answer any first-run dialog, and follow up in place.
// No headless mode, no hidden prompt text, no completion sentinel: what the
// user sees in the dialog and the pane is exactly what the agent gets.
//
// The judgment work (cluster, judge, author) is the agent's, per the skill's
// own instructions; this module owns the run lifecycle around it: refresh the
// installed copy of the skill, compose the run prompt, and report progress by
// watching the run dir's artifacts. Mined edits get no special attribution —
// a change is a change, surfaced by the ordinary dirty/review machinery.
//
// skill-term depends on this crate, so this module cannot spawn terminals
// itself; the route layer (skill-server) passes the spawn/alive/kill
// operations in. Everything else lives here.
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use walkdir::WalkDir;

use crate::agents::{self, LaunchCtx};
use crate::sync::{install_skill, IGNORED_DIRS};
use crate::secrets;

/// Where the miner's transcript adapters look, mirrored here only to give the
/// source-picker honest per-agent session counts (a cheap mtime walk — the
/// real parsing happens inside the run).
const SOURCES: [(&str, &str); 2] = [("claude-code", "Claude Code"), ("codex", "Codex")];

const MINER_SKILL: &str = "skill-miner";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MineSource {
    pub id: String,
    pub label: String,
    /// Transcript files modified within the window.
    pub sessions: usize,
}

/// The persisted record of the active (most recent) run. When a new run starts
/// the active run is archived verbatim — record plus its `out/` artifacts —
/// under `history/<id>/`, so past runs are kept rather than wiped (see
/// [`reset_for_new_run`]).
#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct RunRecord {
    id: String,
    started_unix: u64,
    days: u64,
    sources: Vec<String>,
    agent: String,
    /// Model / effort overrides the run was started with ("" = CLI default);
    /// reviving the conversation resumes with the same tuning.
    #[serde(default)]
    model: String,
    #[serde(default)]
    effort: String,
    improve: bool,
    /// The exact prompt the run was launched with — the agent's actual initial
    /// message, including any edits made in the dialog. Shown on the mining
    /// page in place of a derived "window", which a hand-edited prompt can
    /// silently diverge from. Defaulted for records written before it existed.
    #[serde(default)]
    prompt: String,
    terminal_id: String,
    /// When a revival terminal was last spawned; within the startup grace the
    /// recorded terminal is trusted without probing (see [`continue_run`]).
    #[serde(default)]
    continued_unix: u64,
    /// "running" (the TUI is up in the recorded terminal) | "ended" (sticky:
    /// the terminal or the agent in it is gone; revivals don't reopen it).
    status: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MineState {
    /// "idle" (never ran / no record) | "running" | "ended".
    pub status: String,
    /// While running: "scanning" | "analyzing" | "reviewing".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
    /// Sessions found by the discover step (inventory rows).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub found: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_unix: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_id: Option<String>,
    /// Run parameters from the record (absent when idle) — the mining page's
    /// run summary. `agent` is the AgentOption id; model/effort are "" when
    /// the run used the CLI defaults.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub days: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sources: Option<Vec<String>>,
    /// The prompt the run was launched with — the mining page shows it instead
    /// of a derived window. Empty for runs recorded before it was captured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
}

/// Children of the mining root: the active run's record, and the archive of
/// past runs (`history/<id>/`). `out/` (the agent's artifacts) sits beside them.
const RUN_FILE: &str = "run.json";
const HISTORY_DIR: &str = "history";

fn mining_dir() -> Result<PathBuf, String> {
    Ok(secrets::config_dir()?.join("mining"))
}
/// The active (most recent) run lives directly at the mining root; finished
/// runs are archived under `history/<id>/` (see [`reset_for_new_run`]). The
/// archive is the one child of the run dir that isn't part of the live run.
fn run_dir() -> Result<PathBuf, String> {
    mining_dir()
}
fn history_dir() -> Result<PathBuf, String> {
    Ok(mining_dir()?.join(HISTORY_DIR))
}
fn run_file() -> Result<PathBuf, String> {
    Ok(run_dir()?.join(RUN_FILE))
}

fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Serializes every read-modify-write of the run record: routes run on
/// concurrent workers, and an unguarded pair of continues would each spawn a
/// revival terminal (the loser leaks — a live TUI is never GC'd).
static RUN_LOCK: Mutex<()> = Mutex::new(());

fn run_lock() -> MutexGuard<'static, ()> {
    // The record lives on disk, not in the mutex — a poisoned lock is safe.
    RUN_LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

fn load_run() -> Option<RunRecord> {
    let raw = std::fs::read_to_string(run_file().ok()?).ok()?;
    serde_json::from_str(&raw).ok()
}

fn save_run(rec: &RunRecord) -> Result<(), String> {
    let path = run_file()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(rec).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| e.to_string())
}

fn count_lines(path: &Path) -> Option<usize> {
    let raw = std::fs::read_to_string(path).ok()?;
    Some(raw.lines().filter(|l| !l.trim().is_empty()).count())
}

/// Move the active run (the record + everything beside it, except the history
/// archive itself) into `history/<id>/`, keyed by the run's own id. Called
/// right before a new run lays down a fresh run dir, so past runs are kept
/// instead of wiped. No record on disk ⇒ nothing to preserve (a prepared-but-
/// never-started run carries no metadata to show, so it isn't a "past run").
fn archive_active_in(mdir: &Path) -> Result<(), String> {
    let run_file = mdir.join(RUN_FILE);
    if !run_file.exists() {
        return Ok(());
    }
    let id = std::fs::read_to_string(&run_file)
        .ok()
        .and_then(|raw| serde_json::from_str::<RunRecord>(&raw).ok())
        .map(|r| r.id)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("mine-{}", now_unix()));
    let hdir = mdir.join(HISTORY_DIR);
    let dest = hdir.join(&id);
    if dest.exists() {
        std::fs::remove_dir_all(&dest).map_err(|e| e.to_string())?;
    }
    std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
    if let Ok(rd) = std::fs::read_dir(mdir) {
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            if p == hdir {
                continue; // never move the archive into itself
            }
            std::fs::rename(&p, dest.join(e.file_name())).map_err(|err| err.to_string())?;
        }
    }
    Ok(())
}

/// Ready the run dir for a fresh run: archive the previous run under
/// `history/<id>/`, clear anything left at the root (a never-recorded leftover
/// has no archive to go to), and recreate an empty `out/`. The history archive
/// is the one thing preserved across the reset.
fn reset_for_new_run() -> Result<(), String> {
    reset_for_new_run_in(&mining_dir()?)
}

fn reset_for_new_run_in(mdir: &Path) -> Result<(), String> {
    let hdir = mdir.join(HISTORY_DIR);
    std::fs::create_dir_all(mdir).map_err(|e| e.to_string())?;
    archive_active_in(mdir)?;
    if let Ok(rd) = std::fs::read_dir(mdir) {
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            if p == hdir {
                continue;
            }
            let _ = if p.is_dir() { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
        }
    }
    std::fs::create_dir_all(mdir.join("out")).map_err(|e| e.to_string())?;
    Ok(())
}

/// One-time upgrade of the pre-history layout, which kept the active run in a
/// `current/` subdir. Promote that run to the new active slot (the mining root)
/// so the upgrade is seamless — it then archives normally on the next run.
/// Idempotent: a no-op once `current/` is gone, and it never clobbers an
/// already-migrated root.
fn migrate_legacy() {
    if let Ok(mdir) = mining_dir() {
        migrate_legacy_in(&mdir);
    }
}

fn migrate_legacy_in(mdir: &Path) {
    let legacy = mdir.join("current");
    if !legacy.exists() {
        return;
    }
    // A new-layout run already exists ⇒ the legacy dir is stale; just drop it.
    if mdir.join(RUN_FILE).exists() {
        let _ = std::fs::remove_dir_all(&legacy);
        return;
    }
    if let Ok(rd) = std::fs::read_dir(&legacy) {
        for e in rd.filter_map(|e| e.ok()) {
            let _ = std::fs::rename(e.path(), mdir.join(e.file_name()));
        }
    }
    let _ = std::fs::remove_dir_all(&legacy);
}

/// Per-source transcript counts within the window — powers the source picker.
pub fn sources(days: u64) -> Vec<MineSource> {
    let cutoff = SystemTime::now() - std::time::Duration::from_secs(days.max(1) * 86400);
    let home = dirs::home_dir().unwrap_or_default();
    SOURCES
        .iter()
        .map(|(id, label)| {
            let n = match *id {
                "claude-code" => count_recent(&home.join(".claude/projects"), "jsonl", 2, cutoff),
                "codex" => count_recent(&home.join(".codex/sessions"), "jsonl", 8, cutoff),
                _ => 0,
            };
            MineSource { id: (*id).into(), label: (*label).into(), sessions: n }
        })
        .collect()
}

/// Count files with `ext` under `base` (up to `depth` levels) modified after
/// `cutoff`. Cheap stat-only walk; never recurses past the depth budget.
fn count_recent(base: &Path, ext: &str, depth: usize, cutoff: SystemTime) -> usize {
    fn walk(dir: &Path, ext: &str, depth: usize, cutoff: SystemTime, n: &mut usize) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            if p.is_dir() {
                if depth > 0 {
                    walk(&p, ext, depth - 1, cutoff, n);
                }
            } else if p.extension().and_then(|x| x.to_str()) == Some(ext)
                && e.metadata().and_then(|m| m.modified()).map(|m| m >= cutoff).unwrap_or(false)
            {
                *n += 1;
            }
        }
    }
    let mut n = 0;
    walk(base, ext, depth, cutoff, &mut n);
    n
}

/// First-use install of the bundled skill-miner, into every canonical skills
/// location whose agent cohort is present — the same destinations the
/// load-secrets activation skill uses (the shared `~/.agents/skills`, plus the
/// holdouts that don't read it: Claude Code's `~/.claude/skills`, OpenClaw's).
/// Per location it installs only when missing: each installed copy is the
/// user's afterwards — editable and versionable — and runs never overwrite it;
/// deleting a copy restores the official version on the next run. Returns the
/// primary copy (the shared dir when eligible).
fn ensure_installed_in(home: &Path, bundled: Option<&Path>) -> Result<PathBuf, String> {
    let bundled = bundled.filter(|s| s.join("SKILL.md").exists());
    let missing_src = || "Bundled skill-miner skill not found.".to_string();
    let mut primary: Option<PathBuf> = None;
    for skills_dir in agents::install_dirs(home) {
        let target = skills_dir.join(MINER_SKILL);
        if !target.join("SKILL.md").exists() {
            install_skill(bundled.ok_or_else(missing_src)?, &target)?;
        }
        primary.get_or_insert(target);
    }
    match primary {
        Some(p) => Ok(p),
        // No canonical dir on this machine (bare home) — fall back to creating
        // the shared standard dir rather than dead-ending the run.
        None => {
            let target = home.join(".agents/skills").join(MINER_SKILL);
            if !target.join("SKILL.md").exists() {
                install_skill(bundled.ok_or_else(missing_src)?, &target)?;
            }
            Ok(target)
        }
    }
}

/// Force-reinstall the bundled skill-miner into every canonical location a
/// run would use — the mine dialog's explicit "reinstall official version"
/// action, for when an installed copy has drifted or broken. Same destination
/// walk as [`ensure_installed_in`], but unconditional. Returns the restored
/// roots.
pub fn reinstall_miner(bundled: Option<&Path>) -> Result<Vec<String>, String> {
    let home = dirs::home_dir().ok_or_else(|| "Cannot locate home directory.".to_string())?;
    reinstall_miner_in(&home, bundled)
}

fn reinstall_miner_in(home: &Path, bundled: Option<&Path>) -> Result<Vec<String>, String> {
    let bundled = bundled
        .filter(|s| s.join("SKILL.md").exists())
        .ok_or_else(|| "Bundled skill-miner skill not found.".to_string())?;
    let mut restored = Vec::new();
    for skills_dir in agents::install_dirs(home) {
        let target = skills_dir.join(MINER_SKILL);
        install_skill(bundled, &target)?;
        commit_synced(&target);
        restored.push(target.to_string_lossy().into_owned());
    }
    if restored.is_empty() {
        let target = home.join(".agents/skills").join(MINER_SKILL);
        install_skill(bundled, &target)?;
        commit_synced(&target);
        restored.push(target.to_string_lossy().into_owned());
    }
    Ok(restored)
}

/// Commit the just-restored copy so the refresh doesn't linger as an uncommitted
/// diff against the user's version history. Best-effort: silently skips an
/// untracked copy, a no-op (already in sync), or a machine with no git identity
/// — the reinstall itself already succeeded.
fn commit_synced(target: &Path) {
    if target.join(".git").is_dir() {
        let _ = crate::gitops::git_commit(&target.to_string_lossy(), "sync to vendored version");
    }
}

/// Readiness of the installed skill-miner relative to the bundled official copy,
/// for the mine dialog. `drifted` is a NEUTRAL signal — the installed copy is
/// deliberately editable, so it can differ because the user customized it OR
/// because a newer VibeStudio shipped an updated skill (e.g. a new transcript
/// adapter) the user hasn't pulled — either way "Reinstall" is the way to the
/// official version.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MinerStatus {
    /// At least one canonical copy of the skill is installed.
    pub installed: bool,
    /// An installed copy's content differs from the bundled version.
    pub drifted: bool,
}

/// Content signature of a skill tree: relative-path → content hash, over every
/// file except the build/VCS junk `install_skill` skips (`.git`, `__pycache__`
/// a run leaves behind, …). Not cryptographic — only ever compared against
/// another signature computed the same way in the same process, to spot drift.
fn tree_sig(root: &Path) -> std::collections::BTreeMap<String, u64> {
    use std::hash::{Hash, Hasher};
    let mut sig = std::collections::BTreeMap::new();
    let ignored = |p: &Path| {
        p.file_name()
            .and_then(|n| n.to_str())
            .map(|n| IGNORED_DIRS.contains(&n))
            .unwrap_or(false)
    };
    for entry in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| !ignored(e.path()))
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(root) else { continue };
        let Ok(bytes) = std::fs::read(entry.path()) else { continue };
        let mut h = std::collections::hash_map::DefaultHasher::new();
        bytes.hash(&mut h);
        sig.insert(rel.to_string_lossy().replace('\\', "/"), h.finish());
    }
    sig
}

/// Compare each installed skill-miner copy against the bundled official version.
/// Best-effort: if the bundled source can't be located we report `drifted: false`
/// rather than nag with no way to fix it.
pub fn miner_status(bundled: Option<&Path>) -> MinerStatus {
    match dirs::home_dir() {
        Some(home) => miner_status_in(&home, bundled),
        None => MinerStatus { installed: false, drifted: false },
    }
}

fn miner_status_in(home: &Path, bundled: Option<&Path>) -> MinerStatus {
    let copies: Vec<PathBuf> = agents::install_dirs(home)
        .iter()
        .map(|d| d.join(MINER_SKILL))
        .filter(|t| t.join("SKILL.md").exists())
        .collect();
    let installed = !copies.is_empty();
    let Some(bundled) = bundled.filter(|s| s.join("SKILL.md").exists()) else {
        return MinerStatus { installed, drifted: false };
    };
    let want = tree_sig(bundled);
    let drifted = copies.iter().any(|c| tree_sig(c) != want);
    MinerStatus { installed, drifted }
}

/// Everything the route layer needs to spawn the run's terminal.
pub struct PreparedRun {
    pub run_dir: String,
    pub prompt: String,
    days: u64,
    sources: Vec<String>,
    improve: bool,
}

fn default_sources(sources: &[String]) -> Vec<String> {
    if sources.is_empty() {
        SOURCES.iter().map(|(id, _)| (*id).to_string()).collect()
    } else {
        sources.to_vec()
    }
}

/// The prompt a run with these settings would send — the dialog's editable
/// preview. Pure read: no run-dir reset, no skill install, no record.
pub fn preview_prompt(days: u64, improve: bool) -> Result<String, String> {
    Ok(compose_prompt(days, improve))
}

/// Set up a run: install the skill-miner where missing, reset the run dir,
/// and compose the agent prompt (or take the dialog's edited
/// `prompt_override` verbatim). The caller spawns the terminal and then calls
/// [`record_run`].
pub fn prepare_run(
    agent_family: &str,
    days: u64,
    sources: &[String],
    improve: bool,
    prompt_override: Option<&str>,
    bundled_miner: Option<&Path>,
) -> Result<PreparedRun, String> {
    migrate_legacy();
    if load_run().map(|r| r.status == "running").unwrap_or(false) {
        return Err("A mining run is already in progress.".into());
    }
    if !agents::can_launch(agent_family) {
        return Err(format!("{agent_family} can't run skill mining yet."));
    }
    let home = dirs::home_dir().ok_or_else(|| "Cannot locate home directory.".to_string())?;

    ensure_installed_in(&home, bundled_miner)?;

    // Archive the previous run under history/<id>/, then lay down a fresh run dir.
    reset_for_new_run()?;
    let rdir = run_dir()?;

    let sources = default_sources(sources);
    let prompt = match prompt_override.map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) => p.to_string(),
        None => compose_prompt(days, improve),
    };
    Ok(PreparedRun {
        run_dir: rdir.to_string_lossy().into_owned(),
        prompt,
        days,
        sources,
        improve,
    })
}

fn compose_prompt(days: u64, improve: bool) -> String {
    // Run-specific parameters ONLY — the skill's own SKILL.md covers the
    // pipeline, caps, staging area, quality bar, report content, and the
    // output location (./out of the launch dir — the run dir, since the
    // session spawns with cwd = run_dir; `state` watches it). Don't repeat
    // any of it here. ensure_installed_in put the skill in the agent's skills
    // dir, so the "mine agent conversations" ask matches its description in
    // the agent's available-skills list.
    let mut lines = vec![format!(
        "Mine agent conversations in the past {days} days for skills to create / update"
    )];
    if !improve {
        lines.push(String::new());
        lines.push("Do not edit any existing skill in place; only stage brand-new skills.".into());
    }
    // No report file: the skill's own report step is the final message (visible
    // in the terminal, where the conversation stays open for follow-ups).
    // No structured results either — staged proposals surface via the skills
    // scan, in-place edits via git dirty tracking — the prompt carries no
    // output contract.
    lines.join("\n")
}

/// The full shell line for a mining run: the agent's interactive TUI from the
/// agent registry, launched with the run prompt pre-submitted — an ordinary
/// agent session in the run dir's terminal, where the user watches, answers
/// any first-run dialog, and follows up in place.
///
/// None when the family has no launch line — the caller surfaces that as
/// "this agent can't power mining runs".
pub fn launch_cmd(
    agent_family: &str,
    bin: &str,
    prompt: &str,
    model: Option<&str>,
    effort: Option<&str>,
) -> Option<String> {
    let def = agents::by_family(agent_family)?;
    Some((def.launch?)(&LaunchCtx { bin, prompt, model, effort }))
}

/// Persist the run record once the terminal is up.
pub fn record_run(
    prep: PreparedRun,
    agent: &str,
    model: &str,
    effort: &str,
    terminal_id: &str,
) -> Result<MineState, String> {
    let rec = RunRecord {
        id: format!("mine-{}", now_unix()),
        started_unix: now_unix(),
        days: prep.days,
        sources: prep.sources,
        agent: agent.to_string(),
        model: model.to_string(),
        effort: effort.to_string(),
        improve: prep.improve,
        prompt: prep.prompt,
        terminal_id: terminal_id.to_string(),
        continued_unix: 0,
        status: "running".into(),
    };
    let _guard = run_lock();
    save_run(&rec)?;
    state_inner(|_| true, |_| true) // the session was just created; it's alive
}

/// A revival younger than this is trusted without probing: under the
/// `bash -lc` wrapper the TUI may not have forked yet, and probing it would
/// read "exited" and spawn a duplicate.
const REVIVE_GRACE_SECS: u64 = 60;

/// "Continue the conversation" target: the recorded terminal while the agent
/// is still live in it (the run's own TUI, or an earlier revival), else a
/// fresh terminal reviving the run dir's conversation — for when the TUI was
/// quit or its terminal closed. The dead pane is left alone (its scrollback
/// holds the run's report and any failure output; the terminal GC reaps it);
/// the record just moves to the new terminal.
/// `spawn_resume` is the route layer's
/// `create_session_resume(agent_id, cwd, model, effort)` — the terminal
/// API's resume path, so the resume line is built in exactly one place.
pub fn continue_run(
    session_exists: impl Fn(&str) -> bool,
    agent_running: impl Fn(&str) -> bool,
    spawn_resume: impl Fn(&str, &str, Option<&str>, Option<&str>) -> Result<String, String>,
) -> Result<String, String> {
    let _guard = run_lock();
    migrate_legacy();
    let mut rec = load_run().ok_or_else(|| "No mining run on record.".to_string())?;
    if rec.status == "running"
        || now_unix().saturating_sub(rec.continued_unix) <= REVIVE_GRACE_SECS
        || (session_exists(&rec.terminal_id) && agent_running(&rec.terminal_id))
    {
        return Ok(rec.terminal_id);
    }
    let rdir = run_dir()?;
    let id = spawn_resume(
        &rec.agent,
        &rdir.to_string_lossy(),
        Some(rec.model.as_str()).filter(|m| !m.is_empty()),
        Some(rec.effort.as_str()).filter(|e| !e.is_empty()),
    )?;
    rec.terminal_id = id.clone();
    rec.continued_unix = now_unix();
    save_run(&rec)?;
    Ok(id)
}

/// Current run state. The two probes are supplied by the route layer (it owns
/// skill-term) and only consulted while the record still says "running":
/// `session_exists` = the tmux session is listed; `agent_running` = the pane
/// isn't back to a plain shell. An interactive run has no completion
/// sentinel — the agent reports in its pane and the conversation stays open —
/// so "running" simply means the TUI is still up in the recorded terminal.
/// A startup grace period covers the moment the launch line is still spinning
/// up under the pane's shell.
pub fn state(
    session_exists: impl Fn(&str) -> bool,
    agent_running: impl Fn(&str) -> bool,
) -> Result<MineState, String> {
    let _guard = run_lock();
    migrate_legacy();
    state_inner(session_exists, agent_running)
}

fn state_inner(
    session_exists: impl Fn(&str) -> bool,
    agent_running: impl Fn(&str) -> bool,
) -> Result<MineState, String> {
    let Some(mut rec) = load_run() else {
        return Ok(MineState {
            status: "idle".into(),
            stage: None,
            found: None,
            started_unix: None,
            terminal_id: None,
            agent: None,
            model: None,
            effort: None,
            days: None,
            sources: None,
            prompt: None,
        });
    };
    let rdir = run_dir()?;

    // Records from the retired headless pipeline used "done" / "stopped".
    if rec.status == "done" || rec.status == "stopped" {
        rec.status = "ended".into();
        let _ = save_run(&rec);
    } else if rec.status == "running" {
        let past_grace = now_unix().saturating_sub(rec.started_unix) > 60;
        if !session_exists(&rec.terminal_id) || (past_grace && !agent_running(&rec.terminal_id)) {
            rec.status = "ended".into();
            let _ = save_run(&rec);
        }
    }

    let stage = (rec.status == "running").then(|| {
        if rdir.join("out/conversations.jsonl").exists() {
            "reviewing".to_string()
        } else if rdir.join("out/inventory.jsonl").exists() {
            "analyzing".to_string()
        } else {
            "scanning".to_string()
        }
    });
    let found = count_lines(&rdir.join("out/inventory.jsonl"));

    Ok(MineState {
        status: rec.status.clone(),
        stage,
        found,
        started_unix: Some(rec.started_unix),
        terminal_id: Some(rec.terminal_id.clone()),
        agent: Some(rec.agent.clone()),
        model: Some(rec.model.clone()),
        effort: Some(rec.effort.clone()),
        days: Some(rec.days),
        sources: Some(rec.sources.clone()),
        prompt: Some(rec.prompt.clone()),
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MineFile {
    pub rel: String,
    pub size: u64,
    pub modified_unix: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MineFiles {
    pub run_dir: String,
    pub files: Vec<MineFile>,
}

/// Everything in the active run dir — rel path, size, mtime, newest first.
/// The `history/` archive of past runs is skipped (it isn't part of the live
/// run). Powers the mining page's artifacts listing; viewing a file goes
/// through the generic read-file route with `run_dir` as the root.
pub fn files() -> Result<MineFiles, String> {
    fn walk(base: &Path, dir: &Path, skip: &Path, out: &mut Vec<MineFile>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            if p == skip {
                continue;
            }
            if p.is_dir() {
                walk(base, &p, skip, out);
            } else if let Ok(meta) = e.metadata() {
                let rel = p.strip_prefix(base).unwrap_or(&p).to_string_lossy().into_owned();
                let modified_unix = meta
                    .modified()
                    .ok()
                    .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                out.push(MineFile { rel, size: meta.len(), modified_unix });
            }
        }
    }
    migrate_legacy();
    let rdir = run_dir()?;
    let hdir = history_dir()?;
    let mut files = Vec::new();
    walk(&rdir, &rdir, &hdir, &mut files);
    files.sort_by(|a, b| b.modified_unix.cmp(&a.modified_unix).then_with(|| a.rel.cmp(&b.rel)));
    Ok(MineFiles { run_dir: rdir.to_string_lossy().into_owned(), files })
}

/// A past run's summary for the mining page's "Past runs" list — read straight
/// from each `history/<id>/run.json`. Display-only for now: the artifacts stay
/// on disk under the id, but there's no per-session reopen yet.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MineHistoryEntry {
    pub id: String,
    pub agent: String,
    pub model: String,
    pub effort: String,
    pub days: u64,
    pub sources: Vec<String>,
    pub started_unix: u64,
    /// The prompt this run was launched with (empty for pre-capture records).
    pub prompt: String,
    /// Always "ended" — an archived run is no longer live.
    pub status: String,
}

/// Archived past runs, newest first. Best-effort: a history dir whose run.json
/// is missing or unreadable is skipped rather than failing the whole listing.
pub fn history() -> Result<Vec<MineHistoryEntry>, String> {
    migrate_legacy();
    history_in(&mining_dir()?)
}

fn history_in(mdir: &Path) -> Result<Vec<MineHistoryEntry>, String> {
    let hdir = mdir.join(HISTORY_DIR);
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&hdir) {
        for e in rd.filter_map(|e| e.ok()) {
            if !e.path().is_dir() {
                continue;
            }
            let raw = match std::fs::read_to_string(e.path().join(RUN_FILE)) {
                Ok(r) => r,
                Err(_) => continue,
            };
            let Ok(rec) = serde_json::from_str::<RunRecord>(&raw) else { continue };
            out.push(MineHistoryEntry {
                id: rec.id,
                agent: rec.agent,
                model: rec.model,
                effort: rec.effort,
                days: rec.days,
                sources: rec.sources,
                started_unix: rec.started_unix,
                prompt: rec.prompt,
                status: "ended".into(),
            });
        }
    }
    out.sort_by(|a, b| b.started_unix.cmp(&a.started_unix).then_with(|| b.id.cmp(&a.id)));
    Ok(out)
}

/// Stop a running run: the route layer kills the terminal; we mark the record.
pub fn stop(kill: impl Fn(&str) -> Result<(), String>) -> Result<(), String> {
    let _guard = run_lock();
    let Some(mut rec) = load_run() else { return Ok(()) };
    if rec.status == "running" {
        let _ = kill(&rec.terminal_id);
        rec.status = "ended".into();
        save_run(&rec)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_cmd_uses_interactive_tuis() {
        // claude: the interactive TUI (no -p / stream-json), prompt as the
        // positional initial message — FIRST, since the variadic --add-dir
        // would swallow a trailing positional — and auto permission mode.
        let c = launch_cmd("claude", "/bin/claude", "do the thing", Some("opus"), Some("max"))
            .expect("claude has a launch line");
        assert!(c.starts_with("'/bin/claude' 'do the thing' --permission-mode auto --model 'opus' --effort 'max'"));
        assert!(!c.contains(" -p ") && !c.contains("--output-format"), "interactive, not print mode");
        // No chained resume: continuing the conversation is continue_run's job.
        assert!(!c.contains("--resume") && !c.contains("--continue"));
        // model/effort omitted entirely when unset.
        let c2 = launch_cmd("claude", "/bin/claude", "p", None, None).unwrap();
        assert!(!c2.contains("--model") && !c2.contains("--effort"));
        // codex: the TUI (not the headless exec subcommand) with its native
        // approval prompts — someone is watching the pane now.
        let x = launch_cmd("codex", "/bin/codex", "do the thing", Some("gpt-5.5"), Some("xhigh"))
            .expect("codex has a launch line");
        assert!(x.starts_with("'/bin/codex' 'do the thing' -m 'gpt-5.5' -c 'model_reasoning_effort=\"xhigh\"'"));
        assert!(!x.contains("exec") && !x.contains("approval_policy"));
        // gemini submits via -i (a bare positional would run headless).
        let g = launch_cmd("gemini", "/bin/gemini", "p", Some("pro"), None).unwrap();
        assert_eq!(g, "'/bin/gemini' -m 'pro' -i 'p'");
        // cursor: positional prompt, --model is the only tuning knob.
        let u = launch_cmd("cursor", "/bin/cursor-agent", "p", Some("gpt-5.5"), None).unwrap();
        assert_eq!(u, "'/bin/cursor-agent' 'p' --model 'gpt-5.5'");
        // No launch line (or unknown family) ⇒ no run.
        assert!(launch_cmd("openclaw", "/bin/o", "p", None, None).is_none());
        assert!(launch_cmd("shell", "/bin/bash", "p", None, None).is_none());
    }

    #[test]
    fn ensure_installed_covers_claude_and_never_overwrites() {
        let base = std::env::temp_dir().join(format!("ass_mine_ensure_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        // Claude Code (own folder) and Codex (shared-dir cohort) are present.
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        let src = base.join("bundled/skill-miner");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("SKILL.md"), "official").unwrap();

        let primary = ensure_installed_in(&home, Some(&src)).unwrap();

        // Both the shared dir and Claude's own dir get a copy; the shared one
        // is primary (it's what the run prompt references).
        let shared = home.join(".agents/skills/skill-miner");
        let claude = home.join(".claude/skills/skill-miner");
        assert_eq!(primary, shared);
        assert!(shared.join("SKILL.md").exists());
        assert!(claude.join("SKILL.md").exists());

        // User edits are never overwritten by later runs — per copy.
        std::fs::write(shared.join("SKILL.md"), "user-tuned").unwrap();
        let again = ensure_installed_in(&home, Some(&src)).unwrap();
        assert_eq!(again, shared);
        assert_eq!(std::fs::read_to_string(shared.join("SKILL.md")).unwrap(), "user-tuned");

        // Deleting one copy restores just that copy on the next run.
        std::fs::remove_dir_all(&claude).unwrap();
        ensure_installed_in(&home, Some(&src)).unwrap();
        assert_eq!(std::fs::read_to_string(claude.join("SKILL.md")).unwrap(), "official");
        assert_eq!(std::fs::read_to_string(shared.join("SKILL.md")).unwrap(), "user-tuned");

        // A bare home (no agent dotdirs) falls back to the shared standard dir.
        let bare = base.join("bare-home");
        std::fs::create_dir_all(&bare).unwrap();
        let p = ensure_installed_in(&bare, Some(&src)).unwrap();
        assert_eq!(p, bare.join(".agents/skills/skill-miner"));
        assert!(p.join("SKILL.md").exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn miner_status_flags_only_real_content_drift() {
        let base = std::env::temp_dir().join(format!("ass_mine_status_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        std::fs::create_dir_all(home.join(".codex")).unwrap(); // cohort → shared dir
        // Bundled official source: SKILL.md + a nested script (the drift lives in scripts/).
        let src = base.join("bundled/skill-miner");
        std::fs::create_dir_all(src.join("scripts")).unwrap();
        std::fs::write(src.join("SKILL.md"), "official").unwrap();
        std::fs::write(src.join("scripts/common.py"), "v1").unwrap();

        // Nothing installed yet.
        let s = miner_status_in(&home, Some(&src));
        assert!(!s.installed && !s.drifted);

        // Install a copy → identical content → no drift.
        ensure_installed_in(&home, Some(&src)).unwrap();
        let installed = home.join(".agents/skills/skill-miner");
        let s = miner_status_in(&home, Some(&src));
        assert!(s.installed && !s.drifted, "fresh install must not look drifted");

        // A __pycache__ a run leaves behind is ignored (not drift).
        std::fs::create_dir_all(installed.join("scripts/__pycache__")).unwrap();
        std::fs::write(installed.join("scripts/__pycache__/common.cpython-312.pyc"), "junk").unwrap();
        assert!(!miner_status_in(&home, Some(&src)).drifted, "build junk must not count");

        // Editing a script (e.g. an outdated copy missing the opencode adapter) IS drift.
        std::fs::write(installed.join("scripts/common.py"), "v2-stale").unwrap();
        assert!(miner_status_in(&home, Some(&src)).drifted, "script content change is drift");

        // No bundled source to compare against → never nag.
        assert!(!miner_status_in(&home, None).drifted);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn reinstall_commits_a_tracked_copy_so_no_diff_lingers() {
        use crate::gitops;
        if !gitops::git_available() {
            return; // skip on machines without git
        }
        let base = std::env::temp_dir().join(format!("ass_mine_reinstall_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        std::fs::create_dir_all(home.join(".codex")).unwrap(); // cohort → shared dir
        let src = base.join("bundled/skill-miner");
        std::fs::create_dir_all(src.join("scripts")).unwrap();
        std::fs::write(src.join("SKILL.md"), "official v1").unwrap();
        std::fs::write(src.join("scripts/common.py"), "adapter v1").unwrap();

        // First install + version it (as auto-track would), with a local identity.
        ensure_installed_in(&home, Some(&src)).unwrap();
        let copy = home.join(".agents/skills/skill-miner");
        let root = copy.to_string_lossy().to_string();
        gitops::git_init(&root).unwrap();
        let _ = gitops::git(&copy, &["config", "user.email", "test@example.com"]);
        let _ = gitops::git(&copy, &["config", "user.name", "Test"]);
        gitops::git_commit(&root, "baseline").unwrap();
        assert!(gitops::git_status(&root).unwrap().is_empty(), "clean after baseline");

        // A newer bundled version (e.g. adds the opencode adapter) → reinstall.
        std::fs::write(src.join("scripts/common.py"), "adapter v2 + opencode").unwrap();
        reinstall_miner_in(&home, Some(&src)).unwrap();

        // The restore is committed as "sync to vendored version" — no lingering diff.
        assert!(gitops::git_status(&root).unwrap().is_empty(), "no uncommitted diff after reinstall");
        assert_eq!(gitops::recent_subjects(&root, 1).first().map(String::as_str), Some("sync to vendored version"));
        assert!(
            std::fs::read_to_string(copy.join("scripts/common.py")).unwrap().contains("opencode"),
            "the new bundled content actually landed"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Lay down an active run (record + an `out/` artifact) at `dir`.
    fn write_record(dir: &Path, id: &str, started: u64, agent: &str) {
        std::fs::create_dir_all(dir.join("out")).unwrap();
        std::fs::write(dir.join("out/inventory.jsonl"), "{}\n{}\n").unwrap();
        let rec = RunRecord {
            id: id.into(),
            started_unix: started,
            days: 30,
            sources: vec!["claude-code".into()],
            agent: agent.into(),
            model: String::new(),
            effort: String::new(),
            improve: true,
            prompt: format!("Mine the past 30 days ({id})"),
            terminal_id: "t1".into(),
            continued_unix: 0,
            status: "ended".into(),
        };
        std::fs::write(dir.join(RUN_FILE), serde_json::to_string_pretty(&rec).unwrap()).unwrap();
    }

    #[test]
    fn new_run_archives_the_previous_instead_of_wiping() {
        let base = std::env::temp_dir().join(format!("ass_mine_archive_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let mdir = base.join("mining");
        std::fs::create_dir_all(&mdir).unwrap();

        // First run lives at the mining root; starting a second archives it.
        write_record(&mdir, "mine-100", 100, "claude");
        reset_for_new_run_in(&mdir).unwrap();
        assert!(!mdir.join(RUN_FILE).exists(), "active record cleared for the new run");
        assert!(
            std::fs::read_dir(mdir.join("out")).unwrap().next().is_none(),
            "the new run starts with an empty out/"
        );
        let arch1 = mdir.join(HISTORY_DIR).join("mine-100");
        assert!(arch1.join(RUN_FILE).exists(), "the first run is kept under its id");
        assert!(arch1.join("out/inventory.jsonl").exists(), "its artifacts moved with it");

        // Run + start a third → the second is archived too, the first untouched.
        write_record(&mdir, "mine-200", 200, "codex");
        reset_for_new_run_in(&mdir).unwrap();
        assert!(mdir.join(HISTORY_DIR).join("mine-200").join(RUN_FILE).exists());
        assert!(arch1.join(RUN_FILE).exists(), "history accumulates, never wiped");

        // Listing is newest-first and carries the agent/id for display.
        let hist = history_in(&mdir).unwrap();
        assert_eq!(hist.iter().map(|h| h.id.as_str()).collect::<Vec<_>>(), ["mine-200", "mine-100"]);
        assert_eq!(hist[0].agent, "codex");
        assert_eq!(hist[0].prompt, "Mine the past 30 days (mine-200)", "the launch prompt is kept for display");
        assert!(hist.iter().all(|h| h.status == "ended"));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn migrate_promotes_a_legacy_current_run() {
        let base = std::env::temp_dir().join(format!("ass_mine_migrate_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let mdir = base.join("mining");
        // Pre-history layout kept the active run in current/.
        write_record(&mdir.join("current"), "mine-50", 50, "claude");

        migrate_legacy_in(&mdir);
        assert!(!mdir.join("current").exists(), "legacy dir removed after promotion");
        assert!(mdir.join(RUN_FILE).exists(), "run promoted to the new active slot");
        assert!(mdir.join("out/inventory.jsonl").exists(), "its artifacts came along");

        // Idempotent: a second pass is a no-op and keeps the promoted run.
        migrate_legacy_in(&mdir);
        assert!(mdir.join(RUN_FILE).exists());

        let _ = std::fs::remove_dir_all(&base);
    }
}
