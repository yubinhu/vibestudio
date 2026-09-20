//! Read the owning machine's visible tmux screen without attaching an xterm.
//!
//! Rows are preserved (no `capture-pane -J`) because upstream detector rules use
//! line boundaries. We capture no scrollback. Commands have bounded time/output;
//! a failed or truncated read is unavailable evidence, never an empty idle screen.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_SCREEN_BYTES: usize = 256 * 1024;
const MAX_PROCESS_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetectionSnapshot {
    pub screen: String,
    /// tmux retains OSC 0/2 titles here. OSC 9;4 progress is not retained by
    /// capture-pane, so callers must not synthesize progress from ordinary text.
    pub title: String,
    pub foreground_command: String,
    pub pane_pid: u32,
    pub process_exited: bool,
    pub agent_process: Option<AgentProcess>,
}

/// Identity is internal evidence only: neither process arguments nor start data
/// are serialized through the terminal API. The start token guards PID reuse.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentProcess {
    pub pid: u32,
    pub started_at: String,
}

struct Process {
    identity: AgentProcess,
    name: String,
    command_line: String,
}

/// One process inventory serves a whole batch. A launch shell may retain the
/// tty foreground while its agent runs below it; inspect the actual descendants
/// using the bundled executable/script aliases, never arbitrary non-shell programs.
pub struct ProcessSnapshot {
    processes: HashMap<u32, Process>,
    children: HashMap<u32, Vec<u32>>,
}

impl ProcessSnapshot {
    pub fn capture() -> Result<Self, String> {
        // Take generation tokens before reading process names/arguments. A PID
        // reused during either ps call must fail the later identity check, not
        // attach an old agent's metadata to a replacement process's token.
        #[cfg(target_os = "linux")]
        let start_tokens = process_start_tokens()?;
        // Keep comm last in its own table so executable paths with spaces do not
        // shift argv parsing. Both forms work in Linux and macOS ps. They run on
        // the same owning host as tmux (including Linux when the host is WSL).
        let output = |format: &str| {
            let mut command = skill_core::process::hidden_command("ps");
            command.env("LC_ALL", "C").args(["-ww", "-axo", format]);
            bounded_output(command, MAX_PROCESS_BYTES)
        };
        let metadata = output("pid=,ppid=,lstart=,comm=")?;
        let arguments = output("pid=,args=")?;
        let snapshot = Self::parse(&metadata, &arguments);
        // Linux ps derives lstart from the wall clock and uptime; its rounded
        // value can move by a second for a process that has never restarted.
        // Use the kernel's immutable start ticks for recognized agents instead.
        #[cfg(target_os = "linux")]
        let snapshot = snapshot.with_start_tokens(|pid| {
            start_tokens
                .get(&pid)
                .cloned()
                .ok_or_else(|| "agent identity missing from initial process snapshot".into())
        })?;
        Ok(snapshot)
    }

    #[cfg(any(target_os = "linux", test))]
    fn with_start_tokens(
        mut self,
        mut start_token: impl FnMut(u32) -> Result<String, String>,
    ) -> Result<Self, String> {
        for process in self.processes.values_mut() {
            if skill_core::agent_detection::identify_process(&process.name, &process.command_line)
                .is_some()
            {
                // A disappearing/unreadable process is unavailable evidence.
                // Never fall back to a different identity format for one poll.
                process.identity.started_at = start_token(process.identity.pid)?;
            }
        }
        Ok(self)
    }

    fn parse(metadata: &str, arguments: &str) -> Self {
        let arguments: HashMap<u32, &str> = arguments
            .lines()
            .filter_map(|line| {
                let line = line.trim_start();
                let (pid, rest) = line.split_once(char::is_whitespace)?;
                Some((pid.parse().ok()?, rest.trim_start()))
            })
            .collect();
        let mut processes = HashMap::new();
        let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
        for line in metadata.lines() {
            let mut rest = line.trim_start();
            let mut take = || {
                let (token, tail) = rest.split_once(char::is_whitespace)?;
                rest = tail.trim_start();
                Some(token)
            };
            let (Some(pid), Some(ppid)) = (
                take().and_then(|value| value.parse::<u32>().ok()),
                take().and_then(|value| value.parse::<u32>().ok()),
            ) else {
                continue;
            };
            // LC_ALL=C lstart is always five tokens: Sun Sep 6 16:00:00 2026.
            let started: Option<Vec<_>> = (0..5).map(|_| take()).collect();
            let (Some(started), Some(command_line)) = (started, arguments.get(&pid)) else {
                continue;
            };
            processes.insert(
                pid,
                Process {
                    identity: AgentProcess {
                        pid,
                        started_at: started.join(" "),
                    },
                    name: rest.to_string(),
                    command_line: command_line.to_string(),
                },
            );
            children.entry(ppid).or_default().push(pid);
        }
        // Stable breadth-first traversal prefers the long-lived launch agent
        // over its same-family helper children, regardless of ps output order.
        for children in children.values_mut() {
            children.sort_unstable();
        }
        Self {
            processes,
            children,
        }
    }

    fn agent_process(&self, pane_pid: u32, expected: &str) -> Result<Option<AgentProcess>, String> {
        if !self.processes.contains_key(&pane_pid) {
            return Err("pane process changed during detection capture".into());
        }
        let mut pending = std::collections::VecDeque::from([pane_pid]);
        let mut visited = HashSet::new();
        while let Some(pid) = pending.pop_front() {
            if !visited.insert(pid) {
                continue;
            }
            let Some(process) = self.processes.get(&pid) else {
                return Err("incomplete process snapshot".into());
            };
            if skill_core::agent_detection::matches_process(
                expected,
                &process.name,
                &process.command_line,
            ) {
                return Ok(Some(process.identity.clone()));
            }
            pending.extend(self.children.get(&pid).into_iter().flatten().copied());
        }
        Ok(None)
    }
}

// Positive evidence must also be current. If an old agent exits and a new one
// starts between the batch ps and screen read, never run the new screen through
// the old generation's tracker. One small PID-only probe avoids another full ps.
fn verify_current_process(process: &AgentProcess) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    let started = process_start_token(process.pid)?;
    #[cfg(not(target_os = "linux"))]
    let started = {
        let mut command = skill_core::process::hidden_command("ps");
        command
            .env("LC_ALL", "C")
            .args(["-ww", "-p", &process.pid.to_string(), "-o", "lstart="]);
        let started = bounded_output(command, 1024)
            .map_err(|error| format!("agent identity probe: {error}"))?;
        started.split_whitespace().collect::<Vec<_>>().join(" ")
    };
    if started == process.started_at {
        Ok(())
    } else {
        Err("agent process changed during detection capture".into())
    }
}

#[cfg(target_os = "linux")]
fn process_start_tokens() -> Result<HashMap<u32, String>, String> {
    let entries = std::fs::read_dir("/proc")
        .map_err(|error| format!("process identity inventory: {error}"))?;
    Ok(entries
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
        // Unrelated processes may disappear or be inaccessible. A recognized
        // agent without a token makes the completed snapshot unavailable.
        .filter_map(|pid| process_start_token(pid).ok().map(|token| (pid, token)))
        .collect())
}

#[cfg(target_os = "linux")]
fn process_start_token(pid: u32) -> Result<String, String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .map_err(|error| format!("agent identity probe: {error}"))?;
    linux_start_token(&stat)
}

#[cfg(any(target_os = "linux", test))]
fn linux_start_token(stat: &str) -> Result<String, String> {
    // comm (field 2) may contain spaces and closing parentheses. The final ')'
    // closes it; starttime is field 22, or index 19 after that delimiter.
    let ticks = stat
        .rsplit_once(')')
        .and_then(|(_, fields)| fields.split_whitespace().nth(19))
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or("invalid process start time")?;
    Ok(format!("proc:{ticks}"))
}

// A batch inventory predates individual screen reads. Before declaring exit,
// verify the absence after the screen capture: a just-started agent must not be
// overridden to Idle using old shell-only process evidence.
fn confirmed_process(
    processes: &ProcessSnapshot,
    pane_pid: u32,
    expected: &str,
    verify_absence: bool,
    verify_current: impl FnOnce(&AgentProcess) -> Result<(), String>,
    fresh: impl FnOnce() -> Result<ProcessSnapshot, String>,
) -> Result<Option<AgentProcess>, String> {
    match processes.agent_process(pane_pid, expected) {
        Ok(Some(process)) => {
            verify_current(&process)?;
            Ok(Some(process))
        }
        absent if !verify_absence => absent,
        Ok(None) | Err(_) => {
            let process = fresh()?.agent_process(pane_pid, expected)?;
            if let Some(process) = &process {
                verify_current(process)?;
            }
            Ok(process)
        }
    }
}

/// Capture only a validated studio session. The function is read-only and never
/// selects a pane/window or changes tmux options.
pub fn capture(
    session_id: &str,
    expected: &str,
    verify_absence: bool,
    processes: &ProcessSnapshot,
) -> Result<DetectionSnapshot, String> {
    capture_with(session_id, expected, verify_absence, processes, super::tmux)
}

fn capture_with(
    session_id: &str,
    expected: &str,
    verify_absence: bool,
    processes: &ProcessSnapshot,
    tmux: impl Fn() -> Command,
) -> Result<DetectionSnapshot, String> {
    if !super::valid_session_name(session_id) {
        return Err("invalid terminal session id".into());
    }
    // The trailing colon selects a session within tmux's pane-target grammar.
    // Without it, =name is treated as a window and yields empty pane fields.
    // The equals sign still ensures an exact session match, never a prefix.
    let target = format!("={session_id}:");
    let read_metadata = || {
        let mut command = tmux();
        command.args([
            "display-message",
            "-p",
            "-t",
            &target,
            "#{pane_id}\t#{pane_pid}\t#{pane_dead}\t#{pane_current_command}\t#{pane_title}",
        ]);
        bounded_output(command, MAX_SCREEN_BYTES)
            .map_err(|error| format!("tmux pane metadata: {error}"))
    };
    let metadata = read_metadata()?;
    let mut fields = metadata.trim_end_matches('\n').splitn(5, '\t');
    let pane_id = fields.next().ok_or("missing pane id")?;
    if !pane_id
        .strip_prefix('%')
        .is_some_and(|id| !id.is_empty() && id.bytes().all(|c| c.is_ascii_digit()))
    {
        return Err("invalid pane id in tmux snapshot".into());
    }
    let pane_pid: u32 = fields
        .next()
        .and_then(|pid| pid.parse().ok())
        .ok_or("invalid pane pid")?;
    let dead = fields.next().ok_or("missing pane state")? == "1";
    let foreground_command = fields.next().ok_or("missing pane command")?.to_string();
    let title = fields.next().ok_or("missing pane title")?.to_string();
    let mut screen = tmux();
    // Default capture is the visible pane only, excluding history. Omitting -J
    // retains wrapped physical rows; detectors receive exactly those rows.
    screen.args(["capture-pane", "-p", "-t", pane_id]);
    let screen = bounded_output(screen, MAX_SCREEN_BYTES)
        .map_err(|error| format!("tmux visible screen: {error}"))?;
    if read_metadata()? != metadata {
        return Err("pane changed during detection capture".into());
    }
    let agent_process = if dead {
        None
    } else {
        confirmed_process(
            processes,
            pane_pid,
            expected,
            verify_absence,
            verify_current_process,
            ProcessSnapshot::capture,
        )?
    };
    Ok(DetectionSnapshot {
        screen,
        title,
        foreground_command,
        pane_pid,
        process_exited: agent_process.is_none(),
        agent_process,
    })
}

fn bounded_output(mut command: Command, limit: usize) -> Result<String, String> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("detection capture unavailable: {error}"))?;
    let stdout = child.stdout.take().ok_or("detection stdout unavailable")?;
    // Drain concurrently so a large screen/process table cannot wedge the child
    // on a full pipe; keep a bounded prefix and reject truncation below.
    let reader = std::thread::spawn(move || {
        let mut stream = stdout;
        let mut bytes = Vec::new();
        (&mut stream)
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)?;
        std::io::copy(&mut stream, &mut std::io::sink())?;
        Ok::<_, std::io::Error>(bytes)
    });
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err("detection capture timed out".to_string());
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(format!("detection capture failed: {error}"));
            }
        }
    };
    let bytes = reader
        .join()
        .map_err(|_| "detection reader stopped")?
        .map_err(|error| format!("detection read failed: {error}"))?;
    if !status?.success() {
        return Err("tmux/process snapshot unavailable".into());
    }
    if bytes.len() > limit {
        return Err("detection snapshot exceeds capture limit".into());
    }
    String::from_utf8(bytes).map_err(|_| "detection snapshot is not UTF-8".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn processes(rows: &[(u32, u32, &str, &str)]) -> ProcessSnapshot {
        let metadata = rows
            .iter()
            .map(|(pid, ppid, name, _)| format!("{pid} {ppid} Sun Sep 6 16:00:00 2026 {name}\n"))
            .collect::<String>();
        let args = rows
            .iter()
            .map(|(pid, _, _, args)| format!("{pid} {args}\n"))
            .collect::<String>();
        ProcessSnapshot::parse(&metadata, &args)
    }

    #[test]
    fn recognizes_real_agent_descendant_but_never_shell_wrapper_or_sleep() {
        let live = processes(&[
            (10, 1, "/bin/bash", "bash -lc codex; exec bash"),
            (11, 10, "node", "node /opt/codex/bin/codex.js"),
        ]);
        assert_eq!(live.agent_process(10, "codex").unwrap().unwrap().pid, 11);
        assert!(live.agent_process(10, "claude").unwrap().is_none());
        let exited = processes(&[(10, 1, "/bin/bash", "bash"), (12, 10, "sleep", "sleep 30")]);
        assert!(exited.agent_process(10, "codex").unwrap().is_none());
        assert!(exited.agent_process(99, "codex").is_err());
    }

    #[test]
    fn exit_requires_post_capture_process_confirmation() {
        let before = processes(&[(10, 1, "/bin/bash", "bash")]);
        let after = || {
            Ok(processes(&[
                (10, 1, "/bin/bash", "bash"),
                (11, 10, "codex", "codex"),
            ]))
        };
        assert_eq!(
            confirmed_process(&before, 10, "codex", true, |_| Ok(()), after)
                .unwrap()
                .unwrap()
                .pid,
            11
        );
        assert!(confirmed_process(
            &before,
            10,
            "codex",
            true,
            |_| Ok(()),
            || Err("ps unavailable".into())
        )
        .is_err());
    }

    #[test]
    fn process_generation_includes_start_time_and_preserves_spaced_paths() {
        let first = ProcessSnapshot::parse(
            "10 1 Sun Sep 6 16:00:00 2026 /some path/codex\n",
            "10 codex\n",
        );
        let second = ProcessSnapshot::parse(
            "10 1 Sun Sep 6 16:00:01 2026 /some path/codex\n",
            "10 codex\n",
        );
        assert_ne!(
            first.agent_process(10, "codex").unwrap(),
            second.agent_process(10, "codex").unwrap()
        );
    }

    #[test]
    fn kernel_start_token_survives_wall_clock_start_rounding_but_detects_pid_reuse() {
        let snapshot = |second: u8, ticks: &str| {
            ProcessSnapshot::parse(
                &format!("10 1 Sun Sep 6 16:00:{second:02} 2026 codex\n"),
                "10 codex\n",
            )
            .with_start_tokens(|_| Ok(format!("proc:{ticks}")))
            .unwrap()
            .agent_process(10, "codex")
            .unwrap()
            .unwrap()
        };
        let first = snapshot(0, "40705554");
        assert_eq!(first, snapshot(1, "40705554"));
        assert_ne!(first, snapshot(1, "40705654"));
    }

    #[test]
    fn unavailable_kernel_identity_does_not_fall_back_to_wall_clock() {
        let snapshot = processes(&[
            (10, 1, "bash", "bash"),
            (11, 10, "node", "node /opt/codex/bin/codex.js"),
        ]);
        assert!(snapshot
            .with_start_tokens(|pid| {
                assert_eq!(pid, 11); // unrelated shells do not need a probe
                Err("process exited during inventory".into())
            })
            .is_err());
    }

    #[test]
    fn proc_start_time_parser_handles_spaces_and_parentheses_in_process_names() {
        let fields = "S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 40705554 99";
        assert_eq!(
            linux_start_token(&format!("10 (codex (helper)) {fields}\n")).unwrap(),
            "proc:40705554"
        );
        assert!(linux_start_token("10 (codex) S 1 2").is_err());
        assert!(
            linux_start_token(&format!("10 (codex) {}", fields.replace("40705554", "bad")))
                .is_err()
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn current_process_probe_uses_kernel_identity_and_rejects_different_generation() {
        let pid = std::process::id();
        let process = AgentProcess {
            pid,
            started_at: process_start_token(pid).unwrap(),
        };
        assert!(verify_current_process(&process).is_ok());
        assert!(verify_current_process(&AgentProcess {
            pid,
            started_at: "proc:0".into(),
        })
        .is_err());
        // Old Codex metadata must not acquire the replacement PID's new token.
        // Keep the token from before the metadata read, then verify it against
        // the current process after the screen capture.
        let stale_metadata = processes(&[(pid, 1, "codex", "codex")])
            .with_start_tokens(|_| Ok("proc:0".into()))
            .unwrap();
        assert!(confirmed_process(
            &stale_metadata,
            pid,
            "codex",
            false,
            verify_current_process,
            || panic!("discard the mixed-generation capture"),
        )
        .is_err());
    }

    #[test]
    fn newly_found_process_after_absence_also_requires_current_identity() {
        let before = processes(&[(10, 1, "bash", "bash")]);
        let after = || {
            processes(&[(10, 1, "bash", "bash"), (11, 10, "codex", "codex")])
                .with_start_tokens(|_| Ok("proc:100".into()))
        };
        assert!(confirmed_process(
            &before,
            10,
            "codex",
            true,
            |process| {
                assert_eq!(process.started_at, "proc:100");
                Err("PID now belongs to a different generation".into())
            },
            after,
        )
        .is_err());
    }

    #[test]
    fn replaced_positive_identity_discards_screen_instead_of_reusing_old_tracker() {
        let before = processes(&[(10, 1, "codex", "codex")]);
        assert!(confirmed_process(
            &before,
            10,
            "codex",
            false,
            |_| Err("agent process changed".into()),
            || panic!("discard the mixed-generation capture"),
        )
        .is_err());
    }

    #[test]
    fn released_shell_does_not_need_an_extra_exit_probe() {
        let before = processes(&[(10, 1, "/bin/bash", "bash")]);
        assert!(confirmed_process(
            &before,
            10,
            "codex",
            false,
            |_| Ok(()),
            || panic!("unnecessary probe")
        )
        .unwrap()
        .is_none());
    }

    #[cfg(unix)]
    #[test]
    fn title_or_pane_change_during_capture_is_unavailable_evidence() {
        let processes = processes(&[(10, 1, "codex", "codex")]);
        let call = std::cell::Cell::new(0);
        let command = || {
            let index = call.get();
            call.set(index + 1);
            let output = match index {
                0 => "printf '%%1\t10\t0\tcodex\tAction Required\n'",
                1 => "printf 'new working screen\n'",
                _ => "printf '%%1\t10\t0\tcodex\tWorking\n'",
            };
            let mut command = skill_core::process::hidden_command("sh");
            command.args(["-c", output]);
            command
        };
        assert!(capture_with("ass-900-900-2", "codex", false, &processes, command).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn captures_visible_rows_title_and_process_without_an_attached_client() {
        if super::super::tmux().arg("-V").output().is_err() {
            eprintln!("tmux not installed — skipping detection integration test");
            return;
        }
        // A private socket keeps this test completely separate from live agents.
        let fixture_dir = std::path::PathBuf::from("/tmp").join(format!(
            "vs-detect-{}-{}",
            std::process::id(),
            super::super::new_uuid()
        ));
        std::fs::create_dir(&fixture_dir).unwrap();
        let socket = fixture_dir.join("tmux.sock");
        // A harmless script supplies a recognized live runtime/script identity.
        // Relocating a signed macOS system binary can terminate it at launch.
        use std::os::unix::fs::PermissionsExt;
        let executable = fixture_dir.join("codex");
        std::fs::write(&executable, "#!/bin/sh\n/bin/sleep 30\nprintf finished\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let tmux = || {
            let mut command = super::super::tmux();
            command.arg("-S").arg(&socket).args(["-f", "/dev/null"]);
            command
        };
        struct Cleanup(PathBuf);
        use std::path::PathBuf;
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = super::super::tmux()
                    .arg("-S")
                    .arg(self.0.join("tmux.sock"))
                    .arg("kill-server")
                    .output();
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(fixture_dir.clone());
        let id = "ass-900-900-1";
        let status = tmux()
            .args([
                "new-session",
                "-d",
                "-s",
                id,
                "-x",
                "60",
                "-y",
                "10",
                &format!(
                    "printf '\\033]2;Needs input\\007first row\\nsecond row\\n'; exec {} 30",
                    executable.display()
                ),
            ])
            .status()
            .expect("tmux required for detection integration test");
        assert!(status.success());
        let deadline = Instant::now() + Duration::from_secs(2);
        let snapshot = loop {
            let capture = ProcessSnapshot::capture()
                .and_then(|processes| capture_with(id, "codex", true, &processes, tmux));
            match capture {
                Ok(snapshot)
                    if snapshot.screen.contains("second row") && !snapshot.process_exited =>
                {
                    break snapshot
                }
                result => {
                    // Startup may legitimately race metadata/screen/identity;
                    // require a successful coherent capture before the deadline.
                    assert!(
                        Instant::now() < deadline,
                        "tmux fixture capture failed: {result:?}"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        };
        assert!(
            snapshot.screen.contains("first row\nsecond row\n"),
            "{}",
            snapshot.screen
        );
        assert_eq!(snapshot.title, "Needs input");
        assert!(!snapshot.process_exited);
        assert!(capture_with(
            "not-ours",
            "codex",
            true,
            &ProcessSnapshot::capture().unwrap(),
            tmux
        )
        .is_err());
    }

    #[cfg(unix)]
    #[test]
    fn oversized_or_failed_capture_is_never_an_empty_screen() {
        let command = |script: &str| {
            let mut command = skill_core::process::hidden_command("sh");
            command.args(["-c", script]);
            command
        };
        assert!(bounded_output(command("printf 123456789"), 4).is_err());
        assert!(bounded_output(command("exit 1"), 4).is_err());
        assert_eq!(
            bounded_output(command("printf 'a\\nb\\n'"), 4).unwrap(),
            "a\nb\n"
        );
    }
}
