// Secret manager: a machine-local store of environment variables that skills
// load at runtime via the bundled `load-secrets` activation skill. Values live
// in a JSON store (the source of truth the UI edits) and are rendered to a
// shell-sourceable env file (`export KEY=VALUE`) that `activate.sh` reads.
//
// Storage is a 0600 file, not an OS keychain: WSL (the primary target) has no
// Secret Service, and the rendered env file must be plaintext for shells to
// source it anyway. A keyring backend can slot in behind this interface later
// for native desktop without changing callers.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::sync::install_skill;

const BOOTSTRAP_SKILL: &str = "load-secrets";

/// Former names of the activation skill. Installed copies under an old name are
/// removed wherever the current one gets installed, so they can't linger (or
/// surface in the UI as the user's own "personal" skills).
const LEGACY_SKILL_NAMES: [&str; 1] = ["skill-studio"];

/// An agent and the skills dirs (relative to home) it reads. Cohort agents list
/// the shared `.agents/skills` standard dir first, so one copy there reaches all
/// of them; Claude Code and OpenClaw read only their own folders.
struct Agent {
    name: &'static str,
    /// Existence of this home dotdir = the agent is installed on this machine.
    home_dotdir: &'static str,
    /// Dirs (relative to home) where the activation skill would be reachable.
    skill_dirs: &'static [&'static str],
}

const AGENTS: [Agent; 6] = [
    Agent { name: "Claude Code", home_dotdir: ".claude", skill_dirs: &[".claude/skills"] },
    Agent { name: "Codex", home_dotdir: ".codex", skill_dirs: &[".agents/skills", ".codex/skills"] },
    Agent { name: "Cursor", home_dotdir: ".cursor", skill_dirs: &[".agents/skills", ".cursor/skills"] },
    Agent { name: "Gemini CLI", home_dotdir: ".gemini", skill_dirs: &[".agents/skills", ".gemini/skills"] },
    Agent { name: "OpenClaw", home_dotdir: ".openclaw", skill_dirs: &[".openclaw/skills"] },
    // opencode's config home is ~/.config/opencode; it reads the shared standard
    // (one copy there covers it) plus its own ~/.config/opencode/skills.
    Agent { name: "opencode", home_dotdir: ".config/opencode", skill_dirs: &[".agents/skills", ".config/opencode/skills"] },
];

/// Canonical locations bundled skills are installed into, each gated by the
/// presence of any "trigger" home dotdir. The shared `.agents/skills` dir covers
/// the whole standard cohort in a single copy; Claude Code and OpenClaw read
/// only their own folders. Shared with `mining` so skill-miner installs to the
/// same set of places as the activation skill.
pub(crate) struct InstallDest {
    pub(crate) skills_rel: &'static str,
    pub(crate) triggers: &'static [&'static str],
}

pub(crate) const INSTALL_DESTS: [InstallDest; 3] = [
    InstallDest { skills_rel: ".agents/skills", triggers: &[".agents", ".codex", ".cursor", ".gemini", ".config/opencode"] },
    InstallDest { skills_rel: ".claude/skills", triggers: &[".claude"] },
    InstallDest { skills_rel: ".openclaw/skills", triggers: &[".openclaw"] },
];

/// Per-agent legacy dirs now superseded by `.agents/skills`. A stale activation
/// skill here is removed once the shared copy is in place, so agents that read
/// both don't see it twice (and an older copy can't linger with outdated content).
const LEGACY_SUPERSEDED: [&str; 2] = [".codex/skills", ".cursor/skills"];

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretEntry {
    key: String,
    value: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentInstall {
    agent: String,
    /// The agent's home dir exists on this machine.
    installed: bool,
    /// The `load-secrets` activation skill is present for this agent.
    has_skill: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretsStatus {
    configured: bool,
    store_path: String,
    env_path: String,
    count: usize,
    agents: Vec<AgentInstall>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupResult {
    env_path: String,
    store_path: String,
    installed_agents: Vec<String>,
    skill_installed: bool,
}

pub(crate) fn config_dir() -> Result<PathBuf, String> {
    // One resolver for the whole app (incl. the pre-rebrand → vibestudio migration).
    crate::paths::config_dir()
}

fn store_path() -> Result<PathBuf, String> {
    Ok(config_dir()?.join("secrets.json"))
}
/// Path of the rendered shell-sourceable env file. Public because the terminal
/// launcher (`skill-term`) sources it when spinning up an agent session, so
/// managed secrets are present in the agent's environment from the start.
pub fn env_path() -> Result<PathBuf, String> {
    Ok(config_dir()?.join("env"))
}

#[cfg(unix)]
pub(crate) fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
}
#[cfg(not(unix))]
pub(crate) fn set_mode(_path: &Path, _mode: u32) {}

pub(crate) fn ensure_dir() -> Result<PathBuf, String> {
    let dir = config_dir()?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    set_mode(&dir, 0o700);
    Ok(dir)
}

fn load_store() -> Result<BTreeMap<String, String>, String> {
    match std::fs::read(store_path()?) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| format!("Corrupt secrets store: {e}")),
        Err(_) => Ok(BTreeMap::new()),
    }
}

fn save_store(map: &BTreeMap<String, String>) -> Result<(), String> {
    ensure_dir()?;
    let path = store_path()?;
    let json = serde_json::to_vec_pretty(map).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| e.to_string())?;
    set_mode(&path, 0o600);
    render_env(map)
}

/// Single-quote a value for a POSIX shell, escaping embedded single quotes.
pub(crate) fn sh_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

fn render_env(map: &BTreeMap<String, String>) -> Result<(), String> {
    ensure_dir()?;
    let path = env_path()?;
    let mut body = String::new();
    if !map.is_empty() {
        body.push_str("# Rendered by VibeStudio — do not edit by hand.\n");
        for (k, val) in map {
            body.push_str(&format!("export {k}={}\n", sh_quote(val)));
        }
    }
    std::fs::write(&path, body).map_err(|e| e.to_string())?;
    set_mode(&path, 0o600);
    Ok(())
}

/// A valid environment-variable name: leading letter/underscore, then word chars.
fn valid_key(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        Some(c) if c == '_' || c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

pub fn secrets_list() -> Result<Vec<SecretEntry>, String> {
    Ok(load_store()?
        .into_iter()
        .map(|(key, value)| SecretEntry { key, value })
        .collect())
}

/// Names of all stored secrets — the candidate set for scanning a skill's files
/// to auto-detect which env vars it references.
pub fn secret_keys() -> Result<Vec<String>, String> {
    Ok(load_store()?.into_keys().collect())
}

/// Quote a value for a portable `.env` file (consumed by dotenv loaders, not
/// `source`). Single-quoted dotenv values are literal — no interpolation, no
/// escapes — so use them for the common case (API tokens) to dodge `$`/escape
/// surprises. Single quotes can't be escaped inside single-quoted dotenv values,
/// so fall back to double quotes (which loaders unescape) when the value
/// contains a `'`, newline, or carriage return.
fn dotenv_quote(s: &str) -> String {
    if !s.contains('\'') && !s.contains('\n') && !s.contains('\r') {
        return format!("'{s}'");
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Render the requested env-var names that exist in the store as a dotenv file
/// body (`NAME='value'`). Names not in the store are skipped; empty if none are
/// present. Used to optionally bundle live secrets into an exported skill .zip.
pub fn render_dotenv(names: &[String]) -> Result<String, String> {
    let store = load_store()?;
    let mut body = String::new();
    for name in names {
        if let Some(val) = store.get(name) {
            if body.is_empty() {
                body.push_str("# Secrets bundled by VibeStudio. Keep this file private.\n");
            }
            body.push_str(&format!("{name}={}\n", dotenv_quote(val)));
        }
    }
    Ok(body)
}

/// Parse a `.env` body into (key, value) pairs — the inverse of [`dotenv_quote`],
/// tolerant of `export ` prefixes, `#` comments, blank lines, and single/double
/// quoted values. Entries whose key isn't a valid env-var name are skipped. Used
/// by skill import to offer a bundled `.env` to the secret store instead of writing
/// it into the imported folder.
pub fn parse_dotenv(body: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
        let Some((key, raw)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if !valid_key(key) {
            continue;
        }
        out.push((key.to_string(), unquote_dotenv(raw.trim())));
    }
    out
}

/// Strip matching single/double quotes from a dotenv value; double-quoted values
/// have their `\n \r \" \\` escapes resolved (matching [`dotenv_quote`]).
fn unquote_dotenv(s: &str) -> String {
    let bytes = s.as_bytes();
    let n = bytes.len();
    if n >= 2 && bytes[0] == b'\'' && bytes[n - 1] == b'\'' {
        return s[1..n - 1].to_string();
    }
    if n >= 2 && bytes[0] == b'"' && bytes[n - 1] == b'"' {
        let mut out = String::with_capacity(n - 2);
        let mut chars = s[1..n - 1].chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some('n') => out.push('\n'),
                    Some('r') => out.push('\r'),
                    Some('"') => out.push('"'),
                    Some('\\') => out.push('\\'),
                    Some(other) => {
                        out.push('\\');
                        out.push(other);
                    }
                    None => out.push('\\'),
                }
            } else {
                out.push(c);
            }
        }
        return out;
    }
    s.to_string()
}

/// A `.env` pair offered to the store (from a skill import or a pasted/uploaded
/// `.env` file), flagged when a secret of that name already exists (loading
/// would overwrite it).
#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DotenvEntry {
    pub key: String,
    pub value: String,
    pub exists: bool,
}

/// Parse a `.env` body into store-ready entries, flagging keys that already
/// exist. The preview half of ".env import" — the caller applies the chosen
/// entries via [`secret_set`]. The `exists` flags are advisory, so an
/// unreadable store degrades to "no flags" rather than failing the parse.
pub fn preview_dotenv(body: &str) -> Vec<DotenvEntry> {
    let existing: std::collections::HashSet<String> =
        secret_keys().unwrap_or_default().into_iter().collect();
    parse_dotenv(body)
        .into_iter()
        .map(|(key, value)| DotenvEntry { exists: existing.contains(&key), key, value })
        .collect()
}

pub fn secret_set(key: &str, value: &str) -> Result<(), String> {
    let key = key.trim();
    if !valid_key(key) {
        return Err(
            "Invalid name. Use letters, digits, and underscores, not starting with a digit (e.g. OPENAI_API_KEY)."
                .into(),
        );
    }
    let mut map = load_store()?;
    map.insert(key.to_string(), value.to_string());
    save_store(&map)
}

pub fn secret_delete(key: &str) -> Result<(), String> {
    let mut map = load_store()?;
    map.remove(key);
    save_store(&map)
}

fn home() -> Result<PathBuf, String> {
    dirs::home_dir().ok_or_else(|| "Cannot locate home directory.".to_string())
}

/// The activation skill is reachable by `a` if it's present in any dir it reads
/// (the shared `.agents/skills` standard dir, or the agent's own folder).
fn agent_has_skill(home: &Path, a: &Agent) -> bool {
    a.skill_dirs
        .iter()
        .any(|d| home.join(d).join(BOOTSTRAP_SKILL).join("SKILL.md").exists())
}

pub fn secrets_status() -> Result<SecretsStatus, String> {
    let store = store_path()?;
    let env = env_path()?;
    let home = home()?;
    let agents = AGENTS
        .iter()
        .map(|a| AgentInstall {
            agent: a.name.to_string(),
            installed: home.join(a.home_dotdir).exists(),
            has_skill: agent_has_skill(&home, a),
        })
        .collect();
    Ok(SecretsStatus {
        configured: store.exists(),
        count: load_store()?.len(),
        store_path: store.to_string_lossy().into_owned(),
        env_path: env.to_string_lossy().into_owned(),
        agents,
    })
}

/// Install the bundled `load-secrets` activation skill into the canonical shared
/// location (and the holdouts that don't read it), rather than duplicating it
/// into every agent's private dir. Consolidates onto `.agents/skills` and clears
/// stale per-agent copies. Returns the agents now covered.
pub fn install_bootstrap_skill(skill_src: &Path) -> Result<Vec<String>, String> {
    install_bootstrap_skill_in(&home()?, skill_src)
}

fn install_bootstrap_skill_in(home: &Path, skill_src: &Path) -> Result<Vec<String>, String> {
    if !skill_src.join("SKILL.md").exists() {
        return Err("Bundled load-secrets skill not found.".into());
    }
    // Install into each canonical location whose cohort is present on this machine.
    for dest in &INSTALL_DESTS {
        if !dest.triggers.iter().any(|t| home.join(t).exists()) {
            continue;
        }
        let skills_dir = home.join(dest.skills_rel);
        let target = skills_dir.join(BOOTSTRAP_SKILL);
        install_skill(skill_src, &target)?;
        // Clear copies installed under a former name in the same location.
        for old in LEGACY_SKILL_NAMES {
            let stale = skills_dir.join(old);
            if stale.exists() {
                let _ = std::fs::remove_dir_all(&stale);
            }
        }
    }
    // Once the shared copy is in place, drop superseded per-agent copies so the
    // cohort doesn't see the skill twice (and no stale older copy lingers).
    if home.join(".agents/skills").join(BOOTSTRAP_SKILL).join("SKILL.md").exists() {
        for legacy in LEGACY_SUPERSEDED {
            for name in std::iter::once(BOOTSTRAP_SKILL).chain(LEGACY_SKILL_NAMES) {
                let stale = home.join(legacy).join(name);
                if stale.exists() {
                    let _ = std::fs::remove_dir_all(&stale);
                }
            }
        }
    }
    Ok(AGENTS
        .iter()
        .filter(|a| home.join(a.home_dotdir).exists() && agent_has_skill(home, a))
        .map(|a| a.name.to_string())
        .collect())
}

/// First-run setup: materialize the store + env file and (re)install the
/// activation skill into every installed agent. `skill_src` is the bundled
/// `load-secrets` folder, resolved by the caller (Tauri resource / server path).
pub fn secrets_setup(skill_src: Option<&Path>) -> Result<SetupResult, String> {
    let map = load_store()?;
    save_store(&map)?; // creates the store + env file if absent
    let installed_agents = match skill_src {
        Some(src) if src.join("SKILL.md").exists() => install_bootstrap_skill(src)?,
        _ => Vec::new(),
    };
    Ok(SetupResult {
        env_path: env_path()?.to_string_lossy().into_owned(),
        store_path: store_path()?.to_string_lossy().into_owned(),
        skill_installed: !installed_agents.is_empty(),
        installed_agents,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_validated() {
        assert!(valid_key("OPENAI_API_KEY"));
        assert!(valid_key("_x"));
        assert!(!valid_key("1ABC"));
        assert!(!valid_key("a-b"));
        assert!(!valid_key(""));
    }

    #[test]
    fn quoting_is_shell_safe() {
        assert_eq!(sh_quote("abc"), "'abc'");
        assert_eq!(sh_quote("a b"), "'a b'");
        assert_eq!(sh_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn install_consolidates_to_shared_and_clears_legacy() {
        let base = std::env::temp_dir().join(format!("ass_secrets_install_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        // Agents present: Codex (cohort → shared dir) and Claude Code (holdout).
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        // A stale per-agent copy from an older install lives in ~/.codex/skills.
        let stale = home.join(".codex/skills/load-secrets");
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::write(stale.join("SKILL.md"), "old").unwrap();
        // A copy installed under the skill's former name lingers in a live dir.
        let renamed = home.join(".claude/skills/skill-studio");
        std::fs::create_dir_all(&renamed).unwrap();
        std::fs::write(renamed.join("SKILL.md"), "pre-rename").unwrap();
        // Bundled source skill.
        let src = base.join("src/load-secrets");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("SKILL.md"), "new").unwrap();
        std::fs::write(src.join("activate.sh"), "#!/usr/bin/env bash\n").unwrap();

        let covered = install_bootstrap_skill_in(&home, &src).unwrap();

        // Shared dir gets the copy; Claude Code gets its own; stale copies under
        // the current AND former names are cleared.
        assert!(home.join(".agents/skills/load-secrets/SKILL.md").exists());
        assert!(home.join(".claude/skills/load-secrets/SKILL.md").exists());
        assert!(!stale.exists(), "stale ~/.codex/skills copy must be cleared");
        assert!(!home.join(".codex/skills/load-secrets").exists());
        assert!(!renamed.exists(), "old-name skill-studio copy must be cleared");
        assert!(covered.iter().any(|a| a == "Codex"), "Codex covered via shared dir");
        assert!(covered.iter().any(|a| a == "Claude Code"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn dotenv_parse_inverts_render() {
        // render_dotenv → parse_dotenv round-trips both quoting styles.
        let single = "API='hello world'\n";
        let double = "TOK=\"it's\\n\"\n";
        let mut pairs = parse_dotenv(single);
        pairs.extend(parse_dotenv(double));
        assert_eq!(pairs[0], ("API".to_string(), "hello world".to_string()));
        assert_eq!(pairs[1], ("TOK".to_string(), "it's\n".to_string()));

        // Tolerates `export `, comments, blanks; skips invalid keys + junk lines.
        let messy = "# comment\n\nexport FOO=bar\n1BAD=x\nnokeyvalue\nBAZ='q'\n";
        let got = parse_dotenv(messy);
        assert_eq!(
            got,
            vec![("FOO".to_string(), "bar".to_string()), ("BAZ".to_string(), "q".to_string())]
        );
    }

    #[test]
    fn set_list_render_delete_roundtrip() {
        const PROBE: &str = "VIBESTUDIO_TEST_SECRET_STORE";
        if std::env::var_os(PROBE).is_none() {
            let dir = crate::state_store::tests::TempDir::new();
            let output = crate::process::hidden_command(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "secrets::tests::set_list_render_delete_roundtrip",
                    "--nocapture",
                ])
                .env(PROBE, "1")
                .env("XDG_CONFIG_HOME", &dir.0)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "Secret store probe failed:\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        secret_set("OPENAI_API_KEY", "sk-test'x").unwrap();
        secret_set("FOO", "bar baz").unwrap();
        assert_eq!(secrets_list().unwrap().len(), 2);

        let env = std::fs::read_to_string(env_path().unwrap()).unwrap();
        assert!(env.contains("export FOO='bar baz'"));
        assert!(env.contains(r"export OPENAI_API_KEY='sk-test'\''x'"));

        // secret_keys + render_dotenv: only present names, dotenv-quoted (no `export`).
        let keys = secret_keys().unwrap();
        assert!(keys.contains(&"FOO".to_string()) && keys.contains(&"OPENAI_API_KEY".to_string()));
        let dotenv = render_dotenv(&["FOO".to_string(), "MISSING".to_string()]).unwrap();
        assert!(dotenv.contains("FOO='bar baz'")); // plain value → single-quoted literal
        assert!(!dotenv.contains("export "));
        assert!(!dotenv.contains("MISSING"));
        assert!(render_dotenv(&["MISSING".to_string()]).unwrap().is_empty());
        // A value containing a single quote falls back to double-quoting (dotenv loaders unescape it).
        let dq = render_dotenv(&["OPENAI_API_KEY".to_string()]).unwrap();
        assert!(dq.contains("OPENAI_API_KEY=\"sk-test'x\""), "single-quote value double-quoted: {dq}");

        secret_delete("FOO").unwrap();
        assert_eq!(secrets_list().unwrap().len(), 1);
        assert!(secret_set("1bad", "x").is_err());

        // Empty store renders an empty env file (so activate.sh reports "none").
        secret_delete("OPENAI_API_KEY").unwrap();
        assert!(std::fs::read_to_string(env_path().unwrap()).unwrap().is_empty());
    }
}
