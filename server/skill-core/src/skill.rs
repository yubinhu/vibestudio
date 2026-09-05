// Filesystem layer, ported from the original lib/server.ts. Transport-agnostic
// (no Tauri) — reused by the desktop commands and the headless server.
use std::collections::BTreeSet;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};

use base64::Engine;
use serde::Serialize;
use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;

use crate::filetypes;
use crate::pathsafe::{normalize_lexical, resolve_root, resolve_within_real};

const MAX_TEXT_BYTES: u64 = 2 * 1024 * 1024; // 2 MB
const MAX_ASSET_BYTES: usize = 25 * 1024 * 1024; // 25 MB — pasted/dropped media
const MAX_TREE_ENTRIES: i64 = 5000;
const MAX_TOTAL: u64 = 100 * 1024 * 1024; // 100 MB zip cap
const IGNORED_DIRS: [&str; 5] = [".git", "node_modules", ".next", "__pycache__", ".venv"];

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TreeNode {
    name: String,
    rel: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    is_skill_md: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    children: Option<Vec<TreeNode>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RawSkill {
    root: String,
    dir_name: String,
    raw: String,
    tree: Vec<TreeNode>,
    files: Vec<String>,
    file_count: usize,
    dir_count: usize,
    total_bytes: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileView {
    rel: String,
    category: String,
    language: String,
    label: String,
    size: u64,
    /// Content fingerprint the editor echoes back on write so the server can refuse
    /// to overwrite a version newer than the one it loaded. Absent for images /
    /// too-large files (not edited through the text path).
    #[serde(skip_serializing_if = "Option::is_none")]
    etag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    too_large: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    is_binary: Option<bool>,
}

/// Short content fingerprint (first 8 bytes of sha256, hex) used as an optimistic-
/// concurrency tag: read returns it, write echoes it back, and the server refuses a
/// write whose tag no longer matches what's on disk. 64 bits is ample to detect an
/// intervening external edit; kept short to stay cheap on the wire.
fn etag_of(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut s = String::with_capacity(16);
    for b in &digest[..8] {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Result of [`write_file_impl`]. `Written` carries the new tag the editor adopts as
/// its baseline; `Stale` means disk advanced past the tag the editor sent — the
/// write was refused and the caller gets the current disk bytes to reconcile.
#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum WriteOutcome {
    Written {
        etag: String,
    },
    Stale {
        #[serde(rename = "diskEtag")]
        disk_etag: String,
        #[serde(rename = "diskContent")]
        disk_content: String,
    },
}

/// Cheap change signal for the editor's show-latest poll: modified-time + size,
/// metadata only (no file read or hash). A move in either is the gate for a full
/// re-read; tiny on the wire, so it's fine to poll over the remote tunnel.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileStat {
    mtime_ms: u64,
    size: u64,
}

#[derive(Serialize)]
pub struct ImageData {
    pub mime: String,
    pub base64: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirEntry {
    name: String,
    is_dir: bool,
    is_skill: bool,
    /// A markdown-family file (.md/.markdown/.mdx) — lets the picker offer "Open"
    /// only on files the loose-markdown editor can render. Always false for dirs.
    is_markdown: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirListing {
    path: String,
    parent: Option<String>,
    entries: Vec<DirEntry>,
}

fn to_posix(abs: &Path, root: &Path) -> String {
    let rel = abs.strip_prefix(root).unwrap_or(abs);
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

struct BuildAcc {
    files: Vec<String>,
    file_count: usize,
    dir_count: usize,
    total_bytes: u64,
    budget: i64,
}

fn walk_tree(dir: &Path, root: &Path, acc: &mut BuildAcc) -> Vec<TreeNode> {
    let mut entries: Vec<_> = match std::fs::read_dir(dir) {
        Ok(rd) => rd.filter_map(|e| e.ok()).collect(),
        Err(_) => return vec![],
    };
    entries.sort_by(|a, b| {
        let a_is_file = a.file_type().map(|t| !t.is_dir()).unwrap_or(true);
        let b_is_file = b.file_type().map(|t| !t.is_dir()).unwrap_or(true);
        a_is_file
            .cmp(&b_is_file)
            .then_with(|| a.file_name().cmp(&b.file_name()))
    });

    let mut nodes = Vec::new();
    for entry in entries {
        if acc.budget <= 0 {
            break;
        }
        acc.budget -= 1;

        let name = entry.file_name().to_string_lossy().into_owned();
        let ft = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        let abs = dir.join(&name);
        let rel = to_posix(&abs, root);

        if ft.is_dir() {
            if IGNORED_DIRS.contains(&name.as_str()) {
                continue;
            }
            acc.dir_count += 1;
            let children = walk_tree(&abs, root, acc);
            nodes.push(TreeNode {
                name,
                rel,
                kind: "dir".into(),
                size: None,
                category: None,
                language: None,
                label: None,
                is_skill_md: None,
                children: Some(children),
            });
        } else if ft.is_file() {
            acc.file_count += 1;
            let size = std::fs::metadata(&abs).map(|m| m.len()).unwrap_or(0);
            acc.total_bytes += size;
            acc.files.push(rel.clone());
            let (category, language, label) = filetypes::file_type(&name);
            let is_skill_md = rel == "SKILL.md";
            nodes.push(TreeNode {
                name,
                rel,
                kind: "file".into(),
                size: Some(size),
                category: Some(category.into()),
                language: Some(language.into()),
                label: Some(label.into()),
                is_skill_md: Some(is_skill_md),
                children: None,
            });
        }
    }
    nodes
}

/// Resolve a skill path: `~`/absolute via resolve_root; a relative path (bundled
/// examples) against `examples_base`, then the working dir.
pub fn resolve_skill_input(input: &str, examples_base: Option<&Path>) -> PathBuf {
    let trimmed = input.trim();
    if trimmed == "~" || trimmed.starts_with("~/") || Path::new(trimmed).is_absolute() {
        return resolve_root(trimmed);
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(base) = examples_base {
        candidates.push(base.join(trimmed));
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join(trimmed));
    }
    for c in &candidates {
        if c.exists() {
            return normalize_lexical(c);
        }
    }
    normalize_lexical(&candidates.into_iter().next().unwrap_or_else(|| PathBuf::from(trimmed)))
}

/// Read + analyze a skill directory.
pub fn build_raw_skill(root: &Path) -> Result<RawSkill, String> {
    let meta = std::fs::metadata(root).map_err(|_| format!("Path not found: {}", root.display()))?;
    if !meta.is_dir() {
        return Err(format!("Not a directory: {}", root.display()));
    }
    let skill_md = root.join("SKILL.md");
    if !skill_md.exists() {
        return Err(format!(
            "No SKILL.md found in {}. A skill directory must contain a SKILL.md file.",
            root.display()
        ));
    }
    let raw = std::fs::read_to_string(&skill_md).map_err(|e| format!("Failed to read SKILL.md: {e}"))?;

    let mut acc = BuildAcc {
        files: Vec::new(),
        file_count: 0,
        dir_count: 0,
        total_bytes: 0,
        budget: MAX_TREE_ENTRIES,
    };
    let tree = walk_tree(root, root, &mut acc);
    let dir_name = root
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();

    Ok(RawSkill {
        root: root.to_string_lossy().into_owned(),
        dir_name,
        raw,
        tree,
        files: acc.files,
        file_count: acc.file_count,
        dir_count: acc.dir_count,
        total_bytes: acc.total_bytes,
    })
}

pub fn read_file_impl(root: &str, rel: &str) -> Result<FileView, String> {
    let root_path = PathBuf::from(root);
    let abs = resolve_within_real(&root_path, rel, true)?;
    let meta = std::fs::metadata(&abs).map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err(format!("Not a file: {rel}"));
    }
    let name = abs
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (category, language, label) = filetypes::file_type(&name);
    let size = meta.len();

    let mut view = FileView {
        rel: rel.to_string(),
        category: category.into(),
        language: language.into(),
        label: label.into(),
        size,
        etag: None,
        content: None,
        too_large: None,
        is_binary: None,
    };

    if filetypes::is_image(&name) {
        return Ok(view);
    }
    if size > MAX_TEXT_BYTES {
        view.too_large = Some(true);
        return Ok(view);
    }
    let bytes = std::fs::read(&abs).map_err(|e| e.to_string())?;
    view.etag = Some(etag_of(&bytes));
    if !filetypes::is_textual(&name) && bytes.contains(&0u8) {
        view.is_binary = Some(true);
        view.category = "binary".into();
        return Ok(view);
    }
    view.content = Some(String::from_utf8_lossy(&bytes).into_owned());
    Ok(view)
}

/// Stat a file for the show-latest poll: modified-time (ms since epoch, 0 if the
/// platform won't report it) + size, without reading or hashing the contents.
pub fn stat_file_impl(root: &str, rel: &str) -> Result<FileStat, String> {
    let root_path = PathBuf::from(root);
    let abs = resolve_within_real(&root_path, rel, true)?;
    let meta = std::fs::metadata(&abs).map_err(|e| e.to_string())?;
    let mtime_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    Ok(FileStat { mtime_ms, size: meta.len() })
}

/// Write `content` to `rel`. When `expected_etag` is set this is a compare-and-swap:
/// if the file on disk no longer matches that tag — an external process (an agent,
/// git, an editor) wrote it since we loaded it — the write is REFUSED and the current
/// disk bytes come back as [`WriteOutcome::Stale`] for the caller to reconcile,
/// honoring "never overwrite a disk version newer than the one you loaded." A `None`
/// tag keeps the legacy unconditional overwrite (callers not yet tracking a baseline).
/// An absent file is not a conflict — the write recreates it.
pub fn write_file_impl(
    root: &str,
    rel: &str,
    content: &str,
    expected_etag: Option<&str>,
) -> Result<WriteOutcome, String> {
    let root_path = PathBuf::from(root);
    let abs = resolve_within_real(&root_path, rel, false)?;
    if let Some(expected) = expected_etag {
        if let Ok(disk) = std::fs::read(&abs) {
            let disk_etag = etag_of(&disk);
            if disk_etag != expected {
                return Ok(WriteOutcome::Stale {
                    disk_etag,
                    disk_content: String::from_utf8_lossy(&disk).into_owned(),
                });
            }
        }
    }
    if let Some(parent) = abs.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&abs, content).map_err(|e| e.to_string())?;
    Ok(WriteOutcome::Written { etag: etag_of(content.as_bytes()) })
}

/// Reduce a client/clipboard-supplied filename to a safe `(stem, ext)`: drop any
/// directory part, lowercase the extension, and replace anything outside
/// `[A-Za-z0-9._-]` with `-` so the on-disk name stays predictable and link-safe.
/// Empty stem → "image"; empty ext → "png" (the paste path always carries one).
fn split_asset_name(name: &str) -> (String, String) {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let (stem, ext) = match base.rfind('.') {
        Some(i) if i > 0 => (&base[..i], &base[i + 1..]),
        _ => (base, ""),
    };
    let sanitize = |s: &str, fallback: &str| {
        let cleaned: String = s
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '-' })
            .collect();
        let trimmed = cleaned.trim_matches(['-', '.']);
        if trimmed.is_empty() { fallback.to_string() } else { trimmed.to_string() }
    };
    (sanitize(stem, "image"), sanitize(&ext.to_lowercase(), "png"))
}

/// Write a binary asset (base64) into the skill under `dir`, choosing a filename
/// derived from `name` that doesn't clobber an existing file (`stem.ext`, then
/// `stem-1.ext`, `stem-2.ext`, …). Returns the path written relative to `root`
/// (POSIX) — ready to drop into a markdown link. Mirrors [`write_file_impl`]'s
/// sandboxing (`resolve_within_real`) but takes raw bytes; pasted media should
/// accumulate, never overwrite, so an existing name is stepped past rather than
/// replaced.
pub fn write_asset_impl(root: &str, dir: &str, name: &str, data_b64: &str) -> Result<String, String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data_b64.trim())
        .map_err(|_| "The pasted data wasn’t valid base64.".to_string())?;
    if bytes.is_empty() {
        return Err("The pasted file was empty.".into());
    }
    if bytes.len() > MAX_ASSET_BYTES {
        return Err("The file is too large (max 25 MB).".into());
    }
    let (stem, ext) = split_asset_name(name);
    let root_path = PathBuf::from(root);
    let dir_rel = dir.replace('\\', "/");
    let dir_rel = dir_rel.trim_matches('/');
    let dir_rel = if dir_rel == "." { "" } else { dir_rel };
    for n in 0..1000 {
        let fname = if n == 0 { format!("{stem}.{ext}") } else { format!("{stem}-{n}.{ext}") };
        let rel = if dir_rel.is_empty() { fname } else { format!("{dir_rel}/{fname}") };
        let abs = resolve_within_real(&root_path, &rel, false)?;
        if abs.exists() {
            continue;
        }
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&abs, &bytes).map_err(|e| e.to_string())?;
        return Ok(rel);
    }
    Err("Couldn’t find a free filename for the asset.".into())
}

/// Delete a file or directory inside the skill (directories are removed
/// recursively). Two things are protected: the skill root itself and the
/// top-level `SKILL.md` — removing either would stop the folder being a skill
/// (deleting the whole skill is a separate, guarded operation). Path containment
/// (incl. symlink escape) is enforced by `resolve_within_real`; `symlink_metadata`
/// keeps us from following a symlink and deleting its target — we unlink the link.
pub fn delete_path_impl(root: &str, rel: &str) -> Result<(), String> {
    let cleaned = rel.replace('\\', "/");
    let cleaned = cleaned.trim_matches('/');
    if cleaned.is_empty() || cleaned == "." {
        return Err("Refusing to delete the skill folder.".into());
    }
    if cleaned.eq_ignore_ascii_case("SKILL.md") {
        return Err("SKILL.md can’t be deleted — it defines the skill.".into());
    }
    let root_path = PathBuf::from(root);
    let abs = resolve_within_real(&root_path, rel, true)?;
    let meta = std::fs::symlink_metadata(&abs).map_err(|e| e.to_string())?;
    if meta.is_dir() {
        std::fs::remove_dir_all(&abs).map_err(|e| e.to_string())?;
    } else {
        std::fs::remove_file(&abs).map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub fn read_image_impl(root: &str, rel: &str) -> Result<ImageData, String> {
    let root_path = PathBuf::from(root);
    let abs = resolve_within_real(&root_path, rel, true)?;
    let meta = std::fs::metadata(&abs).map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err(format!("File not found: {rel}"));
    }
    let bytes = std::fs::read(&abs).map_err(|e| e.to_string())?;
    let name = abs
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(ImageData {
        mime: filetypes::image_mime(&name).into(),
        base64: base64::engine::general_purpose::STANDARD.encode(&bytes),
    })
}

/// List subdirectories of `path` (for a remote folder picker). Shows hidden dirs
/// (skills live under e.g. ~/.codex) and flags which dirs are skills. With
/// `include_files`, regular files are listed too (flagged `is_markdown`) so the
/// picker can open a loose markdown file; without it the listing is dirs-only,
/// exactly as the skill-folder picker has always seen it.
pub fn list_dir_impl(path: &str, include_files: bool) -> Result<DirListing, String> {
    let p = if path.trim().is_empty() {
        dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
    } else {
        resolve_root(path)
    };
    let meta = std::fs::metadata(&p).map_err(|e| e.to_string())?;
    if !meta.is_dir() {
        return Err(format!("Not a directory: {}", p.display()));
    }
    let mut entries = Vec::new();
    {
        let rd = std::fs::read_dir(&p).map_err(|e| e.to_string())?;
        for e in rd.filter_map(|e| e.ok()) {
            let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
            let name = e.file_name().to_string_lossy().into_owned();
            if is_dir {
                let is_skill = p.join(&name).join("SKILL.md").exists();
                entries.push(DirEntry {
                    name,
                    is_dir: true,
                    is_skill,
                    is_markdown: false,
                });
            } else if include_files {
                let is_markdown = filetypes::file_type(&name).0 == "markdown";
                entries.push(DirEntry {
                    name,
                    is_dir: false,
                    is_skill: false,
                    is_markdown,
                });
            }
        }
    }
    // Dirs first, then files, each alphabetical. Dirs-only mode (the skill picker)
    // collapses to the previous plain alphabetical order.
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(DirListing {
        path: p.to_string_lossy().into_owned(),
        parent: p.parent().map(|pp| pp.to_string_lossy().into_owned()),
        entries,
    })
}

/// Resolve + validate a skill root and return (filename, zip bytes). When
/// `env_vars` is non-empty, the values of those (managed) secrets are rendered
/// into a `.env` inside the bundle so the recipient can run it immediately —
/// the opt-in "bundle secrets" path. Names absent from the store are skipped.
pub fn zip_skill_bytes(root_input: &str, env_vars: &[String]) -> Result<(String, Vec<u8>), String> {
    let root = resolve_root(root_input);
    let meta = std::fs::metadata(&root).map_err(|_| format!("Skill not found: {}", root.display()))?;
    if !meta.is_dir() {
        return Err(format!("Skill not found: {}", root.display()));
    }
    if !root.join("SKILL.md").exists() {
        return Err("Not a skill directory (no SKILL.md).".into());
    }
    // Gate packaging on valid frontmatter so an emitted `.skill` is guaranteed to
    // install cleanly — the loader and other agents only honour a well-formed head.
    validate_skill_md(&root)?;
    let dir_name = root
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "skill".into());
    let buf = build_zip(&root, &dir_name, env_vars)?;
    // A `.skill` is a deflate zip (one top-level `name/` folder), minus `.git`,
    // `.venv`, build junk and any on-disk `.env` — the shareable install unit.
    Ok((format!("{dir_name}.skill"), buf))
}

/// Package the skill and write the `.skill` into the user's Downloads folder,
/// returning where it landed. The desktop app's own export path: the webview's
/// blob download saves silently with no native UI and no path it can report, so
/// on the machine the user is at we write the file ourselves and hand back the
/// destination — the UI names it and offers "Reveal in folder". A name clash is
/// suffixed (`foo.skill` → `foo (1).skill`) so a previous export is never
/// clobbered.
pub fn save_skill_to_downloads(root_input: &str, env_vars: &[String]) -> Result<PathBuf, String> {
    let (filename, bytes) = zip_skill_bytes(root_input, env_vars)?;
    let dir = dirs::download_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join("Downloads")))
        .ok_or("Couldn't locate your Downloads folder.")?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("Couldn't open {}: {e}", dir.display()))?;
    let dest = unique_path(&dir, &filename);
    std::fs::write(&dest, &bytes).map_err(|e| format!("Couldn't save {}: {e}", dest.display()))?;
    Ok(dest)
}

/// `dir/name`, or the first free `dir/stem (N).ext` (N≥1) — so a repeat export
/// sits beside the previous file instead of overwriting it (mirrors the webview
/// download's own " (N)" collision suffixing).
fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) => (s.to_string(), format!(".{e}")),
        None => (name.to_string(), String::new()),
    };
    let mut n = 1;
    loop {
        let candidate = dir.join(format!("{stem} ({n}){ext}"));
        if !candidate.exists() {
            return candidate;
        }
        n += 1;
    }
}

/// Frontmatter keys a `.skill` may declare; anything else fails validation.
/// Mirrors the skill-creator packager's allow-list (`metadata` is the catch-all
/// for nested fields like `required-env`).
const ALLOWED_FRONTMATTER_KEYS: [&str; 6] =
    ["name", "description", "license", "allowed-tools", "metadata", "compatibility"];

/// Validate a skill's `SKILL.md` head before packaging: a parseable YAML mapping,
/// only known keys, a kebab-case `name` (≤64) and an angle-bracket-free
/// `description` (≤1024); `compatibility` (≤500) if present. Returns a
/// human-readable reason on the first failure (surfaced to the user on export).
pub fn validate_skill_md(root: &Path) -> Result<(), String> {
    let raw = std::fs::read_to_string(root.join("SKILL.md"))
        .map_err(|_| "Couldn't read SKILL.md.".to_string())?;
    let block = crate::discover::extract_frontmatter(&raw)
        .ok_or("SKILL.md has no `---` frontmatter block.")?;
    let value: serde_yaml::Value =
        serde_yaml::from_str(&block).map_err(|e| format!("SKILL.md frontmatter isn't valid YAML: {e}"))?;
    let map = value
        .as_mapping()
        .ok_or("SKILL.md frontmatter must be a set of `key: value` fields.")?;

    // One pass: reject unknown keys, capture the ones we constrain.
    let (mut name, mut description, mut compatibility) = (None, None, None);
    for (key, val) in map {
        let k = key.as_str().unwrap_or_default();
        if !ALLOWED_FRONTMATTER_KEYS.contains(&k) {
            return Err(format!(
                "Unknown frontmatter key `{k}`. Allowed: {}.",
                ALLOWED_FRONTMATTER_KEYS.join(", ")
            ));
        }
        match k {
            "name" => name = val.as_str(),
            "description" => description = val.as_str(),
            "compatibility" => compatibility = Some(val),
            _ => {}
        }
    }

    let name = name.ok_or("SKILL.md frontmatter is missing a `name`.")?;
    if !is_kebab_case(name) {
        return Err(format!(
            "`name` must be kebab-case — lowercase letters, digits and single hyphens (got `{name}`)."
        ));
    }
    if name.len() > 64 {
        return Err("`name` must be 64 characters or fewer.".into());
    }

    let description = description.ok_or("SKILL.md frontmatter is missing a `description`.")?;
    if description.contains('<') || description.contains('>') {
        return Err("`description` can't contain angle brackets (`<` or `>`).".into());
    }
    if description.chars().count() > 1024 {
        return Err("`description` must be 1024 characters or fewer.".into());
    }

    if let Some(compat) = compatibility {
        let c = compat.as_str().ok_or("`compatibility` must be a string.")?;
        if c.chars().count() > 500 {
            return Err("`compatibility` must be 500 characters or fewer.".into());
        }
    }
    Ok(())
}

/// Kebab-case: non-empty, lowercase alphanumerics in hyphen-separated segments,
/// with no leading/trailing or doubled hyphens.
fn is_kebab_case(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('-')
        && !s.ends_with('-')
        && !s.contains("--")
        && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn build_zip(root: &Path, dir_name: &str, env_vars: &[String]) -> Result<Vec<u8>, String> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::<u8>::new()));
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let mut total: u64 = 0;
    walk_zip(root, "", dir_name, &mut zip, &options, &mut total)?;
    if !env_vars.is_empty() {
        let body = crate::secrets::render_dotenv(env_vars)?;
        if !body.is_empty() {
            zip.start_file(format!("{dir_name}/.env"), options)
                .map_err(|e| e.to_string())?;
            zip.write_all(body.as_bytes()).map_err(|e| e.to_string())?;
        }
    }
    let cursor = zip.finish().map_err(|e| e.to_string())?;
    Ok(cursor.into_inner())
}

fn walk_zip(
    dir: &Path,
    prefix: &str,
    dir_name: &str,
    zip: &mut zip::ZipWriter<Cursor<Vec<u8>>>,
    options: &SimpleFileOptions,
    total: &mut u64,
) -> Result<(), String> {
    let rd = std::fs::read_dir(dir).map_err(|e| e.to_string())?;
    for entry in rd.filter_map(|e| e.ok()) {
        let name = entry.file_name().to_string_lossy().into_owned();
        let ft = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if ft.is_symlink() {
            continue;
        }
        let abs = entry.path();
        if ft.is_dir() {
            if IGNORED_DIRS.contains(&name.as_str()) {
                continue;
            }
            walk_zip(&abs, &format!("{prefix}{name}/"), dir_name, zip, options, total)?;
        } else if ft.is_file() {
            // Never ship a `.env` from disk — it's secret-bearing, and the opt-in
            // bundle writes an authoritative one (so this also avoids a duplicate
            // `{dir_name}/.env` zip entry when both exist).
            if name == ".env" {
                continue;
            }
            let data = match std::fs::read(&abs) {
                Ok(d) => d,
                Err(_) => continue,
            };
            *total += data.len() as u64;
            if *total > MAX_TOTAL {
                return Err("Skill is too large to download.".into());
            }
            zip.start_file(format!("{dir_name}/{prefix}{name}"), *options)
                .map_err(|e| e.to_string())?;
            zip.write_all(&data).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Extract a `.zip`'s bytes into `dest_dir`, returning the directory that holds
/// SKILL.md — the inverse of [`zip_skill_bytes`]. Defends against zip-slip (entries
/// that escape `dest_dir` via `..`/absolute paths) and the 100 MB total cap. The
/// returned path is `dest_dir` itself when SKILL.md sits at the archive root, or the
/// single wrapping subdir (our export's `name/SKILL.md` layout).
pub fn extract_zip(bytes: &[u8], dest_dir: &Path) -> Result<PathBuf, String> {
    let mut archive =
        zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| format!("Not a valid .zip: {e}"))?;
    std::fs::create_dir_all(dest_dir).map_err(|e| e.to_string())?;
    let mut total: u64 = 0;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
        // `enclosed_name` is None for any entry that would escape the root.
        let out = match entry.enclosed_name() {
            Some(rel) => dest_dir.join(rel),
            None => return Err("Refusing to extract: the archive contains an unsafe path.".into()),
        };
        if !out.starts_with(dest_dir) {
            return Err("Refusing to extract: the archive contains an unsafe path.".into());
        }
        if entry.is_dir() {
            std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        total += entry.size();
        if total > MAX_TOTAL {
            return Err("Archive is too large to import.".into());
        }
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf).map_err(|e| e.to_string())?;
        std::fs::write(&out, &buf).map_err(|e| e.to_string())?;
    }
    find_skill_root(dest_dir).ok_or_else(|| "The archive has no SKILL.md (not a skill).".into())
}

/// Locate the directory containing SKILL.md: `base` itself, or a single top-level
/// subdirectory (the common `name/SKILL.md` layout). Searches one level deep.
fn find_skill_root(base: &Path) -> Option<PathBuf> {
    if base.join("SKILL.md").exists() {
        return Some(base.to_path_buf());
    }
    let rd = std::fs::read_dir(base).ok()?;
    for entry in rd.filter_map(|e| e.ok()) {
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            let cand = entry.path();
            if cand.join("SKILL.md").exists() {
                return Some(cand);
            }
        }
    }
    None
}

/// Scan a skill's text files for which of `candidates` (env-var names) appear as
/// whole tokens, so the `metadata.required-env` declaration can be auto-detected
/// from the secrets the scripts actually reference. Skips symlinks, IGNORED_DIRS,
/// oversized files, and binaries. Returns the matched names, sorted.
pub fn scan_for_env_vars(root: &Path, candidates: &[String]) -> Vec<String> {
    let mut found: BTreeSet<String> = BTreeSet::new();
    scan_dir(root, candidates, &mut found);
    found.into_iter().collect()
}

fn scan_dir(dir: &Path, candidates: &[String], found: &mut BTreeSet<String>) {
    if candidates.iter().all(|c| found.contains(c)) {
        return; // nothing left to look for
    }
    let rd = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return,
    };
    for entry in rd.filter_map(|e| e.ok()) {
        let name = entry.file_name().to_string_lossy().into_owned();
        let ft = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if ft.is_symlink() {
            continue;
        }
        let abs = entry.path();
        if ft.is_dir() {
            if IGNORED_DIRS.contains(&name.as_str()) {
                continue;
            }
            scan_dir(&abs, candidates, found);
        } else if ft.is_file() {
            let meta = match std::fs::metadata(&abs) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if meta.len() > MAX_TEXT_BYTES {
                continue;
            }
            let bytes = match std::fs::read(&abs) {
                Ok(b) => b,
                Err(_) => continue,
            };
            if bytes.contains(&0u8) {
                continue; // binary
            }
            let text = String::from_utf8_lossy(&bytes);
            for c in candidates {
                if !found.contains(c) && contains_token(&text, c) {
                    found.insert(c.clone());
                }
            }
        }
    }
}

/// True if `needle` occurs in `hay` not flanked by another identifier char, so
/// `OPENAI_API_KEY` matches `$OPENAI_API_KEY` but not `MY_OPENAI_API_KEY_2`.
fn contains_token(hay: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let bytes = hay.as_bytes();
    for (pos, _) in hay.match_indices(needle) {
        let before_ok = pos == 0 || !is_ident_byte(bytes[pos - 1]);
        let after = pos + needle.len();
        let after_ok = after >= bytes.len() || !is_ident_byte(bytes[after]);
        if before_ok && after_ok {
            return true;
        }
    }
    false
}

fn is_ident_byte(b: u8) -> bool {
    b == b'_' || b.is_ascii_alphanumeric()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_boundary_rejects_substrings() {
        assert!(contains_token("export OPENAI_API_KEY=x", "OPENAI_API_KEY"));
        assert!(contains_token("\"OPENAI_API_KEY\"", "OPENAI_API_KEY"));
        assert!(contains_token("os.environ['GITHUB_TOKEN']", "GITHUB_TOKEN"));
        assert!(!contains_token("MY_OPENAI_API_KEY_2", "OPENAI_API_KEY"));
        assert!(!contains_token("OPENAI_API_KEYS", "OPENAI_API_KEY"));
    }

    #[test]
    fn scan_detects_referenced_keys_only() {
        let base = std::env::temp_dir().join(format!("ass_scan_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("scripts")).unwrap();
        std::fs::write(base.join("SKILL.md"), "Reads OPENAI_API_KEY at runtime.").unwrap();
        std::fs::write(base.join("scripts/run.sh"), "#!/bin/sh\necho \"$GITHUB_TOKEN\"\n").unwrap();
        // A near-miss substring must NOT trigger UNUSED_KEY's cousin.
        std::fs::write(base.join("notes.txt"), "see MY_OPENAI_API_KEY_2 elsewhere").unwrap();

        let candidates = vec![
            "OPENAI_API_KEY".to_string(),
            "GITHUB_TOKEN".to_string(),
            "UNUSED_KEY".to_string(),
        ];
        let found = scan_for_env_vars(&base, &candidates);
        assert_eq!(found, vec!["GITHUB_TOKEN".to_string(), "OPENAI_API_KEY".to_string()]);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn list_dir_files_gated_and_flagged() {
        let base = std::env::temp_dir().join(format!("ass_listdir_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("a-skill")).unwrap();
        std::fs::write(base.join("a-skill/SKILL.md"), "x").unwrap();
        std::fs::create_dir_all(base.join("plain-dir")).unwrap();
        std::fs::write(base.join("notes.md"), "# hi").unwrap();
        std::fs::write(base.join("data.txt"), "x").unwrap();

        // Dirs-only (the skill-folder picker): no files surface, regardless of ext.
        let dirs_only = list_dir_impl(&base.to_string_lossy(), false).unwrap();
        assert!(dirs_only.entries.iter().all(|e| e.is_dir));
        assert_eq!(dirs_only.entries.len(), 2);
        assert!(dirs_only.entries.iter().any(|e| e.name == "a-skill" && e.is_skill));

        // include_files: files appear, .md flagged is_markdown, dirs sort first.
        let with_files = list_dir_impl(&base.to_string_lossy(), true).unwrap();
        assert_eq!(with_files.entries.len(), 4);
        assert!(with_files.entries[0].is_dir && with_files.entries[1].is_dir, "dirs sort before files");
        let md = with_files.entries.iter().find(|e| e.name == "notes.md").unwrap();
        assert!(!md.is_dir && md.is_markdown);
        let txt = with_files.entries.iter().find(|e| e.name == "data.txt").unwrap();
        assert!(!txt.is_dir && !txt.is_markdown);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn write_asset_dedupes_and_sandboxes() {
        let base = std::env::temp_dir().join(format!("ass_asset_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let root = base.to_string_lossy().to_string();
        // 1x1 transparent PNG (base64). Real bytes so the write round-trips.
        let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

        // First write lands at the requested name, under the asset subdir.
        let r1 = write_asset_impl(&root, "assets", "pasted image.png", png).unwrap();
        assert_eq!(r1, "assets/pasted-image.png");
        assert!(base.join("assets/pasted-image.png").is_file());
        // A second write of the same name steps past it instead of clobbering.
        let r2 = write_asset_impl(&root, "assets", "pasted-image.png", png).unwrap();
        assert_eq!(r2, "assets/pasted-image-1.png");
        assert!(base.join("assets/pasted-image-1.png").is_file());
        // "." means the skill root (no subdir).
        let r3 = write_asset_impl(&root, ".", "logo.svg", png).unwrap();
        assert_eq!(r3, "logo.svg");

        // Traversal is rejected by the shared sandbox.
        assert!(write_asset_impl(&root, "../escape", "x.png", png).is_err());
        // Empty / invalid payloads are rejected.
        assert!(write_asset_impl(&root, "assets", "x.png", "").is_err());

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn validate_gate_and_skill_extension() {
        let base = std::env::temp_dir().join(format!("ass_pkg_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let skill = base.join("my-skill");
        std::fs::create_dir_all(&skill).unwrap();
        let root = skill.to_string_lossy().to_string();

        // A valid head packages, and the artifact carries the `.skill` extension.
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: my-skill\ndescription: Does a thing.\n---\nBody\n",
        )
        .unwrap();
        let (filename, bytes) = zip_skill_bytes(&root, &[]).unwrap();
        assert_eq!(filename, "my-skill.skill");
        assert!(!bytes.is_empty());

        // Each malformed head is refused before any bytes are written.
        for (md, needle) in [
            ("---\nname: My_Skill\ndescription: ok\n---\n", "kebab"),
            ("---\nname: my-skill\ndescription: a <b>\n---\n", "angle"),
            ("---\nname: my-skill\ndescription: ok\nversion: 1\n---\n", "unknown frontmatter key"),
            ("---\ndescription: no name\n---\n", "missing a `name`"),
            ("no frontmatter here\n", "frontmatter"),
        ] {
            std::fs::write(skill.join("SKILL.md"), md).unwrap();
            let err = zip_skill_bytes(&root, &[]).unwrap_err().to_lowercase();
            assert!(err.contains(needle), "want `{needle}` in `{err}`");
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn write_file_compare_and_swap() {
        let base = std::env::temp_dir().join(format!("ass_cas_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let root = base.to_string_lossy().to_string();
        std::fs::write(base.join("note.md"), "hello").unwrap();

        // Read hands back the tag the editor will echo on write.
        let view = read_file_impl(&root, "note.md").unwrap();
        let etag = view.etag.clone().expect("text file gets an etag");
        assert_eq!(view.content.as_deref(), Some("hello"));

        // A CAS write with the matching tag lands and returns the new tag.
        match write_file_impl(&root, "note.md", "hello world", Some(&etag)).unwrap() {
            WriteOutcome::Written { etag: new } => assert_ne!(new, etag),
            WriteOutcome::Stale { .. } => panic!("matching tag should not be stale"),
        }
        assert_eq!(std::fs::read_to_string(base.join("note.md")).unwrap(), "hello world");

        // An external process edits the file behind our back...
        std::fs::write(base.join("note.md"), "EXTERNAL EDIT").unwrap();
        // ...so a CAS write with the now-stale tag is refused, not clobbered, and
        // returns the current disk bytes for the caller to reconcile.
        match write_file_impl(&root, "note.md", "my unsaved edits", Some(&etag)).unwrap() {
            WriteOutcome::Stale { disk_content, disk_etag } => {
                assert_eq!(disk_content, "EXTERNAL EDIT");
                assert!(!disk_etag.is_empty());
            }
            WriteOutcome::Written { .. } => panic!("stale tag must not overwrite a newer disk version"),
        }
        assert_eq!(std::fs::read_to_string(base.join("note.md")).unwrap(), "EXTERNAL EDIT");

        // No expected tag = legacy unconditional overwrite (back-compat path).
        assert!(matches!(
            write_file_impl(&root, "note.md", "forced", None).unwrap(),
            WriteOutcome::Written { .. }
        ));
        assert_eq!(std::fs::read_to_string(base.join("note.md")).unwrap(), "forced");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn write_outcome_json_shape() {
        // The client reads these exact keys — lock the wire contract.
        let written = serde_json::to_value(WriteOutcome::Written { etag: "abc".into() }).unwrap();
        assert_eq!(written, serde_json::json!({ "status": "written", "etag": "abc" }));

        let stale = serde_json::to_value(WriteOutcome::Stale {
            disk_etag: "def".into(),
            disk_content: "on disk".into(),
        })
        .unwrap();
        assert_eq!(
            stale,
            serde_json::json!({ "status": "stale", "diskEtag": "def", "diskContent": "on disk" })
        );
    }
}
