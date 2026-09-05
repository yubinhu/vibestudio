//! The connection transport, plus host discovery. A target is reached one of two ways:
//! the user's own `ssh` for a real host (inheriting their keys/config/ProxyJump — "any
//! host you can already ssh to" comes free), or `wsl.exe` for a local WSL/WSL2 distro on
//! Windows (which don't run an sshd). Both run the same provisioning scripts and launch
//! the same Linux `skill-server`; only the command they shell out to differs.
//!
//! Auth on the ssh path is key-based (`BatchMode=yes`): a host that needs an interactive
//! password fails fast with a hint rather than hanging the GUI.
use std::io::{Read, Write};
use std::process::{Command, Stdio};

use skill_core::process::hidden_command;

use crate::RemoteHost;

/// Options for a one-shot remote command over ssh.
const COMMON_OPTS: &[&str] = &[
    "-o", "BatchMode=yes",
    "-o", "ConnectTimeout=15",
    "-o", "StrictHostKeyChecking=accept-new",
];

/// Extra options for the long-lived launch+tunnel ssh: fail loudly if the `-L` forward
/// can't be set up, and keepalive so a dead tunnel is noticed (not unique to launch, but
/// only the persistent connection benefits).
const LAUNCH_OPTS: &[&str] = &[
    "-o", "BatchMode=yes",
    "-o", "ConnectTimeout=15",
    // accept-new = trust-on-first-use: a host not yet in known_hosts is auto-pinned
    // (no prompt under BatchMode), but a CHANGED key is still rejected. Matches VS Code
    // Remote-SSH's first-contact behaviour; hosts you've ssh'd to before are already
    // pinned and get full strict checking.
    "-o", "StrictHostKeyChecking=accept-new",
    "-o", "ExitOnForwardFailure=yes",
    "-o", "ServerAliveInterval=15",
    "-o", "ServerAliveCountMax=3",
];

/// How we reach a target. The connection id the UI passes is `wsl:<distro>` for a WSL
/// distro, or a plain ssh alias / `user@host[:port]` otherwise.
pub enum Transport {
    Ssh { host: String },
    Wsl { distro: String },
}

impl Transport {
    /// Parse a connection id. `wsl:<distro>` → the WSL transport; anything else is an
    /// ssh destination (the host-validation in `remote_api` has already vetted the chars).
    pub fn parse(host: &str) -> Transport {
        match host.strip_prefix("wsl:") {
            Some(distro) => Transport::Wsl { distro: distro.to_string() },
            None => Transport::Ssh { host: host.to_string() },
        }
    }

    /// WSL shares the loopback with Windows (WSL2 `localhostForwarding`, WSL1's shared
    /// stack), so the server's port IS reachable on Windows directly — no `ssh -L`, and
    /// the local and remote port must match.
    pub fn same_port(&self) -> bool {
        matches!(self, Transport::Wsl { .. })
    }

    /// The program we shell out to (for error messages).
    fn program(&self) -> &'static str {
        match self {
            Transport::Ssh { .. } => "ssh",
            Transport::Wsl { .. } => "wsl.exe",
        }
    }

    /// Build a command that runs `remote_cmd` (a shell script) on the target.
    fn run_command(&self, remote_cmd: &str) -> Command {
        match self {
            Transport::Ssh { host } => {
                let mut c = hidden_command("ssh");
                // `--` ends option parsing so a host can never be read as an ssh flag.
                c.args(COMMON_OPTS).arg("--").arg(host).arg(remote_cmd);
                c
            }
            Transport::Wsl { distro } => wsl_command(distro, remote_cmd),
        }
    }

    /// Build the launch command: starts the remote server and, on the ssh path, forwards
    /// `local_port → 127.0.0.1:remote_port`. The WSL path needs no forward (see
    /// [`same_port`](Self::same_port)) so the ports passed in are equal there.
    pub fn launch_command(&self, remote_cmd: &str, local_port: u16, remote_port: u16) -> Command {
        match self {
            Transport::Ssh { host } => {
                let mut c = hidden_command("ssh");
                c.args(LAUNCH_OPTS)
                    .arg("-L")
                    .arg(format!("{local_port}:127.0.0.1:{remote_port}"))
                    .arg("--")
                    .arg(host)
                    .arg(remote_cmd);
                c
            }
            Transport::Wsl { .. } => self.run_command(remote_cmd),
        }
    }
}

/// Build the `wsl.exe` invocation for `remote_cmd`. We base64-encode the script and run a
/// tiny fixed wrapper that decodes it and runs THAT — so the script body never passes
/// through `wsl.exe`/Windows command-line quoting (the base64 alphabet has no quotes,
/// spaces, or glob chars).
///
/// The wrapper ITSELF must also be quoting-proof, because `wsl.exe` runs our
/// `bash -lc "<wrapper>"` through an extra interop shell that parses the double-quoted
/// arg first — so any `$`/backtick in the wrapper gets expanded a round too early (a
/// `$(mktemp)`/`$f` wrapper arrived at our bash as `f=…;…|base64 -d>;bash ` — the temp
/// var eaten — and died on the dangling `>`). Process substitution avoids every `$`: it
/// runs the decoded script with the PROCESS stdin intact (a `--lifeline-stdin` server, or
/// a `cat >` pipe install, inherits our held stdin — `bash` reads the script from
/// `/dev/fd`, not stdin) using only chars that survive a double-quote pass untouched.
/// `bash -l` gives a login shell so `curl`/`wget` are on PATH, mirroring ssh.
fn wsl_command(distro: &str, remote_cmd: &str) -> Command {
    let b64 = base64(remote_cmd.as_bytes());
    let wrapper = format!("bash <(echo {b64}|base64 -d)");
    let mut c = hidden_command("wsl.exe");
    c.arg("-d").arg(distro).arg("--").arg("bash").arg("-lc").arg(wrapper);
    c
}

/// Standard base64 (with `+/` and `=` padding) — decodable by coreutils/busybox
/// `base64 -d`. Inlined to keep the crate dependency-free.
fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 { T[((n >> 6) & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
    }
    out
}

pub struct RunError {
    /// The remote command's exit code, when the failure was a non-zero exit (vs. the
    /// transport itself failing to run). Provisioning uses code `3` to signal "no downloader".
    pub code: Option<i32>,
    pub message: String,
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// Run a remote command and return its stdout. Err carries the exit code plus a
/// friendly hint derived from stderr.
pub fn run(t: &Transport, remote_cmd: &str) -> Result<String, RunError> {
    let out = t
        .run_command(remote_cmd)
        .output()
        .map_err(|e| RunError { code: None, message: format!("failed to run {}: {e}", t.program()) })?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(RunError { code: out.status.code(), message: hint(t, &String::from_utf8_lossy(&out.stderr)) })
    }
}

/// Like [`run`], but maps any failure to a plain message (detection paths where the
/// exit code is irrelevant).
pub fn capture(t: &Transport, remote_cmd: &str) -> Result<String, String> {
    run(t, remote_cmd).map_err(|e| e.message)
}

/// Run a remote command feeding it `stdin` bytes (used to pipe a downloaded binary to
/// `cat > …` on a no-internet remote). Drains stdout/stderr on their own threads while
/// writing, so a chatty remote (e.g. a verbose shell rc) can't fill a pipe and deadlock
/// the multi-MB write; then closes stdin so the remote `cat` sees EOF.
pub fn run_with_stdin(t: &Transport, remote_cmd: &str, stdin: &[u8]) -> Result<(), RunError> {
    let mut child = t
        .run_command(remote_cmd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| RunError { code: None, message: format!("failed to run {}: {e}", t.program()) })?;

    let mut sin = child.stdin.take().unwrap();
    let mut out = child.stdout.take().unwrap();
    let mut err = child.stderr.take().unwrap();
    // Concurrent drains so the write below never blocks on a full output pipe.
    let out_t = std::thread::spawn(move || {
        let _ = std::io::copy(&mut out, &mut std::io::sink());
    });
    let err_t = std::thread::spawn(move || {
        let mut s = Vec::new();
        let _ = err.read_to_end(&mut s);
        s
    });

    let write_res = sin.write_all(stdin);
    drop(sin); // EOF → the remote `cat` finishes and the command exits
    let _ = out_t.join();
    let err_bytes = err_t.join().unwrap_or_default();
    let status = child.wait().map_err(|e| RunError { code: None, message: e.to_string() })?;

    if let Err(e) = write_res {
        return Err(RunError { code: None, message: format!("writing to {} failed: {e}", t.program()) });
    }
    if status.success() {
        Ok(())
    } else {
        Err(RunError { code: status.code(), message: hint(t, &String::from_utf8_lossy(&err_bytes)) })
    }
}

/// All targets the user can pick from: WSL/WSL2 distros (Windows only) first, then the
/// `~/.ssh/config` aliases. The UI also accepts a free-form `user@host` or `wsl:<distro>`.
pub fn list_targets() -> Result<Vec<RemoteHost>, String> {
    let mut hosts = list_wsl_distros();
    hosts.extend(list_ssh_hosts()?);
    Ok(hosts)
}

/// List `Host` aliases from `~/.ssh/config` (skipping wildcard patterns). These are
/// exactly the names you can `ssh <name>`.
fn list_ssh_hosts() -> Result<Vec<RemoteHost>, String> {
    let mut hosts = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let Some(home) = dirs::home_dir() else { return Ok(hosts) };
    let Ok(text) = std::fs::read_to_string(home.join(".ssh").join("config")) else {
        return Ok(hosts); // no config = no aliases; free-form entry still works
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(kw) = parts.next() else { continue };
        if !kw.eq_ignore_ascii_case("host") {
            continue;
        }
        for alias in parts {
            // Skip pattern entries (`Host *`, negations) — they aren't connectable names.
            if alias.contains('*') || alias.contains('?') || alias.starts_with('!') {
                continue;
            }
            if seen.insert(alias.to_string()) {
                hosts.push(RemoteHost { name: alias.to_string(), detail: None });
            }
        }
    }
    Ok(hosts)
}

/// Discover WSL/WSL2 distros on this Windows machine (empty everywhere else). Each is
/// offered as a `wsl:<distro>` target with a `WSL1`/`WSL2` detail badge; connecting runs
/// the same provisioning/launch as ssh, but over `wsl.exe`.
fn list_wsl_distros() -> Vec<RemoteHost> {
    if !cfg!(windows) {
        return Vec::new();
    }
    let mut out = parse_wsl_verbose();
    if out.is_empty() {
        out = parse_wsl_quiet(); // older WSL without `--verbose`
    }
    out
}

/// Run `wsl.exe <args>` and decode its UTF-16LE stdout (wsl emits wide chars).
fn wsl_output(args: &[&str]) -> Option<String> {
    let o = hidden_command("wsl.exe").args(args).output().ok()?;
    let wide: Vec<u16> = o.stdout.chunks(2).filter_map(|c| c.try_into().ok().map(u16::from_le_bytes)).collect();
    Some(String::from_utf16_lossy(&wide))
}

/// Parse `wsl --list --verbose` (NAME / STATE / VERSION columns, a `*` marks the default).
fn parse_wsl_verbose() -> Vec<RemoteHost> {
    let Some(text) = wsl_output(&["--list", "--verbose"]) else { return Vec::new() };
    let mut hosts = Vec::new();
    for line in text.lines() {
        let line = line.trim().trim_start_matches('*').trim();
        if line.is_empty() {
            continue;
        }
        let cols: Vec<&str> = line.split_whitespace().collect();
        let (Some(name), Some(ver)) = (cols.first(), cols.last()) else { continue };
        // The VERSION column is `1` or `2`; this also skips the header row (`…VERSION`).
        if *ver != "1" && *ver != "2" {
            continue;
        }
        if !connectable_distro(name) {
            continue;
        }
        hosts.push(RemoteHost { name: format!("wsl:{name}"), detail: Some(format!("WSL{ver}")) });
    }
    hosts
}

/// Fallback for WSL builds without `--verbose`: `--list --quiet` (names only).
fn parse_wsl_quiet() -> Vec<RemoteHost> {
    let Some(text) = wsl_output(&["--list", "--quiet"]) else { return Vec::new() };
    text.lines()
        .map(|l| l.trim())
        .filter(|name| connectable_distro(name))
        .map(|name| RemoteHost { name: format!("wsl:{name}"), detail: Some("WSL".into()) })
        .collect()
}

/// A distro we can offer: a non-empty name with only hostname-safe chars (so `wsl:<name>`
/// round-trips the API's host validation), excluding Docker Desktop's internal distros.
fn connectable_distro(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with("docker-desktop")
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// Turn the transport's stderr into an actionable message.
fn hint(t: &Transport, stderr: &str) -> String {
    let host = match t {
        Transport::Ssh { host } => host.as_str(),
        Transport::Wsl { distro } => distro.as_str(),
    };
    let s = stderr.trim();
    if s.contains("Permission denied") || s.contains("publickey") {
        format!("authentication to {host} failed — ensure key-based SSH access (e.g. your key is loaded in ssh-agent). ssh said: {s}")
    } else if s.contains("Could not resolve") || s.contains("Name or service not known") {
        format!("could not resolve host {host}. ssh said: {s}")
    } else if s.contains("Connection refused") || s.contains("timed out") || s.contains("Operation timed out") {
        format!("could not connect to {host}. ssh said: {s}")
    } else if s.is_empty() {
        format!("connecting to {host} failed")
    } else {
        format!("{host}: {s}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The `wsl.exe` wrapper is parsed by an extra interop shell before our `bash -lc`
    // sees it, so it must contain NO char that an outer double-quote pass would touch
    // (`$`, backtick, backslash, `"`) — otherwise `$(…)`/`$var` expand a round early and
    // the command arrives mangled (the original `$(mktemp)`/`$f` bug).
    #[test]
    fn wsl_wrapper_survives_an_outer_quote_pass() {
        let c = wsl_command("Ubuntu", "uname -sm");
        let wrapper = c.get_args().last().unwrap().to_string_lossy().into_owned();
        for bad in ['$', '`', '\\', '"'] {
            assert!(!wrapper.contains(bad), "wrapper must not contain {bad:?}: {wrapper}");
        }
        // Still decodes the script and runs it with stdin intact (process substitution,
        // not a stdin pipe), and the body is carried as opaque base64.
        assert!(wrapper.contains("base64 -d"), "decodes the payload: {wrapper}");
        assert!(wrapper.contains("bash <("), "runs via process substitution: {wrapper}");
        assert!(wrapper.contains(&base64(b"uname -sm")), "carries the b64 body: {wrapper}");
    }
}
