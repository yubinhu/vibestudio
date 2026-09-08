//! The transport seam. [`Remote`] is "a way to reach the target: run one-shot commands and
//! open a tunnel/keepalive session to its durable host service." Two impls, chosen by [`build_remote`]:
//!
//! - [`SshRemote`] (desktop) shells out to the user's `ssh` / `wsl.exe` — it inherits their
//!   keys, `~/.ssh/config`, agent, and ProxyJump ("any host you can already ssh to").
//! - `RusshRemote` (the mobile switchboard, feature `russh-transport`) speaks SSH in-process
//!   via russh, because iOS can't spawn `ssh`.
//!
//! `session.rs`/`provision.rs` are written against the trait, so ONE connect orchestration
//! (detect → provision → launch/reattach → tunnel + keep-alive + monitor) drives both. The
//! desktop path here is the pre-seam `session.rs` code moved verbatim, so its behaviour is
//! unchanged.
use std::io::{BufRead, BufReader, Read};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::ssh::{self, RunError, Transport};

/// A remote reachable by the switchboard.
pub trait Remote: Send + Sync {
    /// Run a command, mapping any failure to a plain message (detection paths).
    fn capture(&self, cmd: &str) -> Result<String, String>;
    /// Run a command; `Ok(stdout)` or a [`RunError`] carrying the exit code.
    fn run(&self, cmd: &str) -> Result<String, RunError>;
    /// Run a command, feeding it `stdin` (the piped-binary provisioning path).
    fn run_with_stdin(&self, cmd: &str, stdin: &[u8]) -> Result<(), RunError>;
    /// WSL shares Windows loopback (local port == remote, no `-L`); everything else forwards.
    fn same_port(&self) -> bool;
    /// Forward `local_port → 127.0.0.1:remote_port`, hold a tunnel-only keepalive
    /// channel, and wait for its READY line. The worker was started separately. `host` is only for
    /// error messages.
    fn open_session(
        &self,
        remote_cmd: &str,
        local_port: u16,
        remote_port: u16,
        host: &str,
    ) -> Result<Box<dyn SessionHandle>, LaunchError>;
}

/// A live tunnel and keepalive channel, independent from the worker process.
pub trait SessionHandle: Send {
    /// Non-blocking: is the tunnel/connection still up? (drives the disconnect monitor.)
    fn is_alive(&self) -> bool;
    /// Close this accessor's tunnel; the host service and its agents keep running.
    fn teardown(&mut self);
}

/// Why a launch attempt failed. `PortConflict` is retried with fresh ports; `Fatal` isn't.
pub enum LaunchError {
    /// A local or remote port was taken — retry with fresh ports.
    PortConflict(String),
    /// Auth/connectivity/other — don't retry.
    Fatal(String),
}

/// Pick the transport for `host`. Desktop → the user's `ssh`/`wsl.exe`. The mobile
/// switchboard (feature `russh-transport`) → russh with the stored/env-supplied
/// credentials; falling through to `ssh` only if no russh credentials resolve (so a desktop
/// build compiled with the feature still behaves normally). On iOS there is no `ssh` to
/// fall through to — an unresolved id is an error with the actual fix in it.
#[cfg_attr(not(feature = "russh-transport"), allow(unused_variables))]
pub fn build_remote(host: &str, store: Option<&dyn crate::SecureStore>) -> Result<Box<dyn Remote>, String> {
    #[cfg(feature = "russh-transport")]
    {
        if let Some(creds) = super::russh_tx::creds_for(host, store)? {
            return Ok(Box::new(super::russh_tx::RusshRemote::connect(creds)?));
        }
    }
    #[cfg(target_os = "ios")]
    {
        return Err(format!("No saved connection for “{host}” — add one (host, user, and a key) first."));
    }
    #[allow(unreachable_code)]
    Ok(Box::new(SshRemote::new(host)))
}

/// Desktop transport: the user's own `ssh` / `wsl.exe` (see [`Transport`]).
pub struct SshRemote {
    transport: Transport,
}

impl SshRemote {
    pub fn new(host: &str) -> Self {
        Self { transport: Transport::parse(host) }
    }
}

impl Remote for SshRemote {
    fn capture(&self, cmd: &str) -> Result<String, String> {
        ssh::capture(&self.transport, cmd)
    }
    fn run(&self, cmd: &str) -> Result<String, RunError> {
        ssh::run(&self.transport, cmd)
    }
    fn run_with_stdin(&self, cmd: &str, stdin: &[u8]) -> Result<(), RunError> {
        ssh::run_with_stdin(&self.transport, cmd, stdin)
    }
    fn same_port(&self) -> bool {
        self.transport.same_port()
    }
    fn open_session(
        &self,
        remote_cmd: &str,
        local_port: u16,
        remote_port: u16,
        host: &str,
    ) -> Result<Box<dyn SessionHandle>, LaunchError> {
        spawn_session(&self.transport, host, remote_cmd, local_port, remote_port)
    }
}

/// The desktop session: one `ssh`/`wsl.exe` child that is simultaneously the tunnel, the
/// lifeline (its held stdin), and the READY reader.
struct ProcSession {
    /// Shared so the monitor can `try_wait` it while `teardown` can kill it.
    child: Arc<Mutex<Child>>,
    _stdin: ChildStdin, // held open = the tunnel-only `cat` keepalive
}

impl SessionHandle for ProcSession {
    fn is_alive(&self) -> bool {
        // "Still running" is the only non-exit case; Some(status) or a wait error (already
        // reaped by teardown) both mean the child is gone.
        match self.child.lock() {
            Ok(mut c) => matches!(c.try_wait(), Ok(None)),
            Err(_) => false, // poisoned mutex — treat as gone
        }
    }
    fn teardown(&mut self) {
        // Kill only the ssh/WSL tunnel child. The detached host service owns no
        // descriptor from this channel. (`_stdin` also closes as the struct drops.)
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Spawn the transport child for `remote_cmd` (which either launches the server or, on a
/// reattach, just tunnels), hold its stdin as the lifeline/keepalive, and block until the
/// READY line before returning the live handle. (Moved verbatim from the pre-seam
/// `session.rs`.)
fn spawn_session(
    transport: &Transport,
    host: &str,
    remote_cmd: &str,
    local_port: u16,
    remote_port: u16,
) -> Result<Box<dyn SessionHandle>, LaunchError> {
    // The transport builds the right invocation: ssh with `-L` (and `--` so a host beginning
    // with `-` can't be read as an option), or `wsl.exe` with the script base64-wrapped.
    let mut child = transport
        .launch_command(remote_cmd, local_port, remote_port)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| LaunchError::Fatal(format!("failed to start the remote connection: {e}")))?;

    let stdin = child.stdin.take().unwrap(); // HOLD = tunnel-only keepalive
    let stdout = child.stdout.take().unwrap();

    // Drain stdout on a thread, forwarding lines until the READY line (or the child dies).
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(LaunchError::Fatal(format!("timed out waiting for the remote server on {host}")));
        }
        match rx.recv_timeout(remaining.min(Duration::from_millis(500))) {
            Ok(line) if is_ready(&line) => {
                // Drain stderr for the session's lifetime too, so a chatty ssh can't fill a
                // pipe and wedge the tunnel.
                if let Some(mut err) = child.stderr.take() {
                    std::thread::spawn(move || {
                        let _ = std::io::copy(&mut err, &mut std::io::sink());
                    });
                }
                return Ok(Box::new(ProcSession { child: Arc::new(Mutex::new(child)), _stdin: stdin }));
            }
            Ok(_) => {} // some other startup line — keep waiting for READY
            Err(mpsc::RecvTimeoutError::Timeout) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                // The child may have exited (remote bind conflict, auth failure, …).
                if let Ok(Some(_)) = child.try_wait() {
                    return Err(classify_exit(host, &read_stderr(&mut child)));
                }
            }
        }
    }
}

fn classify_exit(host: &str, stderr: &str) -> LaunchError {
    let s = stderr.trim();
    if s.contains("Address already in use") || s.contains("failed to bind") {
        LaunchError::PortConflict(s.to_string())
    } else if s.contains("Permission denied") || s.contains("publickey") {
        LaunchError::Fatal(format!(
            "authentication to {host} failed — ensure key-based SSH access (ssh-agent). ssh said: {s}"
        ))
    } else if s.is_empty() {
        LaunchError::Fatal(format!("the remote server on {host} exited before it was ready"))
    } else {
        LaunchError::Fatal(format!("remote server on {host} failed to start: {s}"))
    }
}

fn is_ready(line: &str) -> bool {
    line.trim_start().starts_with("SKILL_SERVER_READY")
}

fn read_stderr(child: &mut Child) -> String {
    let mut s = String::new();
    if let Some(mut e) = child.stderr.take() {
        let _ = e.read_to_string(&mut s);
    }
    s
}
