//! Pure-Rust SSH transport (russh) for the **mobile switchboard**. iOS has no `ssh`
//! binary and forbids spawning a subprocess, but allows outbound sockets — so the phone
//! speaks the SSH protocol in-process, exactly as Termius does. Desktop keeps the `ssh`
//! shell-out (`ssh.rs`) so it inherits the user's `~/.ssh/config`, agent, and ProxyJump;
//! this path exists only where that machinery doesn't (feature `russh-transport`).
//!
//! It reproduces everything the shell-out gives `session.rs`/`provision.rs`: a one-shot
//! remote command ([`RusshSession::exec`]), a command fed stdin ([`exec_with_stdin`]),
//! the `ssh -L` local→remote forward ([`open_forward`]), and the held-stdin lifeline that
//! makes `skill-server --lifeline-stdin` self-exit on disconnect ([`open_lifeline`]).
//!
//! russh is async (tokio); the switchboard around it is sync (`tiny_http`). We own one
//! small multi-thread runtime and `block_on` it, so the outer API stays blocking.
//!
//! [`exec_with_stdin`]: RusshSession::exec_with_stdin
//! [`open_forward`]: RusshSession::open_forward
//! [`open_lifeline`]: RusshSession::open_lifeline
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use russh::client::{self, Config, Handle, Handler};
use russh::keys::{decode_secret_key, load_secret_key, HashAlg, PrivateKey, PrivateKeyWithHashAlg, PublicKey};
use russh::ChannelMsg;
use tokio::io::copy_bidirectional;
use tokio::net::TcpListener;
use tokio::runtime::Runtime;
use tokio::sync::oneshot;

use super::conn::{LaunchError, Remote, SessionHandle};
use super::ssh::RunError;

/// Host-key verification, trust-on-first-use. iOS has no `~/.ssh/known_hosts`, so we keep our
/// own: the first time a host is seen its key fingerprint is pinned; thereafter only that
/// exact key is accepted, and a CHANGED key is rejected (a possible MITM). `host` is keyed as
/// `host:port`.
struct HostKeyPolicy {
    host: String,
}

impl Handler for HostKeyPolicy {
    type Error = russh::Error;

    async fn check_server_key(&mut self, server_public_key: &PublicKey) -> Result<bool, Self::Error> {
        let fingerprint = server_public_key.fingerprint(HashAlg::Sha256).to_string();
        Ok(tofu_accept(&self.host, &fingerprint))
    }
}

/// Serializes TOFU reads/appends: two first-sight connections racing (a probe and a
/// connect, parallel tests) must not interleave their appends — a torn line reads
/// back as a mismatched pin and bricks the host forever (TOFU fails closed).
static TOFU_LOCK: Mutex<()> = Mutex::new(());

/// TOFU decision for `host` given the server's key `fingerprint`: pin-and-accept on first
/// sight, accept an unchanged key, reject a changed one. Fails CLOSED (reject) on any store
/// I/O error rather than trusting blindly.
fn tofu_accept(host: &str, fingerprint: &str) -> bool {
    let _serialized = TOFU_LOCK.lock().unwrap();
    let Ok(path) = known_hosts_path() else { return false };
    if let Ok(contents) = std::fs::read_to_string(&path) {
        for line in contents.lines() {
            if let Some((h, fp)) = line.split_once(' ') {
                if h == host {
                    return fp == fingerprint; // known host: match ⇒ trust, differ ⇒ reject
                }
            }
        }
    }
    // First sight — pin it, the whole line in ONE write (O_APPEND makes a single
    // write atomic, so even another PROCESS can't tear it). If we can't persist
    // the pin we can't guarantee future checks, so reject rather than
    // trust-without-recording.
    use std::io::Write;
    match std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        Ok(mut f) => f.write_all(format!("{host} {fingerprint}\n").as_bytes()).is_ok(),
        Err(_) => false,
    }
}

fn known_hosts_path() -> Result<PathBuf, String> {
    Ok(skill_core::paths::ensure_config_dir()?.join("russh_known_hosts"))
}

/// Result of a one-shot remote command (mirrors what `ssh::run`/`capture` yield).
pub struct Output {
    pub stdout: String,
    pub stderr: String,
    pub code: u32,
}

/// A live SSH session to one host. Holds its own tokio runtime; dropping it shuts the
/// runtime down and disconnects.
pub struct RusshSession {
    rt: Runtime,
    handle: Arc<Handle<HostKeyPolicy>>,
}

/// A running local→remote TCP forward (the `ssh -L` equivalent). Dropping it stops
/// accepting new connections.
pub struct Forward {
    local_port: u16,
    _stop: oneshot::Sender<()>,
}

impl Forward {
    /// The actual bound local port (useful when connecting with port 0 = ephemeral).
    pub fn local_port(&self) -> u16 {
        self.local_port
    }
}

/// The disconnect lifeline: an open exec channel whose stdin is never closed. Dropping it
/// closes the channel, EOFing the remote server's stdin so `--lifeline-stdin` self-exits —
/// the same contract the desktop gets by holding the ssh child's stdin. `alive` flips false
/// when the channel ends on its own (remote crash / dropped connection).
pub struct Lifeline {
    alive: Arc<AtomicBool>,
    _stop: oneshot::Sender<()>,
}

impl Lifeline {
    /// Whether the remote server / connection is still up.
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }
}

/// Where a connection's private key comes from: an on-disk file (the desktop/dev
/// env path), or in-memory OpenSSH text (the mobile path — the key lives in the
/// OS keystore and must never touch disk).
pub enum KeyMaterial {
    Path { path: PathBuf, passphrase: Option<String> },
    Openssh { text: String },
}

impl KeyMaterial {
    /// Decode into the key russh authenticates with. `Path` honors its passphrase;
    /// keystore-held text is stored decrypted (the keystore is the protection).
    fn load(&self) -> Result<PrivateKey, String> {
        match self {
            KeyMaterial::Path { path, passphrase } => load_secret_key(path, passphrase.as_deref())
                .map_err(|e| format!("could not load the SSH key {}: {e}", path.display())),
            KeyMaterial::Openssh { text } => decode_secret_key(text, None)
                .map_err(|e| format!("could not read the stored SSH key: {e}")),
        }
    }
}

impl RusshSession {
    /// Connect and authenticate with a private key.
    pub fn connect(host: &str, port: u16, user: &str, key: &KeyMaterial) -> Result<Self, String> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|e| format!("could not start the SSH runtime: {e}"))?;
        let key = key.load()?;
        let host = host.to_string();
        let user = user.to_string();

        let handle = rt.block_on(async {
            let config = Config {
                // Never reap an idle connection: an attached terminal or an /api/events SSE
                // stream can sit silent for minutes and MUST survive (the switchboard's top
                // runtime risk). A dead peer is caught by keepalives instead — mirroring the
                // shell-out's ServerAliveInterval=15 / CountMax=3.
                inactivity_timeout: None,
                keepalive_interval: Some(Duration::from_secs(15)),
                keepalive_max: 3,
                ..Config::default()
            };
            let policy = HostKeyPolicy { host: format!("{host}:{port}") };
            let mut h = client::connect(Arc::new(config), (host.as_str(), port), policy)
                .await
                .map_err(|e| format!("could not connect to {host}:{port}: {e}"))?;
            let auth = h
                .authenticate_publickey(user.as_str(), PrivateKeyWithHashAlg::new(Arc::new(key), None))
                .await
                .map_err(|e| format!("authentication to {host} failed: {e}"))?;
            if !auth.success() {
                return Err(format!(
                    "authentication to {host} was rejected — the key is not authorized for {user}"
                ));
            }
            Ok::<_, String>(h)
        })?;

        Ok(Self { rt, handle: Arc::new(handle) })
    }

    /// Run a command and capture its output (the `ssh::run`/`capture` equivalent).
    pub fn exec(&self, cmd: &str) -> Result<Output, String> {
        self.rt.block_on(exec_channel(&self.handle, cmd, None))
    }

    /// Run a command, feeding it `stdin`, then capture its output (the `run_with_stdin`
    /// equivalent — used to pipe a downloaded binary to `cat >` on a no-internet remote).
    pub fn exec_with_stdin(&self, cmd: &str, stdin: &[u8]) -> Result<Output, String> {
        self.rt.block_on(exec_channel(&self.handle, cmd, Some(stdin)))
    }

    /// Forward `127.0.0.1:local_port` → `remote_host:remote_port` over the session (the
    /// `ssh -L` tunnel). `local_port` 0 binds an ephemeral port; read it back with
    /// [`Forward::local_port`].
    pub fn open_forward(&self, local_port: u16, remote_host: &str, remote_port: u16) -> Result<Forward, String> {
        let handle = self.handle.clone();
        let remote_host = remote_host.to_string();
        let (stop_tx, mut stop_rx) = oneshot::channel::<()>();

        // Bind synchronously so a port collision is reported to the caller (which retries).
        let (listener, actual_port) = self.rt.block_on(async {
            let listener = TcpListener::bind(("127.0.0.1", local_port))
                .await
                .map_err(|e| format!("could not bind local port {local_port}: {e}"))?;
            let port = listener.local_addr().map_err(|e| e.to_string())?.port();
            Ok::<_, String>((listener, port))
        })?;

        self.rt.spawn(async move {
            loop {
                let accepted = tokio::select! {
                    _ = &mut stop_rx => break,
                    a = listener.accept() => a,
                };
                let Ok((mut socket, _)) = accepted else { break };
                let handle = handle.clone();
                let remote_host = remote_host.clone();
                tokio::spawn(async move {
                    let ch = match handle
                        .channel_open_direct_tcpip(remote_host, remote_port as u32, "127.0.0.1", 0)
                        .await
                    {
                        Ok(c) => c,
                        Err(_) => return, // remote refused the onward connection
                    };
                    let mut stream = ch.into_stream();
                    let _ = copy_bidirectional(&mut socket, &mut stream).await;
                });
            }
        });

        Ok(Forward { local_port: actual_port, _stop: stop_tx })
    }

    /// Exec `cmd`, read startup output until a line contains `ready_marker`, then keep the
    /// channel open as the remote server's lifeline (a drain task consumes further output
    /// so the channel never back-pressures — the async twin of the desktop's stdout/stderr
    /// drain threads). Dropping the returned [`Lifeline`] closes the channel.
    pub fn open_lifeline(&self, cmd: &str, ready_marker: &str) -> Result<Lifeline, String> {
        let handle = self.handle.clone();
        let cmd = cmd.to_string();
        let marker = ready_marker.to_string();
        let (stop_tx, mut stop_rx) = oneshot::channel::<()>();
        let alive = Arc::new(AtomicBool::new(true));
        let alive_task = alive.clone();

        self.rt.block_on(async move {
            let mut ch = handle
                .channel_open_session()
                .await
                .map_err(|e| format!("could not open the remote session: {e}"))?;
            ch.exec(true, cmd).await.map_err(|e| format!("could not launch the remote server: {e}"))?;

            let mut out = String::new();
            let mut err = String::new();
            let ready = tokio::time::timeout(Duration::from_secs(30), async {
                loop {
                    match ch.wait().await {
                        Some(ChannelMsg::Data { data }) => {
                            out.push_str(&String::from_utf8_lossy(&data));
                            if out.lines().any(|l| l.contains(&marker)) {
                                return true;
                            }
                        }
                        Some(ChannelMsg::ExtendedData { data, ext: 1 }) => {
                            err.push_str(&String::from_utf8_lossy(&data)); // capture stderr for diagnosis
                        }
                        Some(ChannelMsg::Close) | None => return false,
                        _ => {} // exit-status / window adjust — keep waiting
                    }
                }
            })
            .await
            .map_err(|_| format!("timed out waiting for the remote server to become ready{}", tail(&err)))?;

            if !ready {
                return Err(format!("the remote server exited before it was ready{}", tail(&err)));
            }

            // Own the channel in a drain task until we're told to stop; dropping it then
            // closes the channel → remote stdin EOF → the server exits. When the channel ends
            // on its own (remote crash / dropped connection) mark the session dead so the
            // monitor can recover to Local.
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = &mut stop_rx => break,
                        msg = ch.wait() => {
                            if matches!(msg, None | Some(ChannelMsg::Close)) { break; }
                        }
                    }
                }
                alive_task.store(false, Ordering::Relaxed);
            });
            Ok::<_, String>(())
        })?;

        Ok(Lifeline { alive, _stop: stop_tx })
    }
}

/// Open a session channel, exec `cmd` (optionally feeding `stdin` then EOF), and drain it
/// to completion collecting stdout/stderr and the exit status.
async fn exec_channel(handle: &Handle<HostKeyPolicy>, cmd: &str, stdin: Option<&[u8]>) -> Result<Output, String> {
    let mut ch = handle
        .channel_open_session()
        .await
        .map_err(|e| format!("could not open the remote session: {e}"))?;
    ch.exec(true, cmd).await.map_err(|e| format!("could not run the remote command: {e}"))?;
    if let Some(bytes) = stdin {
        ch.data_bytes(bytes.to_vec()).await.map_err(|e| format!("could not send input: {e}"))?;
        ch.eof().await.map_err(|e| format!("could not finish input: {e}"))?;
    }

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut code = 0u32;
    loop {
        match ch.wait().await {
            Some(ChannelMsg::Data { data }) => stdout.extend_from_slice(&data),
            Some(ChannelMsg::ExtendedData { data, ext: 1 }) => stderr.extend_from_slice(&data),
            Some(ChannelMsg::ExitStatus { exit_status }) => code = exit_status,
            Some(ChannelMsg::Close) | None => break,
            _ => {}
        }
    }
    Ok(Output {
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        code,
    })
}

/// Connection credentials for the russh transport. On the phone these come from a stored
/// connection profile (host/port/user + a Keychain-held key); off-device they can be supplied
/// via env (see [`creds_for`]).
pub struct RusshCreds {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub key: KeyMaterial,
}

/// Resolve credentials for a connection id: `Ok(None)` falls back to the `ssh` shell-out.
///
/// The device's [`SecureStore`] wins — an id matching a saved profile connects with its
/// Keychain-held key, and a profile whose key has gone missing is an ERROR (the user must
/// re-add the connection), never a silent fall-through to a transport iOS doesn't have.
/// Off-device (dev/tests) credentials come from the environment instead, gated on
/// `VIBESTUDIO_RUSSH=1`: the id is parsed as `user@host[:port]`, the key path is
/// `VIBESTUDIO_RUSSH_KEY` (optional `VIBESTUDIO_RUSSH_PASSPHRASE`).
///
/// [`SecureStore`]: crate::SecureStore
pub fn creds_for(host: &str, store: Option<&dyn crate::SecureStore>) -> Result<Option<RusshCreds>, String> {
    if let Some(store) = store {
        if let Some(profile) = store.get_profile(host)? {
            let text = store.get_private_key(host)?.ok_or_else(|| {
                format!("The key for {host} is missing from the keystore — remove the connection and add it again.")
            })?;
            return Ok(Some(RusshCreds {
                host: profile.host,
                port: profile.port,
                user: profile.user,
                key: KeyMaterial::Openssh { text },
            }));
        }
    }
    if std::env::var("VIBESTUDIO_RUSSH").ok().as_deref() != Some("1") {
        return Ok(None);
    }
    let Some((user, h, port)) = parse_target(host) else { return Ok(None) };
    let Some(path) = std::env::var("VIBESTUDIO_RUSSH_KEY").ok().filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let passphrase = std::env::var("VIBESTUDIO_RUSSH_PASSPHRASE").ok().filter(|s| !s.is_empty());
    Ok(Some(RusshCreds { host: h, port, user, key: KeyMaterial::Path { path: path.into(), passphrase } }))
}

/// Parse `user@host[:port]` (port defaults to 22).
fn parse_target(spec: &str) -> Option<(String, String, u16)> {
    let (user, hostport) = spec.split_once('@')?;
    if user.is_empty() || hostport.is_empty() {
        return None;
    }
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse().ok()?),
        None => (hostport.to_string(), 22),
    };
    Some((user.to_string(), host, port))
}

/// The [`Remote`] impl backed by a persistent russh session (one auth, reused for every
/// command + the tunnel + the lifeline). Held in an `Arc` so a live `RusshSessionHandle` keeps
/// the session — and its runtime — alive after the connect flow returns.
pub struct RusshRemote {
    session: Arc<RusshSession>,
}

impl RusshRemote {
    pub fn connect(creds: RusshCreds) -> Result<Self, String> {
        let session = RusshSession::connect(&creds.host, creds.port, &creds.user, &creds.key)?;
        Ok(Self { session: Arc::new(session) })
    }
}

impl Remote for RusshRemote {
    fn capture(&self, cmd: &str) -> Result<String, String> {
        let out = self.session.exec(cmd)?;
        if out.code == 0 {
            Ok(out.stdout)
        } else {
            Err(cmd_error(&out))
        }
    }

    fn run(&self, cmd: &str) -> Result<String, RunError> {
        let out = self.session.exec(cmd).map_err(|m| RunError { code: None, message: m })?;
        if out.code == 0 {
            Ok(out.stdout)
        } else {
            Err(RunError { code: Some(out.code as i32), message: cmd_error(&out) })
        }
    }

    fn run_with_stdin(&self, cmd: &str, stdin: &[u8]) -> Result<(), RunError> {
        let out = self.session.exec_with_stdin(cmd, stdin).map_err(|m| RunError { code: None, message: m })?;
        if out.code == 0 {
            Ok(())
        } else {
            Err(RunError { code: Some(out.code as i32), message: cmd_error(&out) })
        }
    }

    fn same_port(&self) -> bool {
        false // russh always forwards a distinct local port to the remote loopback
    }

    fn open_session(
        &self,
        remote_cmd: &str,
        local_port: u16,
        remote_port: u16,
        host: &str,
    ) -> Result<Box<dyn SessionHandle>, LaunchError> {
        // Launch the server and wait for READY (the lifeline), THEN open the forward — by then
        // the server is listening, so there's no connect-before-bind race.
        let lifeline = self.session.open_lifeline(remote_cmd, "SKILL_SERVER_READY").map_err(|e| classify_launch(host, e))?;
        let forward = self.session.open_forward(local_port, "127.0.0.1", remote_port).map_err(LaunchError::PortConflict)?;
        Ok(Box::new(RusshSessionHandle {
            forward: Some(forward),
            lifeline: Some(lifeline),
            _session: self.session.clone(),
        }))
    }
}

/// The russh [`SessionHandle`]: the forward + the lifeline, plus an `Arc` keeping the session
/// (and its tokio runtime) alive for as long as the connection is held.
struct RusshSessionHandle {
    forward: Option<Forward>,
    lifeline: Option<Lifeline>,
    _session: Arc<RusshSession>,
}

impl SessionHandle for RusshSessionHandle {
    fn is_alive(&self) -> bool {
        self.lifeline.as_ref().is_some_and(|l| l.is_alive())
    }
    fn teardown(&mut self) {
        // Drop the forward (stop accepting) and the lifeline (close the channel → remote
        // stdin EOF → the server exits). The session Arc drops with the handle.
        self.forward.take();
        self.lifeline.take();
    }
}

/// Turn a failed command's output into a one-line message (exit codes carry the real signal;
/// this is for humans / logs).
fn cmd_error(out: &Output) -> String {
    let s = out.stderr.trim();
    if s.is_empty() {
        format!("remote command exited {}", out.code)
    } else {
        s.to_string()
    }
}

/// Classify a lifeline-launch failure the way the ssh path does: a remote bind clash is
/// retriable (fresh ports), everything else is fatal.
fn classify_launch(host: &str, msg: String) -> LaunchError {
    if msg.contains("Address already in use") || msg.contains("failed to bind") {
        LaunchError::PortConflict(msg)
    } else {
        LaunchError::Fatal(format!("remote server on {host}: {msg}"))
    }
}

/// Append a remote stderr tail to an error message, if any.
fn tail(stderr: &str) -> String {
    let s = stderr.trim();
    if s.is_empty() {
        String::new()
    } else {
        format!(" — remote said: {s}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// An in-memory SecureStore standing in for the iOS Keychain-backed one.
    struct MemStore {
        profiles: Vec<crate::SshProfile>,
        keys: Mutex<HashMap<String, String>>,
    }

    impl crate::SecureStore for MemStore {
        fn list_profiles(&self) -> Result<Vec<crate::SshProfile>, String> {
            Ok(self.profiles.clone())
        }
        fn get_profile(&self, id: &str) -> Result<Option<crate::SshProfile>, String> {
            Ok(self.profiles.iter().find(|p| p.id == id).cloned())
        }
        fn put_profile(&self, _profile: &crate::SshProfile, _key: &str) -> Result<(), String> {
            unimplemented!("not exercised here")
        }
        fn delete_profile(&self, _id: &str) -> Result<(), String> {
            unimplemented!("not exercised here")
        }
        fn get_private_key(&self, id: &str) -> Result<Option<String>, String> {
            Ok(self.keys.lock().unwrap().get(id).cloned())
        }
    }

    fn profile(id: &str) -> crate::SshProfile {
        crate::SshProfile { id: id.into(), host: "pi.local".into(), port: 2022, user: "harvey".into() }
    }

    // The store is the mobile path's source of truth: a saved profile resolves to
    // in-memory key material (never a disk path), carrying the profile's own
    // host/port/user — not a parse of the connection id.
    #[test]
    fn creds_resolve_from_the_store_first() {
        let id = "harvey@pi.local:2022";
        let store = MemStore {
            profiles: vec![profile(id)],
            keys: Mutex::new(HashMap::from([(id.to_string(), "KEYTEXT".to_string())])),
        };
        let creds = creds_for(id, Some(&store)).expect("no store error").expect("resolves");
        assert_eq!((creds.host.as_str(), creds.port, creds.user.as_str()), ("pi.local", 2022, "harvey"));
        assert!(
            matches!(creds.key, KeyMaterial::Openssh { ref text } if text == "KEYTEXT"),
            "key must be the stored text, in memory"
        );
    }

    // A profile whose key vanished from the keystore must ERROR — falling through
    // to the ssh shell-out would be a dead end on iOS and a confusing one anywhere.
    #[test]
    fn missing_stored_key_is_an_error_not_a_fallthrough() {
        let id = "harvey@pi.local:2022";
        let store = MemStore { profiles: vec![profile(id)], keys: Mutex::new(HashMap::new()) };
        let Err(e) = creds_for(id, Some(&store)) else { panic!("must error") };
        assert!(e.contains("missing"), "should say the key is missing: {e}");
    }

    // An id with no saved profile falls through (→ the ssh shell-out on desktop);
    // the store's presence alone must not capture every connect.
    #[test]
    fn unknown_id_falls_through_to_the_default_transport() {
        let store = MemStore { profiles: vec![], keys: Mutex::new(HashMap::new()) };
        // (Relies on VIBESTUDIO_RUSSH not being set to 1 in the test env, same as
        // every non-opt-in run of this suite.)
        assert!(creds_for("some-alias", Some(&store)).expect("no error").is_none());
    }
}

// Integration test against a REAL OpenSSH server. Skipped unless RUSSH_IT_* are set, so
// `cargo test` in CI (no sshd) is a no-op; run it by pointing the vars at a live host:
//   RUSSH_IT_HOST=127.0.0.1 RUSSH_IT_PORT=2222 RUSSH_IT_USER=$USER \
//   RUSSH_IT_KEY=/path/to/key cargo test -p skill-server --features russh-transport -- --nocapture
#[cfg(test)]
mod it {
    use super::*;
    use std::io::Read;

    fn target() -> Option<(String, u16, String, String)> {
        Some((
            std::env::var("RUSSH_IT_HOST").ok()?,
            std::env::var("RUSSH_IT_PORT").ok()?.parse().ok()?,
            std::env::var("RUSSH_IT_USER").ok()?,
            std::env::var("RUSSH_IT_KEY").ok()?,
        ))
    }

    #[test]
    fn russh_transport_end_to_end() {
        let Some((host, port, user, key)) = target() else {
            eprintln!("skipping: set RUSSH_IT_HOST/PORT/USER/KEY to run against a live sshd");
            return;
        };
        let key = KeyMaterial::Path { path: key.into(), passphrase: None };
        let sess = RusshSession::connect(&host, port, &user, &key).expect("connect+auth");

        // 1. exec + capture stdout.
        let out = sess.exec("echo hello_russh; uname -s").expect("exec");
        assert!(out.stdout.contains("hello_russh"), "stdout was {:?}", out.stdout);
        assert_eq!(out.code, 0, "exit code; stderr={:?}", out.stderr);

        // 2. non-zero exit is reported.
        let bad = sess.exec("exit 7").expect("exec exit 7");
        assert_eq!(bad.code, 7);

        // 3. stdin is delivered (the `cat >` provisioning path).
        let piped = sess.exec_with_stdin("cat", b"piped_payload_42").expect("exec_with_stdin");
        assert_eq!(piped.stdout, "piped_payload_42");

        // 4. the -L forward: tunnel to the sshd's own port; the far side must greet us with
        //    an SSH identification banner, proving bytes flow local→remote and back.
        let fwd = sess.open_forward(0, "127.0.0.1", port).expect("open_forward");
        let mut conn = std::net::TcpStream::connect(("127.0.0.1", fwd.local_port())).expect("connect fwd");
        conn.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut banner = [0u8; 8];
        conn.read_exact(&mut banner).expect("read banner through forward");
        assert_eq!(&banner, b"SSH-2.0-", "forwarded banner was {:?}", String::from_utf8_lossy(&banner));

        // 5. the lifeline: read a READY marker while holding stdin open, like spawn_session.
        let life = sess.open_lifeline("echo SKILL_SERVER_READY; cat", "SKILL_SERVER_READY").expect("lifeline");
        drop(life); // closing it must not hang the runtime
    }

    // The switchboard's top runtime risk: an attached terminal / SSE stream that sits idle
    // for minutes must NOT be reaped mid-tunnel. Proven here by dripping one connection with
    // an 18s silent gap (past keepalive_interval=15s) and requiring the post-gap event to
    // still arrive. Opt-in (RUSSH_IT_SOAK=1) since it costs ~20s of wall time.
    #[test]
    fn russh_forward_survives_idle_gap() {
        let Some((host, port, user, key)) = target() else {
            eprintln!("skipping: set RUSSH_IT_* to run against a live sshd");
            return;
        };
        if std::env::var("RUSSH_IT_SOAK").is_err() {
            eprintln!("skipping soak: set RUSSH_IT_SOAK=1 (~20s)");
            return;
        }

        // A local "SSE-like" server: on one connection, send EVENT-A, go silent past the
        // keepalive window, then send EVENT-B.
        let drip = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let drip_port = drip.local_addr().unwrap().port();
        std::thread::spawn(move || {
            use std::io::Write;
            if let Ok((mut s, _)) = drip.accept() {
                let _ = s.write_all(b"EVENT-A\n");
                let _ = s.flush();
                std::thread::sleep(Duration::from_secs(18));
                let _ = s.write_all(b"EVENT-B\n");
                let _ = s.flush();
                std::thread::sleep(Duration::from_secs(2));
            }
        });

        let key = KeyMaterial::Path { path: key.into(), passphrase: None };
        let sess = RusshSession::connect(&host, port, &user, &key).expect("connect");
        let fwd = sess.open_forward(0, "127.0.0.1", drip_port).expect("open_forward");
        let mut conn = std::net::TcpStream::connect(("127.0.0.1", fwd.local_port())).unwrap();
        conn.set_read_timeout(Some(Duration::from_secs(25))).unwrap();

        let mut got = Vec::new();
        let mut chunk = [0u8; 64];
        loop {
            match conn.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    got.extend_from_slice(&chunk[..n]);
                    if got.windows(7).any(|w| w == b"EVENT-B") {
                        break;
                    }
                }
                Err(_) => break, // read timeout = the tunnel stalled
            }
        }
        let seen = String::from_utf8_lossy(&got);
        assert!(seen.contains("EVENT-A"), "never got the first event: {seen:?}");
        assert!(seen.contains("EVENT-B"), "idle forward was reaped before the post-gap event: {seen:?}");
    }
}
