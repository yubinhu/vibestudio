// Per-skill git version control. Shells out to the system `git` (like editors do)
// so no native git library is bundled. Every op is a no-op or a clear error when
// git is unavailable or the directory isn't a repository.
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::process::hidden_command;

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GitInfo {
    available: bool,
    pub(crate) is_repo: bool,
    in_parent_repo: bool,
    toplevel: Option<String>,
    branch: Option<String>,
    dirty: bool,
    has_remote: bool,
    has_identity: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitResult {
    sha: String,
    summary: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Commit {
    /// Full SHA — the handle used to fetch this commit's diff.
    sha: String,
    short: String,
    message: String,
    author: String,
    /// ISO-8601 author date (for an absolute-date tooltip).
    iso_date: String,
    relative_date: String,
    /// 1-based version number: the commit's position in linear history (first
    /// commit = 1, newest = total). Monotonic for the single-user, no-merge repos
    /// the studio creates.
    number: usize,
}

/// One entry in the working tree's change set (a `git status` line).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileChange {
    /// Path relative to the repo root (the new path for a rename).
    path: String,
    /// The previous path for a rename/copy.
    orig_path: Option<String>,
    /// added | modified | deleted | renamed | copied | untracked | typechange | unmerged
    kind: String,
    /// Recorded in the index (staged for the next commit).
    staged: bool,
    /// Differs in the working tree beyond what's staged.
    unstaged: bool,
}

/// The working tree's uncommitted state: a per-file summary plus one unified
/// diff covering every change (tracked edits vs HEAD + synthesized adds for
/// untracked files), so the UI can render the whole thing or slice it per file.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeDiff {
    files: Vec<FileChange>,
    /// Concatenated unified diff text (empty when the tree is clean).
    diff: String,
    /// The diff hit the size cap and was cut short.
    truncated: bool,
}

/// A single commit's metadata and its full unified diff (vs its first parent;
/// the root commit diffs against the empty tree).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitDetail {
    sha: String,
    short: String,
    subject: String,
    body: String,
    author: String,
    email: String,
    iso_date: String,
    relative_date: String,
    diff: String,
    truncated: bool,
    /// 1-based version number (this commit's position in linear history).
    number: usize,
}

pub(crate) fn git(root: &Path, args: &[&str]) -> Result<std::process::Output, String> {
    hidden_command("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|e| format!("Failed to run git: {e}"))
}

/// Run a git command, returning trimmed stdout only on success.
pub(crate) fn git_ok(root: &Path, args: &[&str]) -> Option<String> {
    let out = git(root, args).ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The skill's path within its repo, with a trailing slash (e.g. "skills/foo/"),
/// or "" when the skill *is* the repo's top level. git reports status/diff paths
/// from the repo root, so inside a parent repo we strip this to recover
/// skill-relative paths (what the rest of the studio works in).
fn repo_prefix(root: &Path) -> String {
    git_ok(root, &["rev-parse", "--show-prefix"]).unwrap_or_default()
}

/// Strip the repo prefix off a repo-root-relative path, yielding a skill-relative
/// one. A no-op when `prefix` is empty (the skill is its own repo).
fn strip_repo_prefix(path: &str, prefix: &str) -> String {
    path.strip_prefix(prefix).unwrap_or(path).to_string()
}

pub fn git_available() -> bool {
    hidden_command("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Patterns VibeStudio keeps out of every skill repo's version history: build
/// artifacts and machine-local secrets. `git add -A` (run on every "Save
/// version") would otherwise capture them — and a published repo's history is
/// shared. Written to the repo's LOCAL `.git/info/exclude`, NOT a committed
/// `.gitignore`: per-repo, never committed, never pushed, never global, so
/// nothing clutters the worktree or the published repo.
const EXCLUDE_PATTERNS: &[&str] =
    &["__pycache__/", "*.py[cod]", ".venv/", "node_modules/", ".DS_Store", ".env", ".env.*"];

/// Ensure this repo's local `.git/info/exclude` carries [`EXCLUDE_PATTERNS`].
/// Idempotent and additive: any lines already there (incl. the user's own) are
/// kept; only missing patterns are appended. Best-effort — failure never blocks
/// a read or a commit. Caller must only invoke this for a repo we own (not a
/// skill living inside someone else's parent repo).
fn ensure_exclude(root: &Path) {
    // Resolve the real path — handles linked worktrees and `.git`-file repos.
    let rel = git_ok(root, &["rev-parse", "--git-path", "info/exclude"])
        .unwrap_or_else(|| ".git/info/exclude".into());
    let path = root.join(rel);
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let have: std::collections::HashSet<&str> = existing.lines().map(str::trim).collect();
    let missing: Vec<&str> = EXCLUDE_PATTERNS.iter().copied().filter(|p| !have.contains(p)).collect();
    if missing.is_empty() {
        return;
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut body = existing;
    if body.is_empty() {
        body.push_str("# VibeStudio — local-only ignores (build junk + secrets); not committed.\n");
    } else if !body.ends_with('\n') {
        body.push('\n');
    }
    for p in missing {
        body.push_str(p);
        body.push('\n');
    }
    let _ = std::fs::write(&path, body);
}

pub fn git_info(root: &str) -> Result<GitInfo, String> {
    let root_path = PathBuf::from(root);
    let mut info = GitInfo {
        available: git_available(),
        ..Default::default()
    };
    if !info.available {
        return Ok(info);
    }
    let canon_root = std::fs::canonicalize(&root_path).unwrap_or_else(|_| root_path.clone());
    if let Some(top) = git_ok(&root_path, &["rev-parse", "--show-toplevel"]) {
        let canon_top = std::fs::canonicalize(&top).unwrap_or_else(|_| PathBuf::from(&top));
        info.toplevel = Some(canon_top.to_string_lossy().into_owned());
        if canon_top == canon_root {
            info.is_repo = true;
        } else {
            info.in_parent_repo = true;
        }
    }
    if info.is_repo {
        // Seed the local ignore the moment a skill's own repo is viewed, so the
        // dirty/change-list computed just below already excludes build junk —
        // existing repos get covered without waiting for the next save. Only for
        // repos we own; a skill inside a parent repo is someone else's to ignore.
        ensure_exclude(&root_path);
        info.branch = git_ok(&root_path, &["branch", "--show-current"]).filter(|s| !s.is_empty());
        info.has_remote = git_ok(&root_path, &["remote"]).map(|s| !s.is_empty()).unwrap_or(false);
    }
    // `dirty` + identity are meaningful for your own repo AND for a skill living
    // inside a parent repo — scope the status to this folder (`-- .`) so changes
    // elsewhere in a parent repo don't count this skill as dirty.
    if info.is_repo || info.in_parent_repo {
        info.dirty = git_ok(&root_path, &["status", "--porcelain", "--", "."])
            .map(|s| !s.is_empty())
            .unwrap_or(false);
        info.has_identity = git_ok(&root_path, &["config", "user.email"])
            .map(|s| !s.is_empty())
            .unwrap_or(false);
    }
    Ok(info)
}

/// A skill root paired with whether its own folder has uncommitted changes.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirtyState {
    pub(crate) root: String,
    pub(crate) dirty: bool,
}

/// Batch "has uncommitted changes?" for the home page — one cheap
/// `git status --porcelain -- .` per skill root (scoped to the skill's own folder,
/// so changes elsewhere in a parent repo don't count). Roots not under git (or
/// when git is missing) report `dirty: false`. Far less chatter than a full
/// `git_info` round-trip per card.
pub fn git_dirty_many(roots: &[String]) -> Vec<DirtyState> {
    let available = git_available();
    roots
        .iter()
        .map(|r| {
            let dirty = available
                && git_ok(Path::new(r), &["status", "--porcelain", "--", "."])
                    .map(|s| !s.is_empty())
                    .unwrap_or(false);
            DirtyState { root: r.clone(), dirty }
        })
        .collect()
}

pub fn git_init(root: &str) -> Result<GitInfo, String> {
    if !git_available() {
        return Err("Git isn't installed.".into());
    }
    let root_path = PathBuf::from(root);
    let out = git(&root_path, &["init"])?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    git_info(root)
}

// ---- auto-tracking & opt-out -------------------------------------------------
//
// Personal skills are version-tracked by default: discovery inits a repo and
// lands a baseline commit for each standalone one. Opting out deletes the skill's
// `.git` and records the choice in a small denylist (canonicalized roots) so the
// next discovery doesn't simply re-create it.

fn untracked_store() -> Result<PathBuf, String> {
    Ok(crate::secrets::config_dir()?.join("untracked.json"))
}

/// Canonicalized skill root — the stable denylist key, so a path and its
/// symlinked/relative spellings compare equal. Falls back to the raw string when
/// the path can't be resolved.
fn canonical(root: &str) -> String {
    std::fs::canonicalize(root).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| root.to_string())
}

fn load_untracked() -> std::collections::BTreeSet<String> {
    untracked_store()
        .ok()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_untracked(set: &std::collections::BTreeSet<String>) {
    let Ok(path) = untracked_store() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_vec_pretty(set) {
        let _ = std::fs::write(path, json);
    }
}

fn add_untracked(root: &str) {
    let mut set = load_untracked();
    if set.insert(canonical(root)) {
        save_untracked(&set);
    }
}

fn remove_untracked(root: &str) {
    let mut set = load_untracked();
    if set.remove(&canonical(root)) {
        save_untracked(&set);
    }
}

/// `git init` + seed the local exclude + a baseline "Initial version" commit when
/// a git identity is set. The baseline keeps a fresh repo CLEAN (no false
/// "everything changed" state) and syncable right away. Without an identity we
/// stop at the empty repo — the Save-version flow surfaces the identity prompt on
/// the first manual save.
fn init_and_baseline(root: &str) -> Result<(), String> {
    let root_path = PathBuf::from(root);
    let out = git(&root_path, &["init"])?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    // Seed the ignore BEFORE the catch-all add so secrets/junk are never staged.
    ensure_exclude(&root_path);
    let has_identity = git_ok(&root_path, &["config", "user.email"]).map(|s| !s.is_empty()).unwrap_or(false)
        && git_ok(&root_path, &["config", "user.name"]).map(|s| !s.is_empty()).unwrap_or(false);
    if has_identity {
        let _ = git(&root_path, &["add", "-A"]);
        // "nothing to commit" (e.g. an empty skill dir) is fine — the repo stands.
        let _ = git(&root_path, &["commit", "-m", "Initial version"]);
    }
    Ok(())
}

/// Begin (or resume) tracking a skill: clear any prior opt-out, then init with a
/// baseline commit. Backs the "Start tracking" button — also the re-track path
/// for a skill the user previously opted out of.
pub fn git_track(root: &str) -> Result<GitInfo, String> {
    if !git_available() {
        return Err("Git isn't installed.".into());
    }
    remove_untracked(root);
    init_and_baseline(root)?;
    git_info(root)
}

/// Opt a skill out of tracking: record the choice (so auto-tracking won't undo
/// it) and delete the skill's OWN `.git`. Refuses when a parent repository owns
/// the history. Destructive — callers confirm first.
pub fn git_untrack(root: &str) -> Result<(), String> {
    if !git_available() {
        return Err("Git isn't installed.".into());
    }
    let info = git_info(root)?;
    if info.in_parent_repo {
        return Err("This skill is versioned by a parent repository — manage its history there.".into());
    }
    if !info.is_repo {
        add_untracked(root); // not a repo (already gone / never tracked): just record the opt-out
        return Ok(());
    }
    // Only ever remove the skill's own repo: confirm the repo top-level IS this
    // folder before deleting, so a parent repo's `.git` can never be wiped.
    let root_path = std::fs::canonicalize(root).map_err(|e| e.to_string())?;
    let top = info
        .toplevel
        .as_deref()
        .map(|t| std::fs::canonicalize(t).unwrap_or_else(|_| PathBuf::from(t)));
    if top.as_deref() != Some(root_path.as_path()) {
        return Err("Refusing to remove version history — this isn't the skill's own repository.".into());
    }
    // Record the opt-out before deleting so a half-failed delete still won't bounce
    // back to tracked on the next discovery (auto-track checks the denylist first).
    add_untracked(root);
    std::fs::remove_dir_all(root_path.join(".git")).map_err(|e| format!("Couldn't remove version history: {e}"))?;
    Ok(())
}

/// Ensure each personal-skill root is tracked. Cheaply skips roots that are
/// already repos (a `.git` stat) or opted out, and does the real work for the
/// rest off the request thread (the first run may init many repos). A root that
/// turns out to sit inside a parent repo is left alone — never a nested repo.
pub fn auto_track_personal(roots: Vec<String>) {
    if !git_available() {
        return;
    }
    let denied = load_untracked();
    let todo: Vec<String> = roots
        .into_iter()
        .filter(|r| !denied.contains(&canonical(r)) && !Path::new(r).join(".git").exists())
        .collect();
    if todo.is_empty() {
        return;
    }
    std::thread::spawn(move || {
        for root in todo {
            match git_info(&root) {
                Ok(info) if !info.is_repo && !info.in_parent_repo => {
                    let _ = init_and_baseline(&root);
                }
                _ => {}
            }
        }
    });
}

pub fn git_commit(root: &str, message: &str) -> Result<CommitResult, String> {
    if !git_available() {
        return Err("Git isn't installed.".into());
    }
    let root_path = PathBuf::from(root);
    let msg = message.trim();
    if msg.is_empty() {
        return Err("Enter a commit message.".into());
    }
    if git_ok(&root_path, &["config", "user.email"]).map(|s| s.is_empty()).unwrap_or(true) {
        return Err(
            "No git identity set. Run: git config --global user.email \"you@example.com\" (and user.name).".into(),
        );
    }
    // Guarantee build junk / secrets are ignored right before the catch-all add,
    // even if the repo was created before this guard or the exclude was removed.
    ensure_exclude(&root_path);
    let add = git(&root_path, &["add", "-A"])?;
    if !add.status.success() {
        return Err(String::from_utf8_lossy(&add.stderr).trim().to_string());
    }
    let out = git(&root_path, &["commit", "-m", msg])?;
    if !out.status.success() {
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        if combined.contains("nothing to commit") {
            return Err("Nothing to commit — no changes since the last version.".into());
        }
        return Err(combined.trim().to_string());
    }
    Ok(CommitResult {
        sha: git_ok(&root_path, &["rev-parse", "HEAD"]).unwrap_or_default(),
        summary: git_ok(&root_path, &["log", "-1", "--pretty=%s"]).unwrap_or_default(),
    })
}

pub fn git_log(root: &str, limit: usize) -> Result<Vec<Commit>, String> {
    if !git_available() {
        return Ok(vec![]);
    }
    let root_path = PathBuf::from(root);
    let n = limit.clamp(1, 200).to_string();
    // List from the live branch — or, while previewing a past version (detached
    // HEAD), the branch we detached from — so the full version list stays visible,
    // including versions NEWER than the one currently being previewed.
    let href = history_ref(&root_path);
    // Unit-separator (0x1f) between fields; newline between commits.
    let out = git(&root_path, &["log", "-n", &n, "--pretty=%H%x1f%h%x1f%s%x1f%an%x1f%aI%x1f%ar", &href])?;
    if !out.status.success() {
        return Ok(vec![]); // not a repo yet / no commits
    }
    // Total commits reachable from the history ref → the newest commit's version
    // number. Each line (newest first) is one history position, so line i has
    // number total - i, correct even when the log is capped below the total.
    let total: usize = git_ok(&root_path, &["rev-list", "--count", &href])
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let text = String::from_utf8_lossy(&out.stdout);
    let mut commits = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let parts: Vec<&str> = line.split('\u{1f}').collect();
        if parts.len() == 6 {
            commits.push(Commit {
                sha: parts[0].to_string(),
                short: parts[1].to_string(),
                message: parts[2].to_string(),
                author: parts[3].to_string(),
                iso_date: parts[4].to_string(),
                relative_date: parts[5].to_string(),
                number: total.saturating_sub(i),
            });
        }
    }
    Ok(commits)
}

/// Largest diff we ship to the UI. Skill repos are small; this only guards
/// against a stray huge/binary blob blowing up the payload.
const MAX_DIFF_BYTES: usize = 2_000_000;

/// True for a string git can safely take as a revision: a hex SHA (full or
/// abbreviated). Keeps caller-supplied values from being read as git options.
fn is_hex_rev(rev: &str) -> bool {
    let len = rev.len();
    (4..=64).contains(&len) && rev.chars().all(|c| c.is_ascii_hexdigit())
}

/// Map a porcelain XY code to (kind, staged, unstaged).
fn classify(code: &str) -> (&'static str, bool, bool) {
    if code == "??" {
        return ("untracked", false, true);
    }
    let x = code.chars().next().unwrap_or(' ');
    let y = code.chars().nth(1).unwrap_or(' ');
    let staged = x != ' ' && x != '?';
    let unstaged = y != ' ' && y != '?';
    let kind = if x == 'U' || y == 'U' || code == "AA" || code == "DD" {
        "unmerged"
    } else if x == 'R' || y == 'R' {
        "renamed"
    } else if x == 'C' || y == 'C' {
        "copied"
    } else if x == 'A' {
        "added"
    } else if x == 'D' || y == 'D' {
        "deleted"
    } else if x == 'T' || y == 'T' {
        "typechange"
    } else {
        "modified"
    };
    (kind, staged, unstaged)
}

/// Parse `git status --porcelain=v1 -z -uall` into a change list. The `-z`
/// stream is NUL-separated; a rename/copy entry is followed by its old path in
/// the next field, so we walk the tokens with a cursor rather than line-split.
fn parse_status(root: &Path) -> Vec<FileChange> {
    // `-- .` scopes the status to this folder so, when the skill lives inside a
    // larger parent repo, that repo's changes elsewhere don't show up here.
    let out = match git(root, &["status", "--porcelain=v1", "-z", "-uall", "--", "."]) {
        Ok(o) if o.status.success() => o.stdout,
        _ => return vec![],
    };
    // Paths come back relative to the repo root; inside a parent repo that carries
    // the skill's own sub-path prefix, which we strip to keep them skill-relative.
    let prefix = repo_prefix(root);
    let text = String::from_utf8_lossy(&out);
    let tokens: Vec<&str> = text.split('\0').filter(|t| !t.is_empty()).collect();
    let mut files = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let entry = tokens[i];
        i += 1;
        if entry.len() < 3 {
            continue;
        }
        let code = &entry[..2];
        let path = strip_repo_prefix(&entry[3..], &prefix); // skip the single space after XY
        let (kind, staged, unstaged) = classify(code);
        let orig_path = if kind == "renamed" || kind == "copied" {
            // The old path is the following NUL-separated token.
            let p = tokens.get(i).map(|s| strip_repo_prefix(s, &prefix));
            i += 1;
            p
        } else {
            None
        };
        files.push(FileChange { path, orig_path, kind: kind.to_string(), staged, unstaged });
    }
    files
}

pub fn git_status(root: &str) -> Result<Vec<FileChange>, String> {
    if !git_available() {
        return Ok(vec![]);
    }
    Ok(parse_status(&PathBuf::from(root)))
}

/// Append `text` to `buf`, stopping once `buf` reaches `cap` bytes; returns true
/// if `text` was cut short.
fn push_capped(buf: &mut String, text: &str, cap: usize) -> bool {
    let room = cap.saturating_sub(buf.len());
    if text.len() <= room {
        buf.push_str(text);
        false
    } else {
        // Back off to a UTF-8 char boundary — slicing mid-character panics.
        let mut end = room;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        buf.push_str(&text[..end]);
        true
    }
}

pub fn git_worktree_diff(root: &str) -> Result<WorktreeDiff, String> {
    if !git_available() {
        return Err("Git isn't installed.".into());
    }
    let root_path = PathBuf::from(root);
    let files = parse_status(&root_path);

    let mut diff = String::new();
    let mut truncated = false;

    // Tracked edits vs the last commit (covers staged + unstaged). Skipped when
    // there are no commits yet (unborn HEAD) — those files show up as untracked.
    if git_ok(&root_path, &["rev-parse", "--verify", "HEAD"]).is_some() {
        // -M detects renames so a moved file reads as a rename (matching the
        // per-commit diff from `git show`) rather than a delete + add pair.
        // --relative confines the diff to this folder and rewrites its a/b paths
        // to be skill-relative — so a skill nested in a parent repo diffs cleanly
        // (a no-op when the skill is its own repo and already at the root).
        if let Ok(out) = git(&root_path, &["-c", "core.quotepath=false", "diff", "--no-color", "-M", "--relative", "HEAD"]) {
            if out.status.success() {
                truncated |= push_capped(&mut diff, &String::from_utf8_lossy(&out.stdout), MAX_DIFF_BYTES);
            }
        }
    }

    // Untracked files have no HEAD blob to diff against; `--no-index` against
    // /dev/null renders them as clean "new file" additions (exit 1 == differs).
    // The leading `--` stops a dash-prefixed filename being read as an option;
    // quotepath=false keeps unicode/space paths literal (matching the tracked half).
    for f in files.iter().filter(|f| f.kind == "untracked") {
        if truncated {
            break;
        }
        if let Ok(out) = git(
            &root_path,
            &["-c", "core.quotepath=false", "diff", "--no-index", "--no-color", "--", "/dev/null", &f.path],
        ) {
            let code = out.status.code().unwrap_or(-1);
            if code == 0 || code == 1 {
                // A binary file with no NUL in its leading bytes comes back as raw
                // bytes; emit a synthetic new-file header instead of lossy junk.
                let chunk = match std::str::from_utf8(&out.stdout) {
                    Ok(s) => s.to_string(),
                    Err(_) => format!("diff --git a/{p} b/{p}\nnew file mode 100644\n--- /dev/null\n+++ b/{p}\n", p = f.path),
                };
                truncated |= push_capped(&mut diff, &chunk, MAX_DIFF_BYTES);
            }
        }
    }

    Ok(WorktreeDiff { files, diff, truncated })
}

/// The worktree's unified diff text only — the input for on-device commit-message
/// generation (same content as `git_worktree_diff().diff`). Lives here so it can
/// read the otherwise-private field; `commitmsg` consumes it.
pub fn worktree_diff_text(root: &str) -> Result<String, String> {
    Ok(git_worktree_diff(root)?.diff)
}

/// The subjects of the most recent `n` commits (newest first), for seeding the
/// generator with the repo's existing commit style. Empty when there's no
/// history yet or git is unavailable.
pub fn recent_subjects(root: &str, n: usize) -> Vec<String> {
    git_log(root, n)
        .map(|commits| commits.into_iter().map(|c| c.message).collect())
        .unwrap_or_default()
}

pub fn git_commit_diff(root: &str, sha: &str) -> Result<CommitDetail, String> {
    if !git_available() {
        return Err("Git isn't installed.".into());
    }
    // "HEAD" or a hex SHA. HEAD lets the version-preview review work after a reload,
    // when the previewed commit is reached via the detached HEAD rather than a SHA.
    if sha != "HEAD" && !is_hex_rev(sha) {
        return Err("Invalid commit reference.".into());
    }
    let root_path = PathBuf::from(root);

    // Metadata in one shot: SHA, short, subject, body, author, email, dates.
    let meta = git(
        &root_path,
        &["show", "-s", "--format=%H%x1f%h%x1f%s%x1f%b%x1f%an%x1f%ae%x1f%aI%x1f%ar", sha],
    )?;
    if !meta.status.success() {
        return Err(String::from_utf8_lossy(&meta.stderr).trim().to_string());
    }
    let meta_text = String::from_utf8_lossy(&meta.stdout);
    let p: Vec<&str> = meta_text.trim_end_matches('\n').split('\u{1f}').collect();
    if p.len() < 8 {
        return Err("Couldn't read that commit.".into());
    }

    // The patch on its own. Empty --format suppresses the commit header so we
    // get just the diff; --root makes the first commit diff against nothing.
    let patch = git(&root_path, &["show", "--no-color", "--format=", "--patch", "--root", sha])?;
    if !patch.status.success() {
        return Err(String::from_utf8_lossy(&patch.stderr).trim().to_string());
    }
    let mut diff = String::new();
    let truncated = push_capped(&mut diff, String::from_utf8_lossy(&patch.stdout).trim_start_matches('\n'), MAX_DIFF_BYTES);

    // Version number = commits reachable from this sha (its position in history).
    let number: usize = git_ok(&root_path, &["rev-list", "--count", sha])
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    Ok(CommitDetail {
        sha: p[0].to_string(),
        short: p[1].to_string(),
        subject: p[2].to_string(),
        body: p[3].trim().to_string(),
        author: p[4].to_string(),
        email: p[5].to_string(),
        iso_date: p[6].to_string(),
        relative_date: p[7].to_string(),
        diff,
        truncated,
        number,
    })
}

/// The contents of `path` at revision `rev` (e.g. "HEAD") — the "original" the
/// in-editor diff overlay compares the working buffer against. Returns "" when
/// the file doesn't exist at that rev (a newly added file), so the whole file
/// reads as an addition. `rev` must be "HEAD" or a hex SHA; `path` is relative
/// to `root` and rides as one `rev:./path` arg (the `./` resolves it against the
/// cwd `root`, not the repo top-level, and stops it being read as an option).
pub fn git_file_at(root: &str, rev: &str, path: &str) -> Result<String, String> {
    if !git_available() {
        return Err("Git isn't installed.".into());
    }
    // Accept "HEAD" or a hex SHA, each optionally with a single trailing "^" (first
    // parent). The parent rev backs the "what changed in this version" review of a
    // past version: HEAD is detached onto that version, so HEAD^ is the version
    // before it. Anything else can't slip through as a git option.
    let base = rev.strip_suffix('^').unwrap_or(rev);
    if base != "HEAD" && !is_hex_rev(base) {
        return Err("Invalid revision.".into());
    }
    let rel = path.trim_start_matches("./");
    let out = git(&PathBuf::from(root), &["show", &format!("{rev}:./{rel}")])?;
    if !out.status.success() {
        return Ok(String::new()); // absent at that rev → treat as empty (added)
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The tracked file paths at revision `rev` (a commit SHA or "HEAD"), for browsing
/// a past version's files. `-z` keeps unicode/space paths literal (no quoting).
pub fn git_files_at(root: &str, rev: &str) -> Result<Vec<String>, String> {
    if !git_available() {
        return Err("Git isn't installed.".into());
    }
    if rev != "HEAD" && !is_hex_rev(rev) {
        return Err("Invalid revision.".into());
    }
    let out = git(&PathBuf::from(root), &["ls-tree", "-r", "--name-only", "-z", rev])?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(text.split('\0').filter(|s| !s.is_empty()).map(|s| s.to_string()).collect())
}

/// Discard one path's working-tree changes back to HEAD: a tracked file is
/// restored (index + worktree); an untracked file is removed. `path` is kept
/// inside `root` and passed after `--` so it can't be read as an option.
pub fn git_discard(root: &str, path: &str) -> Result<(), String> {
    if !git_available() {
        return Err("Git isn't installed.".into());
    }
    let root_path = PathBuf::from(root);
    crate::pathsafe::safe_resolve(&root_path, path)?; // reject `..` / absolute escapes
    let tracked = git(&root_path, &["ls-files", "--error-unmatch", "--", path])
        .map(|o| o.status.success())
        .unwrap_or(false);
    let out = if tracked {
        // git >= 2.23: restore index + worktree to HEAD. Fall back to checkout.
        let r = git(&root_path, &["restore", "--staged", "--worktree", "--", path])?;
        if r.status.success() {
            r
        } else {
            git(&root_path, &["checkout", "HEAD", "--", path])?
        }
    } else {
        git(&root_path, &["clean", "-f", "--", path])?
    };
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(())
}

/// Discard ALL uncommitted changes back to HEAD (tracked restored, untracked
/// removed). Destructive — callers must confirm first.
pub fn git_discard_all(root: &str) -> Result<(), String> {
    if !git_available() {
        return Err("Git isn't installed.".into());
    }
    let root_path = PathBuf::from(root);
    let r = git(&root_path, &["restore", "--staged", "--worktree", "."])?;
    if !r.status.success() {
        let _ = git(&root_path, &["checkout", "HEAD", "--", "."]);
    }
    let c = git(&root_path, &["clean", "-fd"])?;
    if !c.status.success() {
        return Err(String::from_utf8_lossy(&c.stderr).trim().to_string());
    }
    Ok(())
}

// ---- version preview (stash + detached checkout, linear reconcile) ----------
//
// Viewing a past "version" reuses the FULL live editor by making the working tree
// BE that version: we stash any uncommitted work, then detach HEAD onto the chosen
// commit so the on-disk skill IS the old version (markdown renders, files browse,
// edits autosave — all the normal UI, unchanged). Returning restores the work we
// set aside. Editing a previewed version and SAVING it lands a single forward
// commit on the branch tip (linear history); the set-aside work is then discarded.
// These mutate the WHOLE repo (stash/detach), so they're gated to a skill that IS
// its own repository — a skill nested in a parent repo manages versions there.

/// Stash message tagging the work we set aside to preview a version, so later we
/// consume EXACTLY that stash (never an unrelated one the user made) and so a
/// crash leaves a findable, single entry rather than a pile.
const PREVIEW_STASH_MSG: &str = "vibestudio: version preview";
/// Local-config key remembering the branch we detached from, so returning to
/// "current" — or recovering after a crash/reload — reattaches to the right place.
const PREVIEW_BRANCH_CFG: &str = "vibestudio.previewbranch";

/// The branch HEAD points at, or None when detached (i.e. mid-preview).
pub(crate) fn current_branch(root: &Path) -> Option<String> {
    git_ok(root, &["symbolic-ref", "--short", "-q", "HEAD"]).filter(|s| !s.is_empty())
}

/// The ref whose linear history defines the version list: the live branch when
/// attached, else the branch we detached from to preview a version (kept in
/// local config), else HEAD. Lets the full version list show during a preview.
fn history_ref(root: &Path) -> String {
    if let Some(b) = current_branch(root) {
        return b;
    }
    git_ok(root, &["config", "--local", "--get", PREVIEW_BRANCH_CFG])
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "HEAD".to_string())
}

/// The `stash@{N}` ref of our version-preview stash, if present. Matched by
/// message so we only ever touch the stash WE created (not the user's own).
fn find_preview_stash(root: &Path) -> Option<String> {
    let list = git_ok(root, &["stash", "list", "--format=%gd%x1f%gs"])?;
    for line in list.lines() {
        let mut parts = line.split('\u{1f}');
        let reflog = parts.next().unwrap_or("");
        let subject = parts.next().unwrap_or("");
        if subject.contains(PREVIEW_STASH_MSG) {
            return Some(reflog.to_string());
        }
    }
    None
}

/// Gate the whole-repo version ops to a skill that IS its own git repository
/// (stash + detached checkout would otherwise disturb an unrelated parent repo).
fn ensure_own_repo(root: &str) -> Result<PathBuf, String> {
    if !git_available() {
        return Err("Git isn't installed.".into());
    }
    let info = git_info(root)?;
    if !info.is_repo {
        return Err("Version preview needs the skill to be tracked in its own repository.".into());
    }
    Ok(PathBuf::from(root))
}

/// Set aside uncommitted work + reattach to the branch, restoring that work. The
/// shared "leave preview" path: also used to unwind a prior preview before
/// entering a new one (so previews never stack), and to recover after a crash.
/// Discards any unsaved detached-state edits (a preview edit is kept only by
/// SAVING it). Never leaves our preview stash behind.
fn exit_preview(root_path: &Path) -> Result<(), String> {
    let branch = current_branch(root_path).unwrap_or_else(|| history_ref(root_path));
    if branch != "HEAD" {
        // -f discards the detached preview edits and reattaches to the branch tip.
        let co = git(root_path, &["checkout", "-f", &branch])?;
        if !co.status.success() {
            return Err(String::from_utf8_lossy(&co.stderr).trim().to_string());
        }
    }
    if let Some(stash_ref) = find_preview_stash(root_path) {
        // Restore the work we set aside on entry. A conflict here means the branch
        // tip moved under the preview (e.g. an external commit), so the set-aside
        // work no longer applies cleanly. NEVER leave conflict markers / an
        // unmerged index behind: wordless autosave would silently overwrite the
        // markers, and a later re-enter would fail on the unmerged index. Clear the
        // half-applied merge back to a clean tip and KEEP the work safe in the
        // stash for manual recovery (`git stash pop`) rather than dropping it.
        let pop = git(root_path, &["stash", "pop", &stash_ref])?;
        if !pop.status.success() && branch != "HEAD" {
            let _ = git(root_path, &["checkout", "-f", &branch]);
        }
    }
    let _ = git(root_path, &["config", "--local", "--unset", PREVIEW_BRANCH_CFG]);
    Ok(())
}

/// The result of entering version preview, for the UI's banner/state.
#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PreviewState {
    /// True when uncommitted work was set aside (stashed) to show this version.
    stashed: bool,
    /// The branch we'll return to (recorded so exit/keep is crash-safe).
    branch: Option<String>,
}

/// Enter "version preview": make the working tree BE the past version `sha`
/// (detaching HEAD onto it) so the full live editor renders it. Any uncommitted
/// work is stashed first and restored on exit. Idempotent — a preview already in
/// progress is unwound first, so previews never stack and stashes never pile up.
pub fn git_enter_version(root: &str, sha: &str) -> Result<PreviewState, String> {
    let root_path = ensure_own_repo(root)?;
    if !is_hex_rev(sha) {
        return Err("Invalid version reference.".into());
    }
    // Resolve to a real commit object (rejects partial/garbage refs cleanly).
    let target = git_ok(&root_path, &["rev-parse", "--verify", "-q", &format!("{sha}^{{commit}}")])
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "That version no longer exists.".to_string())?;

    // Already mid-preview (detached)? Unwind it first — restores the original work
    // and avoids stacking a second stash on top of the first.
    if current_branch(&root_path).is_none() {
        exit_preview(&root_path)?;
    }

    let branch = current_branch(&root_path)
        .ok_or_else(|| "Couldn't determine the current branch to return to.".to_string())?;
    // Remember where to return BEFORE detaching (survives a crash/reload).
    let _ = git(&root_path, &["config", "--local", PREVIEW_BRANCH_CFG, &branch]);

    // Set aside uncommitted work (tracked + untracked) so the version shows clean.
    let dirty = git_ok(&root_path, &["status", "--porcelain"]).map(|s| !s.is_empty()).unwrap_or(false);
    let mut stashed = false;
    if dirty {
        let out = git(&root_path, &["stash", "push", "--include-untracked", "-m", PREVIEW_STASH_MSG])?;
        if !out.status.success() {
            let _ = git(&root_path, &["config", "--local", "--unset", PREVIEW_BRANCH_CFG]);
            return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
        }
        stashed = find_preview_stash(&root_path).is_some();
    }

    // Detach onto the version: the working tree now IS that version, clean.
    let co = git(&root_path, &["-c", "advice.detachedHead=false", "checkout", "--detach", &target])?;
    if !co.status.success() {
        let msg = String::from_utf8_lossy(&co.stderr).trim().to_string();
        let _ = exit_preview(&root_path); // never strand the user: restore + reattach
        return Err(msg);
    }

    Ok(PreviewState { stashed, branch: Some(branch) })
}

/// Leave version preview: discard unsaved preview edits, reattach to the branch we
/// detached from, and restore the work set aside on entry. Returns fresh GitInfo.
pub fn git_exit_version(root: &str) -> Result<GitInfo, String> {
    let root_path = ensure_own_repo(root)?;
    exit_preview(&root_path)?;
    git_info(root)
}

/// Save the previewed-(and-edited) version as a NEW version with LINEAR history:
/// commit the current working tree onto the branch TIP (not the detached old
/// commit), advance the branch, and reattach HEAD to it. The set-aside work is
/// then discarded (the user chose this direction) so no stash piles up.
pub fn git_keep_version(root: &str, message: &str) -> Result<CommitResult, String> {
    let root_path = ensure_own_repo(root)?;
    let msg = message.trim();
    if msg.is_empty() {
        return Err("Enter a version description.".into());
    }
    // commit-tree needs BOTH name and email; checking only email would let the
    // guard pass and then fail opaquely ("empty ident name") inside commit-tree.
    let missing_ident = git_ok(&root_path, &["config", "user.email"]).map(|s| s.is_empty()).unwrap_or(true)
        || git_ok(&root_path, &["config", "user.name"]).map(|s| s.is_empty()).unwrap_or(true);
    if missing_ident {
        return Err(
            "No git identity set. Run: git config --global user.email \"you@example.com\" and user.name \"Your Name\".".into(),
        );
    }
    // Not actually mid-preview (HEAD attached) → an ordinary save on the branch.
    if current_branch(&root_path).is_some() {
        return git_commit(root, message);
    }
    let branch = git_ok(&root_path, &["config", "--local", "--get", PREVIEW_BRANCH_CFG])
        .filter(|s| !s.is_empty())
        .or_else(|| {
            // Marker lost (crash) but a single local branch → unambiguous fallback.
            let list = git_ok(&root_path, &["branch", "--format=%(refname:short)"]).unwrap_or_default();
            let mut it = list.lines().filter(|l| !l.is_empty());
            match (it.next(), it.next()) {
                (Some(only), None) => Some(only.to_string()),
                _ => None,
            }
        })
        .ok_or_else(|| "Couldn't determine which branch to save onto.".to_string())?;

    let tip = git_ok(&root_path, &["rev-parse", "--verify", &format!("refs/heads/{branch}")])
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "Couldn't find the current branch tip.".to_string())?;

    // Stage the working tree (old version + your edits) and snapshot it as a tree.
    let add = git(&root_path, &["add", "-A"])?;
    if !add.status.success() {
        return Err(String::from_utf8_lossy(&add.stderr).trim().to_string());
    }
    let tree =
        git_ok(&root_path, &["write-tree"]).filter(|s| !s.is_empty()).ok_or_else(|| "Couldn't snapshot your changes.".to_string())?;
    // One forward commit on top of the tip → single parent → history stays linear.
    let ct = git(&root_path, &["commit-tree", &tree, "-p", &tip, "-m", msg])?;
    if !ct.status.success() {
        return Err(String::from_utf8_lossy(&ct.stderr).trim().to_string());
    }
    let new = String::from_utf8_lossy(&ct.stdout).trim().to_string();
    if new.is_empty() {
        return Err("Couldn't create the new version.".into());
    }
    // Advance the branch (guarded by the expected old tip) and reattach HEAD to it.
    let upd = git(&root_path, &["update-ref", &format!("refs/heads/{branch}"), &new, &tip])?;
    if !upd.status.success() {
        return Err(String::from_utf8_lossy(&upd.stderr).trim().to_string());
    }
    let sym = git(&root_path, &["symbolic-ref", "HEAD", &format!("refs/heads/{branch}")])?;
    if !sym.status.success() {
        return Err(String::from_utf8_lossy(&sym.stderr).trim().to_string());
    }
    // Discard the set-aside work + the branch marker — clean slate, no pile-up.
    if let Some(stash_ref) = find_preview_stash(&root_path) {
        let _ = git(&root_path, &["stash", "drop", &stash_ref]);
    }
    let _ = git(&root_path, &["config", "--local", "--unset", PREVIEW_BRANCH_CFG]);

    Ok(CommitResult {
        sha: new.clone(),
        summary: git_ok(&root_path, &["log", "-1", "--pretty=%s", &new]).unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_capped_respects_utf8_boundaries() {
        // "é" is two bytes (0xC3 0xA9). A cap of 1 must NOT slice mid-character.
        let mut buf = String::new();
        let cut = push_capped(&mut buf, "é", 1);
        assert!(cut && buf.is_empty()); // dropped the whole char rather than panic

        let mut buf = String::from("ab");
        let cut = push_capped(&mut buf, "cd", 10);
        assert!(!cut && buf == "abcd"); // fits, no truncation

        let mut buf = String::new();
        let cut = push_capped(&mut buf, "aé", 2);
        assert!(cut && buf == "a"); // keeps the ascii byte, drops the split char
    }

    #[test]
    fn round_trip_when_git_present() {
        if !git_available() {
            return; // skip on machines without git
        }
        let base = std::env::temp_dir().join(format!("ass_git_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("SKILL.md"), "---\nname: t\n---\nhi").unwrap();
        let root = base.to_string_lossy().to_string();

        git_init(&root).unwrap();
        // Local identity so the commit works regardless of global config.
        let _ = git(&base, &["config", "user.email", "test@example.com"]);
        let _ = git(&base, &["config", "user.name", "Test"]);

        let info = git_info(&root).unwrap();
        assert!(info.is_repo && info.available);

        let res = git_commit(&root, "initial version").unwrap();
        assert!(!res.sha.is_empty());

        let log = git_log(&root, 10).unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].message, "initial version");

        // Nothing-to-commit path.
        assert!(git_commit(&root, "again").is_err());

        let sha = log[0].sha.clone();
        assert_eq!(sha.len(), 40);

        // Commit diff: the initial commit adds SKILL.md against the empty tree.
        let detail = git_commit_diff(&root, &sha).unwrap();
        assert_eq!(detail.subject, "initial version");
        assert!(detail.diff.contains("new file"));
        assert!(detail.diff.contains("SKILL.md"));
        // A bogus / non-hex ref is rejected before reaching git.
        assert!(git_commit_diff(&root, "not-a-sha!!").is_err());

        // Working tree: edit a tracked file + add untracked ones (incl. a name
        // beginning with '-', which must not be parsed by git as an option).
        std::fs::write(base.join("SKILL.md"), "---\nname: t\n---\nhi there").unwrap();
        std::fs::write(base.join("NOTES.md"), "fresh\n").unwrap();
        std::fs::write(base.join("-dash.md"), "dashed\n").unwrap();
        let wt = git_worktree_diff(&root).unwrap();
        let kinds: Vec<(&str, &str)> =
            wt.files.iter().map(|f| (f.path.as_str(), f.kind.as_str())).collect();
        assert!(kinds.contains(&("SKILL.md", "modified")));
        assert!(kinds.contains(&("NOTES.md", "untracked")));
        // The diff carries the tracked edit and every untracked add, including
        // the dash-prefixed file (would be dropped without the `--` separator).
        assert!(wt.diff.contains("+hi there"));
        assert!(wt.diff.contains("NOTES.md") && wt.diff.contains("+fresh"));
        assert!(wt.diff.contains("-dash.md") && wt.diff.contains("+dashed"));
        assert!(!wt.truncated);

        // File-at-rev: SKILL.md exists at HEAD (its committed text); a never-
        // committed path returns "" (so the overlay shows it all as added).
        let at_head = git_file_at(&root, "HEAD", "SKILL.md").unwrap();
        assert!(at_head.contains("name: t") && !at_head.contains("hi there"));
        assert_eq!(git_file_at(&root, "HEAD", "NOTES.md").unwrap(), "");
        assert!(git_file_at(&root, "zzz", "SKILL.md").is_err()); // non-hex rev rejected

        // Discard: a tracked file is restored to HEAD, an untracked file removed.
        git_discard(&root, "SKILL.md").unwrap();
        let restored = std::fs::read_to_string(base.join("SKILL.md")).unwrap();
        assert!(restored.contains("name: t") && !restored.contains("hi there"));
        git_discard(&root, "NOTES.md").unwrap();
        assert!(!base.join("NOTES.md").exists());
        assert!(git_discard(&root, "../escape").is_err()); // traversal rejected

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn local_exclude_keeps_build_junk_and_secrets_out() {
        if !git_available() {
            return;
        }
        let base = std::env::temp_dir().join(format!("ass_excl_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("SKILL.md"), "---\nname: t\n---\nhi").unwrap();
        let root = base.to_string_lossy().to_string();

        git_init(&root).unwrap();
        let _ = git(&base, &["config", "user.email", "test@example.com"]);
        let _ = git(&base, &["config", "user.name", "Test"]);

        // The junk Python/Node skills leave behind, plus a stray secret.
        std::fs::create_dir_all(base.join("scripts/__pycache__")).unwrap();
        std::fs::write(base.join("scripts/__pycache__/_config.cpython-312.pyc"), [0u8, 1, 2]).unwrap();
        std::fs::create_dir_all(base.join(".venv/bin")).unwrap();
        std::fs::write(base.join(".venv/bin/python"), "x").unwrap();
        std::fs::create_dir_all(base.join("node_modules/left-pad")).unwrap();
        std::fs::write(base.join("node_modules/left-pad/index.js"), "x").unwrap();
        std::fs::write(base.join(".env"), "SECRET=1\n").unwrap();

        // Viewing the repo seeds the local exclude — it's never a committed file…
        let info = git_info(&root).unwrap();
        assert!(info.is_repo);
        assert!(!base.join(".gitignore").exists(), "no committed .gitignore");
        // …and the worktree reads clean of the junk (only SKILL.md is untracked).
        let wt = git_worktree_diff(&root).unwrap();
        let paths: Vec<&str> = wt.files.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&"SKILL.md"));
        assert!(
            !paths.iter().any(|p| p.contains("__pycache__") || p.contains(".venv") || p.contains("node_modules") || p.ends_with(".env")),
            "junk hidden: {paths:?}"
        );

        // The commit captures SKILL.md but none of the env/dep/build junk.
        git_commit(&root, "v1").unwrap();
        let tracked = git_ok(&base, &["ls-files"]).unwrap();
        assert!(tracked.contains("SKILL.md"));
        assert!(
            !["__pycache__", ".venv", "node_modules", ".env"].iter().any(|j| tracked.contains(j)),
            "tracked: {tracked}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn parent_repo_paths_are_skill_relative() {
        if !git_available() {
            return; // skip on machines without git
        }
        // A parent repo that holds the skill in a nested sub-path, plus a file
        // OUTSIDE the skill — the studio must never surface that one.
        let base = std::env::temp_dir().join(format!("ass_parent_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let skill = base.join("skills/my-skill");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(skill.join("SKILL.md"), "---\nname: t\n---\nhi").unwrap();
        std::fs::write(base.join("README.md"), "outer\n").unwrap();
        let parent = base.to_string_lossy().to_string();
        let skill_root = skill.to_string_lossy().to_string();

        git_init(&parent).unwrap();
        let _ = git(&base, &["config", "user.email", "test@example.com"]);
        let _ = git(&base, &["config", "user.name", "Test"]);
        let _ = git(&base, &["add", "-A"]);
        let _ = git(&base, &["commit", "-m", "init"]);

        // info: the skill is seen as living inside a parent repo, not its own.
        let info = git_info(&skill_root).unwrap();
        assert!(info.in_parent_repo && !info.is_repo);
        assert!(!info.dirty); // clean so far

        // Edit inside the skill, add an untracked file inside it, and touch a file
        // OUTSIDE the skill in the same parent repo.
        std::fs::write(skill.join("SKILL.md"), "---\nname: t\n---\nhi there").unwrap();
        std::fs::write(skill.join("NOTES.md"), "fresh\n").unwrap();
        std::fs::write(base.join("README.md"), "outer changed\n").unwrap();

        // status is scoped to the skill AND paths are skill-relative (no
        // "skills/my-skill/" prefix), and the outside README never appears.
        let changes = git_status(&skill_root).unwrap();
        let paths: Vec<&str> = changes.iter().map(|c| c.path.as_str()).collect();
        assert!(paths.contains(&"SKILL.md"), "got {paths:?}");
        assert!(paths.contains(&"NOTES.md"), "got {paths:?}");
        assert!(!paths.iter().any(|p| p.contains("README") || p.contains("skills/")), "leaked: {paths:?}");

        assert!(git_info(&skill_root).unwrap().dirty); // now dirty (scoped)

        // worktree diff is likewise scoped + skill-relative.
        let wt = git_worktree_diff(&skill_root).unwrap();
        assert!(wt.diff.contains("a/SKILL.md") && wt.diff.contains("+hi there"));
        assert!(wt.diff.contains("NOTES.md") && wt.diff.contains("+fresh"));
        assert!(!wt.diff.contains("README"), "leaked outside change: {}", wt.diff);
        assert!(!wt.diff.contains("skills/my-skill"), "unstripped prefix: {}", wt.diff);

        // file-at-rev resolves against the skill dir (skill-relative path).
        let head = git_file_at(&skill_root, "HEAD", "SKILL.md").unwrap();
        assert!(head.contains("name: t") && !head.contains("hi there"));

        // discard one skill file → restored from the parent's HEAD, untouched
        // outside the skill.
        git_discard(&skill_root, "SKILL.md").unwrap();
        let restored = std::fs::read_to_string(skill.join("SKILL.md")).unwrap();
        assert!(restored.contains("name: t") && !restored.contains("hi there"));
        assert_eq!(std::fs::read_to_string(base.join("README.md")).unwrap(), "outer changed\n");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn version_preview_stash_checkout_and_linear_reconcile() {
        if !git_available() {
            return; // skip on machines without git
        }
        let base = std::env::temp_dir().join(format!("ass_preview_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let root = base.to_string_lossy().to_string();
        let write = |name: &str, body: &str| std::fs::write(base.join(name), body).unwrap();

        git_init(&root).unwrap();
        let _ = git(&base, &["config", "user.email", "test@example.com"]);
        let _ = git(&base, &["config", "user.name", "Test"]);
        let _ = git(&base, &["config", "commit.gpgsign", "false"]);

        // Three versions on one linear branch.
        write("SKILL.md", "---\nname: t\n---\nv1");
        git_commit(&root, "v1").unwrap();
        write("SKILL.md", "---\nname: t\n---\nv2");
        git_commit(&root, "v2").unwrap();
        write("SKILL.md", "---\nname: t\n---\nv3");
        git_commit(&root, "v3").unwrap();

        let log = git_log(&root, 10).unwrap();
        assert_eq!(log.len(), 3);
        let v1_sha = log[2].sha.clone(); // oldest
        let v3_sha = log[0].sha.clone(); // newest = tip
        let branch = current_branch(&base).expect("on a branch");

        // Uncommitted work: a tracked edit + an untracked file.
        write("SKILL.md", "---\nname: t\n---\nwip");
        write("NOTES.md", "wipnote");

        // Enter v1: work is set aside, the working tree BECOMES v1, HEAD detaches,
        // and the FULL version list still shows (logs the branch, not detached HEAD).
        let st = git_enter_version(&root, &v1_sha).unwrap();
        assert!(st.stashed, "uncommitted work was stashed");
        assert!(current_branch(&base).is_none(), "HEAD detached during preview");
        assert!(std::fs::read_to_string(base.join("SKILL.md")).unwrap().contains("v1"));
        assert!(!base.join("NOTES.md").exists(), "untracked work set aside");
        assert!(find_preview_stash(&base).is_some());
        assert_eq!(git_log(&root, 10).unwrap().len(), 3, "full version list visible mid-preview");

        // Edit the previewed version and SAVE → a new version on top of the TIP.
        write("SKILL.md", "---\nname: t\n---\nv1-edited");
        let res = git_keep_version(&root, "edit based on v1").unwrap();
        assert!(!res.sha.is_empty());
        assert_eq!(current_branch(&base).as_deref(), Some(branch.as_str()), "reattached to the branch");
        let log2 = git_log(&root, 10).unwrap();
        assert_eq!(log2.len(), 4, "exactly one new version");
        assert_eq!(log2[0].sha, res.sha, "the new version is the tip");
        // Linear history: the new commit's ONLY parent is the previous tip (v3).
        let parents = git_ok(&base, &["rev-list", "--parents", "-n", "1", &res.sha]).unwrap();
        let cols: Vec<&str> = parents.split_whitespace().collect();
        assert_eq!(cols.len(), 2, "single parent — no branch/merge");
        assert_eq!(cols[1], v3_sha, "new version sits on the old tip");
        assert!(std::fs::read_to_string(base.join("SKILL.md")).unwrap().contains("v1-edited"));
        assert!(!base.join("NOTES.md").exists(), "set-aside work discarded on save");
        assert!(find_preview_stash(&base).is_none(), "no stash pile-up after save");

        // Exit (no save) restores the set-aside work and creates no version.
        write("SKILL.md", "---\nname: t\n---\nwip2");
        write("NOTES.md", "wip2note");
        git_enter_version(&root, &v1_sha).unwrap();
        assert!(std::fs::read_to_string(base.join("SKILL.md")).unwrap().contains("v1"));
        git_exit_version(&root).unwrap();
        assert_eq!(current_branch(&base).as_deref(), Some(branch.as_str()));
        assert!(std::fs::read_to_string(base.join("SKILL.md")).unwrap().contains("wip2"), "edit restored");
        assert!(base.join("NOTES.md").exists(), "untracked work restored on exit");
        assert!(find_preview_stash(&base).is_none(), "no stash left behind");
        assert_eq!(git_log(&root, 10).unwrap().len(), 4, "exit creates no version");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn exit_preview_recovers_cleanly_when_tip_moves_under_it() {
        if !git_available() {
            return;
        }
        let base = std::env::temp_dir().join(format!("ass_preview_conflict_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let root = base.to_string_lossy().to_string();
        let w = |body: &str| std::fs::write(base.join("SKILL.md"), body).unwrap();

        git_init(&root).unwrap();
        let _ = git(&base, &["config", "user.email", "test@example.com"]);
        let _ = git(&base, &["config", "user.name", "Test"]);
        let _ = git(&base, &["config", "commit.gpgsign", "false"]);

        w("---\nname: t\n---\nv1");
        git_commit(&root, "v1").unwrap();
        w("---\nname: t\n---\nv2");
        git_commit(&root, "v2").unwrap();
        let log = git_log(&root, 10).unwrap();
        let v1 = log[1].sha.clone();
        let tip = log[0].sha.clone();
        let branch = current_branch(&base).unwrap();

        // Uncommitted WIP, then enter v1 (stashes the WIP, detaches onto v1).
        w("---\nname: t\n---\nWIP-LINE");
        git_enter_version(&root, &v1).unwrap();
        assert!(find_preview_stash(&base).is_some());

        // Move the branch tip OUT from under the preview to a commit that conflicts
        // with the stashed WIP (simulating an external edit to the same file).
        w("---\nname: t\n---\nEXTERNAL-LINE");
        let _ = git(&base, &["add", "-A"]);
        let tree = git_ok(&base, &["write-tree"]).unwrap();
        let ext = git_ok(&base, &["commit-tree", &tree, "-p", &tip, "-m", "external"]).unwrap();
        let _ = git(&base, &["update-ref", &format!("refs/heads/{branch}"), &ext]);
        let _ = git(&base, &["checkout", "-f", &v1]); // back to the clean detached preview

        // Exit: the stash pop CONFLICTS (its base is the old tip, not `ext`). The fix
        // must leave NO conflict markers / unmerged index, return to a clean tip, and
        // KEEP the set-aside work in the stash (recoverable, never silently lost).
        git_exit_version(&root).unwrap();
        let content = std::fs::read_to_string(base.join("SKILL.md")).unwrap();
        assert!(!content.contains("<<<<<<<") && !content.contains(">>>>>>>"), "no conflict markers left: {content}");
        assert!(content.contains("EXTERNAL-LINE"), "working tree is the clean new tip, got: {content}");
        let porcelain = git_ok(&base, &["status", "--porcelain"]).unwrap_or_default();
        assert!(!porcelain.contains("UU"), "no unmerged index entry: {porcelain}");
        assert!(find_preview_stash(&base).is_some(), "set-aside work kept (recoverable), not lost");
        assert_eq!(current_branch(&base).as_deref(), Some(branch.as_str()), "reattached to the branch");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn baseline_commit_lands_and_excludes_secrets() {
        if !git_available() {
            return;
        }
        // The auto-track baseline path: init_and_baseline lands an "Initial version"
        // capturing the skill but NOT a stray .env. Pre-seed the repo + a LOCAL
        // identity so the commit is deterministic regardless of the machine's global
        // git config; init_and_baseline touches no config dir, so no XDG isolation.
        let base = std::env::temp_dir().join(format!("ass_baseline_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("SKILL.md"), "---\nname: t\n---\nhi").unwrap();
        std::fs::write(base.join(".env"), "SECRET=1\n").unwrap();
        let root = base.to_string_lossy().to_string();

        git_init(&root).unwrap();
        let _ = git(&base, &["config", "user.email", "test@example.com"]);
        let _ = git(&base, &["config", "user.name", "Test"]);

        init_and_baseline(&root).unwrap();
        let log = git_log(&root, 10).unwrap();
        assert_eq!(log.len(), 1, "exactly one baseline commit");
        assert_eq!(log[0].message, "Initial version");
        let tracked = git_ok(&base, &["ls-files"]).unwrap();
        assert!(tracked.contains("SKILL.md"));
        assert!(!tracked.contains(".env"), "baseline must not capture secrets: {tracked}");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn untrack_refuses_a_parent_repo_skill() {
        if !git_available() {
            return;
        }
        // A skill nested in a parent repo: untrack must refuse and never delete the
        // parent's .git. This guard returns before any denylist write, so the test
        // needs no config-dir isolation.
        let base = std::env::temp_dir().join(format!("ass_untrack_parent_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let skill = base.join(".claude/skills/nested");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(skill.join("SKILL.md"), "---\nname: nested\n---\nhi").unwrap();
        let parent = base.to_string_lossy().to_string();
        let skill_root = skill.to_string_lossy().to_string();

        git_init(&parent).unwrap();
        let _ = git(&base, &["config", "user.email", "test@example.com"]);
        let _ = git(&base, &["config", "user.name", "Test"]);
        let _ = git(&base, &["add", "-A"]);
        let _ = git(&base, &["commit", "-m", "init"]);

        assert!(git_info(&skill_root).unwrap().in_parent_repo);
        assert!(git_untrack(&skill_root).is_err(), "won't delete a parent repo's history");
        assert!(base.join(".git").exists(), "parent .git left intact");

        let _ = std::fs::remove_dir_all(&base);
    }
}
