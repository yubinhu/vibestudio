// Ported from Herdr, Copyright Herdr contributors, Apache-2.0.
// Upstream: 4b5e9bda239a0b6903889062d756424578e94691; see server/skill-core/src/agent_detection/NOTICE.txt.

use super::{agent_label, parse_agent_label, Agent};

/// Host-neutral process evidence, retaining upstream argv boundaries when known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    pub argv0: Option<String>,
    pub argv: Option<Vec<String>>,
    pub cmdline: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessJob {
    pub process_group_id: u32,
    pub processes: Vec<ProcessInfo>,
}

/// Identify one process using its executable name and `ps` command line.
/// The string transport cannot recover unquoted argv boundaries. Shell command
/// payloads (`bash -lc ...`, etc.) are therefore rejected before the unchanged
/// upstream matcher runs; the live child process supplies identity instead.
/// Call `identify_agent_in_job` with native argv when it is available.
pub fn identify_process(name: &str, command_line: &str) -> Option<Agent> {
    let argv: Vec<String> = command_line.split_whitespace().map(str::to_owned).collect();
    let executable = normalized_agent_lookup_name(path_basename(name));
    if matches!(
        executable.trim_start_matches('-'),
        "sh" | "bash" | "zsh" | "fish" | "dash" | "ash" | "ksh" | "tcsh"
    ) && argv
        .iter()
        .skip(1)
        .take_while(|arg| arg.starts_with('-'))
        .any(|arg| {
            arg == "--command"
                || (!arg.starts_with("--") && arg.trim_start_matches('-').contains('c'))
        })
    {
        return None;
    }
    let process = ProcessInfo {
        pid: 0,
        name: name.to_string(),
        argv0: None,
        argv: Some(argv),
        cmdline: Some(command_line.to_string()),
    };
    identify_agent(&normalized_process_name(&process))
}

pub fn matches_process(expected: &str, name: &str, command_line: &str) -> bool {
    parse_agent_label(expected)
        .is_some_and(|agent| identify_process(name, command_line) == Some(agent))
}

/// Identify which agent is running from the process name.
/// Returns `None` for plain shells or unrecognized programs.
pub fn identify_agent(process_name: &str) -> Option<Agent> {
    parse_agent_label(process_name)
}

pub fn identify_agent_in_job(job: &ProcessJob) -> Option<(Agent, String)> {
    if let Some(process) = job
        .processes
        .iter()
        .find(|process| process.pid == job.process_group_id)
    {
        let candidate = normalized_process_name(process);
        if let Some(agent) = identify_agent(&candidate) {
            return Some((agent, candidate));
        }
    }

    let mut best: Option<(u8, Agent, String)> = None;

    for process in &job.processes {
        let candidate = normalized_process_name(process);
        let Some(agent) = identify_agent(&candidate) else {
            continue;
        };
        let score = process_priority(process, &candidate);

        match &best {
            Some((best_score, _, _)) if *best_score >= score => {}
            _ => best = Some((score, agent, candidate)),
        }
    }

    best.map(|(_, agent, name)| (agent, name))
}

fn normalized_process_name(process: &ProcessInfo) -> String {
    let effective = process.argv0.as_deref().unwrap_or(&process.name);
    let lower_effective = effective.to_lowercase();

    if is_generic_runtime_or_shell(&lower_effective) {
        if let Some(wrapped_agent) =
            wrapped_agent_name_from_runtime_argv(&lower_effective, process.argv.as_deref())
        {
            return wrapped_agent;
        }
    }

    if identify_agent(effective).is_some() {
        return effective.to_string();
    }

    if let Some(runtime) = process.argv.as_deref().and_then(|argv| argv.first()) {
        let runtime_name = normalized_agent_lookup_name(path_basename(runtime));
        if matches!(runtime_name.as_str(), "node" | "bun") {
            if let Some(wrapped_agent) =
                wrapped_agent_name_from_runtime_argv(runtime, process.argv.as_deref())
            {
                if identify_agent(&wrapped_agent) == Some(Agent::Qwen) {
                    return wrapped_agent;
                }
            }
        }
    }

    if let Some(wrapped_agent) = argv0_agent_name(process.argv.as_deref())
        .or_else(|| cmdline_argv0_agent_name(process.cmdline.as_deref().unwrap_or_default()))
    {
        return wrapped_agent;
    }

    effective.to_string()
}

fn wrapped_agent_name_from_runtime_argv(runtime: &str, argv: Option<&[String]>) -> Option<String> {
    let argv = argv?;
    let runtime_name = normalized_agent_lookup_name(path_basename(runtime));

    match runtime_name.as_str() {
        "node" => cursor_agent_name_from_bundled_node_argv(argv)
            .or_else(|| script_arg_agent_name(argv, &["-e", "--eval", "-p", "--print"], &[])),
        "bun" => script_arg_agent_name(argv, &["-e", "--eval", "-p", "--print"], &[]),
        name if is_python_runtime(name) => script_arg_agent_name(argv, &["-c"], &["-m"]),
        "sh" | "bash" | "zsh" | "fish" => script_arg_agent_name(argv, &["-c"], &[]),
        "cmd" => windows_cmd_arg_agent_name(argv),
        "powershell" | "pwsh" => powershell_arg_agent_name(argv),
        "tmux" => None,
        _ => None,
    }
}

fn cursor_agent_name_from_bundled_node_argv(argv: &[String]) -> Option<String> {
    let (runtime_parent, runtime_name) = path_parent_and_basename(argv.first()?)?;
    let (script_parent, script_name) = path_parent_and_basename(argv.get(1)?)?;
    if !runtime_name.eq_ignore_ascii_case("node.exe")
        || !script_name.eq_ignore_ascii_case("index.js")
        || !runtime_parent.eq_ignore_ascii_case(script_parent)
    {
        return None;
    }

    let mut tail = runtime_parent
        .rsplit(['/', '\\'])
        .filter(|component| !component.is_empty());
    let (Some(version), Some(versions), Some(package)) = (tail.next(), tail.next(), tail.next())
    else {
        return None;
    };
    (package.eq_ignore_ascii_case("cursor-agent")
        && versions.eq_ignore_ascii_case("versions")
        && !version.trim().is_empty())
    .then(|| agent_label(Agent::Cursor).to_string())
}

fn path_parent_and_basename(path: &str) -> Option<(&str, &str)> {
    let split = path.rfind(['/', '\\'])?;
    let parent = path[..split].trim_end_matches(['/', '\\']);
    let basename = &path[split + 1..];
    (!parent.is_empty() && !basename.is_empty()).then_some((parent, basename))
}

fn windows_cmd_arg_agent_name(argv: &[String]) -> Option<String> {
    let mut args = argv.iter().skip(1);
    while let Some(arg) = args.next() {
        let flag = arg.trim_matches('"').to_lowercase();
        match flag.as_str() {
            "/c" | "/k" => {
                return args
                    .next()
                    .and_then(|command| command_text_agent_name(command))
            }
            "/d" | "/s" | "/q" | "/a" | "/u" | "/e:on" | "/e:off" | "/f:on" | "/f:off"
            | "/v:on" | "/v:off" => continue,
            _ => {}
        }
    }
    None
}

fn powershell_arg_agent_name(argv: &[String]) -> Option<String> {
    let mut args = argv.iter().skip(1);
    while let Some(arg) = args.next() {
        let flag = arg.trim_matches('"').to_lowercase();
        match flag.as_str() {
            "-file" | "-f" | "/file" => {
                return args
                    .next()
                    .and_then(|path| agent_name_from_path_token(path));
            }
            "-command" | "-c" | "/command" | "/c" => {
                return args
                    .next()
                    .and_then(|command| command_text_agent_name(command));
            }
            "-encodedcommand" | "-enc" | "/encodedcommand" | "/enc" => return None,
            "-configurationname" | "-executionpolicy" | "-outputformat" | "-psconsolefile"
            | "-version" | "-windowstyle" | "-workingdirectory" => {
                let _ = args.next();
            }
            _ if flag.starts_with('-') || flag.starts_with('/') => {}
            _ => return agent_name_from_path_token(arg),
        }
    }
    None
}

fn command_text_agent_name(command: &str) -> Option<String> {
    let mut rest = command;
    while let Some((token, next)) = command_text_token(rest) {
        let token = token.trim();
        if token.eq_ignore_ascii_case("&")
            || token.eq_ignore_ascii_case(".")
            || token.eq_ignore_ascii_case("call")
        {
            rest = next;
            continue;
        }
        return agent_name_from_path_token(token);
    }
    None
}

fn command_text_token(input: &str) -> Option<(&str, &str)> {
    let input = input.trim_start();
    let first = input.chars().next()?;
    if first == '"' || first == '\'' {
        let start = first.len_utf8();
        if let Some(end) = input[start..].find(first) {
            let end = start + end;
            return Some((&input[start..end], &input[end + first.len_utf8()..]));
        }
        return Some((&input[start..], ""));
    }

    let end = input.find(char::is_whitespace).unwrap_or(input.len());
    Some((&input[..end], &input[end..]))
}

fn script_arg_agent_name(
    argv: &[String],
    eval_flags: &[&str],
    module_flags: &[&str],
) -> Option<String> {
    let mut args = argv.iter().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--" {
            return args
                .next()
                .and_then(|token| agent_name_from_path_token(token));
        }

        if flag_matches(arg, eval_flags) || flag_matches(arg, module_flags) {
            return None;
        }

        if arg.starts_with('-') {
            if option_takes_value(arg) {
                let _ = args.next();
            }
            continue;
        }

        return agent_name_from_path_token(arg);
    }

    None
}

fn flag_matches(arg: &str, flags: &[&str]) -> bool {
    flags
        .iter()
        .any(|flag| arg == *flag || short_flag_payload(arg, flag) || long_flag_value(arg, flag))
}

fn short_flag_payload(arg: &str, flag: &str) -> bool {
    flag.starts_with('-')
        && !flag.starts_with("--")
        && arg.starts_with(flag)
        && arg.len() > flag.len()
}

fn long_flag_value(arg: &str, flag: &str) -> bool {
    flag.starts_with("--")
        && arg
            .strip_prefix(flag)
            .is_some_and(|rest| rest.starts_with('='))
}

fn option_takes_value(arg: &str) -> bool {
    matches!(
        arg,
        "-r" | "--require"
            | "--loader"
            | "--import"
            | "--experimental-loader"
            | "--inspect-port"
            | "-W"
            | "-X"
            | "-S"
            | "-L"
            | "-o"
    )
}

fn argv0_agent_name(argv: Option<&[String]>) -> Option<String> {
    agent_name_from_path_token(argv?.first()?)
}

fn cmdline_argv0_agent_name(cmdline: &str) -> Option<String> {
    agent_name_from_path_token(cmdline.split_whitespace().next()?)
}

fn agent_name_from_path_token(token: &str) -> Option<String> {
    let trimmed = token.trim_matches(|c| matches!(c, '"' | '\''));
    if trimmed.is_empty() || trimmed.starts_with('-') {
        return None;
    }

    agent_name_from_basename(path_basename(trimmed))
        .or_else(|| agent_name_from_known_package_path(trimmed))
        .or_else(|| resolved_agent_name_from_path_token(trimmed))
}

fn agent_name_from_known_package_path(path: &str) -> Option<String> {
    let raw_components: Vec<&str> = path
        .split(['/', '\\'])
        .filter(|component| !component.is_empty())
        .collect();
    let ends_with = |suffix: &[&str]| {
        raw_components.len() >= suffix.len()
            && raw_components[raw_components.len() - suffix.len()..]
                .iter()
                .zip(suffix)
                .all(|(actual, expected)| actual.eq_ignore_ascii_case(expected))
    };
    if ends_with(&[
        "node_modules",
        "@earendil-works",
        "pi-coding-agent",
        "dist",
        "cli.js",
    ]) || ends_with(&[
        "node_modules",
        "@earendil-works",
        "pi-coding-agent",
        "dist",
        "bundle",
        "cli.js",
    ]) {
        return Some(agent_label(Agent::Pi).to_string());
    }

    let components: Vec<String> = raw_components
        .into_iter()
        .map(normalized_agent_lookup_name)
        .collect();
    for window in components.windows(5) {
        if window == ["node_modules", "@qwen-code", "qwen-code", "dist", "index"] {
            return Some(agent_label(Agent::Qwen).to_string());
        }
    }
    for window in components.windows(4) {
        if window == ["node_modules", "mastracode", "dist", "cli"] {
            return Some(agent_label(Agent::Mastracode).to_string());
        }
    }
    None
}

fn resolved_agent_name_from_path_token(token: &str) -> Option<String> {
    let path = std::path::Path::new(token);
    if path.components().count() < 2 {
        return None;
    }

    let resolved = std::fs::canonicalize(path).ok()?;
    let basename = resolved.file_name()?.to_str()?;
    agent_name_from_basename(basename)
}

fn agent_name_from_basename(basename: &str) -> Option<String> {
    let agent = parse_agent_label(basename)?;
    Some(agent_label(agent).to_string())
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

fn process_priority(process: &ProcessInfo, normalized_name: &str) -> u8 {
    let lower_name = normalized_name.to_lowercase();
    if lower_name != process.name.to_lowercase() {
        return 3;
    }
    if !is_generic_runtime_or_shell(&lower_name) {
        return 2;
    }
    1
}

fn is_generic_runtime_or_shell(name: &str) -> bool {
    let name = normalized_agent_lookup_name(path_basename(name));
    is_python_runtime(&name)
        || matches!(
            name.as_str(),
            "sh" | "bash"
                | "zsh"
                | "fish"
                | "tmux"
                | "node"
                | "bun"
                | "cmd"
                | "powershell"
                | "pwsh"
        )
}

fn is_python_runtime(name: &str) -> bool {
    name == "python"
        || name.strip_prefix("python").is_some_and(|version| {
            !version.is_empty()
                && version
                    .split('.')
                    .all(|part| !part.is_empty() && part.chars().all(|ch| ch.is_ascii_digit()))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_detection::parse_canonical_agent_label;

    fn foreground_process(pid: u32, name: &str, argv: &[&str]) -> ProcessInfo {
        ProcessInfo {
            pid,
            name: name.to_string(),
            argv0: None,
            argv: Some(argv.iter().map(|arg| (*arg).to_string()).collect()),
            cmdline: Some(argv.join(" ")),
        }
    }

    #[cfg(unix)]
    fn temp_detection_path(name: &str) -> std::path::PathBuf {
        let unique = format!(
            "herdr-detect-tests-{}-{}-{}",
            name,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time should be after unix epoch")
                .as_nanos()
        );
        std::env::temp_dir().join(unique)
    }

    // ---- Agent identification ----

    #[test]
    fn identify_known_agents() {
        assert_eq!(identify_agent("pi"), Some(Agent::Pi));
        assert_eq!(identify_agent("claude"), Some(Agent::Claude));
        assert_eq!(identify_agent("claude-code"), Some(Agent::Claude));
        assert_eq!(identify_agent("codex"), Some(Agent::Codex));
        assert_eq!(identify_agent("gemini"), Some(Agent::Gemini));
        assert_eq!(identify_agent("cursor"), Some(Agent::Cursor));
        assert_eq!(identify_agent("cursor-agent"), Some(Agent::Cursor));
        assert_eq!(identify_agent("devin"), Some(Agent::Devin));
        assert_eq!(identify_agent("devin-cli"), Some(Agent::Devin));
        assert_eq!(identify_agent("agy"), Some(Agent::Antigravity));
        assert_eq!(identify_agent("antigravity-cli"), Some(Agent::Antigravity));
        assert_eq!(identify_agent("cline"), Some(Agent::Cline));
        assert_eq!(identify_agent("omp"), Some(Agent::Omp));
        assert_eq!(identify_agent("mastracode"), Some(Agent::Mastracode));
        assert_eq!(identify_agent("mastra-code"), Some(Agent::Mastracode));
        assert_eq!(identify_agent("opencode"), Some(Agent::OpenCode));
        assert_eq!(identify_agent("opencode.exe"), Some(Agent::OpenCode));
        assert_eq!(identify_agent("opencode2"), Some(Agent::OpenCode));
        assert_eq!(identify_agent("opencode2.exe"), Some(Agent::OpenCode));
        assert_eq!(identify_agent("kimi"), Some(Agent::Kimi));
        assert_eq!(identify_agent("Kimi Code"), Some(Agent::Kimi));
        assert_eq!(identify_agent("kiro"), Some(Agent::Kiro));
        assert_eq!(identify_agent("kiro-cli"), Some(Agent::Kiro));
        assert_eq!(identify_agent("copilot"), Some(Agent::GithubCopilot));
        assert_eq!(identify_agent("ghcs"), Some(Agent::GithubCopilot));
        assert_eq!(identify_agent("grok"), Some(Agent::Grok));
        assert_eq!(identify_agent("grok-build"), Some(Agent::Grok));
        assert_eq!(identify_agent("hermes"), Some(Agent::Hermes));
        assert_eq!(identify_agent("hermes-agent"), Some(Agent::Hermes));
        assert_eq!(identify_agent("kilo"), Some(Agent::Kilo));
        assert_eq!(identify_agent("kilo-code"), Some(Agent::Kilo));
        assert_eq!(identify_agent("qwen"), Some(Agent::Qwen));
        assert_eq!(identify_agent("Qwen Code"), Some(Agent::Qwen));
        assert_eq!(identify_agent("maki"), Some(Agent::Maki));
        assert_eq!(identify_agent("muse"), Some(Agent::Muse));
        assert_eq!(identify_agent("muse-code"), Some(Agent::Muse));
        assert_eq!(identify_agent("muse-cli"), Some(Agent::Muse));
        assert_eq!(identify_agent("muse-bin-0.1.0-R708.1"), Some(Agent::Muse));
        assert_eq!(identify_agent("muse-bin-1.2.3"), Some(Agent::Muse));
        assert_eq!(
            identify_agent("/home/user/.local/bin/muse-bin-0.2.1-R1215.1"),
            Some(Agent::Muse)
        );
        assert_eq!(
            identify_agent(r"C:\Users\user\muse-bin-0.2.1-R1215.1.exe"),
            Some(Agent::Muse)
        );
    }

    #[test]
    fn parse_known_agent_labels() {
        assert_eq!(parse_agent_label("pi"), Some(Agent::Pi));
        assert_eq!(parse_agent_label("claude"), Some(Agent::Claude));
        assert_eq!(parse_agent_label("cursor-agent"), Some(Agent::Cursor));
        assert_eq!(parse_agent_label("devin-cli"), Some(Agent::Devin));
        assert_eq!(parse_agent_label("agy"), Some(Agent::Antigravity));
        assert_eq!(parse_agent_label("antigravity"), Some(Agent::Antigravity));
        assert_eq!(parse_agent_label("omp"), Some(Agent::Omp));
        assert_eq!(parse_agent_label("mastracode"), Some(Agent::Mastracode));
        assert_eq!(parse_agent_label("mastra code"), Some(Agent::Mastracode));
        assert_eq!(parse_agent_label("opencode.exe"), Some(Agent::OpenCode));
        assert_eq!(parse_agent_label("copilot"), Some(Agent::GithubCopilot));
        assert_eq!(parse_agent_label("kimi-code"), Some(Agent::Kimi));
        assert_eq!(
            parse_agent_label("github-copilot"),
            Some(Agent::GithubCopilot)
        );
        assert_eq!(parse_agent_label("amp-local"), Some(Agent::Amp));
        assert_eq!(parse_agent_label("kiro-cli"), Some(Agent::Kiro));
        assert_eq!(parse_agent_label("grok-build"), Some(Agent::Grok));
        assert_eq!(parse_agent_label("hermes-agent"), Some(Agent::Hermes));
        assert_eq!(parse_agent_label("qwen-code"), Some(Agent::Qwen));
        assert_eq!(parse_agent_label("maki"), Some(Agent::Maki));
        assert_eq!(parse_agent_label("kilo-code"), Some(Agent::Kilo));
    }

    #[test]
    fn every_agent_label_round_trips_through_canonical_and_alias_parsers() {
        for agent in Agent::ALL {
            let label = agent_label(agent);
            assert_eq!(parse_canonical_agent_label(label), Some(agent));
            assert_eq!(parse_agent_label(label), Some(agent));
        }
    }

    #[test]
    fn canonical_agent_labels_are_strict() {
        assert_eq!(parse_canonical_agent_label("claude-code"), None);
        assert_eq!(parse_canonical_agent_label("Pi"), None);
        assert_eq!(parse_canonical_agent_label(" pi "), None);
        assert_eq!(parse_canonical_agent_label("opencode.exe"), None);
    }

    #[test]
    fn identify_unknown_processes() {
        assert_eq!(identify_agent("bash"), None);
        assert_eq!(identify_agent("zsh"), None);
        assert_eq!(identify_agent("vim"), None);
        assert_eq!(identify_agent("node"), None);
        assert_eq!(identify_agent("museum"), None);
        assert_eq!(identify_agent("muse-helper"), None);
        assert_eq!(identify_agent("muser"), None);
        assert_eq!(identify_agent("musescore"), None);
        assert_eq!(identify_agent("muse-bin"), None);
        assert_eq!(identify_agent("muse-bin-"), None);
        assert_eq!(identify_agent("muse-binary"), None);
    }

    #[test]
    fn identify_case_insensitive() {
        assert_eq!(identify_agent("Pi"), Some(Agent::Pi));
        assert_eq!(identify_agent("CLAUDE"), Some(Agent::Claude));
        assert_eq!(identify_agent("Codex"), Some(Agent::Codex));
        assert_eq!(identify_agent("Devin"), Some(Agent::Devin));
    }

    #[test]
    fn identify_agent_in_job_prefers_wrapped_codex() {
        let job = ProcessJob {
            process_group_id: 123,
            processes: vec![
                foreground_process(1, "node", &["node", "/path/to/bin/codex"]),
                foreground_process(2, "bash", &["bash"]),
            ],
        };

        assert_eq!(
            identify_agent_in_job(&job),
            Some((Agent::Codex, "codex".to_string()))
        );
    }

    #[test]
    fn identify_agent_in_job_detects_node_wrapped_qwen() {
        for argv in [
            vec!["node", "/home/user/.fnm/bin/qwen"],
            vec![
                "node.exe",
                r"C:\Users\user\AppData\Roaming\npm\node_modules\@qwen-code\qwen-code\dist\index.js",
            ],
        ] {
            let job = ProcessJob {
                process_group_id: 123,
                processes: vec![foreground_process(123, "MainThread", &argv)],
            };

            assert_eq!(
                identify_agent_in_job(&job),
                Some((Agent::Qwen, "qwen".to_string()))
            );
        }
    }

    #[test]
    fn identify_agent_in_job_detects_windows_cursor_install() {
        let job = ProcessJob {
            process_group_id: 123,
            processes: vec![foreground_process(
                123,
                "node.exe",
                &[
                    r"C:\Users\user\AppData\Local\cursor-agent\versions\2026.08.11-e8db854\node.exe",
                    r"C:\Users\user\AppData\Local\cursor-agent\versions\2026.08.11-e8db854\index.js",
                ],
            )],
        };

        assert_eq!(
            identify_agent_in_job(&job),
            Some((Agent::Cursor, "cursor".to_string()))
        );
    }

    #[test]
    fn identify_agent_in_job_ignores_invalid_windows_cursor_install_paths() {
        for script in [
            r"C:\Users\user\AppData\Local\cursor-agent\versions\2026.08.11-e8db854\scripts\postinstall.js",
            r"C:\Users\user\AppData\Local\cursor-agent\versions\2026.08.11-e8db854\index",
            r"C:\Users\user\AppData\Local\cursor-agent\versions\2026.08.11-e8db854\index.exe",
        ] {
            let job = ProcessJob {
                process_group_id: 123,
                processes: vec![foreground_process(
                    123,
                    "node.exe",
                    &[
                        r"C:\Users\user\AppData\Local\cursor-agent\versions\2026.08.11-e8db854\node.exe",
                        script,
                    ],
                )],
            };

            assert_eq!(identify_agent_in_job(&job), None, "script: {script}");
        }

        let lookalike = ProcessJob {
            process_group_id: 123,
            processes: vec![foreground_process(
                123,
                "node.exe",
                &[
                    r"C:\Program Files\nodejs\node.exe",
                    r"C:\workspace\cursor-agent\versions\test\index.js",
                ],
            )],
        };
        assert_eq!(identify_agent_in_job(&lookalike), None);
    }

    #[test]
    fn identify_agent_in_job_prefers_recognized_process_group_leader() {
        let job = ProcessJob {
            process_group_id: 42,
            processes: vec![
                foreground_process(42, "claude", &["claude"]),
                foreground_process(43, "node", &["node", "/tmp/mcp/bin/codex"]),
            ],
        };

        assert_eq!(
            identify_agent_in_job(&job),
            Some((Agent::Claude, "claude".to_string()))
        );
    }

    #[test]
    fn identify_agent_in_job_falls_back_when_process_group_leader_is_unrecognized() {
        let job = ProcessJob {
            process_group_id: 42,
            processes: vec![
                foreground_process(42, "bash", &["bash"]),
                foreground_process(43, "node", &["node", "/tmp/mcp/bin/codex"]),
            ],
        };

        assert_eq!(
            identify_agent_in_job(&job),
            Some((Agent::Codex, "codex".to_string()))
        );
    }

    #[test]
    fn identify_agent_in_job_detects_python_version_wrapped_hermes() {
        let job = ProcessJob {
            process_group_id: 123,
            processes: vec![foreground_process(
                123,
                "python3.12",
                &[
                    "/nix/store/example/bin/python3.12",
                    "/nix/store/example/bin/hermes",
                    "--resume",
                    "session-id",
                ],
            )],
        };

        assert_eq!(
            identify_agent_in_job(&job),
            Some((Agent::Hermes, "hermes".to_string()))
        );
    }

    #[test]
    fn identify_agent_in_job_detects_nix_wrapped_codex_from_cmdline_argv0() {
        let job = ProcessJob {
            process_group_id: 123,
            processes: vec![foreground_process(
                1,
                ".codex-wrapped",
                &["/etc/profiles/per-user/user/bin/codex", "--model", "gpt-5"],
            )],
        };

        assert_eq!(
            identify_agent_in_job(&job),
            Some((Agent::Codex, "codex".to_string()))
        );
    }

    #[test]
    fn identify_agent_in_job_canonicalizes_nix_wrapped_aliases_from_cmdline_argv0() {
        let job = ProcessJob {
            process_group_id: 123,
            processes: vec![foreground_process(
                1,
                ".claude-code-wrapped",
                &["/nix/store/example/bin/claude-code"],
            )],
        };

        assert_eq!(
            identify_agent_in_job(&job),
            Some((Agent::Claude, "claude".to_string()))
        );
    }

    #[test]
    fn identify_agent_in_job_detects_shell_wrapped_pi() {
        let job = ProcessJob {
            process_group_id: 123,
            processes: vec![foreground_process(
                1,
                "sh",
                &["/bin/sh", "/tmp/test-bin/pi"],
            )],
        };

        assert_eq!(
            identify_agent_in_job(&job),
            Some((Agent::Pi, "pi".to_string()))
        );
    }

    #[test]
    fn identify_agent_in_job_detects_bun_wrapped_omp() {
        let job = ProcessJob {
            process_group_id: 123,
            processes: vec![foreground_process(
                123,
                "bun",
                &["bun", "/home/can/.bun/bin/omp"],
            )],
        };

        assert_eq!(
            identify_agent_in_job(&job),
            Some((Agent::Omp, "omp".to_string()))
        );
    }

    #[test]
    fn identify_agent_in_job_detects_node_wrapped_pi_package_cli() {
        let job = ProcessJob {
            process_group_id: 123,
            processes: vec![foreground_process(
                123,
                "node.exe",
                &[
                    "node.exe",
                    "C:\\Users\\herdr\\AppData\\Roaming\\npm\\node_modules\\@earendil-works\\pi-coding-agent\\dist\\cli.js",
                ],
            )],
        };

        assert_eq!(
            identify_agent_in_job(&job),
            Some((Agent::Pi, "pi".to_string()))
        );
    }

    #[test]
    fn identify_agent_in_job_detects_node_wrapped_pi_bundled_cli() {
        let job = ProcessJob {
            process_group_id: 123,
            processes: vec![foreground_process(
                123,
                "node.exe",
                &[
                    r"C:\Users\herdr\AppData\Local\pi-node\current\node.exe",
                    r"C:\Users\herdr\AppData\Local\pi-node\current/node_modules/@earendil-works/pi-coding-agent/dist/bundle/cli.js",
                ],
            )],
        };

        assert_eq!(
            identify_agent_in_job(&job),
            Some((Agent::Pi, "pi".to_string()))
        );
    }

    #[test]
    fn identify_agent_in_job_detects_node_wrapped_mastracode_package_cli() {
        let job = ProcessJob {
            process_group_id: 123,
            processes: vec![foreground_process(
                123,
                "node.exe",
                &[
                    "node.exe",
                    "C:\\Users\\herdr\\AppData\\Roaming\\npm\\node_modules\\mastracode\\dist\\cli.js",
                ],
            )],
        };

        assert_eq!(
            identify_agent_in_job(&job),
            Some((Agent::Mastracode, "mastracode".to_string()))
        );
    }

    #[test]
    fn identify_agent_in_job_ignores_non_cli_pi_package_scripts() {
        for script in [
            r"C:\Users\herdr\AppData\Roaming\npm\node_modules\@earendil-works\pi-coding-agent\scripts\build.js",
            r"C:\Users\herdr\AppData\Local\pi-node\current\node_modules\@earendil-works\pi-coding-agent\dist\bundle\update.js",
            r"C:\workspace\dist\bundle\cli.js",
            r"C:\workspace\node_modules\other-package\dist\bundle\cli.js",
            r"C:\workspace\node_modules\@earendil-works\pi-coding-agent\dist\cli.exe",
            r"C:\workspace\node_modules\@earendil-works\pi-coding-agent\dist\cli.js\other.js",
            r"C:\workspace\node_modules\@earendil-works\pi-coding-agent\dist\bundle\cli.exe",
            r"C:\workspace\node_modules\@earendil-works\pi-coding-agent\dist\bundle\cli.js\other.js",
        ] {
            let job = ProcessJob {
                process_group_id: 123,
                processes: vec![foreground_process(123, "node.exe", &["node.exe", script])],
            };

            assert_eq!(identify_agent_in_job(&job), None, "script: {script}");
        }
    }

    #[test]
    fn identify_agent_in_job_detects_windows_cmd_wrapped_codex() {
        let job = ProcessJob {
            process_group_id: 123,
            processes: vec![foreground_process(
                1,
                "cmd.exe",
                &[
                    "cmd.exe",
                    "/D",
                    "/S",
                    "/C",
                    "C:\\Users\\herdr\\AppData\\Roaming\\npm\\codex.cmd --model gpt-5",
                ],
            )],
        };

        assert_eq!(
            identify_agent_in_job(&job),
            Some((Agent::Codex, "codex".to_string()))
        );
    }

    #[test]
    fn identify_agent_in_job_detects_powershell_file_wrapped_claude() {
        let job = ProcessJob {
            process_group_id: 123,
            processes: vec![foreground_process(
                1,
                "powershell.exe",
                &[
                    "powershell.exe",
                    "-NoProfile",
                    "-File",
                    "C:\\Users\\herdr\\Documents\\PowerShell\\Scripts\\claude.ps1",
                ],
            )],
        };

        assert_eq!(
            identify_agent_in_job(&job),
            Some((Agent::Claude, "claude".to_string()))
        );
    }

    // A plain shell pane launched with herdr's injected prompt integration
    // must still classify as a shell, not an agent, even though its argv now
    // carries a -Command payload.
    #[test]
    fn identify_agent_in_job_ignores_herdr_powershell_shell_integration_argv() {
        let job = ProcessJob {
            process_group_id: 123,
            processes: vec![foreground_process(
                1,
                "powershell.exe",
                &[
                    "powershell.exe",
                    "-NoExit",
                    "-Command",
                    r"if ($null -eq $global:__HerdrOriginalPrompt) { $global:__HerdrOriginalPrompt = $function:prompt; function global:prompt { $out = @(& $global:__HerdrOriginalPrompt) -join ' '; $loc = $ExecutionContext.SessionState.Path.CurrentLocation; if ($loc.Provider.Name -eq 'FileSystem') { try { [Environment]::CurrentDirectory = $loc.ProviderPath } catch {}; $esc = [string][char]27; $out += $esc + ']9;9;' + $loc.ProviderPath + $esc + '\' }; $out } }",
                ],
            )],
        };

        assert_eq!(identify_agent_in_job(&job), None);
    }

    #[test]
    fn identify_agent_in_job_detects_opencode2_as_opencode() {
        let job = ProcessJob {
            process_group_id: 123,
            processes: vec![foreground_process(
                123,
                "opencode2",
                &["opencode2", "--standalone"],
            )],
        };

        assert_eq!(
            identify_agent_in_job(&job),
            Some((Agent::OpenCode, "opencode2".to_string()))
        );
    }

    #[test]
    fn identify_agent_in_job_detects_opencode_exe_from_pnpm_package() {
        let job = ProcessJob {
            process_group_id: 123,
            processes: vec![foreground_process(
                123,
                "opencode.exe",
                &["/home/user/.local/share/pnpm/global/node_modules/opencode-ai/bin/opencode.exe"],
            )],
        };

        assert_eq!(
            identify_agent_in_job(&job),
            Some((Agent::OpenCode, "opencode.exe".to_string()))
        );
    }

    #[test]
    fn identify_agent_in_job_detects_opencode_exe_from_argv0_path() {
        let job = ProcessJob {
            process_group_id: 123,
            processes: vec![foreground_process(
                123,
                "MainThread",
                &["/home/user/.local/share/pnpm/global/node_modules/opencode-ai/bin/opencode.exe"],
            )],
        };

        assert_eq!(
            identify_agent_in_job(&job),
            Some((Agent::OpenCode, "opencode".to_string()))
        );
    }

    #[test]
    fn wrapped_agent_name_from_runtime_argv_ignores_plain_shell_flags() {
        assert_eq!(
            wrapped_agent_name_from_runtime_argv("bash", Some(&["bash".into(), "-lc".into()])),
            None
        );
    }

    #[test]
    fn identify_agent_in_job_ignores_python_c_argument_named_codex() {
        let job = ProcessJob {
            process_group_id: 123,
            processes: vec![foreground_process(
                1,
                "python3",
                &["python3", "-c", "import time; time.sleep(60)", "/tmp/codex"],
            )],
        };

        assert_eq!(identify_agent_in_job(&job), None);
    }

    #[test]
    fn identify_agent_in_job_ignores_node_eval_argument_named_codex() {
        let job = ProcessJob {
            process_group_id: 123,
            processes: vec![foreground_process(
                1,
                "node",
                &["node", "-e", "setTimeout(() => {}, 60000)", "/tmp/codex"],
            )],
        };

        assert_eq!(identify_agent_in_job(&job), None);
    }

    #[test]
    fn identify_agent_in_job_ignores_shell_c_argument_named_codex() {
        let job = ProcessJob {
            process_group_id: 123,
            processes: vec![foreground_process(
                1,
                "bash",
                &["bash", "-c", "sleep 60", "/tmp/codex"],
            )],
        };

        assert_eq!(identify_agent_in_job(&job), None);
    }

    #[test]
    fn identify_agent_in_job_detects_python_script_named_codex() {
        let job = ProcessJob {
            process_group_id: 123,
            processes: vec![foreground_process(
                1,
                "python3",
                &["python3", "/tmp/codex", "--model", "gpt-5"],
            )],
        };

        assert_eq!(
            identify_agent_in_job(&job),
            Some((Agent::Codex, "codex".to_string()))
        );
    }

    #[test]
    fn cmdline_argv0_agent_name_canonicalizes_known_aliases() {
        assert_eq!(
            cmdline_argv0_agent_name("/nix/store/example/bin/ghcs"),
            Some("copilot".to_string())
        );
    }

    #[test]
    fn cmdline_argv0_agent_name_requires_exact_agent_basename() {
        assert_eq!(cmdline_argv0_agent_name("/tmp/my-codex-helper"), None);
    }

    #[cfg(unix)]
    #[test]
    fn identify_agent_in_job_resolves_cursor_agent_symlink_argv0() {
        let dir = temp_detection_path("cursor-agent-symlink");
        std::fs::create_dir_all(&dir).expect("test directory should be created");
        let target = dir.join("cursor-agent");
        let link = dir.join("agent");
        std::fs::write(&target, b"#!/bin/sh\n").expect("target should be written");
        std::os::unix::fs::symlink(&target, &link).expect("symlink should be created");

        let argv0 = link.to_string_lossy().into_owned();
        let job = ProcessJob {
            process_group_id: 42,
            processes: vec![foreground_process(
                42,
                "MainThread",
                &[&argv0, "--use-system-ca", "/tmp/index.js"],
            )],
        };

        assert_eq!(
            identify_agent_in_job(&job),
            Some((Agent::Cursor, "cursor".to_string()))
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ps_adapter_identifies_live_native_and_script_processes() {
        assert_eq!(
            identify_process("codex", "codex --dangerously-bypass-approvals-and-sandbox"),
            Some(Agent::Codex)
        );
        assert_eq!(
            identify_process("/usr/local/bin/claude", "claude --resume"),
            Some(Agent::Claude)
        );
        assert_eq!(
            identify_process("node", "node /opt/bin/codex.js --resume"),
            Some(Agent::Codex)
        );
        assert_eq!(
            identify_process("node", "node --require /tmp/preload.js /opt/bin/gemini"),
            Some(Agent::Gemini)
        );
        assert_eq!(
            identify_process("bash", "bash /opt/bin/pi"),
            Some(Agent::Pi)
        );
        assert!(matches_process("claude-code", "claude", "claude"));
        assert!(!matches_process("codex", "claude", "claude"));
        assert!(!matches_process("unsupported", "claude", "claude"));
    }

    #[test]
    fn ps_adapter_does_not_acquire_launch_wrappers_or_unrelated_jobs() {
        for command in [
            "bash -lc codex --resume; exec bash -l",
            "bash -ic claude --continue",
            "bash -c codex",
            "bash --command codex",
        ] {
            assert_eq!(identify_process("bash", command), None, "{command}");
        }
        assert_eq!(identify_process("sleep", "sleep 30"), None);
        assert_eq!(identify_process("node", "node -e codex"), None);
        assert_eq!(identify_process("bash", "bash -l"), None);
    }
}
