//! Explicit, bounded agent runtime operations: connector checks and Codex thread
//! renaming. No model turn is started and no connector tool is called. The agent
//! owns authentication and MCP startup.
//!
//! Protocol references (also checked against installed CLI schemas):
//! https://learn.chatgpt.com/docs/app-server#apps-connectors
//! https://github.com/anthropics/claude-agent-sdk-python/blob/main/src/claude_agent_sdk/_internal/query.py
//!
//! Raw agent output, headers, command arguments, and account details never leave
//! this module. Runtime processes use their own process group and are cleaned
//! up on success, protocol failure, timeout, and unwinding.

use crate::agents::{ConnectorRuntime, AGENTS};
use crate::connectors::{ConnectorInventory, ConnectorSource};
use std::path::PathBuf;
use std::sync::Mutex;

static CHECK: Mutex<()> = Mutex::new(());

pub fn discover(project: Option<&str>) -> ConnectorInventory {
    let mut inventory = ConnectorInventory::default();
    let Ok(_guard) = CHECK.try_lock() else {
        inventory.sources.push(source(
            "runtime",
            None,
            "Runtime check",
            "unavailable",
            "Another runtime connector check is already running. Try again when it finishes.",
        ));
        return inventory;
    };
    let cwd = match project.filter(|s| !s.trim().is_empty()) {
        Some(path) => match std::fs::canonicalize(path) {
            Ok(path) if path.is_dir() => path,
            _ => {
                inventory.sources.push(source(
                    "runtime",
                    None,
                    "Runtime check",
                    "error",
                    "Choose an existing project directory before checking its connectors.",
                ));
                return inventory;
            }
        },
        None => match dirs::home_dir() {
            Some(path) => path,
            None => {
                inventory.sources.push(source(
                    "runtime",
                    None,
                    "Runtime check",
                    "unavailable",
                    "The server user's home directory is unavailable.",
                ));
                return inventory;
            }
        },
    };
    #[cfg(unix)]
    std::thread::scope(|scope| {
        let workers: Vec<_> = AGENTS
            .iter()
            .filter_map(|agent| {
                let runtime = agent.connector_runtime?;
                let cwd = cwd.clone();
                Some(scope.spawn(move || unix::check(runtime, &cwd, project.is_some())))
            })
            .collect();
        for worker in workers {
            match worker.join() {
                Ok(result) => inventory.merge(result),
                Err(_) => inventory.sources.push(source(
                    "runtime",
                    None,
                    "Runtime check",
                    "error",
                    "An agent runtime check could not finish.",
                )),
            }
        }
    });
    #[cfg(not(unix))]
    {
        let _ = cwd;
        for agent in AGENTS.iter().filter(|a| a.connector_runtime.is_some()) {
            inventory.sources.push(source(&format!("runtime:{}", agent.family), Some(agent.family),
                agent.label, "unavailable", "Runtime checks are available on macOS and Linux. Local connector configuration is still discovered on this server."));
        }
    }
    inventory
}

/// Rename Codex's persisted thread through its own API, without resuming it.
/// Callers can keep a VibeStudio-owned name when the installed runtime cannot
/// perform the operation. No raw runtime output is included in the error.
pub fn rename_codex_thread(thread_id: &str, name: &str) -> Result<(), String> {
    if thread_id.trim().is_empty() || name.trim().is_empty() {
        return Err("A Codex thread ID and a nonempty name are required.".into());
    }
    #[cfg(unix)]
    {
        unix::rename_codex_thread(thread_id, name)
    }
    #[cfg(not(unix))]
    {
        Err("Native Codex thread renaming is available on macOS and Linux.".into())
    }
}

fn source(
    id: &str,
    agent: Option<&str>,
    label: &str,
    state: &str,
    message: &str,
) -> ConnectorSource {
    ConnectorSource {
        id: id.into(),
        label: label.into(),
        agent_id: agent.map(str::to_owned),
        state: state.into(),
        message: Some(message.into()),
        discovery: "runtime".into(),
    }
}

#[cfg(unix)]
mod unix {
    use super::*;
    use crate::connectors::{
        local_execution_root, local_identity_in, managed_gateway_id, remote_identity, safe_host,
        stable_id, ConnectorAvailability, ConnectorInfo,
    };
    use crate::process::hidden_command;
    use serde_json::{json, Value};
    use std::collections::{HashMap, HashSet};
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;
    use std::path::Path;
    use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
    use std::time::{Duration, Instant};

    const CHECK_TIMEOUT: Duration = Duration::from_secs(18);
    const RENAME_TIMEOUT: Duration = Duration::from_secs(5);
    const MAX_OUTPUT: usize = 8 * 1024 * 1024;
    const MAX_LINE: usize = 1024 * 1024;
    const MAX_PAGES: usize = 20;

    #[derive(Debug, Clone, Copy, PartialEq)]
    enum Failure {
        Timeout,
        OutputLimit,
        Exited,
        Protocol,
        Unsupported,
        Io,
    }

    impl Failure {
        fn message(self) -> &'static str {
            match self {
                Self::Timeout => "The agent did not finish within 18 seconds. Its temporary runtime was stopped; try again after checking the agent's login and network connection.",
                Self::OutputLimit => "The agent's response exceeded the discovery size limit. Its temporary runtime was stopped.",
                Self::Exited => "The agent exited before returning connector status. Check that the selected agent can start and is signed in on this server.",
                Self::Protocol => "The installed agent returned an unsupported connector response. Update the agent and try again.",
                Self::Unsupported => "This installed agent does not support the connector inventory protocol. Update the agent and try again.",
                Self::Io => "The agent runtime could not be started or read on this server.",
            }
        }
    }

    /// Nonblocking pipes avoid both unbounded reader threads and hangs caused
    /// by grandchildren inheriting stdout. All waits share one absolute deadline.
    struct Session {
        child: Child,
        input: ChildStdin,
        output: ChildStdout,
        buffer: Vec<u8>,
        read_bytes: usize,
        deadline: Instant,
    }

    impl Session {
        fn start(mut command: Command, deadline: Instant) -> Result<Self, Failure> {
            command
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .process_group(0);
            let mut child = command.spawn().map_err(|_| Failure::Io)?;
            let input = child.stdin.take().ok_or(Failure::Io)?;
            let output = child.stdout.take().ok_or(Failure::Io)?;
            let session = Self {
                child,
                input,
                output,
                buffer: Vec::new(),
                read_bytes: 0,
                deadline,
            };
            for fd in [session.input.as_raw_fd(), session.output.as_raw_fd()] {
                // SAFETY: both file descriptors are owned by this live Session.
                let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
                if flags < 0
                    || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
                {
                    return Err(Failure::Io);
                }
            }
            Ok(session)
        }

        fn pause(&self) -> Result<(), Failure> {
            if Instant::now() >= self.deadline {
                return Err(Failure::Timeout);
            }
            std::thread::sleep(Duration::from_millis(10));
            Ok(())
        }

        fn send(&mut self, value: &Value) -> Result<(), Failure> {
            let mut bytes = serde_json::to_vec(value).map_err(|_| Failure::Protocol)?;
            bytes.push(b'\n');
            if bytes.len() > MAX_LINE {
                return Err(Failure::OutputLimit);
            }
            let mut offset = 0;
            while offset < bytes.len() {
                if Instant::now() >= self.deadline {
                    return Err(Failure::Timeout);
                }
                match self.input.write(&bytes[offset..]) {
                    Ok(0) => return Err(Failure::Exited),
                    Ok(n) => offset += n,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => self.pause()?,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => return Err(Failure::Io),
                }
            }
            Ok(())
        }

        fn line(&mut self) -> Result<Vec<u8>, Failure> {
            loop {
                if Instant::now() >= self.deadline {
                    return Err(Failure::Timeout);
                }
                if let Some(end) = self.buffer.iter().position(|b| *b == b'\n') {
                    if end > MAX_LINE {
                        return Err(Failure::OutputLimit);
                    }
                    return Ok(self.buffer.drain(..=end).collect());
                }
                if self.buffer.len() > MAX_LINE {
                    return Err(Failure::OutputLimit);
                }
                let mut chunk = [0u8; 16 * 1024];
                match self.output.read(&mut chunk) {
                    Ok(0) => {
                        if self.buffer.is_empty() {
                            return Err(Failure::Exited);
                        }
                        return Ok(std::mem::take(&mut self.buffer));
                    }
                    Ok(n) => {
                        self.read_bytes += n;
                        if self.read_bytes > MAX_OUTPUT {
                            return Err(Failure::OutputLimit);
                        }
                        self.buffer.extend_from_slice(&chunk[..n]);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => self.pause()?,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => return Err(Failure::Io),
                }
            }
        }

        fn response(&mut self, id: &Value, claude: bool) -> Result<Value, Failure> {
            loop {
                let line = self.line()?;
                let Ok(message) = serde_json::from_slice::<Value>(&line) else {
                    continue;
                };
                if let Some(result) = response_payload(&message, id, claude) {
                    return result;
                }
                // Runtime inventory never grants approval or answers an auth
                // challenge. Stop instead of leaving an agent waiting for us.
                if (claude && message["type"] == "control_request")
                    || (!claude && message.get("method").is_some() && message.get("id").is_some())
                {
                    return Err(Failure::Unsupported);
                }
            }
        }

        fn rpc(&mut self, id: u32, method: &str, params: Value) -> Result<Value, Failure> {
            self.send(&json!({"id":id,"method":method,"params":params}))?;
            self.response(&json!(id), false)
        }

        fn control(&mut self, id: &str, request: Value) -> Result<Value, Failure> {
            self.send(&json!({"type":"control_request","request_id":id,"request":request}))?;
            self.response(&json!(id), true)
        }
    }

    impl Drop for Session {
        fn drop(&mut self) {
            // SAFETY: process_group(0) created a group led by this child. Its
            // unreaped PID cannot be reused while we send the group signal.
            unsafe {
                libc::kill(-(self.child.id() as libc::pid_t), libc::SIGKILL);
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    fn response_payload(
        message: &Value,
        id: &Value,
        claude: bool,
    ) -> Option<Result<Value, Failure>> {
        if claude {
            if message["type"] != "control_response" || &message["response"]["request_id"] != id {
                return None;
            }
            let response = &message["response"];
            return Some(if response["subtype"] == "success" {
                response
                    .get("response")
                    .cloned()
                    .filter(Value::is_object)
                    .ok_or(Failure::Protocol)
            } else {
                Err(Failure::Unsupported)
            });
        }
        if message.get("id") != Some(id) {
            return None;
        }
        if let Some(error) = message.get("error") {
            return Some(Err(
                if matches!(error["code"].as_i64(), Some(-32601 | -32602)) {
                    Failure::Unsupported
                } else {
                    Failure::Protocol
                },
            ));
        }
        Some(message.get("result").cloned().ok_or(Failure::Protocol))
    }

    pub(super) fn check(
        runtime: ConnectorRuntime,
        cwd: &Path,
        project: bool,
    ) -> ConnectorInventory {
        let deadline = Instant::now() + CHECK_TIMEOUT;
        let (agent, label) = match runtime {
            ConnectorRuntime::Claude => ("claude", "Claude Code runtime"),
            ConnectorRuntime::Codex => ("codex", "Codex account runtime"),
        };
        let mut result = ConnectorInventory::default();
        let outcome = match resolve_runtime(runtime, cwd, deadline) {
            Ok(binary) => match runtime {
                ConnectorRuntime::Claude => claude(&binary, cwd, project, deadline, &mut result),
                ConnectorRuntime::Codex => codex(&binary, cwd, deadline, &mut result),
            },
            Err(error) => Err(error),
        };
        if let Err(error) = outcome {
            let id = match runtime {
                ConnectorRuntime::Claude => "runtime:claude/mcp",
                ConnectorRuntime::Codex => "runtime:codex/apps",
            };
            result.sources.push(source(
                id,
                Some(agent),
                label,
                if error == Failure::Unsupported {
                    "unavailable"
                } else {
                    "error"
                },
                error.message(),
            ));
        }
        result
    }

    fn candidates(runtime: ConnectorRuntime) -> Vec<PathBuf> {
        let name = match runtime {
            ConnectorRuntime::Claude => "claude",
            ConnectorRuntime::Codex => "codex",
        };
        let mut paths = Vec::new();
        if let Some(path) = crate::commit_agent::resolve(&[name]) {
            paths.push(path);
        }
        if let Some(home) = dirs::home_dir() {
            match runtime {
                ConnectorRuntime::Claude => paths.extend([
                    home.join(".local/bin/claude"),
                    home.join(".claude/local/claude"),
                ]),
                ConnectorRuntime::Codex => {
                    paths.push(home.join(".codex/packages/standalone/current/codex"));
                    if let Some(dir) = std::env::var_os("CODEX_INSTALL_DIR") {
                        paths.push(PathBuf::from(dir).join("codex"));
                    }
                    #[cfg(target_os = "macos")]
                    paths.extend([
                        PathBuf::from("/Applications/ChatGPT.app/Contents/Resources/codex"),
                        PathBuf::from("/Applications/Codex.app/Contents/Resources/codex"),
                    ]);
                }
            }
            let prefix = match runtime {
                ConnectorRuntime::Claude => "anthropic.claude-code-",
                ConnectorRuntime::Codex => "openai.chatgpt-",
            };
            for root in [
                ".vscode/extensions",
                ".vscode-server/extensions",
                ".cursor/extensions",
                ".cursor-server/extensions",
            ] {
                let Ok(entries) = std::fs::read_dir(home.join(root)) else {
                    continue;
                };
                let mut dirs: Vec<_> = entries
                    .take(2048)
                    .flatten()
                    .filter(|entry| entry.file_name().to_string_lossy().starts_with(prefix))
                    .map(|entry| entry.path())
                    .collect();
                dirs.sort();
                dirs.reverse();
                for dir in dirs.into_iter().take(3) {
                    match runtime {
                        ConnectorRuntime::Claude => {
                            paths.push(dir.join("resources/native-binary/claude"))
                        }
                        ConnectorRuntime::Codex => {
                            if let Ok(arches) = std::fs::read_dir(dir.join("bin")) {
                                paths.extend(
                                    arches
                                        .take(8)
                                        .flatten()
                                        .map(|arch| arch.path().join("codex")),
                                );
                            }
                        }
                    }
                }
            }
        }
        let mut seen = HashSet::new();
        paths
            .into_iter()
            .filter_map(|p| std::fs::canonicalize(p).ok())
            .filter(|p| p.is_file() && seen.insert(p.clone()))
            .take(12)
            .collect()
    }

    fn resolve_runtime(
        runtime: ConnectorRuntime,
        cwd: &Path,
        deadline: Instant,
    ) -> Result<PathBuf, Failure> {
        for path in candidates(runtime) {
            if Instant::now() >= deadline {
                return Err(Failure::Timeout);
            }
            let mut command = hidden_command(&path);
            command.arg("--help").current_dir(cwd);
            let until = std::cmp::min(deadline, Instant::now() + Duration::from_secs(2));
            let Ok(mut child) = Session::start(command, until) else {
                continue;
            };
            // Old Codex treats unknown commands as prompts. Only launch after
            // its command table explicitly advertises app-server.
            while let Ok(line) = child.line() {
                let text = String::from_utf8_lossy(&line);
                let supported = match runtime {
                    ConnectorRuntime::Codex => text.split_whitespace().next() == Some("app-server"),
                    ConnectorRuntime::Claude => text.contains("--input-format"),
                };
                if supported {
                    return Ok(path);
                }
            }
        }
        Err(Failure::Unsupported)
    }

    pub(super) fn rename_codex_thread(thread_id: &str, name: &str) -> Result<(), String> {
        let cwd = dirs::home_dir()
            .ok_or_else(|| "The server user's home directory is unavailable.".to_string())?;
        let deadline = Instant::now() + RENAME_TIMEOUT;
        let outcome = resolve_runtime(ConnectorRuntime::Codex, &cwd, deadline).and_then(|binary| {
            let mut command = hidden_command(binary);
            command.arg("app-server").current_dir(cwd);
            codex_set_name(command, deadline, thread_id, name)
        });
        outcome.map_err(|error| {
            match error {
                Failure::Timeout => "Codex did not finish renaming the thread within five seconds.",
                Failure::Unsupported => "The installed Codex runtime does not support thread renaming.",
                Failure::OutputLimit => "Codex returned too much output while renaming the thread.",
                Failure::Exited => "Codex exited before confirming the thread rename.",
                Failure::Protocol => "Codex could not confirm the thread rename.",
                Failure::Io => "The Codex runtime could not be started or read on this server.",
            }
            .to_string()
        })
    }

    fn codex_set_name(
        command: Command,
        deadline: Instant,
        thread_id: &str,
        name: &str,
    ) -> Result<(), Failure> {
        let mut session = Session::start(command, deadline)?;
        session.rpc(1, "initialize", json!({"clientInfo":{"name":"vibestudio_session_names","title":"VibeStudio Session Names","version":env!("CARGO_PKG_VERSION")}}))?;
        session.send(&json!({"method":"initialized","params":{}}))?;
        // This method accepts a persisted rollout, so never attach or resume a
        // live terminal's thread and never create a turn just to rename it.
        let response = session.rpc(
            2,
            "thread/name/set",
            json!({"threadId":thread_id,"name":name}),
        )?;
        if !response.is_object() {
            return Err(Failure::Protocol);
        }
        Ok(())
    }

    fn codex(
        binary: &Path,
        cwd: &Path,
        deadline: Instant,
        inventory: &mut ConnectorInventory,
    ) -> Result<(), Failure> {
        let mut command = hidden_command(binary);
        command.arg("app-server").current_dir(cwd);
        let mut session = Session::start(command, deadline)?;
        session.rpc(1, "initialize", json!({"clientInfo":{"name":"vibestudio_connector_discovery","title":"VibeStudio Connectors","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true,"optOutNotificationMethods":["app/list/updated"]}}))?;
        session.send(&json!({"method":"initialized","params":{}}))?;
        if let Ok(account) = session.rpc(4, "account/read", json!({"refreshToken":false})) {
            if account.get("account").is_none_or(Value::is_null)
                || account["account"]["type"] == "apiKey"
            {
                inventory.sources.push(source("runtime:codex/apps", Some("codex"), "Codex account runtime", "unavailable",
                    "Sign in to Codex with your ChatGPT account on this server to discover account apps. Local MCP configuration is still shown."));
                return Ok(());
            }
        }
        // Prefer the installed snapshot. The broad app directory can be very
        // large, and directory availability alone is not an installation.
        let installed = session.rpc(2, "app/installed", json!({"forceRefresh":true}));
        if let Ok(response) = &installed {
            let rows = response
                .get("apps")
                .and_then(Value::as_array)
                .ok_or(Failure::Protocol)?;
            let mut metadata: Vec<Value> = rows
                .iter()
                .filter_map(|app| {
                    let id = app["id"].as_str()?;
                    Some(
                        json!({"id":id,"name":app["runtimeName"].as_str().unwrap_or(id),
                    "isAccessible":true,"isEnabled":app["enabled"]}),
                    )
                })
                .collect();
            // Seed useful results before the optional naming request, so a
            // timeout cannot hide the installed apps already reported.
            inventory
                .connectors
                .extend(codex_apps(&metadata, Some(rows)));
            let mut names_complete = true;
            for (index, batch) in metadata.chunks_mut(100).enumerate() {
                let ids: Vec<_> = batch.iter().filter_map(|app| app["id"].as_str()).collect();
                match session.rpc(
                    20 + index as u32,
                    "app/read",
                    json!({"appIds":ids,"includeTools":false}),
                ) {
                    Ok(response) => {
                        let Some(names) = response.get("apps").and_then(Value::as_array) else {
                            names_complete = false;
                            break;
                        };
                        for app in batch {
                            if let Some(name) = names
                                .iter()
                                .find(|name| name["id"] == app["id"])
                                .and_then(|name| name["name"].as_str())
                            {
                                app["name"] = json!(name);
                            }
                        }
                    }
                    Err(_) => {
                        names_complete = false;
                        break;
                    }
                }
            }
            inventory.connectors = codex_apps(&metadata, Some(rows));
            let callable = rows.iter().filter(|app| app["callable"] == true).count();
            inventory.sources.push(source("runtime:codex/apps", Some("codex"), "Codex account runtime", "scanned",
                &format!("{} installed apps; {} callable in Codex at this check. Service authentication is unverified, so enabled apps are shown as configured.{}", rows.len(), callable,
                    if names_complete { "" } else { " Some names use the runtime snapshot because app metadata was unavailable." })));
            return Ok(());
        }
        // Older app-server releases only support app/list. Filter accessible
        // entries, preserve disabled state, and never label these connected.
        let installed_error = installed.unwrap_err();
        inventory.sources.push(source(
            "runtime:codex/installed",
            Some("codex"),
            "Codex installed app status",
            if installed_error == Failure::Unsupported {
                "unavailable"
            } else {
                "error"
            },
            installed_error.message(),
        ));
        let mut page = session.rpc(3, "app/list", json!({"limit":20,"forceRefetch":true}))?;
        let mut accessible = Vec::new();
        let mut cursors = HashSet::new();
        for page_index in 0..MAX_PAGES {
            accessible.extend(
                page.get("data")
                    .and_then(Value::as_array)
                    .ok_or(Failure::Protocol)?
                    .iter()
                    .filter(|app| app["isAccessible"] == true)
                    .cloned(),
            );
            // Preserve completed pages if a later page exceeds the deadline.
            inventory.connectors = codex_apps(&accessible, None);
            let Some(cursor) = page["nextCursor"].as_str().filter(|s| !s.is_empty()) else {
                break;
            };
            if page_index + 1 == MAX_PAGES || !cursors.insert(cursor.to_owned()) {
                return Err(Failure::OutputLimit);
            }
            page = session.rpc(
                10 + page_index as u32,
                "app/list",
                json!({"cursor":cursor,"limit":20,"forceRefetch":false}),
            )?;
        }
        inventory.sources.push(source("runtime:codex/apps", Some("codex"), "Codex accessible account apps", "scanned",
            "Accessible account apps found. This older runtime could not report installed/callable state, so these entries are shown as configured."));
        Ok(())
    }

    fn codex_apps(accessible: &[Value], installed: Option<&Vec<Value>>) -> Vec<ConnectorInfo> {
        let states: HashMap<_, _> = installed
            .into_iter()
            .flatten()
            .filter_map(|app| app["id"].as_str().map(|id| (id, app)))
            .collect();
        let mut seen = HashSet::new();
        accessible
            .iter()
            .filter_map(|app| {
                if app["isAccessible"] != true {
                    return None;
                }
                let id = app["id"].as_str()?;
                if id.is_empty() || !seen.insert(id) {
                    return None;
                }
                let runtime = states.get(id);
                let state = if app["isEnabled"] == false
                    || runtime.is_some_and(|r| r["enabled"] == false)
                {
                    "disabled"
                } else {
                    "configured"
                };
                Some(ConnectorInfo {
                    id: stable_id("app", &format!("codex:{id}")),
                    name: display(app["name"].as_str().unwrap_or(id)),
                    kind: "app".into(),
                    host: None,
                    managed_connection_ids: Vec::new(),
                    availability: vec![availability(
                        "codex",
                        state,
                        "account",
                        None,
                        "runtime:codex/apps",
                        "Codex account runtime",
                    )],
                })
            })
            .collect()
    }

    fn claude(
        binary: &Path,
        cwd: &Path,
        project: bool,
        deadline: Instant,
        inventory: &mut ConnectorInventory,
    ) -> Result<(), Failure> {
        let mut command = hidden_command(binary);
        command.current_dir(cwd).env_remove("CLAUDECODE").args([
            "--print",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--no-session-persistence",
            // Hooks can run commands during startup. They are unrelated to
            // connector discovery; retain all normal MCP and auth settings.
            "--settings",
            "{\"disableAllHooks\":true}",
        ]);
        let mut session = Session::start(command, deadline)?;
        session.control("init", json!({"subtype":"initialize","hooks":null}))?;
        let mut response = session.control("mcp", json!({"subtype":"mcp_status"}))?;
        // Startup may return before configured MCPs have connected. Give that
        // background work a short chance to settle without starting a turn.
        for attempt in 0..3 {
            let servers = response
                .get("mcpServers")
                .and_then(Value::as_array)
                .ok_or(Failure::Protocol)?;
            if !servers.is_empty() && !servers.iter().any(|s| s["status"] == "pending") {
                break;
            }
            if Instant::now() + Duration::from_millis(600) >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
            response =
                session.control(&format!("mcp-{attempt}"), json!({"subtype":"mcp_status"}))?;
        }
        let servers = response
            .get("mcpServers")
            .and_then(Value::as_array)
            .ok_or(Failure::Protocol)?;
        let managed_ids: HashSet<String> = match crate::connections::list() {
            Ok(connections) => connections
                .into_iter()
                .map(|connection| connection.id)
                .collect(),
            Err(_) => {
                inventory.sources.push(source("runtime:claude/managed", Some("claude"),
                    "VibeStudio gateway matching", "error",
                    "The managed connector inventory could not be read. Runtime endpoints could not be matched to VibeStudio's managed connectors."));
                HashSet::new()
            }
        };
        inventory.connectors.extend(claude_servers(
            servers,
            if project { Some(cwd) } else { None },
            &managed_ids,
        ));
        inventory.sources.push(source("runtime:claude/mcp", Some("claude"), "Claude Code runtime", "scanned",
            &format!("Claude Code reported {} MCP servers for this runtime. Hooks were disabled; no model prompt or service tool was run.", servers.len())));
        if !servers
            .iter()
            .any(|s| s["scope"] == "claudeai" || s["config"]["type"] == "claudeai-proxy")
        {
            inventory.sources.push(source("runtime:claude/account", Some("claude"), "Claude account connectors", "unavailable",
                "This runtime exposed no Claude.ai connector inventory. That does not establish whether your Claude account has connectors; check /mcp in Claude Code with your Claude.ai login."));
        }
        Ok(())
    }

    fn claude_servers(
        servers: &[Value],
        project: Option<&Path>,
        managed_ids: &HashSet<String>,
    ) -> Vec<ConnectorInfo> {
        servers
            .iter()
            .filter_map(|server| {
                let name = server["name"].as_str()?;
                let config = &server["config"];
                let url = config["url"].as_str();
                let account = config["type"] == "claudeai-proxy" || server["scope"] == "claudeai";
                let scope = if account {
                    "account"
                } else {
                    match server["scope"].as_str() {
                        Some("project" | "local") => "project",
                        Some("managed") => "managed",
                        Some("plugin") => "plugin",
                        _ if name.starts_with("plugin:") => "plugin",
                        _ => "user",
                    }
                };
                let state = match server["status"].as_str() {
                    Some("connected") => "connected",
                    Some("needs-auth") => "needs_auth",
                    Some("disabled") => "disabled",
                    Some("failed") => "error",
                    _ => "configured",
                };
                let managed_id = url
                    .and_then(managed_gateway_id)
                    .filter(|id| !account && managed_ids.contains(id));
                let (kind, id) = if account {
                    (
                        "app",
                        stable_id(
                            "app",
                            &format!("claude:{}", config["id"].as_str().unwrap_or(name)),
                        ),
                    )
                } else if let Some(id) = &managed_id {
                    ("remote", stable_id("managed", id))
                } else if let Some(url) = url {
                    ("remote", remote_identity(url))
                } else if let Some(command) = config.get("command") {
                    let root = local_execution_root(config, project);
                    (
                        "local",
                        local_identity_in(command, config.get("args"), root.as_deref()),
                    )
                } else {
                    ("local", stable_id("runtime", &format!("claude:{name}")))
                };
                let mut observed = availability(
                    "claude",
                    state,
                    scope,
                    if scope == "project" { project } else { None },
                    "runtime:claude/mcp",
                    "Claude Code runtime",
                );
                observed.managed_connection_id = managed_id.clone();
                Some(ConnectorInfo {
                    id,
                    name: display(name),
                    kind: kind.into(),
                    host: url.and_then(safe_host),
                    managed_connection_ids: managed_id.into_iter().collect(),
                    availability: vec![observed],
                })
            })
            .collect()
    }

    fn availability(
        agent: &str,
        state: &str,
        scope: &str,
        project: Option<&Path>,
        id: &str,
        label: &str,
    ) -> ConnectorAvailability {
        ConnectorAvailability {
            agent_id: agent.into(),
            state: state.into(),
            scope: scope.into(),
            project_path: project.map(|p| p.to_string_lossy().into_owned()),
            source_id: id.into(),
            source_label: label.into(),
            source_path: None,
            managed_connection_id: None,
        }
    }

    fn display(text: &str) -> String {
        text.chars().filter(|c| !c.is_control()).take(160).collect()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        struct RenameRuntime {
            root: PathBuf,
        }

        impl RenameRuntime {
            fn new() -> Self {
                let nonce = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos();
                let root = std::env::temp_dir().join(format!(
                    "vibestudio-codex-rename-{}-{nonce}",
                    std::process::id()
                ));
                std::fs::create_dir(&root).unwrap();
                Self { root }
            }

            fn command(&self, reply: Value) -> Command {
                let mut command = hidden_command("/bin/sh");
                command
                    .args([
                        "-c",
                        r#"while IFS= read -r request; do
    printf '%s\n' "$request" >> "$1"
    case "$request" in
        *'"method":"initialize"'*) printf '%s\n' '{"id":1,"result":{}}' ;;
        *'"method":"thread/name/set"'*) printf '%s\n' "$2" ;;
    esac
done"#,
                        "codex-rename-test",
                    ])
                    .arg(self.root.join("requests.jsonl"))
                    .arg(reply.to_string());
                command
            }

            fn requests(&self) -> Vec<Value> {
                std::fs::read_to_string(self.root.join("requests.jsonl"))
                    .unwrap()
                    .lines()
                    .map(|line| serde_json::from_str(line).unwrap())
                    .collect()
            }
        }

        impl Drop for RenameRuntime {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.root);
            }
        }

        #[test]
        fn codex_rename_updates_exact_thread_without_resuming_or_starting_turn() {
            let runtime = RenameRuntime::new();
            let name = "Fix \"quoted\" path \\ 日本語";
            assert_eq!(
                codex_set_name(
                    runtime.command(json!({"id":2,"result":{}})),
                    Instant::now() + Duration::from_secs(3),
                    "thread-exact-id",
                    name,
                ),
                Ok(())
            );
            let requests = runtime.requests();
            assert_eq!(requests.len(), 3);
            assert_eq!(requests[0]["method"], "initialize");
            assert_eq!(requests[1]["method"], "initialized");
            assert_eq!(requests[2], json!({
                "id":2,
                "method":"thread/name/set",
                "params":{"threadId":"thread-exact-id","name":name}
            }));
        }

        #[test]
        fn codex_rename_requires_confirmation_and_rejects_unsupported_runtime() {
            for (reply, expected) in [
                (
                    json!({"id":2,"error":{"code":-32601,"message":"SECRET"}}),
                    Failure::Unsupported,
                ),
                (json!({"id":2,"result":null}), Failure::Protocol),
                (
                    json!({"id":7,"method":"item/tool/requestUserInput","params":{}}),
                    Failure::Unsupported,
                ),
            ] {
                let runtime = RenameRuntime::new();
                assert_eq!(
                    codex_set_name(
                        runtime.command(reply),
                        Instant::now() + Duration::from_secs(3),
                        "thread-exact-id",
                        "New title",
                    ),
                    Err(expected)
                );
                assert_eq!(runtime.requests().len(), 3);
            }
        }

        #[test]
        fn catalog_apps_are_not_connections_and_access_does_not_prove_callable() {
            let apps = vec![
                json!({"id":"catalog","name":"Catalog","isAccessible":false,"isEnabled":true}),
                json!({"id":"access","name":"Accessible","isAccessible":true,"isEnabled":true}),
                json!({"id":"callable","name":"Callable","isAccessible":true,"isEnabled":true}),
                json!({"id":"disabled","name":"Disabled","isAccessible":true,"isEnabled":false}),
            ];
            let installed = vec![
                json!({"id":"callable","enabled":true,"callable":true}),
                json!({"id":"disabled","enabled":true,"callable":true}),
            ];
            let result = codex_apps(&apps, Some(&installed));
            assert_eq!(result.len(), 3);
            assert_eq!(
                result
                    .iter()
                    .map(|r| r.availability[0].state.as_str())
                    .collect::<Vec<_>>(),
                ["configured", "configured", "disabled"]
            );
            assert!(codex_apps(&apps, None)
                .iter()
                .all(|r| r.availability[0].state != "connected"));
        }

        #[test]
        fn claude_account_and_project_status_strip_secrets() {
            let rows = vec![
                json!({"name":"Cloud","status":"connected","scope":"claudeai",
                "config":{"type":"claudeai-proxy","id":"cloud-id","url":"https://user:SECRET@example.com/mcp?token=SECRET"},
                "tools":[{"description":"SECRET"}]}),
                json!({"name":"Local","status":"needs-auth","scope":"local", "config":{"command":"node","args":["server.js"],"env":{"TOKEN":"SECRET"}}}),
            ];
            let result = claude_servers(&rows, Some(Path::new("/workspace")), &HashSet::new());
            assert_eq!(result[0].kind, "app");
            assert_eq!(result[0].availability[0].scope, "account");
            assert_eq!(result[1].availability[0].state, "needs_auth");
            assert_eq!(
                result[1].availability[0].project_path.as_deref(),
                Some("/workspace")
            );
            assert!(!serde_json::to_string(&result).unwrap().contains("SECRET"));
        }

        #[test]
        fn known_managed_gateways_merge_runtime_state_into_the_managed_connector() {
            let known = HashSet::from(["known-id".to_string()]);
            let rows = vec![
                json!({"name":"vibestudio-known-id","status":"connected","scope":"user",
                    "config":{"type":"http","url":"http://127.0.0.1:8765/gw/known-id/mcp"}}),
                json!({"name":"Unmanaged","status":"connected","scope":"user",
                    "config":{"type":"http","url":"http://127.0.0.1:8765/gw/unknown-id/mcp"}}),
                json!({"name":"Remote lookalike","status":"connected","scope":"user",
                    "config":{"type":"http","url":"https://example.test/gw/known-id/mcp"}}),
            ];
            let checked = claude_servers(&rows, None, &known);
            let mut inventory = ConnectorInventory::default();
            inventory.connectors.push(ConnectorInfo {
                id: stable_id("managed", "known-id"),
                name: "Managed service".into(),
                kind: "remote".into(),
                host: Some("service.example".into()),
                availability: Vec::new(),
                managed_connection_ids: vec!["known-id".into()],
            });
            inventory.merge(ConnectorInventory {
                connectors: checked,
                ..Default::default()
            });
            assert_eq!(inventory.connectors.len(), 3);
            let managed = inventory
                .connectors
                .iter()
                .find(|item| item.name == "Managed service")
                .unwrap();
            assert_eq!(managed.availability[0].state, "connected");
            assert_eq!(
                managed.availability[0].managed_connection_id.as_deref(),
                Some("known-id")
            );
            assert_eq!(managed.managed_connection_ids, ["known-id"]);
            assert!(inventory
                .connectors
                .iter()
                .filter(|item| item.name != "Managed service")
                .all(|item| item.managed_connection_ids.is_empty()));
        }

        #[test]
        fn local_runtime_identity_includes_the_selected_execution_root() {
            let rows = vec![
                json!({"name":"project-mcp","status":"connected","scope":"project",
                "config":{"command":"node","args":["./server.js"]}}),
            ];
            let first = claude_servers(&rows, Some(Path::new("/first")), &HashSet::new());
            let second = claude_servers(&rows, Some(Path::new("/second")), &HashSet::new());
            assert_ne!(first[0].id, second[0].id);
            assert_eq!(
                first[0].id,
                local_identity_in(
                    &json!("node"),
                    Some(&json!(["./server.js"])),
                    Some(Path::new("/first"))
                )
            );
        }

        #[test]
        fn protocol_matches_ids_and_rejects_unsupported_without_raw_error() {
            assert!(response_payload(&json!({"id":9,"result":{}}), &json!(1), false).is_none());
            assert_eq!(
                response_payload(
                    &json!({"id":1,"error":{"code":-32601,"message":"SECRET"}}),
                    &json!(1),
                    false
                ),
                Some(Err(Failure::Unsupported))
            );
            assert_eq!(
                response_payload(
                    &json!({"type":"control_response","response":{"request_id":"mcp","subtype":"success","response":{"mcpServers":[]}}}),
                    &json!("mcp"),
                    true
                ),
                Some(Ok(json!({"mcpServers":[]})))
            );
        }

        #[test]
        fn stalled_child_is_bounded_and_reaped() {
            let mut command = hidden_command("/bin/sh");
            command.args(["-c", "sleep 30"]);
            let mut session =
                Session::start(command, Instant::now() + Duration::from_millis(80)).unwrap();
            let pid = session.child.id();
            assert_eq!(session.line(), Err(Failure::Timeout));
            drop(session);
            // SAFETY: signal zero probes existence only.
            assert_eq!(unsafe { libc::kill(pid as libc::pid_t, 0) }, -1);
        }

        #[test]
        fn oversized_unterminated_output_is_capped() {
            let mut command = hidden_command("/bin/dd");
            command.args(["if=/dev/zero", "bs=1048577", "count=1"]);
            let mut session =
                Session::start(command, Instant::now() + Duration::from_secs(3)).unwrap();
            assert_eq!(session.line(), Err(Failure::OutputLimit));
        }
    }
}
