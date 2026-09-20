// Ported from Herdr, Copyright Herdr contributors, Apache-2.0.
// Upstream: 4b5e9bda239a0b6903889062d756424578e94691; see server/skill-core/src/agent_detection/NOTICE.txt.

use super::*;

#[test]
fn known_agent_no_match_defaults_to_idle_fallback() {
    let explain = explain(Agent::Codex, "ordinary prompt text");

    assert_eq!(explain.state, AgentState::Idle);
    assert!(!explain.visible_idle);
    assert_eq!(
        explain.fallback_reason.as_deref(),
        Some(DEFAULT_KNOWN_AGENT_IDLE_FALLBACK)
    );
}

#[test]
fn all_bundled_manifests_parse_and_validate() {
    for agent in Agent::SCREEN_MANIFEST_AGENTS {
        assert!(
            bundled_manifest(agent).is_some(),
            "missing bundled manifest for {}",
            agent_label(agent)
        );
    }
}

#[test]
fn devin_manifest_detects_idle_working_and_blocked_states() {
    let idle = explain(
        Agent::Devin,
        "─────────────────────────────────────────────────────\n❭ Ask Devin to build features, fix bugs, or work on\n  your code\n─────────────────────────────────────────────────────\nSWE-1.6               Context: 16k / 200k tokens (7%)",
    );
    assert_eq!(idle.state, AgentState::Idle);
    assert!(idle.visible_idle);

    let live_footer_idle = explain(
        Agent::Devin,
        "Done.\n\n────────────────────────────────────────────────── (bypass permissions on) ─\n❭\n────────────────────────────────────────────────────────────────────────────\nClaude Opus 4.6 Thinking                                    Context: 38k / 200k tokens (18%)",
    );
    assert_eq!(live_footer_idle.state, AgentState::Idle);
    assert_eq!(
        live_footer_idle
            .matched_rule
            .as_ref()
            .map(|rule| rule.id.as_str()),
        Some("live_prompt_footer")
    );
    assert!(live_footer_idle.visible_idle);

    let welcome_footer_idle = explain(
        Agent::Devin,
        "⠀⠀⠀⠀⠀⣴⣾⣶⡄⠀⠀⠀⠀\n⠀⣴⣾⣶⡾⠛⠿⠟⠃⣴⣾⣶⡄  Devin CLI\n⠀⠛⠿⠟⠃⣴⣾⣶⡾⠛⠿⠟⠃  v2026.5.26-8\n⠀⣤⣶⣦⡄⠻⢿⠿⢷⣤⣶⣦⡄\n⠀⠻⢿⠿⢷⣤⣶⣦⡄⠻⢿⠿⠃  Hybrid\n⠀⠀⠀⠀⠀⠻⢿⠿⠃⠀⠀⠀⠀\n\n───────────────────────────\n❭ Ask Devin to build\n  features, fix bugs, or\n  work on your code\n───────────────────────────\nClaude Opus Looking for\n4.6 Thinkingplan mode? /\n            plan",
    );
    assert_eq!(welcome_footer_idle.state, AgentState::Idle);
    assert_eq!(
        welcome_footer_idle
            .matched_rule
            .as_ref()
            .map(|rule| rule.id.as_str()),
        Some("welcome_prompt_footer")
    );
    assert!(welcome_footer_idle.visible_idle);

    let working = explain(
        Agent::Devin,
        "◔ Reading shell 91b655\n  │ Timeout: 35s\n\n⠀⡆ Running tools · 27s (esc to interrupt)\n─────────────────────────────────────────────────────\n❭ Guide Devin while it works",
    );
    assert_eq!(working.state, AgentState::Working);
    assert!(working.visible_working);

    let trust_prompt = explain(
        Agent::Devin,
        "Do you trust the authors of this directory?\nFor security, devin should not be run in directories\nwith untrusted content.\n❭ 1 Yes, trust /private/tmp/devin-hook-probe\n· 2 No, exit",
    );
    assert_eq!(trust_prompt.state, AgentState::Blocked);
    assert!(trust_prompt.visible_blocker);

    let permission_prompt = explain(
        Agent::Devin,
        "⏺ Running command\n  └ $ sleep 30\n\n❭ 1 Yes  (Approve once)\n· 2 Yes, allow `sleep` commands\n· 3 Yes, always allow `sleep` commands\n· 4 No\n↑↓ select · ↵ confirm · esc cancel",
    );
    assert_eq!(permission_prompt.state, AgentState::Blocked);
    assert!(permission_prompt.visible_blocker);
}

#[test]
fn muse_manifest_requires_complete_live_controls() {
    let working = explain(
        Agent::Muse,
        "⟩ hello\n\n◆ Working (0s · esc to interrupt)\n\n────────────────\n⟩\n────────────────\ngpt-5.4 · minimal · /workspace",
    );
    assert_eq!(working.state, AgentState::Working);
    assert!(working.visible_working);

    let picker = explain(
        Agent::Muse,
        "Which option should I use?\n\n› 1. Alpha\n  2. Beta\n\nEnter to select · ↑/↓ to move · Tab for an optional note · Esc to interrupt\n\n────────────────\n⟩\n────────────────\ngpt-5.4 · minimal · /workspace",
    );
    assert_eq!(picker.state, AgentState::Blocked);
    assert!(picker.visible_blocker);

    let command_approval = explain(
        Agent::Muse,
        "Would you like to run the following command?\n\n$ printf muse-safe-probe\n\n› 1. Allow this stage once (y)\n  2. Always allow in this workspace: printf muse-safe-probe ... (p)\n  3. Abort the entire command (esc)\n────────────────\ngpt-5.4 · minimal · /workspace",
    );
    assert_eq!(command_approval.state, AgentState::Blocked);
    assert!(command_approval.visible_blocker);

    let network_approval = explain(
        Agent::Muse,
        "network: example.com:443 https\nrequested by:\n$ curl -fsS https://example.com\n\n› 1. Yes, proceed (y)\n  2. Yes, don't ask again this session (p)  example.com:443 (https)\n  3. No, and tell Muse Code what to do differently (esc)\n────────────────\ngpt-5.4 · minimal · /workspace",
    );
    assert_eq!(network_approval.state, AgentState::Blocked);
    assert!(network_approval.visible_blocker);

    let menu = explain(
        Agent::Muse,
        "Theme\n\n⟩ Default (active)\n  Dynamic\n\n↑↓ move · enter save · esc go back",
    );
    assert_eq!(menu.state, AgentState::Unknown);
    assert!(menu.skip_state_update);
    assert!(!menu.visible_blocker);

    let ordinary_reply = explain(
        Agent::Muse,
        "⟩ say the phrase\n\n◆ Yes, proceed\n\n────────────────\n⟩\n────────────────\ngpt-5.4 · minimal · /workspace",
    );
    assert_eq!(ordinary_reply.state, AgentState::Idle);
    assert!(ordinary_reply.visible_idle);
}

#[test]
fn manifest_validation_rejects_unknown_fields_empty_rules_invalid_regions_and_regexes() {
    assert!(parse_manifest(
        r#"
id = "codex"

[[rules]]
id = "typo"
state = "working"
contain = ["Working"]
"#
    )
    .is_err());

    assert!(parse_manifest(
        r#"
id = "codex"

[[rules]]
id = "empty"
state = "working"
"#
    )
    .is_err());

    assert!(parse_manifest(
        r#"
id = "codex"

[[rules]]
id = "bad_region"
state = "working"
region = "after_last_promt_marker"
contains = ["Working"]
"#
    )
    .is_err());

    assert!(parse_manifest(
        r#"
id = "codex"

[[rules]]
id = "bad_regex"
state = "working"
regex = ["["]
"#
    )
    .is_err());

    assert!(parse_manifest(
        r#"
id = "codex"

[[rules]]
id = "bad_nested_regex"
state = "working"
any = [{ line_regex = ["["] }]
"#
    )
    .is_err());
}

#[test]
fn manifest_validation_keeps_skip_rules_neutral() {
    assert!(parse_manifest(
        r#"
id = "codex"

[[rules]]
id = "bad_skip_state"
state = "idle"
skip_state_update = true
contains = ["menu"]
"#
    )
    .is_err());

    assert!(parse_manifest(
        r#"
id = "codex"

[[rules]]
id = "bad_skip_visible"
state = "unknown"
skip_state_update = true
visible_blocker = true
contains = ["menu"]
"#
    )
    .is_err());
}

#[test]
fn manifest_validation_rejects_excessive_rule_count() {
    let mut manifest = String::from(
        r#"
id = "codex"
"#,
    );
    for index in 0..129 {
        manifest.push_str(&format!(
            r#"
[[rules]]
id = "rule_{index}"
state = "idle"
contains = ["ready"]
"#
        ));
    }

    assert!(parse_manifest(&manifest).is_err());
}

#[test]
fn manifest_validation_rejects_excessive_gate_depth() {
    let manifest = r#"
id = "codex"

[[rules]]
id = "deep"
state = "idle"
contains = ["ready"]
all = [
  { contains = ["1"], all = [
    { contains = ["2"], all = [
      { contains = ["3"], all = [
        { contains = ["4"], all = [
          { contains = ["5"], all = [
            { contains = ["6"], all = [
              { contains = ["7"], all = [
                { contains = ["8"], all = [
                  { contains = ["9"] },
                ] },
              ] },
            ] },
          ] },
        ] },
      ] },
    ] },
  ] },
]
"#;

    assert!(parse_manifest(manifest).is_err());
}

#[test]
fn manifest_validation_rejects_excessive_matchers() {
    let matchers = (0..33)
        .map(|index| format!(r#""m{index}""#))
        .collect::<Vec<_>>()
        .join(", ");
    let manifest = format!(
        r#"
id = "codex"

[[rules]]
id = "many"
state = "idle"
contains = [{matchers}]
"#
    );

    assert!(parse_manifest(&manifest).is_err());
}

#[test]
fn bottom_non_empty_lines_uses_bottom_occurrence_for_repeated_text() {
    let content = "marker\nold\n\nmiddle\nmarker\nnew\n";

    assert_eq!(
        region(
            DetectionInput {
                screen: content,
                osc_title: "",
                osc_progress: "",
            },
            "bottom_non_empty_lines(2)"
        ),
        "marker\nnew\n"
    );
}

#[test]
fn top_non_empty_lines_uses_top_occurrence_for_repeated_text() {
    let content = "\nmarker\nold\n\nmiddle\nmarker\nnew\n";

    assert_eq!(
        region(
            DetectionInput {
                screen: content,
                osc_title: "",
                osc_progress: "",
            },
            "top_non_empty_lines(2)"
        ),
        "\nmarker\nold\n"
    );
}

#[test]
fn top_non_empty_lines_requires_a_canonical_positive_bounded_count() {
    let name = "top_non_empty_lines";
    assert!(validate_region_name(&format!("{name}(1)")).is_ok());
    assert!(validate_region_name(&format!("{name}({})", u16::MAX)).is_ok());
    for count in ["0", "01", "+1", "65536", "999999999999999999999999"] {
        assert!(
            validate_region_name(&format!("{name}({count})")).is_err(),
            "{name} accepted invalid count {count}"
        );
    }
}

#[test]
fn top_non_empty_lines_requires_engine_three_when_declared() {
    let manifest = r#"
id = "grok"
version = "1"
min_engine_version = 2

[[rules]]
id = "background"
state = "working"
region = " top_non_empty_lines(1) "
contains = ["active"]
"#;

    assert!(parse_manifest(manifest).is_err());
}

// ---------------------------------------------------------------------------
// OSC rule tests — exercise the new osc_title / osc_progress regions against
// the bundled Claude and Codex manifests.
// ---------------------------------------------------------------------------

fn osc_explain(
    agent: Agent,
    screen: &str,
    osc_title: &str,
    osc_progress: &str,
) -> DetectionExplain {
    explain_with_input(
        agent,
        DetectionInput {
            screen,
            osc_title,
            osc_progress,
        },
    )
}

// --- Claude OSC rules ---

#[test]
fn claude_idle_prompt_with_background_shell_is_idle() {
    // Captured from Claude Code 2.1.251 after its foreground turn ended while
    // a long-lived background shell remained active (issue #3414).
    let screen = concat!(
        "✻ Sautéed for 10s · 1 shell still running\n\n",
        "──────────────────────────────────────────────────────── WINDOWS ─\n",
        "❯\n",
        "────────────────────────────────────────────────────────────────\n",
        "  ⏵⏵ auto mode on · 1 shell · ← for agents                     /rc\n",
    );
    let result = osc_explain(Agent::Claude, screen, "", "");

    assert_eq!(result.state, AgentState::Idle);
    assert_eq!(
        result.matched_rule.as_ref().map(|rule| rule.id.as_str()),
        Some("live_prompt_box")
    );
    assert!(result.visible_idle);
    assert!(!result.visible_working);
}

#[test]
fn claude_background_shell_without_foreground_evidence_is_idle_fallback() {
    let result = osc_explain(
        Agent::Claude,
        "  ⏵⏵ auto mode on · 1 shell · ← for agents\n",
        "",
        "",
    );

    assert_eq!(result.state, AgentState::Idle);
    assert_eq!(result.matched_rule, None);
    assert_eq!(
        result.fallback_reason.as_deref(),
        Some(DEFAULT_KNOWN_AGENT_IDLE_FALLBACK)
    );
    assert!(!result.visible_working);
}

#[test]
fn claude_live_turn_with_background_shell_remains_working() {
    let screen = concat!(
        "────────────────────────────────────────────────────────────────\n",
        "❯\n",
        "────────────────────────────────────────────────────────────────\n",
        "  ⏵⏵ auto mode on · 1 shell · esc to interrupt\n",
    );
    let result = osc_explain(Agent::Claude, screen, "", "");

    assert_eq!(result.state, AgentState::Working);
    assert_eq!(
        result.matched_rule.as_ref().map(|rule| rule.id.as_str()),
        Some("live_turn_working")
    );
    assert!(result.visible_working);
}

#[test]
fn claude_blocker_with_background_shell_remains_blocked() {
    let screen = concat!(
        "do you want to proceed?\n",
        "bash command: rm -rf /tmp/test\n",
        "❯ 1. Yes\n",
        "  2. No\n\n",
        "Esc to cancel · Tab to amend · ctrl+e to explain\n",
        "  ⏵⏵ auto mode on · 1 shell · ← for agents\n",
    );
    let result = osc_explain(Agent::Claude, screen, "", "");

    assert_eq!(result.state, AgentState::Blocked);
    assert_eq!(
        result.matched_rule.as_ref().map(|rule| rule.id.as_str()),
        Some("bash_permission_prompt")
    );
    assert!(result.visible_blocker);
    assert!(!result.visible_working);
}

#[test]
fn claude_bash_prompt_with_dont_ask_again_option_matches_bash_rule() {
    // Captured from a Bash approval prompt at its resting cursor position. The
    // "don't ask again" choice pushes No to option 3, so the only cursor-free
    // option line is one bash_permission_prompt used not to cover, which let
    // the narrower generic_permission_prompt claim the prompt instead (#2650).
    let screen = concat!(
        "────────────────────────────────────────────────────────────────
",
        " Bash command

",
        "   curl -sS -o /tmp/probe.html https://example.com
",
        "   Download example.com to /tmp/probe.html

",
        " This command requires approval

",
        " Do you want to proceed?
",
        " ❯ 1. Yes
",
        "   2. Yes, and don't ask again for: curl *
",
        "   3. No

",
        " Esc to cancel · Tab to amend · ctrl+e to explain
",
    );
    let result = osc_explain(Agent::Claude, screen, "", "");

    assert_eq!(result.state, AgentState::Blocked);
    assert_eq!(
        result.matched_rule.as_ref().map(|rule| rule.id.as_str()),
        Some("bash_permission_prompt")
    );
    assert!(result.visible_blocker);
}

#[test]
fn claude_permission_prompt_matches_at_every_cursor_position() {
    // The selected option carries "❯", so no option branch may assume its line
    // is cursor-free. Walk the cursor across both option layouts.
    let layouts: [&[&str]; 2] = [
        &[" ❯ 1. Yes", "   2. No"],
        &[
            " ❯ 1. Yes",
            "   2. Yes, and don't ask again for: curl *",
            "   3. No",
        ],
    ];

    for layout in layouts {
        for selected in 0..layout.len() {
            let options: Vec<String> = layout
                .iter()
                .enumerate()
                .map(|(index, line)| {
                    let bare = line.trim_start().trim_start_matches('❯').trim_start();
                    if index == selected {
                        format!(" ❯ {bare}")
                    } else {
                        format!("   {bare}")
                    }
                })
                .collect();
            let screen = format!(
                concat!(
                    "────────────────────────────────────────────────────────────────
",
                    " Bash command

",
                    "   curl -sS https://example.com

",
                    " Do you want to proceed?
",
                    "{}

",
                    " Esc to cancel · Tab to amend · ctrl+e to explain
",
                ),
                options.join("\n"),
            );
            let result = osc_explain(Agent::Claude, &screen, "", "");

            assert_eq!(
                result.matched_rule.as_ref().map(|rule| rule.id.as_str()),
                Some("bash_permission_prompt"),
                "{options:?} selected={selected}"
            );
            assert_eq!(result.state, AgentState::Blocked, "selected={selected}");
            assert!(result.visible_blocker, "selected={selected}");
        }
    }
}

#[test]
fn claude_osc_title_braille_prefix_is_working() {
    // "⠂" is U+2802, in the braille block U+2800-U+28FF
    let result = osc_explain(Agent::Claude, "", "⠂ project", "");
    assert_eq!(result.state, AgentState::Working);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_working")
    );
    assert!(result.visible_working);
}

#[test]
fn claude_osc_title_half_circle_frames_are_working() {
    for frame in ['◐', '◓', '◑', '◒'] {
        let title = format!("{frame} Initial conversation with Claude");
        let result = osc_explain(Agent::Claude, "", &title, "");
        assert_eq!(result.state, AgentState::Working, "frame {frame}");
        assert_eq!(
            result.matched_rule.as_ref().map(|rule| rule.id.as_str()),
            Some("osc_title_working"),
            "frame {frame}"
        );
        assert!(result.visible_working, "frame {frame}");
    }
}

#[test]
fn claude_osc_title_static_prefix_is_idle() {
    // "✳" is U+2733, static prefix when Claude is not working
    let result = osc_explain(Agent::Claude, "", "✳ Claude Code", "");
    assert_eq!(result.state, AgentState::Idle);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_idle")
    );
    assert!(result.visible_idle);
}

#[test]
fn claude_osc_progress_4_3_alone_does_not_force_working() {
    // Claude leaves progress stuck at 4;3 while waiting for permission, so
    // 4;3 must not be a working signal on its own. With no other evidence it
    // falls back to idle; blocked screen rules can win when present.
    let result = osc_explain(Agent::Claude, "", "", "4;3;");
    assert_eq!(result.state, AgentState::Idle);
    assert_eq!(
        result.fallback_reason.as_deref(),
        Some(DEFAULT_KNOWN_AGENT_IDLE_FALLBACK)
    );
    assert!(!result.visible_working);
}

#[test]
fn claude_blocker_screen_outranks_stale_osc_progress() {
    // Regression: progress 4;3 persists during permission prompts. The
    // blocked form on screen must win because no rule treats 4;3 as working.
    let blocker_screen =
        "──────────\n  1. Yes\n  2. No\n\nEnter to select · ↑/↓ to navigate · Esc to cancel\n";
    let result = osc_explain(Agent::Claude, blocker_screen, "✳ Task title", "4;3;");
    assert_eq!(result.state, AgentState::Blocked);
    assert!(result.visible_blocker);
}

#[test]
fn claude_osc_progress_4_0_is_idle() {
    let result = osc_explain(Agent::Claude, "", "", "4;0;");
    assert_eq!(result.state, AgentState::Idle);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_progress_idle")
    );
}

#[test]
fn claude_blocker_screen_outranks_osc_idle_title() {
    // When the OSC title shows ✳ (idle) but the screen has a bash permission
    // prompt, the blocked rule at priority 850 beats osc_title_idle at 250.
    let blocker_screen = "do you want to proceed?\n\
        bash command: rm -rf /tmp/test\n\
        ❯ 1. Yes\n   2. No\n\n\
        Esc to cancel · Tab to amend · ctrl+e to explain\n";
    let result = osc_explain(Agent::Claude, blocker_screen, "✳ Claude Code", "");
    assert_eq!(result.state, AgentState::Blocked);
    assert!(result.visible_blocker);
}

#[test]
fn claude_mcp_elicitation_is_blocked() {
    // Regression for issue #3283: an MCP elicitation dialog has Accept/Decline
    // controls and an "Esc to cancel" footer but no Enter hint, so no blocked
    // rule matched and the static OSC title reported idle.
    // Live capture uses curly quotes around the server name; the issue report
    // transcribed straight quotes. Both must classify as blocked.
    for screen in [
        "MCP server \u{201c}my-server\u{201d} requests your input\n\nGrant temporary access to the demo gateway for 15 minutes?\n\n\u{276f} Accept    Decline\n\nEsc to cancel \u{b7} \u{2191}/\u{2193} to navigate\n",
        "MCP server \"my-server\" requests your input\n\nserver-supplied message\n\n\u{276f} Accept    Decline\n\nEsc to cancel \u{b7} \u{2191}/\u{2193} to navigate\n",
    ] {
        let result = osc_explain(Agent::Claude, screen, "\u{2733} Claude Code", "");
        assert_eq!(result.state, AgentState::Blocked, "{result:#?}");
        assert!(result.visible_blocker, "{result:#?}");
        assert_eq!(
            result.matched_rule.as_ref().map(|r| r.id.as_str()),
            Some("mcp_elicitation_prompt"),
            "{result:#?}"
        );
    }
}

#[test]
fn claude_empty_osc_empty_screen_is_idle_fallback() {
    // No OSC data, no matching screen rule → fallback idle (unchanged V3 behavior)
    let result = osc_explain(Agent::Claude, "", "", "");
    assert_eq!(result.state, AgentState::Idle);
    assert_eq!(
        result.fallback_reason.as_deref(),
        Some(DEFAULT_KNOWN_AGENT_IDLE_FALLBACK)
    );
    assert!(!result.visible_idle);
}

// --- Codex OSC rules ---

#[test]
fn codex_osc_title_braille_spinner_is_working() {
    // "⠋" is U+280B, in the braille block
    let result = osc_explain(Agent::Codex, "", "⠋ llm-proxy", "");
    assert_eq!(result.state, AgentState::Working);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_working")
    );
    assert!(result.visible_working);
}

#[test]
fn codex_osc_title_action_required_is_blocked() {
    let result = osc_explain(Agent::Codex, "", "[ . ] Action Required | llm-proxy", "");
    assert_eq!(result.state, AgentState::Blocked);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_blocked")
    );
    assert!(result.visible_blocker);
}

#[test]
fn codex_osc_title_plain_is_idle() {
    let result = osc_explain(Agent::Codex, "", "llm-proxy", "");
    assert_eq!(result.state, AgentState::Idle);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_idle")
    );
    assert!(result.visible_idle);
}

#[test]
fn codex_trust_directory_requires_live_top_region() {
    let screen = "> You are in C:\\Users\\user\\project\n\n\
        Do you trust the contents of this\n\
        directory? Working with untrusted\n\
        contents comes with higher risk of\n\
        prompt injection. Trusting the\n\
        directory allows project-local config,\n\
        hooks, and exec policies to load.\n\n\
        › 1. Yes, continue\n\
          2. No, quit\n\n\
        Press enter to continue\n";
    let result = osc_explain(Agent::Codex, screen, "project", "");

    assert_eq!(result.state, AgentState::Blocked);
    assert_eq!(
        result.matched_rule.as_ref().map(|rule| rule.id.as_str()),
        Some("trust_directory")
    );
    assert!(result.visible_blocker);

    let transcript = "› > You are in C:\\Users\\user\\project\n\n\
        Do you trust the contents of this\n\
        directory? Working with untrusted contents comes with higher risk.\n";
    let result = osc_explain(Agent::Codex, transcript, "project", "");

    assert_eq!(result.state, AgentState::Idle);
    assert_ne!(
        result.matched_rule.as_ref().map(|rule| rule.id.as_str()),
        Some("trust_directory")
    );
    assert!(!result.visible_blocker);
}

#[test]
fn codex_startup_update_requires_complete_live_chooser() {
    let chooser = "Update available! 0.153.0 -> 9.8.7\n\
        Run bun add -g @openai/codex to update.\n\n\
        › 1. Update now\n\
          2. Skip until next version\n\n\
        Press enter to continue   \n";
    let wrapped = "✨ Update available! 0.153.0\n\n\
        Release notes: https://example\n\n\
        › 1. Update now (runs `npm\n\
             install -g\n\
             @openai/codex`)\n\
          2. Skip\n\
          3. Skip until next\n\
             version\n\n\
        Press enter to continue\n";

    for screen in [chooser, wrapped] {
        let result = osc_explain(Agent::Codex, screen, "project", "");
        assert_eq!(result.state, AgentState::Blocked);
        assert_eq!(
            result.matched_rule.as_ref().map(|rule| rule.id.as_str()),
            Some("startup_update")
        );
        assert!(result.visible_blocker);
    }

    for screen in [
        chooser.replace("Update now", "Install"),
        format!("{wrapped}\n› Ask Codex to do anything\n"),
    ] {
        let result = osc_explain(Agent::Codex, &screen, "project", "");
        assert_eq!(result.state, AgentState::Idle);
        assert_ne!(
            result.matched_rule.as_ref().map(|rule| rule.id.as_str()),
            Some("startup_update")
        );
        assert!(!result.visible_blocker);
    }
}

#[test]
fn codex_background_terminal_screen_does_not_override_osc_idle() {
    // Background terminal tasks can be long-lived helpers such as dev servers.
    // They should not make Codex look busy once the foreground turn is idle.
    let screen = "background terminal running · /ps to view · /stop to close\n";
    let result = osc_explain(Agent::Codex, screen, "llm-proxy", "");
    assert_eq!(result.state, AgentState::Idle);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_idle")
    );
    assert!(result.visible_idle);
}

#[test]
fn codex_screen_working_fallback_handles_static_osc_title() {
    let screen = "• I’ll run it and wait for completion.\n\n\
        ◦ Working (1m 16s • esc to interrupt) · 1 background…\n\n\
        › Use /skills to list available skills\n\n\
        gpt-5.6-sol default · /work\n";
    let result = osc_explain(Agent::Codex, screen, "project", "");

    assert_eq!(result.state, AgentState::Working);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("screen_working_fallback")
    );
    assert!(result.visible_working);
}

#[test]
fn codex_osc_working_remains_preferred_over_screen_fallback() {
    let screen = "• Working (4s • esc to interrupt)\n\n\
        › Use /skills to list available skills\n\n\
        gpt-5.6-sol default · /work\n";
    let result = osc_explain(Agent::Codex, screen, "⠸ project", "");

    assert_eq!(result.state, AgentState::Working);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_working")
    );
    assert!(result.visible_working);
}

#[test]
fn codex_screen_blocker_outranks_working_fallback() {
    let screen = "• Working (4s • esc to interrupt)\n\
        › 1. Yes, proceed\n\
        Press enter to confirm or esc to cancel\n";
    let result = osc_explain(Agent::Codex, screen, "project", "");

    assert_eq!(result.state, AgentState::Blocked);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("live_strong_blocker")
    );
    assert!(result.visible_blocker);
    assert!(!result.visible_working);
}

#[test]
fn codex_weak_blocker_without_current_prompt_is_blocked() {
    let result = osc_explain(
        Agent::Codex,
        "do you want to continue? [y/n]\n",
        "project",
        "",
    );

    assert_eq!(result.state, AgentState::Blocked);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("weak_blocker")
    );
}

#[test]
fn codex_current_prompt_keeps_weak_text_from_overriding_working_fallback() {
    let screen = "• Working (4s • esc to interrupt)\n\
        do you want to continue? [y/n]\n\
        › Use /skills to list available skills\n";
    let result = osc_explain(Agent::Codex, screen, "project", "");

    assert_eq!(result.state, AgentState::Working);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("screen_working_fallback")
    );
    assert!(result.visible_working);
}

#[test]
fn codex_weak_blocker_ignores_finished_response_above_current_prompt() {
    let screen = "• The `wt rm` transcript now shows [y/N] / esc, matching the real prompt.\n\n\
        ─ Worked for 4m 59s ─\n\n\
        › Ask Codex to do anything\n";
    let result = osc_explain(Agent::Codex, screen, "project", "");

    assert_eq!(result.state, AgentState::Idle);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_idle")
    );
}

#[test]
fn codex_weak_blocker_ignores_wrapped_current_prompt_text() {
    let screen = "› Explain why this prompt wraps before quoting the confirmation text\n\
          [y/N] / esc and whether the docs should include it\n\n\
          gpt-5.6-sol default · /work\n";
    let result = osc_explain(Agent::Codex, screen, "project", "");

    assert_eq!(result.state, AgentState::Idle);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_idle")
    );
}

#[test]
fn codex_transcript_viewer_outranks_working_fallback() {
    let screen = "• Working (4s • esc to interrupt)\n\
        › transcript\n\
        ↑/↓ to scroll · pgup/pgdn to move · home/end to jump · q to quit · esc to edit prev\n";
    let result = osc_explain(Agent::Codex, screen, "project", "");

    assert_eq!(result.state, AgentState::Unknown);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("transcript_viewer")
    );
    assert!(result.skip_state_update);
    assert!(!result.visible_working);
}

#[test]
fn codex_screen_working_fallback_ignores_stale_and_prompt_text() {
    let screens = [
        "◦ Working (1m 16s • esc to interrupt)\n\
         ■ Conversation interrupted\n\
         › Use /skills to list available skills\n\
         gpt-5.6-sol default · /work\n",
        "› Explain the text ◦ Working (1m 16s • esc to interrupt)\n\
         gpt-5.6-sol default · /work\n",
        "  ◦ Working (1m 16s • esc to interrupt)\n\
         › Use /skills to list available skills\n\
         gpt-5.6-sol default · /work\n",
    ];

    for screen in screens {
        let result = osc_explain(Agent::Codex, screen, "project", "");
        assert_eq!(result.state, AgentState::Idle);
        assert_eq!(
            result.matched_rule.as_ref().map(|r| r.id.as_str()),
            Some("osc_title_idle")
        );
        assert!(result.visible_idle);
        assert!(!result.visible_working);
    }
}

#[test]
fn codex_screen_working_fallback_ignores_interrupted_short_terminal() {
    let screen = "◦ Working (1m 16s • esc to interrupt)\n\
        ■ Conversation interrupted\n\
        ›\n";
    let result = osc_explain(Agent::Codex, screen, "project", "");

    assert_eq!(result.state, AgentState::Idle);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_idle")
    );
    assert!(result.visible_idle);
    assert!(!result.visible_working);
}

#[test]
fn codex_osc_working_beats_weak_blocker_screen() {
    // A stale [y/n] on screen triggers weak_blocker at priority 600, but an
    // active braille spinner in the OSC title is priority 1050 — OSC wins.
    let screen = "do you want to continue? [y/n]\n";
    let result = osc_explain(Agent::Codex, screen, "⠋ llm-proxy", "");
    assert_eq!(result.state, AgentState::Working);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_working")
    );
}

#[test]
fn rule_semantics_apply_gates_priority_and_line_regex() {
    let manifest = parse_manifest(&format!(
        "id = \"codex\"\n{}",
        r#"
[[rules]]
id = "low_contains"
state = "idle"
priority = 1
contains = ["match"]

[[rules]]
id = "high_nested_gates"
state = "working"
priority = 10
contains = ["match"]
all = [
  { any = [{ regex = ["w[io]n"] }, { contains = ["fallback"] }] },
]
not = [
  { contains = ["blocked"] },
]

[[rules]]
id = "line_regex"
state = "blocked"
priority = 20
line_regex = ["^exact line$"]
"#,
    ))
    .unwrap();
    let loaded = LoadedManifest {
        compiled_rules: compile_manifest(&manifest).unwrap(),
        manifest,
    };
    let explain = |agent, screen| {
        evaluate_loaded_manifest(
            agent,
            DetectionInput {
                screen,
                osc_title: "",
                osc_progress: "",
            },
            &loaded,
        )
    };

    let high = explain(Agent::Codex, "match win");
    assert_eq!(high.state, AgentState::Working);
    assert_eq!(
        high.matched_rule.as_ref().map(|rule| rule.id.as_str()),
        Some("high_nested_gates")
    );

    let not_gate = explain(Agent::Codex, "match win blocked");
    assert_eq!(not_gate.state, AgentState::Idle);
    assert_eq!(
        not_gate.matched_rule.as_ref().map(|rule| rule.id.as_str()),
        Some("low_contains")
    );

    let line = explain(Agent::Codex, "before\nexact line\nafter");
    assert_eq!(line.state, AgentState::Blocked);
    assert_eq!(
        line.matched_rule.as_ref().map(|rule| rule.id.as_str()),
        Some("line_regex")
    );
}


#[test]
fn pi_current_editor_border_and_legacy_spinner_detect_working() {
    // Pi 0.86.1's published CustomEditor embeds Loader's default frames and
    // the exact label "Working"; previous versions displayed "Working...".
    for screen in [
        "Assistant output\n── ⠋ Working ───────────────────\n>\n────────────────────────────────\n/work/project\n↑ 120 ↓ 80",
        "── ⠹ Working ─── ↑ 5 more ─────\nA multiline prompt\n──────────────────────────────",
        "───⠼─────────────\n>\n─────────────────",
        "── ⠏ ───── ↑ 12 more ─────\n>\n─────────────────────────",
        " ⠋ Working...\n──────────────────────\n>\n──────────────────────",
        " ⠙ Working\n──────────────────────\n>\n──────────────────────",
    ] {
        let result = explain(Agent::Pi, screen);
        assert_eq!(result.state, AgentState::Working, "{screen}: {result:#?}");
        assert!(result.visible_working);
    }
}

#[test]
fn pi_retry_and_compaction_remain_working() {
    for status in [
        "⠋ Retrying (1/3) in 4s... (esc to cancel)",
        "── ⠙ Compacting context... (esc to cancel) ─────",
        "⠹ Auto-compacting... (esc to cancel)",
        "⠼ Context overflow detected, Auto-compacting... (esc to cancel)",
        "⠴ Summarizing branch... (esc to cancel)",
    ] {
        let result = explain(Agent::Pi, status);
        assert_eq!(result.state, AgentState::Working, "{status}: {result:#?}");
        assert_eq!(result.matched_rule.as_ref().map(|r| r.id.as_str()), Some("retry_or_compaction"));
    }
}

#[test]
fn pi_working_words_in_messages_and_stale_scrollback_are_not_live_controls() {
    for screen in [
        "Working on the documentation is complete.\n> Next task",
        "The UI used to say Working... but the task has finished.\n> Next task",
        "Working\n────────────────────────────────\n>\n────────────────────────────────",
        "── Working ─────────────────────\n>\n────────────────────────────────",
        "⠋ Working on the documentation is complete.\n> Next task",
    ] {
        let result = explain(Agent::Pi, screen);
        assert_eq!(result.state, AgentState::Idle, "{screen}: {result:#?}");
        assert!(!result.visible_working);
    }
    let stale = format!("── ⠋ Working ───────────────\n{}", "Finished output\n".repeat(17));
    assert_eq!(explain(Agent::Pi, &stale).state, AgentState::Idle);
}

#[test]
fn hermes_current_cli_working_controls_survive_composer_input() {
    // Hermes v2026.9.14 CLITuiMixin: the running placeholder disappears when
    // text is entered; the caduceus persists in normal and minimal chrome.
    for screen in [
        "────────────────────────\n☤ ❯ msg=interrupt · /queue · /bg · /steer · Ctrl+C cancel\n────────────────────────",
        "model · /workspace\n────────────────────────\n☤ ❯ Please also check tests\n────────────────────────",
        "model · /workspace\n────────────────────────\n☤ Please also check tests",
        "⠋ command in progress · input stays active; Enter queues\n────────────────────────\n⠋ Processing command...\n────────────────────────",
        "⠹ command in progress · input temporarily disabled\n────────────────────────\n⠹ Processing command...",
    ] {
        let result = explain(Agent::Hermes, screen);
        assert_eq!(result.state, AgentState::Working, "{screen}: {result:#?}");
        assert!(result.visible_working);
    }
}

#[test]
fn hermes_current_cli_approval_clarification_and_credentials_block() {
    for screen in [
        "╭────────────────────────╮\n│ ⚠️ Dangerous Command │\n│ rm example │\n│ ❯ 1. Allow once │\n│ 2. Deny │\n╰────────────────────────╯\n↑/↓ to select, Enter to confirm\n────────────────────────\n⚠ ❯\n────────────────────────",
        "╭─ Hermes needs your input ─╮\n│ Which workspace? │\n│ ❯ 1. Current │\n│ 2. Other (type below) │\n╰───────────────────────────╯\n↑/↓ to select, Enter to lock, Tab next question\n────────────────────────\n? ❯\n────────────────────────",
        "Hermes needs your input\n────────────────────────\n✎ ❯ type your answer here and press Enter\n────────────────────────",
        "🔑 Skill Setup Required\nEnter secret below (hidden), ESC or Ctrl+C to skip\n────────────────────────\n🔑 ❯ type secret (hidden), Enter to submit · ESC to skip\n────────────────────────",
        "🔐 Unlock vault\n────────────────────────\n🔐 ❯ type password (hidden), Enter to submit · ESC to skip\n────────────────────────",
        "🔐 Verification code for example\n────────────────────────\n🔐 ❯ type the code, Enter to submit · ESC to skip\n────────────────────────",
        "🔐 Save login for example\n────────────────────────\n🔐 ❯ type your email / username, Enter to continue · ESC to skip\n────────────────────────",
    ] {
        let result = explain(Agent::Hermes, screen);
        assert_eq!(result.state, AgentState::Blocked, "{screen}: {result:#?}");
        assert!(result.visible_blocker);
    }
}

#[test]
fn hermes_idle_and_stale_running_prompt_do_not_remain_working() {
    for screen in [
        "Done.\nmodel · /workspace\n────────────────────────\n❯ Ask about this project\n────────────────────────",
        "Done.\n────────────────────────\nwork ❯",
        "☤ represents the running agent in Hermes.\n────────────────────────\n❯\n────────────────────────",
        "────────────────────────\n☤ ❯ earlier draft\n────────────────────────\nDone.\n────────────────────────\n❯ next task\n────────────────────────",
    ] {
        let result = explain(Agent::Hermes, screen);
        assert_eq!(result.state, AgentState::Idle, "{screen}: {result:#?}");
        assert!(!result.visible_working);
    }
}
