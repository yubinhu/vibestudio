// Sync a skill into a shared/global skills directory so other agents can use it.
// A skill is just a folder with SKILL.md, and SKILL.md is a shared format across
// agents (the Agent Skills open standard), so syncing is a plain copy — or a
// symlink, when you'd rather keep a single source of truth that every agent reads.
//
// Most agents (Codex, Cursor, Gemini CLI, …) now read the standard
// `~/.agents/skills` directory, so one placement there reaches the whole cohort.
// Claude Code is the holdout — it reads `~/.claude/skills` — so it's its own
// destination. The legacy per-agent dirs (`~/.codex/skills`, `~/.cursor/skills`)
// are still honored for *presence* detection so we never offer to add a skill an
// agent can already see (which would make it show up twice in its picker).
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::pathsafe::resolve_root;

pub(crate) const IGNORED_DIRS: [&str; 5] = [".git", "node_modules", ".next", "__pycache__", ".venv"];

/// Bumped per zip import to give each one a unique staging dir (no time/rand dep).
static IMPORT_SEQ: AtomicU64 = AtomicU64::new(0);
const MAX_TOTAL: u64 = 100 * 1024 * 1024; // 100 MB

/// A place a skill can be synced to. `cohort` lists the dirs (relative to home)
/// whose presence means "already reachable from this destination" — the first is
/// the canonical landing dir for new copies/links; the rest are legacy aliases
/// the same agents still read.
struct Dest {
    id: &'static str,
    label: &'static str,
    cohort: &'static [&'static str],
    reaches: &'static [&'static str],
}

const DESTS: [Dest; 2] = [
    Dest {
        id: "universal",
        label: "All agents (Agent Skills standard)",
        cohort: &[".agents/skills", ".codex/skills", ".cursor/skills", ".config/opencode/skills"],
        reaches: &["Codex", "Cursor", "Gemini CLI", "opencode"],
    },
    Dest {
        id: "claude-code",
        label: "Claude Code",
        cohort: &[".claude/skills"],
        reaches: &["Claude Code"],
    },
];

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncTarget {
    /// Stable id passed back to `sync_skill` ("universal" | "claude-code").
    id: String,
    label: String,
    /// Canonical dir a new copy/link lands in.
    dir: String,
    /// Agent display names this destination serves.
    reaches: Vec<String>,
    /// A copy/link is already reachable from this destination.
    present: bool,
    /// The skill natively lives here (don't offer to add it onto itself).
    is_source: bool,
    /// `present` via a symlink — i.e. a shared copy that tracks the source.
    linked: bool,
    /// When present via a legacy alias (not the canonical dir), its basename.
    #[serde(skip_serializing_if = "Option::is_none")]
    reached_via: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncResult {
    dest: String,
    linked: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteResult {
    removed: String,
    /// Only a symlink (a synced shared copy) was removed; the real skill is intact.
    was_link: bool,
}

/// Outcome of accepting a proposed skill: its new canonical root after being
/// moved out of the `generated-skills/` staging folder into the real home.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PromoteResult {
    root: String,
}

/// A directory a brand-new skill can be created in (the same destinations sync
/// targets — "all agents" vs "Claude Code"), with its absolute path resolved.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillHome {
    /// Stable id passed back to `create_skill` ("universal" | "claude-code").
    id: String,
    label: String,
    /// Absolute path of the canonical dir a new skill lands in.
    dir: String,
    /// Agent display names this location serves.
    reaches: Vec<String>,
}


/// Outcome of importing a skill (from a folder or a `.zip`) into a chosen home.
#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ImportResult {
    /// Canonical root path of the imported skill — open it next.
    pub(crate) root: String,
    /// The skill/folder name it was imported as.
    pub(crate) name: String,
    /// The home directory it landed in.
    pub(crate) dir: String,
    /// An existing skill of the same name was replaced.
    pub(crate) overwrote: bool,
    /// `.env` pairs found in the source (not copied into the skill) so the caller
    /// can offer to load them into the secret store. Empty when there was no `.env`.
    pub(crate) env: Vec<crate::secrets::DotenvEntry>,
}

/// The personal/global skills directory each agent reads. Kept for the secret
/// manager, which installs the activation skill into each agent's own dir.
pub fn agent_user_dir(agent: &str) -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let rel = match agent {
        "Claude Code" => ".claude/skills",
        "Codex" => ".codex/skills",
        "Cursor" => ".cursor/skills",
        "OpenClaw" => ".openclaw/skills",
        "opencode" => ".config/opencode/skills",
        _ => return None,
    };
    Some(home.join(rel))
}

fn skill_dir_name(root: &Path) -> Option<String> {
    root.file_name().map(|s| s.to_string_lossy().into_owned())
}

fn dest_by_id(id: &str) -> Option<&'static Dest> {
    DESTS.iter().find(|d| d.id == id)
}

/// For each destination, where the skill would land and whether it's already
/// reachable there (and how — as the source, a copy, or a shared link).
pub fn sync_targets(root: &str) -> Result<Vec<SyncTarget>, String> {
    let home = dirs::home_dir().ok_or_else(|| "No home directory.".to_string())?;
    sync_targets_in(&home, root)
}

fn sync_targets_in(home: &Path, root: &str) -> Result<Vec<SyncTarget>, String> {
    let root_path = PathBuf::from(root);
    let name = skill_dir_name(&root_path).ok_or_else(|| "Invalid skill path.".to_string())?;
    let canon_root = std::fs::canonicalize(&root_path).unwrap_or_else(|_| root_path.clone());

    let mut out = Vec::new();
    for d in &DESTS {
        let canonical_dir = home.join(d.cohort[0]);
        let mut present = false;
        let mut linked = false;
        let mut is_source = false;
        let mut reached_via = None;
        // Canonical dir first; first hit wins so the source/canonical copy is preferred.
        for (i, rel) in d.cohort.iter().enumerate() {
            let cand = home.join(rel).join(&name);
            if !cand.join("SKILL.md").exists() {
                continue;
            }
            present = true;
            let is_symlink = std::fs::symlink_metadata(&cand)
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false);
            let canon_cand = std::fs::canonicalize(&cand).unwrap_or_else(|_| cand.clone());
            if is_symlink {
                linked = true;
            } else if canon_cand == canon_root {
                is_source = true;
            }
            if i != 0 {
                reached_via = Some((*rel).to_string());
            }
            break;
        }
        out.push(SyncTarget {
            id: d.id.into(),
            label: d.label.into(),
            dir: canonical_dir.to_string_lossy().into_owned(),
            reaches: d.reaches.iter().map(|s| s.to_string()).collect(),
            present,
            is_source,
            linked,
            reached_via,
        });
    }
    Ok(out)
}

/// Place the skill into a destination's canonical dir — as a copy, or (when
/// `link`) a symlink that shares the one source. Refuses to overwrite unless
/// asked, and never places a skill onto itself.
pub fn sync_skill(root: &str, target: &str, overwrite: bool, link: bool) -> Result<SyncResult, String> {
    let home = dirs::home_dir().ok_or_else(|| "No home directory.".to_string())?;
    sync_skill_in(&home, root, target, overwrite, link)
}

fn sync_skill_in(
    home: &Path,
    root: &str,
    target: &str,
    overwrite: bool,
    link: bool,
) -> Result<SyncResult, String> {
    let root_path = PathBuf::from(root);
    if !root_path.join("SKILL.md").exists() {
        return Err("Not a skill directory (no SKILL.md).".into());
    }
    let name = skill_dir_name(&root_path).ok_or_else(|| "Invalid skill path.".to_string())?;
    let d = dest_by_id(target).ok_or_else(|| format!("Unknown sync target: {target}"))?;
    let dir = home.join(d.cohort[0]);
    let dest = dir.join(&name);

    let canon_root = std::fs::canonicalize(&root_path).unwrap_or_else(|_| root_path.clone());
    // A real (non-symlink) dir at dest that resolves to the source = placing onto itself.
    let dest_is_self_dir = !is_symlink(&dest)
        && std::fs::canonicalize(&dest).map(|c| c == canon_root).unwrap_or(false);
    if dest_is_self_dir {
        return Err("The skill already lives here.".into());
    }
    if dest.symlink_metadata().is_ok() {
        if !overwrite {
            return Err(format!("A skill named \"{name}\" already exists here."));
        }
        remove_path(&dest)?;
    }
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    if link {
        symlink_dir(&canon_root, &dest)?;
        Ok(SyncResult { dest: dest.to_string_lossy().into_owned(), linked: true })
    } else {
        let mut total: u64 = 0;
        copy_tree(&root_path, &dest, &mut total)?;
        Ok(SyncResult { dest: dest.to_string_lossy().into_owned(), linked: false })
    }
}

/// True if `name` is a valid skill/folder name per the Agent Skills spec:
/// 1-64 chars, lowercase alphanumeric with single hyphens, no leading/trailing
/// or repeated hyphen. Mirrors the frontend's NAME_REGEX (defense in depth — the
/// dialog validates too, but this command can be called directly).
fn valid_skill_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 64 {
        return false;
    }
    if name.starts_with('-') || name.ends_with('-') || name.contains("--") {
        return false;
    }
    name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// The places a new skill can be created — the same destinations as sync, with
/// the canonical landing dir resolved to an absolute path for display.
pub fn skill_homes() -> Result<Vec<SkillHome>, String> {
    let home = dirs::home_dir().ok_or_else(|| "No home directory.".to_string())?;
    Ok(DESTS
        .iter()
        .map(|d| SkillHome {
            id: d.id.into(),
            label: d.label.into(),
            dir: home.join(d.cohort[0]).to_string_lossy().into_owned(),
            reaches: d.reaches.iter().map(|s| s.to_string()).collect(),
        })
        .collect())
}

/// Create a new skill folder `<home dir>/<name>` in the chosen destination and
/// write `content` (a fully-rendered SKILL.md) into it. Refuses to clobber an
/// existing folder. Returns the new skill's canonical root path.
pub fn create_skill(target: &str, name: &str, content: &str) -> Result<String, String> {
    let home = dirs::home_dir().ok_or_else(|| "No home directory.".to_string())?;
    create_skill_in(&home, target, name, content)
}

fn create_skill_in(home: &Path, target: &str, name: &str, content: &str) -> Result<String, String> {
    if !valid_skill_name(name) {
        return Err("Name must be lowercase letters, digits and single hyphens (e.g. \"my-skill\").".into());
    }
    let d = dest_by_id(target).ok_or_else(|| format!("Unknown skill location: {target}"))?;
    let dir = home.join(d.cohort[0]);
    let dest = dir.join(name);
    if dest.symlink_metadata().is_ok() {
        return Err(format!("A skill named \"{name}\" already exists in {}.", dir.display()));
    }
    std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
    std::fs::write(dest.join("SKILL.md"), content).map_err(|e| e.to_string())?;
    let canon = std::fs::canonicalize(&dest).unwrap_or(dest);
    Ok(canon.to_string_lossy().into_owned())
}

/// Import an existing skill *folder* into a chosen home (`target` is a skill-home
/// id). Copies the tree under the skill's name — refusing to clobber unless
/// `overwrite` — and returns the new root plus any `.env` pairs (kept out of the
/// copy) for the caller to optionally load into the secret store.
pub fn import_skill_folder(source: &str, target: &str, overwrite: bool) -> Result<ImportResult, String> {
    let home = dirs::home_dir().ok_or_else(|| "No home directory.".to_string())?;
    let src = resolve_root(source);
    if !src.join("SKILL.md").exists() {
        return Err("Not a skill folder (no SKILL.md).".into());
    }
    import_from_dir(&home, &src, target, overwrite)
}

/// Import a skill from a `.zip` archive's bytes (the inverse of export). Extracts to
/// a temp dir, imports it like a folder, then cleans up. Used by the desktop app
/// (reads the chosen file) and the server (base64 upload via [`import_skill_zip_base64`]).
pub fn import_skill_zip(bytes: &[u8], target: &str, overwrite: bool) -> Result<ImportResult, String> {
    let home = dirs::home_dir().ok_or_else(|| "No home directory.".to_string())?;
    let seq = IMPORT_SEQ.fetch_add(1, Ordering::Relaxed);
    let staging = std::env::temp_dir().join(format!("ass_import_{}_{}", std::process::id(), seq));
    let _ = std::fs::remove_dir_all(&staging);
    let result = (|| {
        let skill_root = crate::skill::extract_zip(bytes, &staging)?;
        import_from_dir(&home, &skill_root, target, overwrite)
    })();
    let _ = std::fs::remove_dir_all(&staging);
    result
}

/// Convenience for the HTTP server (whose JSON bodies are text): decode a base64'd
/// (optionally `data:` URL-prefixed) zip, then import it.
pub fn import_skill_zip_base64(data: &str, target: &str, overwrite: bool) -> Result<ImportResult, String> {
    // Tolerate a `data:application/zip;base64,…` prefix (base64 has no comma).
    let b64 = data.rsplit(',').next().unwrap_or(data).trim();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| format!("Invalid base64 upload: {e}"))?;
    import_skill_zip(&bytes, target, overwrite)
}

/// Shared core: validate the staged skill dir, resolve its destination name, copy
/// it into the home (minus any `.env`), and report the `.env` pairs separately.
fn import_from_dir(home: &Path, src: &Path, target: &str, overwrite: bool) -> Result<ImportResult, String> {
    // Destination name: the skill's declared `name` when valid (so the result is
    // spec-valid, name == folder), else the source directory name.
    let raw = std::fs::read_to_string(src.join("SKILL.md"))
        .map_err(|e| format!("Failed to read SKILL.md: {e}"))?;
    let declared = frontmatter_name(&raw).filter(|n| valid_skill_name(n));
    let dir_name = skill_dir_name(src).filter(|n| valid_skill_name(n));
    let name = declared.or(dir_name).ok_or_else(|| {
        "Couldn't determine a valid skill name — the SKILL.md `name` and folder name are both invalid.".to_string()
    })?;

    let d = dest_by_id(target).ok_or_else(|| format!("Unknown skill location: {target}"))?;
    let dir = home.join(d.cohort[0]);
    let dest = dir.join(&name);

    // Importing an already-installed skill onto itself would copy a dir into itself.
    let canon_src = std::fs::canonicalize(src).unwrap_or_else(|_| src.to_path_buf());
    let dest_is_self =
        !is_symlink(&dest) && std::fs::canonicalize(&dest).map(|c| c == canon_src).unwrap_or(false);
    if dest_is_self {
        return Err("This skill already lives in the chosen location.".into());
    }

    // Read any bundled `.env` from the SOURCE before we copy (we won't copy it in).
    let env = read_env_pairs(src);

    let mut overwrote = false;
    if dest.symlink_metadata().is_ok() {
        if !overwrite {
            return Err(format!("A skill named \"{name}\" already exists here."));
        }
        remove_path(&dest)?;
        overwrote = true;
    }
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut total: u64 = 0;
    copy_tree(src, &dest, &mut total)?;
    // copy_tree brings every file; keep secrets out of the imported folder.
    let _ = std::fs::remove_file(dest.join(".env"));

    let canon = std::fs::canonicalize(&dest).unwrap_or(dest);
    Ok(ImportResult {
        root: canon.to_string_lossy().into_owned(),
        name,
        dir: dir.to_string_lossy().into_owned(),
        overwrote,
        env,
    })
}

/// Parse a skill's root `.env` (if any) into pairs, flagging keys already present
/// in the secret store. Never reads nested `.env` files (export only writes one at root).
fn read_env_pairs(skill_root: &Path) -> Vec<crate::secrets::DotenvEntry> {
    let Ok(body) = std::fs::read_to_string(skill_root.join(".env")) else {
        return Vec::new();
    };
    crate::secrets::preview_dotenv(&body)
}

/// Land a freshly-cloned skill repository (staged in a temp dir) into a chosen
/// home, PRESERVING its `.git` — the clone keeps its `origin`, so the imported
/// skill arrives already connected for remote sync. Unlike the folder/zip path
/// this MOVES rather than copies (a filtering copy would drop the repository),
/// and tracked files are left exactly as cloned — deleting a *tracked* `.env`
/// would just dirty the worktree against the remote, so foreign `.env` pairs
/// are reported for the secret store but the file stays.
pub(crate) fn land_cloned_skill(
    home: &Path,
    staged: &Path,
    fallback_name: Option<&str>,
    target: &str,
    overwrite: bool,
) -> Result<ImportResult, String> {
    let raw = std::fs::read_to_string(staged.join("SKILL.md"))
        .map_err(|e| format!("Failed to read SKILL.md: {e}"))?;
    // The staging dir's own name is synthetic, so fall back to the caller's
    // hint (derived from the remote URL) instead of the folder name.
    let name = frontmatter_name(&raw)
        .filter(|n| valid_skill_name(n))
        .or_else(|| fallback_name.map(str::to_string).filter(|n| valid_skill_name(n)))
        .ok_or_else(|| {
            "Couldn't determine a valid skill name — the SKILL.md `name` and repository name are both invalid."
                .to_string()
        })?;

    let d = dest_by_id(target).ok_or_else(|| format!("Unknown skill location: {target}"))?;
    let dir = home.join(d.cohort[0]);
    let dest = dir.join(&name);

    let env = read_env_pairs(staged);

    let mut overwrote = false;
    if dest.symlink_metadata().is_ok() {
        if !overwrite {
            return Err(format!("A skill named \"{name}\" already exists here."));
        }
        remove_path(&dest)?;
        overwrote = true;
    }
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    // Rename keeps .git for free; the fallback (staging on another device, e.g.
    // a tmpfs /tmp) is an unfiltered recursive copy for the same reason.
    if std::fs::rename(staged, &dest).is_err() {
        if let Err(e) = copy_dir_all(staged, &dest) {
            let _ = std::fs::remove_dir_all(&dest);
            return Err(e);
        }
    }

    let canon = std::fs::canonicalize(&dest).unwrap_or(dest);
    Ok(ImportResult {
        root: canon.to_string_lossy().into_owned(),
        name,
        dir: dir.to_string_lossy().into_owned(),
        overwrote,
        env,
    })
}

/// Unfiltered recursive copy (INCLUDING `.git` and dot-files) — only for the
/// cross-device fallback of [`land_cloned_skill`]; everything else uses the
/// filtering [`copy_tree`].
fn copy_dir_all(src: &Path, dest: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    for entry in std::fs::read_dir(src).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let to = dest.join(entry.file_name());
        if entry.file_type().map_err(|e| e.to_string())?.is_dir() {
            copy_dir_all(&entry.path(), &to)?;
        } else {
            std::fs::copy(entry.path(), &to).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[derive(Deserialize)]
struct FmName {
    name: Option<String>,
}

/// Best-effort read of `name:` from a SKILL.md's YAML frontmatter block (no regex
/// dep): the document must open with a `---` line and close with another.
fn frontmatter_name(raw: &str) -> Option<String> {
    let raw = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    let mut lines = raw.lines();
    if lines.next()?.trim_end() != "---" {
        return None;
    }
    let mut block = String::new();
    for line in lines {
        if line.trim_end() == "---" {
            return serde_yaml::from_str::<FmName>(&block).ok().and_then(|f| f.name);
        }
        block.push_str(line);
        block.push('\n');
    }
    None
}

/// Permanently remove a skill folder. Guarded: it must contain SKILL.md and live
/// inside a recognized skills container, so this can't nuke an arbitrary folder.
/// A symlinked (synced) copy is unlinked, leaving the real skill untouched.
pub fn delete_skill(root: &str) -> Result<DeleteResult, String> {
    let path = PathBuf::from(root);
    let meta = std::fs::symlink_metadata(&path).map_err(|_| "Skill not found.".to_string())?;
    let was_link = meta.file_type().is_symlink();
    if !path.join("SKILL.md").exists() {
        return Err("Not a skill directory (no SKILL.md).".into());
    }
    if !within_skills_container(&path) {
        return Err("Refusing to delete: this folder isn't inside a known skills directory.".into());
    }
    if was_link {
        std::fs::remove_file(&path).map_err(|e| e.to_string())?;
    } else {
        std::fs::remove_dir_all(&path).map_err(|e| e.to_string())?;
    }
    Ok(DeleteResult { removed: path.to_string_lossy().into_owned(), was_link })
}

/// Accept a proposed skill: move it out of its `generated-skills/` staging folder
/// up into the real skills home it sits under (`<home>/generated-skills/<name>` →
/// `<home>/<name>`), so it becomes an ordinary discovered skill. Guarded: the
/// folder must contain SKILL.md, sit directly inside a `generated-skills/` dir,
/// and land in a recognized skills container — and it won't clobber an existing
/// skill of the same name. Returns the new canonical root.
pub fn promote_skill(root: &str) -> Result<PromoteResult, String> {
    let path = PathBuf::from(root);
    if !path.join("SKILL.md").exists() {
        return Err("Not a skill directory (no SKILL.md).".into());
    }
    let staging = path.parent().ok_or_else(|| "Invalid skill path.".to_string())?;
    if staging.file_name().and_then(|n| n.to_str()) != Some("generated-skills") {
        return Err("Not a proposed skill (it isn't inside a generated-skills/ folder).".into());
    }
    let home = staging
        .parent()
        .ok_or_else(|| "Invalid generated-skills location.".to_string())?;
    // The home it lands in must itself be a skills container, so accepting can't
    // drop a folder somewhere unexpected.
    if !matches!(home.file_name().and_then(|n| n.to_str()), Some("skills" | "skills-cursor")) {
        return Err("Refusing to accept: the generated-skills folder isn't inside a skills directory.".into());
    }
    let name = skill_dir_name(&path).ok_or_else(|| "Invalid skill path.".to_string())?;
    let dest = home.join(&name);
    if dest.symlink_metadata().is_ok() {
        return Err(format!("A skill named \"{name}\" already exists in {}.", home.display()));
    }
    // Same filesystem in practice (staging is a subdir of the home), so a rename is
    // atomic; fall back to copy+remove if it ever crosses a device boundary. On any
    // failure in the fallback, roll back the partial destination so the skill is
    // never left in two places — it stays put under generated-skills/.
    if std::fs::rename(&path, &dest).is_err() {
        let mut total: u64 = 0;
        if let Err(e) = copy_tree(&path, &dest, &mut total) {
            let _ = std::fs::remove_dir_all(&dest);
            return Err(e);
        }
        if let Err(e) = std::fs::remove_dir_all(&path) {
            let _ = std::fs::remove_dir_all(&dest);
            return Err(format!("Couldn't remove the staged copy after accepting: {e}"));
        }
    }
    let canon = std::fs::canonicalize(&dest).unwrap_or(dest);
    Ok(PromoteResult { root: canon.to_string_lossy().into_owned() })
}

/// True if some ancestor directory is a skills container (`skills` / `skills-cursor`).
fn within_skills_container(path: &Path) -> bool {
    let mut cur = path.parent();
    while let Some(p) = cur {
        if matches!(p.file_name().and_then(|n| n.to_str()), Some("skills" | "skills-cursor")) {
            return true;
        }
        cur = p.parent();
    }
    false
}

fn is_symlink(p: &Path) -> bool {
    std::fs::symlink_metadata(p).map(|m| m.file_type().is_symlink()).unwrap_or(false)
}

/// Remove a path whether it's a symlink, a file, or a directory tree.
fn remove_path(p: &Path) -> Result<(), String> {
    let meta = std::fs::symlink_metadata(p).map_err(|e| e.to_string())?;
    if meta.file_type().is_symlink() || meta.is_file() {
        std::fs::remove_file(p).map_err(|e| e.to_string())
    } else {
        std::fs::remove_dir_all(p).map_err(|e| e.to_string())
    }
}

#[cfg(unix)]
fn symlink_dir(src: &Path, dst: &Path) -> Result<(), String> {
    std::os::unix::fs::symlink(src, dst).map_err(|e| e.to_string())
}
#[cfg(not(unix))]
fn symlink_dir(_src: &Path, _dst: &Path) -> Result<(), String> {
    Err("Linking isn't supported on this platform — sync a copy instead.".into())
}

pub(crate) fn copy_tree(src: &Path, dst: &Path, total: &mut u64) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    let rd = std::fs::read_dir(src).map_err(|e| e.to_string())?;
    for entry in rd.filter_map(|e| e.ok()) {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        let ft = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if ft.is_symlink() {
            continue;
        }
        let from = entry.path();
        let to = dst.join(&name);
        if ft.is_dir() {
            if IGNORED_DIRS.contains(&name_str.as_ref()) {
                continue;
            }
            copy_tree(&from, &to, total)?;
        } else if ft.is_file() {
            let len = entry.metadata().map(|m| m.len()).unwrap_or(0);
            *total += len;
            if *total > MAX_TOTAL {
                return Err("Skill is too large to sync.".into());
            }
            std::fs::copy(&from, &to).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Install (or reinstall) a bundled skill into `dest`, replacing its content
/// while preserving any `.git` the user created there. A versioned copy thus
/// receives an official update as ordinary uncommitted changes — reviewable
/// and revertable chunk by chunk — never as silent loss of their history.
/// A symlinked `dest` is replaced by a real directory: discovery doesn't
/// follow links, so writing through one leaves the location looking empty.
pub(crate) fn install_skill(src: &Path, dest: &Path) -> Result<(), String> {
    let is_link = std::fs::symlink_metadata(dest).map(|m| m.file_type().is_symlink()).unwrap_or(false);
    if is_link {
        std::fs::remove_file(dest)
            .or_else(|_| std::fs::remove_dir(dest)) // Windows dir-symlinks
            .map_err(|e| e.to_string())?;
    }
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    let rd = std::fs::read_dir(dest).map_err(|e| e.to_string())?;
    for entry in rd.filter_map(|e| e.ok()) {
        if entry.file_name() == ".git" {
            continue;
        }
        let p = entry.path();
        let is_dir = entry.file_type().map(|t| t.is_dir() && !t.is_symlink()).unwrap_or(false);
        let res = if is_dir { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
        res.map_err(|e| e.to_string())?;
    }
    let mut total = 0;
    copy_tree(src, dest, &mut total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_agent_dirs() {
        assert!(agent_user_dir("Claude Code").unwrap().ends_with(".claude/skills"));
        assert!(agent_user_dir("Codex").unwrap().ends_with(".codex/skills"));
        assert!(agent_user_dir("Cursor").unwrap().ends_with(".cursor/skills"));
        assert!(agent_user_dir("OpenClaw").unwrap().ends_with(".openclaw/skills"));
        assert!(agent_user_dir("opencode").unwrap().ends_with(".config/opencode/skills"));
        assert!(agent_user_dir("Nope").is_none());
    }

    #[test]
    fn copies_tree_skipping_ignored_and_symlinks() {
        let base = std::env::temp_dir().join(format!("ass_sync_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let src = base.join("src");
        std::fs::create_dir_all(src.join(".git")).unwrap();
        std::fs::create_dir_all(src.join("scripts")).unwrap();
        std::fs::write(src.join("SKILL.md"), "x").unwrap();
        std::fs::write(src.join(".git/HEAD"), "ref: refs/heads/main").unwrap();
        std::fs::write(src.join("scripts/run.py"), "print(1)").unwrap();

        #[cfg(unix)]
        {
            let outside = base.join("outside");
            std::fs::create_dir_all(&outside).unwrap();
            std::fs::write(outside.join("secret.txt"), "must not be copied").unwrap();
            std::os::unix::fs::symlink(outside.join("secret.txt"), src.join("linked-file")).unwrap();
            std::os::unix::fs::symlink(&outside, src.join("linked-dir")).unwrap();
        }

        let dst = base.join("dst");
        let mut total = 0;
        copy_tree(&src, &dst, &mut total).unwrap();

        assert!(dst.join("SKILL.md").exists());
        assert!(dst.join("scripts/run.py").exists());
        assert!(!dst.join(".git").exists(), ".git must be skipped");
        #[cfg(unix)]
        for name in ["linked-file", "linked-dir"] {
            assert!(std::fs::symlink_metadata(dst.join(name)).is_err(), "copied symlink: {name}");
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn install_skill_preserves_git_and_replaces_content() {
        let base = std::env::temp_dir().join(format!("ass_sync_install_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let src = base.join("src");
        std::fs::create_dir_all(src.join("scripts")).unwrap();
        std::fs::write(src.join("SKILL.md"), "official v2").unwrap();
        std::fs::write(src.join("scripts/new.py"), "new").unwrap();
        // Installed copy: user-versioned (.git), user-edited, with a stale file.
        let dest = base.join("dest");
        std::fs::create_dir_all(dest.join(".git")).unwrap();
        std::fs::write(dest.join(".git/HEAD"), "ref: refs/heads/master").unwrap();
        std::fs::write(dest.join("SKILL.md"), "user-edited").unwrap();
        std::fs::write(dest.join("stale.md"), "removed upstream").unwrap();

        install_skill(&src, &dest).unwrap();

        assert_eq!(std::fs::read_to_string(dest.join("SKILL.md")).unwrap(), "official v2");
        assert!(dest.join(".git/HEAD").exists(), ".git must survive a reinstall");
        assert!(!dest.join("stale.md").exists(), "files dropped upstream are removed");
        assert!(dest.join("scripts/new.py").exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[cfg(unix)]
    #[test]
    fn install_skill_replaces_symlinked_dest_with_real_dir() {
        let base = std::env::temp_dir().join(format!("ass_sync_install_ln_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let src = base.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("SKILL.md"), "official").unwrap();
        // dest is a symlink into another location (the sync-as-link layout).
        let elsewhere = base.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("SKILL.md"), "old").unwrap();
        let dest = base.join("dest");
        std::os::unix::fs::symlink(&elsewhere, &dest).unwrap();

        install_skill(&src, &dest).unwrap();

        assert!(!is_symlink(&dest), "the link is replaced by a real directory");
        assert_eq!(std::fs::read_to_string(dest.join("SKILL.md")).unwrap(), "official");
        // The old link target is left untouched.
        assert_eq!(std::fs::read_to_string(elsewhere.join("SKILL.md")).unwrap(), "old");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn copy_then_link_then_targets() {
        let base = std::env::temp_dir().join(format!("ass_sync_dest_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        let src = base.join("src").join("my-skill");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("SKILL.md"), "---\nname: my-skill\n---\nbody").unwrap();
        let src_str = src.to_string_lossy().into_owned();

        // Copy to the universal dir; link into Claude Code.
        let r1 = sync_skill_in(&home, &src_str, "universal", false, false).unwrap();
        assert!(!r1.linked);
        assert!(home.join(".agents/skills/my-skill/SKILL.md").exists());
        assert!(!is_symlink(&home.join(".agents/skills/my-skill")));

        let r2 = sync_skill_in(&home, &src_str, "claude-code", false, true).unwrap();
        assert!(r2.linked);
        let claude_dest = home.join(".claude/skills/my-skill");
        assert!(is_symlink(&claude_dest));
        assert!(claude_dest.join("SKILL.md").exists(), "link resolves to the skill");

        // Re-adding without overwrite is refused.
        assert!(sync_skill_in(&home, &src_str, "universal", false, false).is_err());

        let targets = sync_targets_in(&home, &src_str).unwrap();
        let uni = targets.iter().find(|t| t.id == "universal").unwrap();
        assert!(uni.present && !uni.linked && !uni.is_source);
        let cc = targets.iter().find(|t| t.id == "claude-code").unwrap();
        assert!(cc.present && cc.linked);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn legacy_presence_is_union_aware() {
        let base = std::env::temp_dir().join(format!("ass_sync_legacy_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        // Skill natively lives in the legacy ~/.codex/skills dir.
        let src = home.join(".codex/skills/legacy-skill");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("SKILL.md"), "x").unwrap();

        let targets = sync_targets_in(&home, &src.to_string_lossy()).unwrap();
        let uni = targets.iter().find(|t| t.id == "universal").unwrap();
        // The universal cohort reads ~/.codex/skills, so it's already reachable
        // there (and it's the source) — we must NOT offer to re-add it.
        assert!(uni.present);
        assert!(uni.is_source);
        assert_eq!(uni.reached_via.as_deref(), Some(".codex/skills"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn validates_skill_names() {
        assert!(valid_skill_name("my-skill"));
        assert!(valid_skill_name("pdf"));
        assert!(valid_skill_name("a1-b2-c3"));
        assert!(!valid_skill_name(""));
        assert!(!valid_skill_name("My-Skill")); // uppercase
        assert!(!valid_skill_name("-skill")); // leading hyphen
        assert!(!valid_skill_name("skill-")); // trailing hyphen
        assert!(!valid_skill_name("a--b")); // repeated hyphen
        assert!(!valid_skill_name("a b")); // space
        assert!(!valid_skill_name("../escape")); // path chars
        assert!(!valid_skill_name(&"x".repeat(65))); // too long
    }

    #[test]
    fn creates_skill_in_destination() {
        let base = std::env::temp_dir().join(format!("ass_create_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");

        let content = "---\nname: new-skill\ndescription: A test skill.\n---\n\nBody.\n";
        let root = create_skill_in(&home, "universal", "new-skill", content).unwrap();
        let dest = home.join(".agents/skills/new-skill");
        assert_eq!(std::fs::canonicalize(&dest).unwrap().to_string_lossy(), root);
        assert_eq!(std::fs::read_to_string(dest.join("SKILL.md")).unwrap(), content);

        // Creating onto an existing folder is refused.
        assert!(create_skill_in(&home, "universal", "new-skill", content).is_err());
        // Bad name is rejected before touching the filesystem.
        assert!(create_skill_in(&home, "claude-code", "Bad Name", content).is_err());
        // Unknown destination is rejected.
        assert!(create_skill_in(&home, "nope", "ok-name", content).is_err());

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn imports_folder_naming_by_frontmatter_and_strips_env() {
        let base = std::env::temp_dir().join(format!("ass_import_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        // Source folder is named differently from the skill's declared name — the
        // import should land under the declared name (spec-valid: name == folder).
        let src = base.join("download/my-skill (1)");
        std::fs::create_dir_all(src.join("scripts")).unwrap();
        std::fs::write(
            src.join("SKILL.md"),
            "---\nname: my-skill\ndescription: A test skill.\n---\n\nBody.\n",
        )
        .unwrap();
        std::fs::write(src.join("scripts/run.py"), "print(1)").unwrap();
        std::fs::write(src.join(".env"), "TOKEN='secret-value'\n").unwrap();

        let r = import_from_dir(&home, &src, "universal", false).unwrap();
        assert_eq!(r.name, "my-skill");
        let dest = home.join(".agents/skills/my-skill");
        assert!(dest.join("SKILL.md").exists());
        assert!(dest.join("scripts/run.py").exists());
        assert!(!dest.join(".env").exists(), ".env must be kept out of the imported folder");
        assert_eq!(r.env.len(), 1);
        assert_eq!(r.env[0].key, "TOKEN");
        assert_eq!(r.env[0].value, "secret-value");

        // Re-importing without overwrite is refused; with overwrite it replaces.
        assert!(import_from_dir(&home, &src, "universal", false).is_err());
        assert!(import_from_dir(&home, &src, "universal", true).unwrap().overwrote);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn imports_from_zip_bytes_via_extract() {
        let base = std::env::temp_dir().join(format!("ass_import_zip_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        // Build a skill, zip it (the export path), then extract+import the bytes.
        let src = base.join("src/zipped-skill");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("SKILL.md"),
            "---\nname: zipped-skill\ndescription: From a zip.\n---\n\nBody.\n",
        )
        .unwrap();
        let (_, bytes) = crate::skill::zip_skill_bytes(&src.to_string_lossy(), &[]).unwrap();

        let staging = base.join("staging");
        let skill_root = crate::skill::extract_zip(&bytes, &staging).unwrap();
        assert!(skill_root.join("SKILL.md").exists());
        let r = import_from_dir(&home, &skill_root, "claude-code", false).unwrap();
        assert_eq!(r.name, "zipped-skill");
        assert!(home.join(".claude/skills/zipped-skill/SKILL.md").exists());

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn promotes_proposed_skill_out_of_staging() {
        let base = std::env::temp_dir().join(format!("ass_promote_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join(".agents/skills");
        let staged = home.join("generated-skills/fresh-skill");
        std::fs::create_dir_all(staged.join("scripts")).unwrap();
        std::fs::write(staged.join("SKILL.md"), "---\nname: fresh-skill\n---\nbody").unwrap();
        std::fs::write(staged.join("scripts/run.py"), "print(1)").unwrap();

        let r = promote_skill(&staged.to_string_lossy()).unwrap();
        let dest = home.join("fresh-skill");
        assert_eq!(std::fs::canonicalize(&dest).unwrap().to_string_lossy(), r.root);
        assert!(dest.join("SKILL.md").exists());
        assert!(dest.join("scripts/run.py").exists(), "the whole folder moves, not just SKILL.md");
        assert!(!staged.exists(), "the staged copy is gone after accepting");

        // A skill that isn't under generated-skills/ can't be promoted.
        assert!(promote_skill(&dest.to_string_lossy()).is_err());

        // Accepting onto an existing skill of the same name is refused (no clobber).
        let staged2 = home.join("generated-skills/fresh-skill");
        std::fs::create_dir_all(&staged2).unwrap();
        std::fs::write(staged2.join("SKILL.md"), "---\nname: fresh-skill\n---\nv2").unwrap();
        assert!(promote_skill(&staged2.to_string_lossy()).is_err());
        assert!(dest.join("SKILL.md").exists(), "the already-accepted skill is untouched");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn deletes_skill_unlinks_and_guards() {
        let base = std::env::temp_dir().join(format!("ass_sync_del_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);

        // A normal skill inside a `skills/` container is deletable.
        let real = base.join("skills/my-skill");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("SKILL.md"), "x").unwrap();
        let r = delete_skill(&real.to_string_lossy()).unwrap();
        assert!(!r.was_link);
        assert!(!real.exists());

        // A folder NOT inside a skills container is refused.
        let stray = base.join("notskills/thing");
        std::fs::create_dir_all(&stray).unwrap();
        std::fs::write(stray.join("SKILL.md"), "x").unwrap();
        assert!(delete_skill(&stray.to_string_lossy()).is_err());
        assert!(stray.exists(), "guard must leave it intact");

        // Deleting a symlinked copy removes only the link, not its target.
        #[cfg(unix)]
        {
            let target = base.join("skills/source-skill");
            std::fs::create_dir_all(&target).unwrap();
            std::fs::write(target.join("SKILL.md"), "x").unwrap();
            let link = base.join("skills/linked-copy");
            std::os::unix::fs::symlink(&target, &link).unwrap();
            let r = delete_skill(&link.to_string_lossy()).unwrap();
            assert!(r.was_link);
            assert!(!link.exists());
            assert!(target.join("SKILL.md").exists(), "real skill survives unlink");
        }

        let _ = std::fs::remove_dir_all(&base);
    }
}
