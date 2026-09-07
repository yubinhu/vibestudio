// Ported from Herdr, Copyright Herdr contributors, Apache-2.0.
// Upstream: 4b5e9bda239a0b6903889062d756424578e94691; see server/skill-core/src/agent_detection/NOTICE.txt.

pub mod manifest;
mod version;
// Preserve upstream scan/transition helpers and their regression tests together.
mod process;
#[allow(dead_code)]
mod stabilization;
mod tracker;
pub use manifest::DetectionExplain as Detection;
pub use process::{
    identify_agent_in_job, identify_process, matches_process, ProcessInfo, ProcessJob,
};
pub use tracker::{AttentionKind, Tracker, Transition};

/// Whether this family has one of the bundled screen manifests.
pub fn supports_agent(family: &str) -> bool {
    parse_agent_label(family).is_some_and(|agent| Agent::SCREEN_MANIFEST_AGENTS.contains(&agent))
}

/// Classify the host's live terminal screen and optional OSC metadata.
pub fn detect(family: &str, screen: &str, title: &str, progress: &str) -> Detection {
    let Some(agent) = parse_agent_label(family) else {
        return manifest::unknown(family);
    };
    manifest::explain_with_input(
        agent,
        manifest::DetectionInput {
            screen,
            osc_title: title,
            osc_progress: progress,
        },
    )
}

fn detect_agent_with_osc(
    agent: Option<Agent>,
    screen: &str,
    title: &str,
    progress: &str,
) -> AgentDetection {
    agent
        .map(|agent| {
            manifest::detect_with_osc(
                agent,
                manifest::DetectionInput {
                    screen,
                    osc_title: title,
                    osc_progress: progress,
                },
            )
        })
        .unwrap_or(AgentDetection {
            state: AgentState::Unknown,
            skip_state_update: false,
            visible_idle: false,
            visible_blocker: false,
            visible_working: false,
        })
}

/// The detected state of a terminal pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    /// Agent finished, prompt visible, nothing happening.
    Idle,
    /// Agent is actively working/processing.
    Working,
    /// Agent needs human input and is blocked on a response.
    Blocked,
    /// Plain shell or unrecognized program.
    Unknown,
}

/// Screen-derived agent state plus confidence metadata used for source arbitration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentDetection {
    pub state: AgentState,
    /// True when the current screen is an agent-owned viewer that shows
    /// transcript/history instead of the live prompt state.
    pub skip_state_update: bool,
    /// True when the current screen visibly shows live idle chrome.
    pub visible_idle: bool,
    /// True when the current screen visibly shows live UI chrome that needs
    /// human input. This is stronger than arbitrary prompt-like text in the
    /// scrollback and may override a non-blocked integration state.
    pub visible_blocker: bool,
    /// True when the current screen visibly shows live working chrome. PTY
    /// activity is the normal working authority; this remains diagnostic
    /// metadata and for non-PTY fallback paths.
    pub visible_working: bool,
}

/// Which agent we detected running in a pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    Pi,
    Claude,
    Codex,
    Gemini,
    Cursor,
    Devin,
    Antigravity,
    Cline,
    Omp,
    Mastracode,
    OpenCode,
    GithubCopilot,
    Kimi,
    Kiro,
    Droid,
    Amp,
    Grok,
    Hermes,
    Kilo,
    Qodercli,
    Qwen,
    Maki,
    Muse,
}

impl Agent {
    pub const ALL: [Self; 23] = [
        Self::Pi,
        Self::Claude,
        Self::Codex,
        Self::Gemini,
        Self::Cursor,
        Self::Devin,
        Self::Antigravity,
        Self::Cline,
        Self::Omp,
        Self::Mastracode,
        Self::OpenCode,
        Self::GithubCopilot,
        Self::Kimi,
        Self::Kiro,
        Self::Droid,
        Self::Amp,
        Self::Grok,
        Self::Hermes,
        Self::Kilo,
        Self::Qodercli,
        Self::Qwen,
        Self::Maki,
        Self::Muse,
    ];

    pub const SCREEN_MANIFEST_AGENTS: [Self; 21] = [
        Self::Pi,
        Self::Claude,
        Self::Codex,
        Self::Gemini,
        Self::Cursor,
        Self::Devin,
        Self::Antigravity,
        Self::Cline,
        Self::OpenCode,
        Self::GithubCopilot,
        Self::Kimi,
        Self::Kiro,
        Self::Droid,
        Self::Amp,
        Self::Grok,
        Self::Hermes,
        Self::Kilo,
        Self::Qodercli,
        Self::Qwen,
        Self::Maki,
        Self::Muse,
    ];
}

pub fn agent_label(agent: Agent) -> &'static str {
    match agent {
        Agent::Pi => "pi",
        Agent::Claude => "claude",
        Agent::Codex => "codex",
        Agent::Gemini => "gemini",
        Agent::Cursor => "cursor",
        Agent::Devin => "devin",
        Agent::Antigravity => "agy",
        Agent::Cline => "cline",
        Agent::Omp => "omp",
        Agent::Mastracode => "mastracode",
        Agent::OpenCode => "opencode",
        Agent::GithubCopilot => "copilot",
        Agent::Kimi => "kimi",
        Agent::Kiro => "kiro",
        Agent::Droid => "droid",
        Agent::Amp => "amp",
        Agent::Grok => "grok",
        Agent::Hermes => "hermes",
        Agent::Kilo => "kilo",
        Agent::Qodercli => "qodercli",
        Agent::Qwen => "qwen",
        Agent::Maki => "maki",
        Agent::Muse => "muse",
    }
}

pub fn parse_agent_label(agent: &str) -> Option<Agent> {
    let name = normalized_agent_lookup_name(agent);
    parse_canonical_agent_label(&name).or_else(|| lookup_agent(&name))
}

pub(crate) fn parse_canonical_agent_label(label: &str) -> Option<Agent> {
    let agent = lookup_agent(label)?;
    (agent_label(agent) == label).then_some(agent)
}

fn lookup_agent(name: &str) -> Option<Agent> {
    let name = path_basename(name);
    match name {
        "pi" => Some(Agent::Pi),
        "claude" | "claude-code" => Some(Agent::Claude),
        "codex" => Some(Agent::Codex),
        "gemini" => Some(Agent::Gemini),
        "cursor" | "cursor-agent" => Some(Agent::Cursor),
        "devin" | "devin-cli" | "devin cli" => Some(Agent::Devin),
        "agy" | "antigravity" | "antigravity-cli" => Some(Agent::Antigravity),
        "cline" => Some(Agent::Cline),
        "omp" => Some(Agent::Omp),
        "mastracode" | "mastra-code" | "mastra code" => Some(Agent::Mastracode),
        "opencode" | "opencode2" | "open-code" => Some(Agent::OpenCode),
        "copilot" | "github-copilot" | "ghcs" => Some(Agent::GithubCopilot),
        "kimi" | "kimi-code" | "kimi code" => Some(Agent::Kimi),
        "kiro" | "kiro-cli" => Some(Agent::Kiro),
        "droid" => Some(Agent::Droid),
        "amp" | "amp-local" => Some(Agent::Amp),
        "grok" | "grok-build" => Some(Agent::Grok),
        "hermes" | "hermes-agent" => Some(Agent::Hermes),
        "kilo" | "kilo-code" | "kilo code" => Some(Agent::Kilo),
        "qodercli" | "qoderclicn" | "qoder" | "qodercn" => Some(Agent::Qodercli),
        "qwen" | "qwen-code" | "qwen code" => Some(Agent::Qwen),
        "maki" => Some(Agent::Maki),
        "muse" | "muse-code" | "muse-cli" => Some(Agent::Muse),
        _ if is_muse_versioned_binary(name) => Some(Agent::Muse),
        _ => None,
    }
}

/// Muse's install-dir launcher script resolves the active release and execs
/// `muse-bin-<version>` (e.g. `muse-bin-0.1.0-R708.1`), so the running
/// process never carries a bare `muse`/`muse-bin` alias. Require a digit
/// immediately after the `muse-bin-` prefix so unrelated binaries such as
/// `muse-binary` or a bare `muse-bin` stay unmatched.
/// Accepts path-qualified `argv0` values by checking only the basename, since
/// the launcher may `exec` with an absolute install-dir path.
fn is_muse_versioned_binary(name: &str) -> bool {
    path_basename(name)
        .strip_prefix("muse-bin-")
        .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
}

fn normalized_agent_lookup_name(name: &str) -> String {
    let mut name = name.trim().to_lowercase();
    for suffix in [".exe", ".cmd", ".bat", ".ps1", ".js"] {
        if name.ends_with(suffix) {
            name.truncate(name.len() - suffix.len());
            break;
        }
    }
    name
}

fn path_basename(path: &str) -> &str {
    path.rsplit(['/', '\\'])
        .find(|component| !component.is_empty())
        .unwrap_or(path)
}
