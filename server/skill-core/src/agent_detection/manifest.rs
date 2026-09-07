// Ported from Herdr, Copyright Herdr contributors, Apache-2.0.
// Upstream: 4b5e9bda239a0b6903889062d756424578e94691; see server/skill-core/src/agent_detection/NOTICE.txt.

use super::{agent_label, version::ManifestVersion, Agent, AgentDetection, AgentState};
use regex::Regex;
use serde::Deserialize;
use std::sync::OnceLock;

pub const DEFAULT_KNOWN_AGENT_IDLE_FALLBACK: &str = "default_known_agent_idle_fallback";

/// Input to the detection engine, carrying the screen snapshot plus any
/// OSC-derived strings captured from the terminal title / progress sequences.
/// Pass empty strings for `osc_title` and `osc_progress` when the data is not
/// available — behavior is identical to the pre-OSC engine in that case.
#[derive(Debug, Clone, Copy)]
pub struct DetectionInput<'a> {
    pub screen: &'a str,
    pub osc_title: &'a str,
    pub osc_progress: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DetectionExplain {
    pub agent: Option<String>,
    pub state: AgentState,
    pub source: Option<ManifestSource>,
    pub matched_rule: Option<MatchedRule>,
    pub screen_detection_skipped: bool,
    pub visible_idle: bool,
    pub visible_blocker: bool,
    pub visible_working: bool,
    pub skip_state_update: bool,
    pub skipped_update_reason: Option<String>,
    pub fallback_reason: Option<String>,
    pub evaluated_rules: Vec<EvaluatedRule>,
    pub warning: Option<String>,
    pub manifest_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ManifestSource {
    Bundled,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct MatchedRule {
    pub id: String,
    pub priority: i32,
    pub region: String,
    pub state: AgentState,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct EvaluatedRule {
    pub id: String,
    pub priority: i32,
    pub region: String,
    pub evidence: RuleEvidence,
    pub state: AgentState,
    pub matched: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RuleEvidence {
    pub contains: Vec<String>,
    pub regex: Vec<String>,
    pub line_regex: Vec<String>,
    pub all_count: usize,
    pub any_count: usize,
    pub not_count: usize,
    pub region_bytes: usize,
    pub region_preview: String,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentManifest {
    // Retained upstream schema fields; the bundled-only loader selects by its
    // compile-time catalog instead of consulting override identities.
    #[allow(dead_code)]
    id: String,
    version: Option<ManifestVersion>,
    min_engine_version: Option<u32>,
    #[serde(rename = "updated_at")]
    _updated_at: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    aliases: Vec<String>,
    #[serde(default)]
    rules: Vec<ManifestRule>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct ManifestRule {
    id: String,
    state: Option<ManifestState>,
    #[serde(default)]
    priority: i32,
    #[serde(default = "default_region")]
    region: String,
    #[serde(default)]
    visible_idle: bool,
    #[serde(default)]
    visible_blocker: bool,
    #[serde(default)]
    visible_working: bool,
    #[serde(default)]
    skip_state_update: bool,
    #[serde(default)]
    all: Vec<ManifestGate>,
    #[serde(default)]
    any: Vec<ManifestGate>,
    #[serde(default, rename = "not")]
    not_gate: Vec<ManifestGate>,
    #[serde(default)]
    contains: Vec<String>,
    #[serde(default)]
    regex: Vec<String>,
    #[serde(default)]
    line_regex: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct ManifestGate {
    #[serde(default)]
    all: Vec<ManifestGate>,
    #[serde(default)]
    any: Vec<ManifestGate>,
    #[serde(default, rename = "not")]
    not_gate: Vec<ManifestGate>,
    #[serde(default)]
    contains: Vec<String>,
    #[serde(default)]
    regex: Vec<String>,
    #[serde(default)]
    line_regex: Vec<String>,
}

#[derive(Debug, Clone)]
struct CompiledRule {
    gate: CompiledGate,
}

#[derive(Debug, Clone)]
struct CompiledGate {
    all: Vec<CompiledGate>,
    any: Vec<CompiledGate>,
    not_gate: Vec<CompiledGate>,
    contains: Vec<String>,
    regex: Vec<Regex>,
    line_regex: Vec<Regex>,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ManifestState {
    Idle,
    Working,
    Blocked,
    Unknown,
}

impl From<ManifestState> for AgentState {
    fn from(value: ManifestState) -> Self {
        match value {
            ManifestState::Idle => AgentState::Idle,
            ManifestState::Working => AgentState::Working,
            ManifestState::Blocked => AgentState::Blocked,
            ManifestState::Unknown => AgentState::Unknown,
        }
    }
}

fn default_region() -> String {
    "whole_recent".to_string()
}

const BUNDLED_MANIFESTS: &[(&str, &str)] = &[
    ("amp", include_str!("manifests/amp.toml")),
    ("agy", include_str!("manifests/antigravity.toml")),
    ("claude", include_str!("manifests/claude.toml")),
    ("cline", include_str!("manifests/cline.toml")),
    ("codex", include_str!("manifests/codex.toml")),
    ("cursor", include_str!("manifests/cursor.toml")),
    ("devin", include_str!("manifests/devin.toml")),
    ("droid", include_str!("manifests/droid.toml")),
    ("gemini", include_str!("manifests/gemini.toml")),
    ("grok", include_str!("manifests/grok.toml")),
    ("hermes", include_str!("manifests/hermes.toml")),
    ("kilo", include_str!("manifests/kilo.toml")),
    ("kimi", include_str!("manifests/kimi.toml")),
    ("kiro", include_str!("manifests/kiro.toml")),
    ("maki", include_str!("manifests/maki.toml")),
    ("muse", include_str!("manifests/muse.toml")),
    ("opencode", include_str!("manifests/opencode.toml")),
    ("pi", include_str!("manifests/pi.toml")),
    ("qodercli", include_str!("manifests/qodercli.toml")),
    ("qwen", include_str!("manifests/qwen.toml")),
    ("copilot", include_str!("manifests/github-copilot.toml")),
];

const MAX_RULES_PER_MANIFEST: usize = 128;
const MAX_GATE_DEPTH: usize = 8;
const MAX_TOTAL_GATES: usize = 512;
const MAX_MATCHERS_PER_GATE: usize = 32;
const MAX_TOTAL_MATCHERS: usize = 1024;
const MAX_MATCHER_CHARS: usize = 512;

#[derive(Debug, Clone)]
struct LoadedManifest {
    manifest: AgentManifest,
    compiled_rules: Vec<CompiledRule>,
}

fn load_manifest(agent: Agent) -> Option<&'static LoadedManifest> {
    static CACHE: OnceLock<Vec<(Agent, LoadedManifest)>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            Agent::SCREEN_MANIFEST_AGENTS
                .into_iter()
                .filter_map(|agent| {
                    let manifest = bundled_manifest(agent)?;
                    let compiled_rules = compile_manifest(&manifest).unwrap_or_else(|error| {
                        panic!(
                            "bundled {} manifest is invalid: {error}",
                            agent_label(agent)
                        )
                    });
                    Some((
                        agent,
                        LoadedManifest {
                            manifest,
                            compiled_rules,
                        },
                    ))
                })
                .collect()
        })
        .iter()
        .find(|(id, _)| *id == agent)
        .map(|(_, manifest)| manifest)
}

pub fn detect_with_osc(agent: Agent, input: DetectionInput<'_>) -> AgentDetection {
    explain_with_input(agent, input).into_detection()
}

pub fn explain(agent: Agent, screen: &str) -> DetectionExplain {
    explain_with_input(
        agent,
        DetectionInput {
            screen,
            osc_title: "",
            osc_progress: "",
        },
    )
}

pub fn explain_with_input(agent: Agent, input: DetectionInput<'_>) -> DetectionExplain {
    match load_manifest(agent) {
        Some(loaded) => evaluate_loaded_manifest(agent, input, loaded),
        None => fallback_explain(Some(agent), None),
    }
}

pub(super) fn unknown(family: &str) -> DetectionExplain {
    let mut result = fallback_explain(None, None);
    result.agent = Some(family.to_owned());
    result.fallback_reason = Some("unknown_agent".into());
    result
}

impl DetectionExplain {
    fn into_detection(self) -> AgentDetection {
        AgentDetection {
            state: self.state,
            skip_state_update: self.skip_state_update,
            visible_idle: self.visible_idle,
            visible_blocker: self.visible_blocker,
            visible_working: self.visible_working,
        }
    }
}

fn evaluate_loaded_manifest(
    agent: Agent,
    input: DetectionInput<'_>,
    loaded: &LoadedManifest,
) -> DetectionExplain {
    let mut matched: Option<(&ManifestRule, String)> = None;
    let mut evaluated_rules = Vec::new();

    for (rule, compiled_rule) in loaded.manifest.rules.iter().zip(&loaded.compiled_rules) {
        let region_text = region(input, &rule.region);
        let matched_rule = compiled_rule_matches(compiled_rule, region_text);
        evaluated_rules.push(EvaluatedRule {
            id: rule.id.clone(),
            priority: rule.priority,
            region: rule.region.clone(),
            evidence: rule_evidence(rule, region_text),
            state: rule
                .state
                .map(AgentState::from)
                .unwrap_or(AgentState::Unknown),
            matched: matched_rule,
        });

        if !matched_rule {
            continue;
        }

        match matched {
            Some((previous, _)) if previous.priority >= rule.priority => {}
            _ => matched = Some((rule, rule.region.clone())),
        }
    }

    let Some((rule, region_name)) = matched else {
        return fallback_explain(Some(agent), Some((loaded, evaluated_rules)));
    };

    let state = rule
        .state
        .map(AgentState::from)
        .unwrap_or(AgentState::Unknown);
    let skipped_update_reason = rule
        .skip_state_update
        .then(|| format!("matched_rule:{}", rule.id));

    DetectionExplain {
        agent: Some(agent_label(agent).to_string()),
        state,
        source: Some(ManifestSource::Bundled),
        matched_rule: Some(MatchedRule {
            id: rule.id.clone(),
            priority: rule.priority,
            region: region_name,
            state,
        }),
        screen_detection_skipped: false,
        visible_idle: rule.visible_idle && state == AgentState::Idle,
        visible_blocker: rule.visible_blocker && state == AgentState::Blocked,
        visible_working: rule.visible_working && state == AgentState::Working,
        skip_state_update: rule.skip_state_update,
        skipped_update_reason,
        fallback_reason: None,
        evaluated_rules,
        warning: None,
        manifest_version: loaded.manifest.version.as_ref().map(ToString::to_string),
    }
}

fn fallback_explain(
    agent: Option<Agent>,
    context: Option<(&LoadedManifest, Vec<EvaluatedRule>)>,
) -> DetectionExplain {
    let known_agent = agent.is_some();
    let (manifest_version, evaluated_rules) = context
        .map(|(loaded, rules)| {
            (
                loaded.manifest.version.as_ref().map(ToString::to_string),
                rules,
            )
        })
        .unwrap_or_default();
    DetectionExplain {
        agent: agent.map(|agent| agent_label(agent).to_string()),
        state: if known_agent {
            AgentState::Idle
        } else {
            AgentState::Unknown
        },
        source: manifest_version.as_ref().map(|_| ManifestSource::Bundled),
        matched_rule: None,
        screen_detection_skipped: false,
        visible_idle: false,
        visible_blocker: false,
        visible_working: false,
        skip_state_update: false,
        skipped_update_reason: None,
        fallback_reason: known_agent.then(|| DEFAULT_KNOWN_AGENT_IDLE_FALLBACK.to_string()),
        evaluated_rules,
        warning: None,
        manifest_version,
    }
}

fn bundled_manifest(agent: Agent) -> Option<AgentManifest> {
    let id = agent_label(agent);
    BUNDLED_MANIFESTS
        .iter()
        .find(|(manifest_id, _)| *manifest_id == id)
        .map(|(_, content)| {
            parse_manifest(content)
                .unwrap_or_else(|err| panic!("bundled {id} manifest is invalid: {err}"))
        })
}

pub(crate) fn parse_manifest(content: &str) -> Result<AgentManifest, String> {
    let manifest = toml::from_str::<AgentManifest>(content).map_err(|err| err.to_string())?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

fn validate_manifest(manifest: &AgentManifest) -> Result<(), String> {
    if manifest.rules.is_empty() {
        return Err("manifest must contain at least one rule".to_string());
    }
    if manifest.rules.len() > MAX_RULES_PER_MANIFEST {
        return Err(format!(
            "manifest contains {} rules, max is {MAX_RULES_PER_MANIFEST}",
            manifest.rules.len()
        ));
    }

    let mut complexity = ManifestComplexity::default();
    for rule in &manifest.rules {
        if rule.id.trim().is_empty() {
            return Err("manifest rule id must not be empty".to_string());
        }
        if rule.skip_state_update {
            if rule.state != Some(ManifestState::Unknown) {
                return Err(format!(
                    "rule {} uses skip_state_update without state = \"unknown\"",
                    rule.id
                ));
            }
            if rule.visible_idle || rule.visible_blocker || rule.visible_working {
                return Err(format!(
                    "rule {} uses skip_state_update with visible state evidence",
                    rule.id
                ));
            }
        }
        validate_region_name(&rule.region)
            .map_err(|err| format!("rule {} uses invalid region: {err}", rule.id))?;
        if rule.region.trim().starts_with("top_non_empty_lines(")
            && manifest
                .min_engine_version
                .is_some_and(|version| version < TOP_NON_EMPTY_LINES_ENGINE_VERSION)
        {
            return Err(format!(
                "rule {} uses top_non_empty_lines but min_engine_version is below {}",
                rule.id, TOP_NON_EMPTY_LINES_ENGINE_VERSION
            ));
        }
        validate_rule_gate(rule, &mut complexity)
            .map_err(|err| format!("rule {} has invalid matcher gates: {err}", rule.id))?;
    }

    Ok(())
}

#[derive(Default)]
struct ManifestComplexity {
    total_gates: usize,
    total_matchers: usize,
}

fn validate_rule_gate(
    rule: &ManifestRule,
    complexity: &mut ManifestComplexity,
) -> Result<(), String> {
    validate_gate(&manifest_gate_from_rule(rule), "rule", 0, complexity)
}

fn validate_gate(
    gate: &ManifestGate,
    context: &str,
    depth: usize,
    complexity: &mut ManifestComplexity,
) -> Result<(), String> {
    if depth > MAX_GATE_DEPTH {
        return Err(format!("{context} exceeds max gate depth {MAX_GATE_DEPTH}"));
    }
    complexity.total_gates += 1;
    if complexity.total_gates > MAX_TOTAL_GATES {
        return Err(format!("manifest exceeds max gate count {MAX_TOTAL_GATES}"));
    }
    validate_matcher_limits(gate, context, complexity)?;
    if !gate_has_positive_matcher(gate) {
        return Err(format!("{context} must contain a positive matcher"));
    }
    validate_regex_patterns(&gate.regex, context, "regex")?;
    validate_regex_patterns(&gate.line_regex, context, "line_regex")?;
    for nested in &gate.all {
        validate_gate(nested, "all gate", depth + 1, complexity)?;
    }
    for nested in &gate.any {
        validate_gate(nested, "any gate", depth + 1, complexity)?;
    }
    for nested in &gate.not_gate {
        if !gate_has_any_matcher(nested) {
            return Err(format!("{context} contains an empty not gate"));
        }
        validate_not_gate(nested, depth + 1, complexity)?;
    }
    Ok(())
}

fn validate_not_gate(
    gate: &ManifestGate,
    depth: usize,
    complexity: &mut ManifestComplexity,
) -> Result<(), String> {
    if depth > MAX_GATE_DEPTH {
        return Err(format!("not gate exceeds max gate depth {MAX_GATE_DEPTH}"));
    }
    complexity.total_gates += 1;
    if complexity.total_gates > MAX_TOTAL_GATES {
        return Err(format!("manifest exceeds max gate count {MAX_TOTAL_GATES}"));
    }
    validate_matcher_limits(gate, "not gate", complexity)?;
    if !gate_has_any_matcher(gate) {
        return Err("not gate must contain a matcher".to_string());
    }
    validate_regex_patterns(&gate.regex, "not gate", "regex")?;
    validate_regex_patterns(&gate.line_regex, "not gate", "line_regex")?;
    for nested in &gate.all {
        validate_gate(nested, "not all gate", depth + 1, complexity)?;
    }
    for nested in &gate.any {
        validate_gate(nested, "not any gate", depth + 1, complexity)?;
    }
    for nested in &gate.not_gate {
        validate_not_gate(nested, depth + 1, complexity)?;
    }
    Ok(())
}

fn validate_matcher_limits(
    gate: &ManifestGate,
    context: &str,
    complexity: &mut ManifestComplexity,
) -> Result<(), String> {
    let matcher_count = gate.contains.len() + gate.regex.len() + gate.line_regex.len();
    if matcher_count > MAX_MATCHERS_PER_GATE {
        return Err(format!(
            "{context} has {matcher_count} direct matchers, max is {MAX_MATCHERS_PER_GATE}"
        ));
    }
    complexity.total_matchers += matcher_count;
    if complexity.total_matchers > MAX_TOTAL_MATCHERS {
        return Err(format!(
            "manifest exceeds max matcher count {MAX_TOTAL_MATCHERS}"
        ));
    }
    for value in gate
        .contains
        .iter()
        .chain(gate.regex.iter())
        .chain(gate.line_regex.iter())
    {
        if value.chars().count() > MAX_MATCHER_CHARS {
            return Err(format!(
                "{context} matcher exceeds max length {MAX_MATCHER_CHARS}"
            ));
        }
    }
    Ok(())
}

fn validate_regex_patterns(patterns: &[String], context: &str, field: &str) -> Result<(), String> {
    for pattern in patterns {
        Regex::new(pattern).map_err(|err| {
            format!("{context} contains invalid {field} pattern {pattern:?}: {err}")
        })?;
    }
    Ok(())
}

fn gate_has_positive_matcher(gate: &ManifestGate) -> bool {
    !gate.contains.is_empty()
        || !gate.regex.is_empty()
        || !gate.line_regex.is_empty()
        || !gate.all.is_empty()
        || !gate.any.is_empty()
}

fn gate_has_any_matcher(gate: &ManifestGate) -> bool {
    gate_has_positive_matcher(gate) || !gate.not_gate.is_empty()
}

fn validate_region_name(spec: &str) -> Result<(), String> {
    let trimmed = spec.trim();
    match trimmed {
        "whole_recent"
        | "after_last_prompt_marker"
        | "before_current_prompt_marker"
        | "whole_recent_without_current_prompt_marker"
        | "current_prompt_block_marker"
        | "after_current_prompt_block_marker"
        | "prompt_box_body"
        | "above_prompt_box"
        | "last_non_empty_above_prompt_box"
        | "after_last_horizontal_rule"
        | "osc_title"
        | "osc_progress" => Ok(()),
        _ if region_count(trimmed, "bottom_lines").is_some()
            || region_count(trimmed, "bottom_non_empty_lines").is_some()
            || top_region_count(trimmed).is_some() =>
        {
            Ok(())
        }
        _ => Err(trimmed.to_string()),
    }
}

fn manifest_gate_from_rule(rule: &ManifestRule) -> ManifestGate {
    ManifestGate {
        all: rule.all.clone(),
        any: rule.any.clone(),
        not_gate: rule.not_gate.clone(),
        contains: rule.contains.clone(),
        regex: rule.regex.clone(),
        line_regex: rule.line_regex.clone(),
    }
}

fn compile_manifest(manifest: &AgentManifest) -> Result<Vec<CompiledRule>, String> {
    manifest
        .rules
        .iter()
        .map(|rule| {
            compile_gate(&manifest_gate_from_rule(rule))
                .map(|gate| CompiledRule { gate })
                .map_err(|err| format!("rule {} could not be compiled: {err}", rule.id))
        })
        .collect()
}

fn compile_gate(gate: &ManifestGate) -> Result<CompiledGate, String> {
    Ok(CompiledGate {
        all: gate
            .all
            .iter()
            .map(compile_gate)
            .collect::<Result<_, _>>()?,
        any: gate
            .any
            .iter()
            .map(compile_gate)
            .collect::<Result<_, _>>()?,
        not_gate: gate
            .not_gate
            .iter()
            .map(compile_gate)
            .collect::<Result<_, _>>()?,
        contains: gate
            .contains
            .iter()
            .map(|needle| needle.to_lowercase())
            .collect(),
        regex: gate
            .regex
            .iter()
            .map(|pattern| Regex::new(pattern).map_err(|err| err.to_string()))
            .collect::<Result<_, _>>()?,
        line_regex: gate
            .line_regex
            .iter()
            .map(|pattern| Regex::new(pattern).map_err(|err| err.to_string()))
            .collect::<Result<_, _>>()?,
    })
}

fn compiled_rule_matches(rule: &CompiledRule, text: &str) -> bool {
    let lower_text = text.to_lowercase();
    compiled_gate_matches(&rule.gate, text, &lower_text)
}

fn rule_evidence(rule: &ManifestRule, region_text: &str) -> RuleEvidence {
    RuleEvidence {
        contains: rule.contains.clone(),
        regex: rule.regex.clone(),
        line_regex: rule.line_regex.clone(),
        all_count: rule.all.len(),
        any_count: rule.any.len(),
        not_count: rule.not_gate.len(),
        region_bytes: region_text.len(),
        region_preview: bounded_preview(region_text),
    }
}

fn bounded_preview(text: &str) -> String {
    const MAX_CHARS: usize = 240;
    let mut preview: String = text.chars().take(MAX_CHARS).collect();
    if text.chars().count() > MAX_CHARS {
        preview.push_str("...");
    }
    preview
}

fn compiled_gate_matches(gate: &CompiledGate, text: &str, lower_text: &str) -> bool {
    if !gate
        .contains
        .iter()
        .all(|needle| lower_text.contains(needle))
    {
        return false;
    }

    if !gate.regex.iter().all(|regex| regex.is_match(text)) {
        return false;
    }

    if !gate
        .line_regex
        .iter()
        .all(|regex| text.lines().any(|line| regex.is_match(line)))
    {
        return false;
    }

    if !gate
        .all
        .iter()
        .all(|nested| compiled_gate_matches(nested, text, lower_text))
    {
        return false;
    }

    if !gate.any.is_empty()
        && !gate
            .any
            .iter()
            .any(|nested| compiled_gate_matches(nested, text, lower_text))
    {
        return false;
    }

    if gate
        .not_gate
        .iter()
        .any(|nested| compiled_gate_matches(nested, text, lower_text))
    {
        return false;
    }

    true
}

fn region<'a>(input: DetectionInput<'a>, spec: &str) -> &'a str {
    let trimmed = spec.trim();
    // OSC regions source from their dedicated fields, not the screen.
    match trimmed {
        "osc_title" => return input.osc_title,
        "osc_progress" => return input.osc_progress,
        _ => {}
    }
    // All other regions operate on the screen content as before.
    let content = input.screen;
    match trimmed {
        "whole_recent" => content,
        "after_last_prompt_marker" => after_last_prompt_marker(content),
        "before_current_prompt_marker" => before_current_prompt_marker(content),
        "whole_recent_without_current_prompt_marker" => {
            whole_recent_without_current_prompt_marker(content)
        }
        "current_prompt_block_marker" => current_prompt_block_marker(content).unwrap_or(""),
        "after_current_prompt_block_marker" => {
            after_current_prompt_block_marker(content).unwrap_or("")
        }
        "prompt_box_body" => prompt_box_body(content).unwrap_or(""),
        "above_prompt_box" => above_prompt_box(content),
        "last_non_empty_above_prompt_box" => last_non_empty_line(above_prompt_box(content)),
        "after_last_horizontal_rule" => after_last_horizontal_rule(content),
        _ => {
            if let Some(count) = region_count(trimmed, "bottom_lines") {
                return bottom_lines(content, count);
            }
            if let Some(count) = region_count(trimmed, "bottom_non_empty_lines") {
                return bottom_non_empty_lines(content, count);
            }
            if let Some(count) = top_region_count(trimmed) {
                return top_non_empty_lines(content, count);
            }
            ""
        }
    }
}

fn region_count(spec: &str, name: &str) -> Option<usize> {
    spec.strip_prefix(name)
        .and_then(|rest| rest.strip_prefix('('))
        .and_then(|rest| rest.strip_suffix(')'))
        .and_then(|count| count.parse::<usize>().ok())
}

const TOP_NON_EMPTY_LINES_ENGINE_VERSION: u32 = 3;
const MAX_TOP_REGION_LINE_COUNT: usize = u16::MAX as usize;

fn top_region_count(spec: &str) -> Option<usize> {
    let count = spec
        .strip_prefix("top_non_empty_lines")?
        .strip_prefix('(')?
        .strip_suffix(')')?;
    if count.starts_with('0') || !count.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    count
        .parse::<usize>()
        .ok()
        .filter(|count| *count <= MAX_TOP_REGION_LINE_COUNT)
}

fn bottom_lines(content: &str, count: usize) -> &str {
    let lines: Vec<&str> = content.lines().collect();
    let start = lines.len().saturating_sub(count);
    slice_from_line_index(content, &lines, start)
}

fn bottom_non_empty_lines(content: &str, count: usize) -> &str {
    let lines: Vec<&str> = content.lines().collect();
    let Some(start_index) = lines
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, line)| !line.trim().is_empty())
        .take(count)
        .last()
        .map(|(index, _)| index)
    else {
        return "";
    };
    slice_from_line_index(content, &lines, start_index)
}

fn top_non_empty_lines(content: &str, count: usize) -> &str {
    let lines: Vec<&str> = content.lines().collect();
    let Some(end_index) = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .take(count)
        .last()
        .map(|(index, _)| index)
    else {
        return "";
    };
    let byte_offset = line_start_offset(content, &lines, end_index + 1);
    &content[..byte_offset]
}

fn after_last_prompt_marker(content: &str) -> &str {
    let lines: Vec<&str> = content.lines().collect();
    let Some(index) = lines.iter().rposition(|line| codex_prompt_line(line)) else {
        return content;
    };
    slice_from_line_index(content, &lines, index + 1)
}

fn before_current_prompt_marker(content: &str) -> &str {
    let lines: Vec<&str> = content.lines().collect();
    let Some(index) = current_codex_prompt_index(&lines) else {
        return content;
    };
    let byte_offset = lines[..index]
        .iter()
        .map(|line| line.len() + 1)
        .sum::<usize>();
    &content[..byte_offset.min(content.len())]
}

fn whole_recent_without_current_prompt_marker(content: &str) -> &str {
    let lines: Vec<&str> = content.lines().collect();
    if current_codex_prompt_index(&lines).is_some() {
        ""
    } else {
        content
    }
}

fn current_prompt_block_marker(content: &str) -> Option<&str> {
    let lines: Vec<&str> = content.lines().collect();
    let prompt_index = current_codex_prompt_index(&lines)?;
    lines[..prompt_index]
        .iter()
        .rev()
        .find(|line| codex_block_marker_line(line))
        .copied()
}

fn after_current_prompt_block_marker(content: &str) -> Option<&str> {
    let lines: Vec<&str> = content.lines().collect();
    let prompt_index = current_codex_prompt_index(&lines)?;
    let block_index = lines[..prompt_index]
        .iter()
        .rposition(|line| codex_block_marker_line(line))?;
    Some(slice_from_line_index(content, &lines, block_index))
}

fn current_codex_prompt_index(lines: &[&str]) -> Option<usize> {
    let prompt_index = lines.iter().rposition(|line| codex_prompt_line(line))?;
    if lines[prompt_index + 1..]
        .iter()
        .any(|line| codex_block_marker_line(line))
    {
        return None;
    }
    Some(prompt_index)
}

fn codex_prompt_line(line: &str) -> bool {
    line == "›" || line.starts_with("› ")
}

fn codex_block_marker_line(line: &str) -> bool {
    line.starts_with('•') || line.starts_with('■') || line.starts_with('✗') || line.starts_with('✓')
}

fn prompt_box_body(content: &str) -> Option<&str> {
    let lines: Vec<&str> = content.lines().collect();
    let top = prompt_box_top_border_index(&lines)?;
    let start = line_start_offset(content, &lines, top + 1);
    let end_index = lines[top + 1..]
        .iter()
        .position(|line| is_horizontal_rule(line))
        .map(|relative| top + 1 + relative)
        .unwrap_or(lines.len());
    let end = line_start_offset(content, &lines, end_index);
    Some(&content[start.min(content.len())..end.min(content.len())])
}

fn above_prompt_box(content: &str) -> &str {
    let lines: Vec<&str> = content.lines().collect();
    let Some(top) = prompt_box_top_border_index(&lines) else {
        return content;
    };
    let end = line_start_offset(content, &lines, top);
    &content[..end.min(content.len())]
}

fn after_last_horizontal_rule(content: &str) -> &str {
    let mut last_rule_end = 0usize;
    let mut offset = 0usize;
    for line in content.lines() {
        let next_offset = offset + line.len() + 1;
        if is_horizontal_rule(line) {
            last_rule_end = next_offset.min(content.len());
        }
        offset = next_offset;
    }
    &content[last_rule_end..]
}

fn last_non_empty_line(content: &str) -> &str {
    content
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
}

fn prompt_box_top_border_index(lines: &[&str]) -> Option<usize> {
    let mut border_count = 0;
    for index in (0..lines.len()).rev() {
        if is_horizontal_rule(lines[index]) {
            border_count += 1;
            if border_count == 2 {
                return Some(index);
            }
        }
    }
    None
}

fn is_horizontal_rule(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }

    let rule_chars = trimmed.chars().take_while(|&ch| ch == '─').count();
    if rule_chars == 0 {
        return false;
    }

    let rule_bytes = trimmed
        .char_indices()
        .nth(rule_chars)
        .map(|(index, _)| index)
        .unwrap_or(trimmed.len());
    let suffix = trimmed[rule_bytes..].trim_start();

    suffix.is_empty() || rule_chars >= 3
}

fn slice_from_line_index<'a>(content: &'a str, lines: &[&str], index: usize) -> &'a str {
    let byte_offset = line_start_offset(content, lines, index);
    &content[byte_offset.min(content.len())..]
}

fn line_start_offset(content: &str, lines: &[&str], index: usize) -> usize {
    lines[..index.min(lines.len())]
        .iter()
        .map(|line| line.len() + 1)
        .sum::<usize>()
        .min(content.len())
}

#[cfg(test)]
mod tests;
