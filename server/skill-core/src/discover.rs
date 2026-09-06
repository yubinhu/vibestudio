// Skill auto-discovery across agents' global/home canonical locations.
// A skill = a directory containing SKILL.md (nested layouts supported). Each
// discovered skill is classified by provenance:
//   "personal" — you authored/customized it (editable, version-controllable)
//   "official" — vendor-bundled (Codex .system, Cursor managed/built-in, Anthropic plugins)
//   "plugin"   — an installed third-party package (marketplaces / external / remote)
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::Serialize;
use walkdir::WalkDir;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredSkill {
    name: Option<String>,
    description: Option<String>,
    root: String,
    kind: String,
    /// Repo/folder name when this is a project-scoped skill (`<repo>/.claude/skills/…`);
    /// None for global/home skills.
    #[serde(skip_serializing_if = "Option::is_none")]
    project: Option<String>,
    /// A machine-generated draft staged in a `generated-skills/` folder (e.g. by
    /// the skill-miner). Still `kind: "personal"`, but the UI surfaces it as a
    /// proposal to accept (promote into the real skills home) or discard.
    proposed: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSkills {
    agent: String,
    skills: Vec<DiscoveredSkill>,
}

#[derive(serde::Deserialize, Default)]
struct Frontmatter {
    name: Option<String>,
    description: Option<String>,
}

fn is_ignored_dir(p: &Path) -> bool {
    p.file_name()
        .and_then(|n| n.to_str())
        .map(|n| matches!(n, ".git" | "node_modules" | ".venv" | ".next" | "__pycache__"))
        .unwrap_or(false)
}

/// Extract the leading `---` ... `---` YAML block (BOM-tolerant).
pub(crate) fn extract_frontmatter(raw: &str) -> Option<String> {
    let s = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    let mut lines = s.lines();
    match lines.next() {
        Some(first) if first.trim_end() == "---" => {}
        _ => return None,
    }
    let mut block = String::new();
    for line in lines {
        if line.trim_end() == "---" {
            return Some(block);
        }
        block.push_str(line);
        block.push('\n');
    }
    None
}

/// True when the skill is staged under a `generated-skills/` folder — i.e. its
/// immediate parent directory is named `generated-skills`. This is the convention
/// the skill-miner writes new drafts to (`<skills-home>/generated-skills/<name>/`),
/// and the level `promote_skill` moves a skill up out of when accepted.
fn is_proposed(skill_dir: &Path) -> bool {
    skill_dir
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        == Some("generated-skills")
}

fn read_meta(skill_md: &Path) -> (Option<String>, Option<String>) {
    let Ok(raw) = std::fs::read_to_string(skill_md) else {
        return (None, None);
    };
    let Some(block) = extract_frontmatter(&raw) else {
        return (None, None);
    };
    match serde_yaml::from_str::<Frontmatter>(&block) {
        Ok(f) => (f.name, f.description),
        Err(_) => (None, None),
    }
}

/// Walk `root`, classifying each discovered skill with `classify(skill_dir) -> kind`.
fn collect(
    root: &Path,
    classify: &dyn Fn(&Path) -> &'static str,
    skills: &mut Vec<DiscoveredSkill>,
    seen: &mut HashSet<PathBuf>,
) {
    if !root.exists() {
        return;
    }
    for entry in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| !is_ignored_dir(e.path()))
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() || entry.file_name() != "SKILL.md" {
            continue;
        }
        let Some(skill_dir) = entry.path().parent().map(|p| p.to_path_buf()) else {
            continue;
        };
        let canon = std::fs::canonicalize(&skill_dir).unwrap_or_else(|_| skill_dir.clone());
        if !seen.insert(canon) {
            continue; // already found via another root
        }
        let (name, description) = read_meta(entry.path());
        let kind = classify(&skill_dir);
        let proposed = is_proposed(&skill_dir);
        skills.push(DiscoveredSkill {
            name,
            description,
            root: skill_dir.to_string_lossy().into_owned(),
            kind: kind.to_string(),
            project: None,
            proposed,
        });
    }
}

// --- per-agent classifiers ---------------------------------------------

/// Claude Code: split Anthropic's official-marketplace `plugins/` from everything
/// else under the plugin trees. `~/.claude/plugins/marketplaces/<m>/(plugins|
/// external_plugins)/<plugin>/skills/<skill>` — official iff <m> is the official
/// marketplace and the skill sits in its `plugins/` (not `external_plugins/`).
fn claude_plugin_kind(skill_dir: &Path) -> &'static str {
    let s = skill_dir.to_string_lossy().replace('\\', "/");
    if let Some(idx) = s.find("/marketplaces/") {
        let rest = &s[idx + "/marketplaces/".len()..];
        let mut parts = rest.splitn(2, '/');
        let marketplace = parts.next().unwrap_or("");
        let after = parts.next().unwrap_or("");
        if after.starts_with("external_plugins/") {
            return "plugin";
        }
        if marketplace.contains("official") && after.starts_with("plugins/") {
            return "official";
        }
    }
    "plugin"
}

/// Codex: bundled skills live under the hidden `.system/` subdir; user skills are
/// direct children of `~/.codex/skills/`.
fn codex_kind(skill_dir: &Path) -> &'static str {
    if skill_dir.components().any(|c| c.as_os_str() == ".system") {
        "official"
    } else {
        "personal"
    }
}

#[derive(Default)]
struct Groups {
    claude: Vec<DiscoveredSkill>,
    codex: Vec<DiscoveredSkill>,
    cursor: Vec<DiscoveredSkill>,
    openclaw: Vec<DiscoveredSkill>,
    opencode: Vec<DiscoveredSkill>,
    shared: Vec<DiscoveredSkill>,
}

fn push_to_agent(g: &mut Groups, agent: &str, skill: DiscoveredSkill) {
    match agent {
        "Claude Code" => g.claude.push(skill),
        "Codex" => g.codex.push(skill),
        "Cursor" => g.cursor.push(skill),
        "OpenClaw" => g.openclaw.push(skill),
        "opencode" => g.opencode.push(skill),
        "Agent Skills" => g.shared.push(skill),
        _ => {}
    }
}

// --- project-scoped discovery ------------------------------------------
// A project skill lives in a repo at `<repo>/<marker>/skills/<name>/SKILL.md`,
// where <marker> is an agent's project dotdir. We walk the home tree, pruning the
// usual build/dependency dirs (and every non-marker dotdir), to find them.

const PROJECT_MARKERS: [(&str, &str); 6] = [
    (".claude", "Claude Code"),
    (".cursor", "Cursor"),
    (".codex", "Codex"),
    (".opencode", "opencode"),
    // Cross-agent shared standard — surfaced under "Agent Skills" to match the
    // home-level `~/.agents`/`~/.agent` scan, not under any single agent.
    (".agents", "Agent Skills"),
    (".agent", "Agent Skills"), // singular variant (e.g. Antigravity)
];

// Non-hidden heavyweight dirs to never descend into (hidden dirs are pruned
// wholesale below, except the markers).
const PROJECT_PRUNE: &[&str] = &[
    "node_modules", "target", "dist", "build", "out", "vendor", "Pods", "coverage",
    "venv", "__pycache__", "site-packages", "Library", "AppData",
];

// Top-level home folders that pop a macOS privacy prompt the first time they're
// read AND never legitimately hold skills: Downloads (TCC-gated) and the media
// library dirs (Music/Movies/Pictures). Pruned ONLY as a direct child of `home`.
// Desktop and Documents are deliberately excluded — developers do keep repos
// there, so we keep scanning them and accept the one justified TCC prompt.
const HOME_TCC_DIRS: &[&str] = &["Downloads", "Music", "Movies", "Pictures"];

fn is_marker(name: &str) -> bool {
    PROJECT_MARKERS.iter().any(|(m, _)| *m == name)
}

/// True if the walker should NOT descend into `path`.
fn prune_project_dir(path: &Path, home: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    if PROJECT_PRUNE.contains(&name) {
        return true;
    }
    // Go module cache (~/go/pkg/mod) — third-party deps, not your projects.
    if name == "pkg"
        && path.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str()) == Some("go")
    {
        return true;
    }
    // Skip non-marker hidden dirs (.git, .cache, .config, .github, .vscode, …).
    if name.starts_with('.') && !is_marker(name) {
        return true;
    }
    // The home-level agent dotdirs are global; they're scanned separately.
    if is_marker(name) && path.parent() == Some(home) {
        return true;
    }
    // macOS TCC-protected / media-library top-level home dirs — skills never
    // live there, and descending into them triggers privacy prompts on first launch.
    if path.parent() == Some(home) && HOME_TCC_DIRS.contains(&name) {
        return true;
    }
    false
}

/// From a skill dir, find the nearest enclosing `<repo>/<marker>/skills/…` and
/// return (agent, repo-name); None if it isn't under a project marker.
fn project_attribution(skill_dir: &Path) -> Option<(&'static str, String)> {
    let mut cur = skill_dir;
    while let Some(parent) = cur.parent() {
        if parent.file_name().and_then(|n| n.to_str()) == Some("skills") {
            if let Some(marker_dir) = parent.parent() {
                if let Some(marker) = marker_dir.file_name().and_then(|n| n.to_str()) {
                    if let Some((_, agent)) = PROJECT_MARKERS.iter().find(|(m, _)| *m == marker) {
                        let project = marker_dir
                            .parent()
                            .and_then(|r| r.file_name())
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "project".into());
                        return Some((agent, project));
                    }
                }
            }
        }
        cur = parent;
    }
    None
}

/// Walk `root` for project-scoped skills, bounded by an entry budget + wall-clock.
fn scan_projects(root: &Path, home: &Path, g: &mut Groups, seen: &mut HashSet<PathBuf>) {
    let start = Instant::now();
    let mut budget: i64 = 1_000_000;
    let walker = WalkDir::new(root)
        .follow_links(false)
        .max_depth(12)
        .into_iter()
        .filter_entry(|e| !prune_project_dir(e.path(), home));
    for entry in walker.filter_map(|e| e.ok()) {
        budget -= 1;
        if budget <= 0 || start.elapsed().as_secs() >= 6 {
            break;
        }
        if !entry.file_type().is_file() || entry.file_name() != "SKILL.md" {
            continue;
        }
        let Some(skill_dir) = entry.path().parent() else {
            continue;
        };
        let Some((agent, project)) = project_attribution(skill_dir) else {
            continue;
        };
        let canon = std::fs::canonicalize(skill_dir).unwrap_or_else(|_| skill_dir.to_path_buf());
        if !seen.insert(canon) {
            continue; // already found (e.g. as a global skill)
        }
        let (name, description) = read_meta(entry.path());
        let proposed = is_proposed(skill_dir);
        push_to_agent(
            g,
            agent,
            DiscoveredSkill {
                name,
                description,
                root: skill_dir.to_string_lossy().into_owned(),
                kind: "personal".into(),
                project: Some(project),
                proposed,
            },
        );
    }
}

/// Discover skills across the per-agent global/home canonical dirs, plus
/// project-scoped skills in repos under the home directory.
pub fn discover_all() -> Result<Vec<AgentSkills>, String> {
    let home = dirs::home_dir().ok_or_else(|| "No home directory.".to_string())?;
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut g = Groups::default();

    // Claude Code — personal skills, plugin trees, remote plugins.
    collect(&home.join(".claude/skills"), &|_: &Path| "personal", &mut g.claude, &mut seen);
    collect(&home.join(".claude/plugins"), &claude_plugin_kind, &mut g.claude, &mut seen);
    collect(&home.join(".claude/remote/plugins"), &|_: &Path| "plugin", &mut g.claude, &mut seen);

    // Codex — ~/.codex/skills, with .system/ being the bundled set.
    collect(&home.join(".codex/skills"), &codex_kind, &mut g.codex, &mut seen);

    // Cursor — everything in `skills-cursor/` is Cursor-provided (their own
    // .gitignore labels it "Built-in Cursor skills"); the user's own skills live
    // in the separate `~/.cursor/skills/`.
    collect(&home.join(".cursor/skills-cursor"), &|_: &Path| "official", &mut g.cursor, &mut seen);
    collect(&home.join(".cursor/skills"), &|_: &Path| "personal", &mut g.cursor, &mut seen);

    // OpenClaw — personal/local roots (bundled skills live in the read-only install dir).
    collect(&home.join(".openclaw/skills"), &|_: &Path| "personal", &mut g.openclaw, &mut seen);

    // opencode — its own global skills under ~/.config/opencode/skills (it also
    // reads the shared standard and ~/.claude/skills, collected under their groups).
    collect(&home.join(".config/opencode/skills"), &|_: &Path| "personal", &mut g.opencode, &mut seen);

    // Agent Skills standard shared dir — read by Codex, Cursor, Gemini CLI, opencode,
    // and the broader cohort. Its own group so a skill synced here isn't mislabeled as one
    // agent's. `.agent` (singular) is the minority variant (e.g. Antigravity).
    collect(&home.join(".agents/skills"), &|_: &Path| "personal", &mut g.shared, &mut seen);
    collect(&home.join(".agent/skills"), &|_: &Path| "personal", &mut g.shared, &mut seen);

    // Project-scoped skills in repos under the home directory.
    scan_projects(&home, &home, &mut g, &mut seen);

    // Shared standard dir first — it's the most broadly-read location.
    Ok(vec![
        AgentSkills { agent: "Agent Skills".into(), skills: g.shared },
        AgentSkills { agent: "Claude Code".into(), skills: g.claude },
        AgentSkills { agent: "Codex".into(), skills: g.codex },
        AgentSkills { agent: "Cursor".into(), skills: g.cursor },
        AgentSkills { agent: "OpenClaw".into(), skills: g.openclaw },
        AgentSkills { agent: "opencode".into(), skills: g.opencode },
    ])
}

/// Like [`discover_all`], but also ensures every eligible personal skill is
/// git-tracked (auto-init + baseline commit) — the side-effecting variant the
/// `/api/discover` route uses. Project-scoped skills (`project: Some`) are left
/// to their parent repo; proposals and non-personal kinds are skipped. The init
/// work runs off the request thread, so discovery stays snappy.
pub fn discover_and_autotrack() -> Result<Vec<AgentSkills>, String> {
    let groups = discover_all()?;
    let roots: Vec<String> = groups
        .iter()
        .flat_map(|g| g.skills.iter())
        .filter(|s| s.kind == "personal" && !s.proposed && s.project.is_none())
        .map(|s| s.root.clone())
        .collect();
    crate::gitops::auto_track_personal(roots);
    Ok(groups)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_frontmatter() {
        let raw = "---\nname: my-skill\ndescription: Does things\n---\n\n# Body\n";
        let block = extract_frontmatter(raw).expect("frontmatter present");
        let fm: Frontmatter = serde_yaml::from_str(&block).unwrap();
        assert_eq!(fm.name.as_deref(), Some("my-skill"));
        assert_eq!(fm.description.as_deref(), Some("Does things"));
    }

    #[test]
    fn no_frontmatter_returns_none() {
        assert!(extract_frontmatter("# Just a heading\n").is_none());
        assert!(extract_frontmatter("").is_none());
    }

    #[test]
    fn bom_tolerant() {
        let raw = "\u{feff}---\nname: x\n---\n";
        assert_eq!(extract_frontmatter(raw).as_deref(), Some("name: x\n"));
    }

    #[test]
    fn classifiers() {
        let p = |s: &str| PathBuf::from(s);
        assert_eq!(
            claude_plugin_kind(&p("/h/.claude/plugins/marketplaces/claude-plugins-official/plugins/code-review/skills/x")),
            "official"
        );
        assert_eq!(
            claude_plugin_kind(&p("/h/.claude/plugins/marketplaces/claude-plugins-official/external_plugins/github/skills/x")),
            "plugin"
        );
        assert_eq!(
            claude_plugin_kind(&p("/h/.claude/plugins/marketplaces/some-community/plugins/foo/skills/x")),
            "plugin"
        );
        assert_eq!(codex_kind(&p("/h/.codex/skills/.system/imagegen")), "official");
        assert_eq!(codex_kind(&p("/h/.codex/skills/my-skill")), "personal");
    }

    #[test]
    fn project_shapes() {
        let p = |s: &str| PathBuf::from(s);
        assert_eq!(
            project_attribution(&p("/home/u/work/Tesseract/.claude/skills/tesseract-debug")),
            Some(("Claude Code", "Tesseract".to_string()))
        );
        assert_eq!(
            project_attribution(&p("/home/u/work/app/.cursor/skills/group/my-skill")),
            Some(("Cursor", "app".to_string()))
        );
        assert_eq!(
            project_attribution(&p("/home/u/repo/.agents/skills/x")),
            Some(("Agent Skills", "repo".to_string()))
        );
        assert_eq!(
            project_attribution(&p("/home/u/work/Tesseract/.agent/skills/skill-miner")),
            Some(("Agent Skills", "Tesseract".to_string()))
        );
        assert_eq!(project_attribution(&p("/home/u/repo/src/skills/x")), None);
        // home-level dotdirs are pruned from the project walk
        let home = p("/home/u");
        assert!(prune_project_dir(&p("/home/u/.claude"), &home));
        assert!(prune_project_dir(&p("/home/u/.agent"), &home));
        assert!(prune_project_dir(&p("/home/u/proj/node_modules"), &home));
        assert!(prune_project_dir(&p("/home/u/proj/.git"), &home));
        // Privacy-prompting, never-skills home dirs are pruned at the home root…
        assert!(prune_project_dir(&p("/home/u/Downloads"), &home));
        assert!(prune_project_dir(&p("/home/u/Music"), &home));
        // …but Desktop/Documents stay scanned (developers keep repos there)…
        assert!(!prune_project_dir(&p("/home/u/Desktop"), &home));
        assert!(!prune_project_dir(&p("/home/u/Documents"), &home));
        // …and a same-named folder nested inside a repo is always scanned.
        assert!(!prune_project_dir(&p("/home/u/proj/Documents"), &home));
        assert!(!prune_project_dir(&p("/home/u/proj/.claude"), &home));
        assert!(!prune_project_dir(&p("/home/u/proj/.agent"), &home));
        assert!(!prune_project_dir(&p("/home/u/proj/src"), &home));
    }

    #[test]
    fn discovers_a_planted_skill() {
        let base = std::env::temp_dir().join(format!("ass_discover_{}", std::process::id()));
        let skill = base.join("nested").join("my-skill");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(skill.join("SKILL.md"), "---\nname: my-skill\ndescription: hi\n---\nbody").unwrap();

        let mut found = Vec::new();
        let mut seen = HashSet::new();
        collect(&base, &|_: &Path| "personal", &mut found, &mut seen);

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name.as_deref(), Some("my-skill"));
        assert_eq!(found[0].kind, "personal");
        assert!(!found[0].proposed, "a skill not under generated-skills/ isn't a proposal");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn flags_generated_skills_as_proposed() {
        let p = |s: &str| PathBuf::from(s);
        // A draft staged under generated-skills/ is a proposal…
        assert!(is_proposed(&p("/h/.agents/skills/generated-skills/my-draft")));
        // …but the real skill it's promoted to (and ordinary skills) are not.
        assert!(!is_proposed(&p("/h/.agents/skills/my-draft")));
        assert!(!is_proposed(&p("/h/.agents/skills/generated-skills")));

        // End-to-end: collect tags the staged skill proposed, the sibling not.
        let base = std::env::temp_dir().join(format!("ass_proposed_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let staged = base.join("generated-skills").join("fresh");
        let real = base.join("settled");
        std::fs::create_dir_all(&staged).unwrap();
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(staged.join("SKILL.md"), "---\nname: fresh\n---\nbody").unwrap();
        std::fs::write(real.join("SKILL.md"), "---\nname: settled\n---\nbody").unwrap();

        let mut found = Vec::new();
        let mut seen = HashSet::new();
        collect(&base, &|_: &Path| "personal", &mut found, &mut seen);
        let by_name = |n: &str| found.iter().find(|s| s.name.as_deref() == Some(n)).unwrap();
        assert!(by_name("fresh").proposed);
        assert!(!by_name("settled").proposed);
        let _ = std::fs::remove_dir_all(&base);
    }

    // Real discovery against this machine; run with:
    // cargo test -p skill-core -- --nocapture live_discovery_smoke
    #[test]
    fn live_discovery_smoke() {
        let groups = discover_all().expect("discovery should not error");
        assert_eq!(groups.len(), 6, "one group per agent + the shared Agent Skills dir");
        let total: usize = groups.iter().map(|g| g.skills.len()).sum();
        println!("\n=== live discovery: {total} skill(s) across {} agents ===", groups.len());
        for g in &groups {
            println!("  {} ({})", g.agent, g.skills.len());
            for s in &g.skills {
                println!(
                    "    - {}  [{}]{}  {}",
                    s.name.as_deref().unwrap_or("(no name)"),
                    s.kind,
                    s.project.as_deref().map(|p| format!("  (project: {p})")).unwrap_or_default(),
                    s.root
                );
            }
        }
    }
}
