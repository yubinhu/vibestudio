//! The agent interface. Every agent CLI VibeStudio integrates with is one
//! [`AgentDef`] entry declaring the shared properties an integration needs:
//!
//! - **skills_dirs** — where the agent discovers skills (its own folders, plus
//!   the shared standard when `reads_shared`),
//! - **launch** — how to start it on a task: the interactive TUI command line
//!   with the initial prompt pre-submitted. An app-driven run is an ordinary
//!   agent session — same harness semantics, same approval prompts, same
//!   lifetime as if the user had typed the prompt themselves — so the caller
//!   must bring the user to its terminal, where any first-run dialog is
//!   answered. (The previous headless pipelines were dropped deliberately:
//!   print modes diverge from a real session — claude's `-p` ends the run the
//!   moment the agent ends its turn and kills its background tasks.)
//! - **resume** — how to reopen the terminal cwd's most recent conversation
//!   as the interactive TUI after the original terminal is gone.
//!
//! Features (mining, install, terminals) consult this registry instead of
//! matching on family names, so supporting a new agent = filling in one entry.
//! A `None` capability means the agent can't do that yet and the UI degrades
//! accordingly (e.g. it isn't offered for mining runs).

use serde_json::{json, Value};

use crate::secrets::sh_quote as q;

/// Home-relative dirs of the shared Agent Skills standard, read by every
/// `reads_shared` agent (Codex, Cursor, Gemini CLI, opencode, …; not Claude Code).
pub const SHARED_SKILLS_DIRS: &[&str] = &[".agents/skills", ".agent/skills"];

/// Context for building an interactive launch line: the agent's TUI in the
/// run's terminal (cwd = the run dir), with `prompt` submitted as the first
/// user message.
pub struct LaunchCtx<'a> {
    pub bin: &'a str,
    pub prompt: &'a str,
    /// Model / reasoning-effort overrides (None = the CLI's default).
    pub model: Option<&'a str>,
    pub effort: Option<&'a str>,
}

/// Context for building a resume line: reopen the most recent conversation in
/// the terminal's cwd (the run dir) as the interactive TUI, same tuning.
pub struct ResumeCtx<'a> {
    pub bin: &'a str,
    pub model: Option<&'a str>,
    pub effort: Option<&'a str>,
}

/// Extract a live terminal's title from the agent's own session store, given
/// `(cwd, terminal spawn time, forced session id)`. See [`crate::session_title`].
pub type SessionTitleFn = fn(&std::path::Path, i64, Option<&str>) -> Option<String>;

/// Extract the agent's last assistant message (a turn-finish notification preview)
/// from the same store, keyed the same way. See [`crate::session_title`].
pub type LastMessageFn = fn(&std::path::Path, i64, Option<&str>) -> Option<String>;

/// Binary discovery metadata shared by terminal and capability consumers.
#[derive(Clone, Copy)]
pub struct CliDef {
    pub path_name: &'static str,
    pub supports_ide: bool,
    pub ext_prefix: &'static str,
    pub ext_rel: ExtRel,
    pub cli_paths: &'static [&'static str],
    pub install_dir_env: &'static str,
}

#[derive(Clone, Copy)]
pub enum ExtRel {
    File(&'static str),
    GlobDir { dir: &'static str, file: &'static str },
}

const fn plain_cli(path_name: &'static str, cli_paths: &'static [&'static str]) -> CliDef {
    CliDef { path_name, supports_ide: false, ext_prefix: "", ext_rel: ExtRel::File(""), cli_paths, install_dir_env: "" }
}

pub struct AgentDef {
    /// Family id — the prefix of skill-term agent ids ("claude" in "claude:cli").
    pub family: &'static str,
    pub label: &'static str,
    pub cli: Option<CliDef>,
    /// Config home relative to the user home (also the installation presence marker).
    pub home_dir: &'static str,
    pub project_marker: Option<&'static str>,
    /// The agent's OWN skill-discovery dirs, home-relative.
    pub skills_dirs: &'static [&'static str],
    /// Whether the agent also reads [`SHARED_SKILLS_DIRS`].
    pub reads_shared: bool,
    pub launch: Option<fn(&LaunchCtx) -> String>,
    pub resume: Option<fn(&ResumeCtx) -> String>,
    /// How to point the agent at a remote MCP server through VibeStudio's
    /// loopback gateway (None = the agent can't consume a remote HTTP MCP). See
    /// [`McpWiring`]. The agent is handed only the gateway URL — VibeStudio
    /// holds the OAuth token — so no header/secret ever appears in agent config.
    pub mcp: Option<McpWiring>,
    /// Passive, metadata-only MCP inventory. Never starts the configured server.
    pub connector_discovery: Option<crate::connectors::ConnectorAdapter>,
    /// Optional inventory through the agent's own authenticated runtime.
    pub connector_runtime: Option<ConnectorRuntime>,
    /// Extract a short human title for a live terminal from the agent's own
    /// session record: `(cwd, terminal spawn time, forced session id)`. The
    /// session id (from `claude --session-id` at launch) gives an exact transcript
    /// match; `None`/empty → correlate by cwd + newest activity. `None` here = the
    /// agent has no readable session store yet; the UI falls back to the cwd. See
    /// [`crate::session_title`].
    pub session_title: Option<SessionTitleFn>,
    /// The agent's last assistant message, for the turn-finish notification body —
    /// read from the same store as `session_title` (structured message text, not a
    /// TUI screen-scrape). `None` = no reader yet; the notifier falls back to its
    /// fixed summons. See [`crate::session_title::claude_last_message`].
    pub last_message: Option<LastMessageFn>,
    /// Bundled Herdr detector label for this agent's interactive TUI. None
    /// retains legacy terminal-bell notifications for unsupported agents.
    pub attention_detector: Option<&'static str>,
}

#[derive(Clone, Copy)]
pub enum ConnectorRuntime {
    Claude,
    Codex,
}

/// The two shapes of "add/remove a remote streamable-HTTP MCP server named
/// `<name>` at `<url>`" across the cohort: a first-class `<bin> mcp …` CLI, or a
/// JSON config file we merge into. All URLs point at `/gw/<id>/mcp` on loopback;
/// no token or headers are ever written (the gateway injects auth upstream).
#[derive(Clone, Copy)]
pub enum McpWiring {
    /// A `<bin> mcp add|remove` CLI. The fns build the argv AFTER the resolved
    /// binary; the caller resolves the binary and runs it (add re-adds
    /// idempotently, so a pre-remove makes reconnects safe).
    Cli {
        /// Binary name to resolve (PATH + the usual agent bin dirs).
        bin: &'static str,
        add: fn(name: &str, url: &str) -> Vec<String>,
        remove: fn(name: &str) -> Vec<String>,
    },
    /// A JSON config file (home-relative `path`) with a servers map at
    /// `servers_key`, keyed by server name. The caller MERGES `entry(url)` in,
    /// preserving every other key, and only acts when `present_dir` exists (our
    /// "the agent is installed" signal, mirroring the load-secrets cohort).
    JsonFile {
        present_dir: &'static str,
        path: &'static str,
        servers_key: &'static str,
        entry: fn(url: &str) -> Value,
    },
}

pub const AGENTS: &[AgentDef] = &[
    AgentDef {
        family: "claude",
        label: "Claude Code",
        cli: Some(CliDef { path_name: "claude", supports_ide: true, ext_prefix: "anthropic.claude-code-", ext_rel: ExtRel::File("resources/native-binary/claude"), cli_paths: &["~/.local/bin/claude", "~/.claude/local/claude"], install_dir_env: "" }),
        home_dir: ".claude",
        project_marker: Some(".claude"),
        skills_dirs: &[".claude/skills"],
        reads_shared: false,
        launch: Some(claude_launch),
        resume: Some(claude_resume),
        mcp: Some(McpWiring::Cli { bin: "claude", add: claude_mcp_add, remove: claude_mcp_remove }),
        connector_discovery: Some(crate::connectors::ConnectorAdapter::ClaudeCode),
        connector_runtime: Some(ConnectorRuntime::Claude),
        session_title: Some(crate::session_title::claude_title),
        last_message: Some(crate::session_title::claude_last_message),
        attention_detector: Some("claude"),
    },
    AgentDef {
        family: "codex",
        label: "Codex",
        cli: Some(CliDef { path_name: "codex", supports_ide: false, ext_prefix: "openai.chatgpt-", ext_rel: ExtRel::GlobDir { dir: "bin", file: "codex" }, cli_paths: &["~/.codex/packages/standalone/current/codex"], install_dir_env: "CODEX_INSTALL_DIR" }),
        home_dir: ".codex",
        project_marker: Some(".codex"),
        skills_dirs: &[".codex/skills"],
        reads_shared: true,
        launch: Some(codex_launch),
        resume: Some(codex_resume),
        mcp: Some(McpWiring::Cli { bin: "codex", add: codex_mcp_add, remove: codex_mcp_remove }),
        connector_discovery: Some(crate::connectors::ConnectorAdapter::Codex),
        connector_runtime: Some(ConnectorRuntime::Codex),
        session_title: Some(crate::session_title::codex_title),
        last_message: None,
        attention_detector: Some("codex"),
    },
    AgentDef {
        family: "cursor",
        label: "Cursor",
        cli: None,
        home_dir: ".cursor",
        project_marker: Some(".cursor"),
        skills_dirs: &[".cursor/skills", ".cursor/skills-cursor"],
        reads_shared: true,
        launch: Some(cursor_launch),
        // `cursor-agent resume` targets the GLOBAL latest session, not the
        // cwd's — wiring it could reopen an unrelated conversation.
        resume: None,
        // No `mcp add` CLI; both the IDE and cursor-agent read ~/.cursor/mcp.json.
        mcp: Some(McpWiring::JsonFile {
            present_dir: ".cursor",
            path: ".cursor/mcp.json",
            servers_key: "mcpServers",
            entry: cursor_mcp_entry,
        }),
        connector_discovery: Some(crate::connectors::ConnectorAdapter::Json {
            user: ".cursor/mcp.json", project: ".cursor/mcp.json", key: "mcpServers",
            clients: &["IDE", "CLI"],
        }),
        connector_runtime: None,
        session_title: Some(crate::session_title::cursor_title),
        last_message: None,
        attention_detector: Some("cursor"),
    },
    AgentDef {
        family: "gemini",
        label: "Gemini CLI",
        cli: None,
        home_dir: ".gemini",
        project_marker: None,
        skills_dirs: &[],
        reads_shared: true,
        launch: Some(gemini_launch),
        resume: Some(gemini_resume),
        mcp: Some(McpWiring::Cli { bin: "gemini", add: gemini_mcp_add, remove: gemini_mcp_remove }),
        connector_discovery: Some(crate::connectors::ConnectorAdapter::Json {
            user: ".gemini/settings.json", project: ".gemini/settings.json", key: "mcpServers",
            clients: &["CLI"],
        }),
        connector_runtime: None,
        session_title: Some(crate::session_title::gemini_title),
        last_message: None,
        attention_detector: Some("gemini"),
    },
    AgentDef {
        family: "openclaw",
        label: "OpenClaw",
        cli: None,
        home_dir: ".openclaw",
        project_marker: None,
        skills_dirs: &[".openclaw/skills"],
        reads_shared: false,
        launch: None,
        resume: None,
        mcp: None,
        connector_discovery: None,
        connector_runtime: None,
        // Not launchable here, and its on-disk transcript format is unverified (no
        // install to test against) — wire this once there's a real store to read.
        session_title: None,
        last_message: None,
        attention_detector: None,
    },
    AgentDef {
        // opencode keeps its own global skills under ~/.config/opencode/skills and
        // also reads the shared standard (and ~/.claude/skills, covered by Claude's
        // own entry).
        family: "opencode",
        label: "opencode",
        cli: Some(CliDef { install_dir_env: "OPENCODE_INSTALL_DIR", ..plain_cli("opencode", &["~/.opencode/bin/opencode"]) }),
        home_dir: ".config/opencode",
        project_marker: Some(".opencode"),
        skills_dirs: &[".config/opencode/skills"],
        reads_shared: true,
        launch: Some(opencode_launch),
        resume: Some(opencode_resume),
        // Own a dedicated opencode.json (opencode merges it with the user's
        // opencode.jsonc), so we never parse/rewrite their JSONC file.
        mcp: Some(McpWiring::JsonFile {
            present_dir: ".config/opencode",
            path: ".config/opencode/opencode.json",
            servers_key: "mcp",
            entry: opencode_mcp_entry,
        }),
        connector_discovery: Some(crate::connectors::ConnectorAdapter::OpenCode),
        connector_runtime: None,
        session_title: Some(crate::session_title::opencode_title),
        last_message: None,
        attention_detector: Some("opencode"),
    },
    AgentDef {
        family: "hermes", label: "Hermes",
        cli: Some(plain_cli("hermes", &["~/.local/bin/hermes"])),
        home_dir: ".hermes", project_marker: Some(".hermes"),
        skills_dirs: &[".hermes/skills"], reads_shared: false,
        launch: Some(hermes_launch),
        // Hermes's latest-session lookup can fall back to a different cwd.
        resume: None, mcp: None, connector_discovery: None, connector_runtime: None,
        session_title: Some(crate::hermes_sessions::title),
        last_message: Some(crate::hermes_sessions::last_message),
        attention_detector: Some("hermes"),
    },
    AgentDef {
        family: "pi", label: "Pi",
        cli: Some(plain_cli("pi", &[])),
        home_dir: ".pi/agent", project_marker: Some(".pi"),
        skills_dirs: &[".pi/agent/skills"], reads_shared: true,
        launch: Some(pi_launch), resume: Some(pi_resume),
        mcp: None, connector_discovery: None, connector_runtime: None,
        session_title: Some(crate::pi_sessions::title),
        last_message: Some(crate::pi_sessions::last_message),
        attention_detector: Some("pi"),
    },

];

/// Look up an agent by family, accepting full skill-term ids ("claude:cli").
pub fn by_family(family_or_id: &str) -> Option<&'static AgentDef> {
    let family = family_or_id.split(':').next().unwrap_or(family_or_id);
    AGENTS.iter().find(|a| a.family == family)
}

/// A short human title for a live terminal, read from the agent's own session
/// store. `None` when the family is unknown or has no session_title capability —
/// callers fall back to the cwd.
pub fn session_title_for(
    family_or_id: &str,
    cwd: &str,
    created_unix: i64,
    session_id: Option<&str>,
) -> Option<String> {
    let def = by_family(family_or_id)?;
    (def.session_title?)(std::path::Path::new(cwd), created_unix, session_id)
}

/// The agent's last assistant message, for a turn-finish notification body, read
/// from its own session store. `None` when the family is unknown or has no reader
/// yet — the notifier then falls back to its fixed summons.
pub fn last_message_for(
    family_or_id: &str,
    cwd: &str,
    created_unix: i64,
    session_id: Option<&str>,
) -> Option<String> {
    let def = by_family(family_or_id)?;
    (def.last_message?)(std::path::Path::new(cwd), created_unix, session_id)
}

/// True when the family has an interactive launch line — the gate for
/// app-driven runs (skill mining): the run starts in a live terminal the
/// user is brought to, so its dialogs and prompts are answerable.
pub fn can_launch(family_or_id: &str) -> bool {
    by_family(family_or_id).map(|a| a.launch.is_some()).unwrap_or(false)
}

/// Every skill dir any known agent reads (shared standard + each agent's own),
/// home-relative — e.g. the writable roots a sandboxed run needs to reach.
pub fn all_skills_dirs() -> Vec<&'static str> {
    let mut out: Vec<&'static str> = SHARED_SKILLS_DIRS.to_vec();
    for a in AGENTS {
        for d in a.skills_dirs {
            if !out.contains(d) {
                out.push(d);
            }
        }
    }
    out
}

/// Append ` --add-dir <dir>` for every skill home that exists on this machine
/// (claude and codex take the same flag): skill writes count as in-workspace
/// edits instead of out-of-tree approval round-trips.
fn push_skill_dirs(cmd: &mut String) {
    if let Some(home) = dirs::home_dir() {
        for rel in all_skills_dirs() {
            let dir = home.join(rel);
            if dir.exists() {
                cmd.push_str(&format!(" --add-dir {}", q(&dir.to_string_lossy())));
            }
        }
    }
}

// ─────────────────────────────── Claude Code ───────────────────────────────

/// The interactive TUI with the prompt as the positional argument (submitted
/// as the first user message — `claude` is interactive by default). The
/// prompt comes FIRST: `--add-dir <directories...>` is variadic and would
/// swallow a trailing positional. Permission mode `auto` keeps the run mostly
/// hands-off, the same option the terminal picker offers (model-gated:
/// Opus/Sonnet 4.6+, not haiku). The first launch in a fresh run dir shows
/// the one-time workspace-trust dialog; the accept persists per directory.
fn claude_launch(c: &LaunchCtx) -> String {
    let mut cmd = format!(
        "{} {} --permission-mode auto{}",
        q(c.bin),
        q(c.prompt),
        claude_tune(c.model, c.effort)
    );
    push_skill_dirs(&mut cmd);
    cmd
}

/// `--continue` reopens the most recent conversation in the current directory
/// (documented as cwd-scoped), so the stable run dir is the only key needed.
fn claude_resume(c: &ResumeCtx) -> String {
    format!("{} --continue{}", q(c.bin), claude_tune(c.model, c.effort))
}

fn claude_tune(model: Option<&str>, effort: Option<&str>) -> String {
    let mut tune = String::new();
    if let Some(m) = model {
        tune.push_str(&format!(" --model {}", q(m)));
    }
    if let Some(e) = effort {
        tune.push_str(&format!(" --effort {}", q(e)));
    }
    tune
}

// ────────────────────────────────── Codex ──────────────────────────────────

/// The TUI with the optional `[PROMPT]` positional (submitted, not prefilled;
/// first for symmetry with claude's line). Sandbox and approvals stay on
/// codex's interactive defaults — the user is watching the pane, so its
/// native prompts are answerable; the old `exec` overrides existed only
/// because nobody was. Effort rides the `-c` config override (the CLI has no
/// dedicated flag).
fn codex_launch(c: &LaunchCtx) -> String {
    let mut cmd = format!("{} {}", q(c.bin), q(c.prompt));
    if let Some(m) = c.model {
        cmd.push_str(&format!(" -m {}", q(m)));
    }
    if let Some(e) = c.effort {
        cmd.push_str(&format!(" -c {}", q(&format!("model_reasoning_effort=\"{e}\""))));
    }
    push_skill_dirs(&mut cmd);
    cmd
}

/// `resume --last` continues the most recent session scoped to the current
/// working directory. Codex documents no model/effort flags on `resume`, so
/// the tuning is the session's own.
fn codex_resume(c: &ResumeCtx) -> String {
    format!("{} resume --last", q(c.bin))
}

// ────────────────────────────── Cursor / Gemini ──────────────────────────────

/// The TUI with the prompt as the positional argument; `--model` is the only
/// documented tuning knob.
fn cursor_launch(c: &LaunchCtx) -> String {
    let mut cmd = format!("{} {}", q(c.bin), q(c.prompt));
    if let Some(m) = c.model {
        cmd.push_str(&format!(" --model {}", q(m)));
    }
    cmd
}

/// `-i/--prompt-interactive` is the documented "execute the prompt, then stay
/// interactive" path — a bare positional prompt would run headless instead.
fn gemini_launch(c: &LaunchCtx) -> String {
    let mut cmd = q(c.bin).to_string();
    if let Some(m) = c.model {
        cmd.push_str(&format!(" -m {}", q(m)));
    }
    cmd.push_str(&format!(" -i {}", q(c.prompt)));
    cmd
}

/// `--resume` (no value) loads the most recent session, project-scoped. The
/// model flag precedes it so the optional-valued `--resume` can't eat it.
fn gemini_resume(c: &ResumeCtx) -> String {
    let mut cmd = q(c.bin).to_string();
    if let Some(m) = c.model {
        cmd.push_str(&format!(" -m {}", q(m)));
    }
    cmd.push_str(" --resume");
    cmd
}

// ────────────────────────────────── opencode ──────────────────────────────────

/// The TUI with `--prompt` pre-submitting the first message — the base `opencode`
/// command is interactive (`opencode run` is the headless mode we avoid, like
/// claude's `-p`). Model is `-m provider/model`. No `--add-dir`: opencode reads
/// the global skills dirs (shared standard, ~/.claude/skills, its own) natively,
/// and reasoning effort (`--variant`) is provider-specific and left to defaults.
fn opencode_launch(c: &LaunchCtx) -> String {
    let mut cmd = q(c.bin).to_string();
    if let Some(m) = c.model {
        cmd.push_str(&format!(" -m {}", q(m)));
    }
    cmd.push_str(&format!(" --prompt {}", q(c.prompt)));
    cmd
}

/// `--continue` reopens the most recent session for the cwd's project, the same
/// cwd-scoped contract as claude's `--continue`.
fn opencode_resume(c: &ResumeCtx) -> String {
    let mut cmd = q(c.bin).to_string();
    if let Some(m) = c.model {
        cmd.push_str(&format!(" -m {}", q(m)));
    }
    cmd.push_str(" --continue");
    cmd
}

// Hermes 0.21+ keeps `chat -q` interactive on a terminal. Pi's default
// mode is interactive; `--` protects prompts beginning with option characters.
fn hermes_launch(c: &LaunchCtx) -> String {
    let mut cmd = format!("{} chat", q(c.bin));
    if let Some(model) = c.model { cmd.push_str(&format!(" --model {}", q(model))); }
    if let Some(effort) = c.effort { cmd.push_str(&format!(" --reasoning {}", q(effort))); }
    cmd.push_str(&format!(" -q {}", q(c.prompt)));
    cmd
}
fn pi_tune(model: Option<&str>, effort: Option<&str>) -> String {
    let mut flags = String::new();
    if let Some(model) = model { flags.push_str(&format!(" --model {}", q(model))); }
    if let Some(effort) = effort { flags.push_str(&format!(" --thinking {}", q(effort))); }
    flags
}
fn pi_launch(c: &LaunchCtx) -> String {
    format!("{}{} -- {}", q(c.bin), pi_tune(c.model, c.effort), q(c.prompt))
}
fn pi_resume(c: &ResumeCtx) -> String {
    format!("{}{} --continue", q(c.bin), pi_tune(c.model, c.effort))
}

impl AgentDef {
    pub fn config_home(&self, home: &std::path::Path) -> std::path::PathBuf {
        match self.family {
            "hermes" => return crate::hermes_sessions::home_dir(home),
            "pi" => return crate::pi_sessions::agent_dir(home),
            _ => {}
        }
        home.join(self.home_dir)
    }

    pub fn own_skill_dirs(&self, home: &std::path::Path) -> Vec<std::path::PathBuf> {
        let config = self.config_home(home);
        self.skills_dirs.iter().map(|rel| {
            std::path::Path::new(rel).strip_prefix(self.home_dir)
                .map(|suffix| config.join(suffix)).unwrap_or_else(|_| home.join(rel))
        }).collect()
    }
}

/// Deduplicated bundled-skill destinations for agents present on this host.
pub(crate) fn install_dirs(home: &std::path::Path) -> Vec<std::path::PathBuf> {
    let present: Vec<_> = AGENTS.iter().filter(|a| a.config_home(home).exists()).collect();
    let mut dirs = Vec::new();
    if home.join(".agents").exists() || present.iter().any(|a| a.reads_shared) {
        dirs.push(home.join(".agents/skills"));
    }
    for agent in present.into_iter().filter(|a| !a.reads_shared) {
        if let Some(dir) = agent.own_skill_dirs(home).into_iter().next() {
            if !dirs.contains(&dir) { dirs.push(dir); }
        }
    }
    dirs
}

// ─────────────────────────────── MCP wiring recipes ───────────────────────────────
// Each recipe was verified against the shipped CLI/schema (2026-07). Remote HTTP
// transport only; url-only, no headers/token — VibeStudio is the OAuth client.

/// `claude mcp add-json <name> '{"type":"http","url":…}' --scope user`.
fn claude_mcp_add(name: &str, url: &str) -> Vec<String> {
    let cfg = json!({ "type": "http", "url": url }).to_string();
    vec!["mcp".into(), "add-json".into(), name.into(), cfg, "--scope".into(), "user".into()]
}
fn claude_mcp_remove(name: &str) -> Vec<String> {
    vec!["mcp".into(), "remove".into(), name.into(), "--scope".into(), "user".into()]
}

/// `codex mcp add <name> --url <url>` — `--url` selects streamable HTTP (native
/// since codex 0.14x; user-global config.toml, no scope flag).
fn codex_mcp_add(name: &str, url: &str) -> Vec<String> {
    vec!["mcp".into(), "add".into(), name.into(), "--url".into(), url.into()]
}
fn codex_mcp_remove(name: &str) -> Vec<String> {
    vec!["mcp".into(), "remove".into(), name.into()]
}

/// `gemini mcp add <name> <url> --transport http --scope user` — `--transport
/// http` selects streamable HTTP (bare URL would default to stdio); `--scope
/// user` writes ~/.gemini/settings.json so every cwd sees it.
fn gemini_mcp_add(name: &str, url: &str) -> Vec<String> {
    vec![
        "mcp".into(), "add".into(), name.into(), url.into(),
        "--transport".into(), "http".into(), "--scope".into(), "user".into(),
    ]
}
fn gemini_mcp_remove(name: &str) -> Vec<String> {
    vec!["mcp".into(), "remove".into(), name.into(), "--scope".into(), "user".into()]
}

/// Cursor infers remote transport from the presence of `url`; no `type` field.
fn cursor_mcp_entry(url: &str) -> Value {
    json!({ "url": url })
}
/// opencode's remote server shape — `type` MUST be the literal "remote".
fn opencode_mcp_entry(url: &str) -> Value {
    json!({ "type": "remote", "url": url })
}

// ─────────────────────────────────── tests ───────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_lookup_accepts_ids_and_families() {
        assert_eq!(by_family("claude").unwrap().label, "Claude Code");
        assert_eq!(by_family("codex:cli").unwrap().label, "Codex");
        assert_eq!(by_family("opencode:cli").unwrap().label, "opencode");
        assert!(by_family("shell").is_none());
        assert!(can_launch("claude:cli") && can_launch("codex"));
        assert!(can_launch("cursor") && can_launch("gemini"));
        assert!(can_launch("opencode:cli"));
        assert!(!can_launch("openclaw") && !can_launch("shell"));
    }

    #[test]
    fn all_skills_dirs_unions_shared_and_own() {
        let dirs = all_skills_dirs();
        for d in [".agents/skills", ".claude/skills", ".codex/skills", ".cursor/skills", ".config/opencode/skills"] {
            assert!(dirs.contains(&d), "missing {d}");
        }
        let dedup: std::collections::HashSet<_> = dirs.iter().collect();
        assert_eq!(dedup.len(), dirs.len(), "no duplicates");
    }

    #[test]
    fn resume_lines_reopen_the_cwds_conversation() {
        let ctx = ResumeCtx { bin: "/bin/claude", model: Some("opus"), effort: None };
        assert_eq!(claude_resume(&ctx), "'/bin/claude' --continue --model 'opus'");

        let ctx = ResumeCtx { bin: "/bin/codex", model: None, effort: None };
        assert_eq!(codex_resume(&ctx), "'/bin/codex' resume --last");

        let ctx = ResumeCtx { bin: "/bin/gemini", model: Some("pro"), effort: None };
        // -m before --resume: --resume takes an optional value and would eat it.
        assert_eq!(gemini_resume(&ctx), "'/bin/gemini' -m 'pro' --resume");

        let ctx = ResumeCtx { bin: "/bin/opencode", model: None, effort: None };
        assert_eq!(opencode_resume(&ctx), "'/bin/opencode' --continue");
    }

    #[test]
    fn optional_capabilities_are_independent() {
        for family in ["hermes", "pi"] {
            let agent = by_family(family).unwrap();
            assert!(agent.cli.is_some() && agent.launch.is_some());
            assert!(agent.session_title.is_some() && agent.last_message.is_some());
            assert!(crate::agent_detection::supports_agent(agent.attention_detector.unwrap()));
            assert!(agent.mcp.is_none());
        }
        assert!(by_family("hermes").unwrap().resume.is_none());
        assert!(by_family("pi").unwrap().resume.is_some());
    }

    #[test]
    fn new_agent_launches_keep_prompts_interactive_and_quoted() {
        let ctx = LaunchCtx { bin: "/bin/my agent", prompt: "--don't $(touch /tmp/oops)", model: Some("provider/model"), effort: Some("high") };
        assert_eq!(hermes_launch(&ctx), format!("{} chat --model 'provider/model' --reasoning 'high' -q {}", q(ctx.bin), q(ctx.prompt)));
        assert_eq!(pi_launch(&ctx), format!("{} --model 'provider/model' --thinking 'high' -- {}", q(ctx.bin), q(ctx.prompt)));
        assert_eq!(pi_resume(&ResumeCtx { bin: "pi", model: None, effort: None }), "'pi' --continue");
    }

    #[test]
    fn mcp_cli_recipes_match_shipped_flags() {
        assert_eq!(
            claude_mcp_add("robinhood-abcd1234", "http://127.0.0.1:8765/gw/ID/mcp"),
            ["mcp", "add-json", "robinhood-abcd1234", r#"{"type":"http","url":"http://127.0.0.1:8765/gw/ID/mcp"}"#, "--scope", "user"]
        );
        assert_eq!(codex_mcp_add("n", "http://u/mcp"), ["mcp", "add", "n", "--url", "http://u/mcp"]);
        assert_eq!(gemini_mcp_add("n", "http://u/mcp"), ["mcp", "add", "n", "http://u/mcp", "--transport", "http", "--scope", "user"]);
        assert_eq!(gemini_mcp_remove("n"), ["mcp", "remove", "n", "--scope", "user"]);
        assert_eq!(codex_mcp_remove("n"), ["mcp", "remove", "n"]);
    }

    #[test]
    fn mcp_file_entries_use_each_schema() {
        // cursor infers transport from `url`; opencode needs type:"remote".
        assert_eq!(cursor_mcp_entry("http://u/mcp"), json!({ "url": "http://u/mcp" }));
        assert_eq!(opencode_mcp_entry("http://u/mcp"), json!({ "type": "remote", "url": "http://u/mcp" }));
    }

    #[test]
    fn opencode_launch_uses_prompt_flag_not_a_positional() {
        // The prompt rides --prompt (the base TUI is interactive); model is
        // -m provider/model; there is no --add-dir.
        let ctx = LaunchCtx {
            bin: "/bin/opencode",
            prompt: "do the thing",
            model: Some("anthropic/claude-sonnet-4-6"),
            effort: Some("high"),
        };
        assert_eq!(
            opencode_launch(&ctx),
            "'/bin/opencode' -m 'anthropic/claude-sonnet-4-6' --prompt 'do the thing'"
        );
    }
}
