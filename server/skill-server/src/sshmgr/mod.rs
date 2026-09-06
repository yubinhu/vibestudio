//! The remote connection manager — the `RemoteControl` impl that a `skill-server`
//! exposes over `/api/remote/*`. It shells out to the system `ssh` (inheriting the
//! user's keys/config/ProxyJump) or, for a local WSL/WSL2 distro on Windows, to
//! `wsl.exe`; it provisions a version-pinned `skill-server` on the target, launches it
//! on a loopback port, and reaches it (`ssh -L` tunnel, or WSL's
//! shared loopback); the local server then proxies `/api/*` to it (see `proxy.rs`).
//!
//! Lives server-side so BOTH entry points get it identically: the desktop's
//! in-process server and the standalone `skill-server` binary (browser-local dev, or
//! a dev box). A *provisioned remote* server leaves `ServerConfig::remote = None`
//! (it's launched with `--lifeline-stdin`), so there's no surprise nested onward-ssh.
use std::sync::{Arc, Mutex};

use crate::{RemoteControl, RemoteHost, RemoteStatus, RemoteTarget, SecureStore};

// The transport seam (Remote/SessionHandle): both the ssh/wsl shell-out and russh plug in
// here, so one connect orchestration drives both.
mod conn;
mod lastconn;
mod provision;
// Pure-Rust SSH transport for the mobile switchboard (iOS can't spawn `ssh`). Desktop
// keeps `ssh.rs`; these compile only under the `russh-transport` feature.
#[cfg(feature = "russh-transport")]
pub mod keygen;
#[cfg(feature = "russh-transport")]
mod russh_tx;
mod session;
mod ssh;

/// Shared connection state. `generation` is bumped on every connect/disconnect so a
/// background connect thread can tell if it has been superseded (the user
/// disconnected, or started a new connect) before it stores its result.
struct State {
    status: RemoteStatus,
    target: Option<RemoteTarget>,
    session: Option<session::Session>,
    busy: bool,
    generation: u64,
    /// The host to auto-reconnect to on launch — loaded from disk at startup, updated
    /// on a successful connect, cleared on an explicit disconnect. The client reads it
    /// (`/api/remote/last`) and drives the resume through the normal connect path.
    last_host: Option<String>,
}

fn idle_status() -> RemoteStatus {
    RemoteStatus { state: "idle".into(), host: None, message: None }
}

/// Update the live status during a connect, unless that connect has been superseded.
fn set_stage(state: &Mutex<State>, generation: u64, stage: &str, host: &str, msg: &str) {
    let mut s = state.lock().unwrap();
    if s.generation != generation {
        return; // a newer connect/disconnect won — don't clobber its status
    }
    s.status = RemoteStatus { state: stage.into(), host: Some(host.into()), message: Some(msg.into()) };
}

pub struct SshRemoteControl {
    state: Arc<Mutex<State>>,
    /// The version whose `skill-server-*` release asset we provision onto remotes —
    /// both desktop and standalone pass their Cargo package version, stamped from
    /// the release tag.
    app_version: String,
    /// Saved-connection credentials for the russh transport (the mobile
    /// switchboard). `None` on desktop/standalone — connects go through the
    /// user's own `ssh` there.
    store: Option<Arc<dyn SecureStore>>,
}

impl SshRemoteControl {
    pub fn new(app_version: String) -> Self {
        Self::with_secure_store(app_version, None)
    }

    /// The mobile switchboard's constructor: `connect(id)` resolves `id` against
    /// `store`'s profiles (Keychain-held key) and speaks russh in-process.
    pub fn with_secure_store(app_version: String, store: Option<Arc<dyn SecureStore>>) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                status: idle_status(),
                target: None,
                session: None,
                busy: false,
                generation: 0,
                last_host: lastconn::load(),
            })),
            app_version,
            store,
        }
    }

    /// Tear down any live session on app exit (no orphaned remote server / tunnel).
    /// `forget=false`: keep the remembered host so the next launch resumes it.
    pub fn shutdown(&self) {
        let _ = self.disconnect(false);
    }

    /// Reconnect after an OS suspend (the mobile shell calls this on resume: iOS
    /// tears the tunnel down within minutes of backgrounding). Off-loads to a
    /// thread so the app's event loop isn't blocked by the liveness probe.
    pub fn resume_check(&self) {
        let state = self.state.clone();
        let app_version = self.app_version.clone();
        let store = self.store.clone();
        std::thread::spawn(move || resume_reconnect(state, app_version, store));
    }
}

/// The resume reconnect, run on its own thread. Reconnects only when the tunnel is
/// actually dead — but decides that by ACTIVELY PROBING it, not by reading the
/// session's cached liveness flag. That flag (russh's keepalive-driven `alive`)
/// reads stale for up to ~45s right after an iOS resume: the whole process, its
/// tokio runtime included, was frozen while the OS killed the socket, so nothing
/// updated the flag. Trusting it here would skip the reconnect this function
/// exists for. A live tunnel answers `/api/health` through its forwarded port in
/// milliseconds; a dead one fails fast (the local forward still accepts, but the
/// onward SSH channel is gone). A short suspend whose tunnel survived reconnects
/// nothing.
fn resume_reconnect(state: Arc<Mutex<State>>, app_version: String, store: Option<Arc<dyn SecureStore>>) {
    // Snapshot under the lock; probe OFF it so status()/disconnect() stay responsive.
    let (generation, host, probe_port) = {
        let s = state.lock().unwrap();
        if s.busy {
            return; // a connect is already in flight
        }
        let Some(host) = s.last_host.clone() else { return };
        (s.generation, host, s.session.as_ref().map(|sess| sess.local_port))
    };

    if let Some(port) = probe_port {
        if tunnel_alive(port) {
            return; // the tunnel survived the suspend — nothing to do
        }
    }

    // Reconnect. Re-take the lock and bail if anything moved while we probed (a
    // user connect/disconnect bumps `generation`), so we never stomp a newer intent.
    let mut s = state.lock().unwrap();
    if s.busy || s.generation != generation || s.last_host.as_deref() != Some(host.as_str()) {
        return;
    }
    s.busy = true;
    s.generation += 1;
    let generation = s.generation;
    s.target = None;
    let dead = s.session.take();
    s.status = RemoteStatus {
        state: "detecting".into(),
        host: Some(host.clone()),
        message: Some("Reconnecting…".into()),
    };
    drop(s);
    if let Some(mut dead) = dead {
        dead.teardown();
    }
    // `resume=true`: a reconnect that races the radio coming back must NOT forget
    // the remembered host on a transient failure (see run_connect), or one offline
    // moment on resume would permanently disable auto-reconnect.
    session::run_connect(state, host, generation, app_version, store, true);
}

/// Actively probe a forwarded tunnel: does the remote server answer `/api/health`
/// through `local_port`? Short timeouts so a dead tunnel fails fast rather than
/// stalling until russh's keepalives notice (~45s). Used only by the resume path,
/// where the cached liveness flag can't yet reflect a tunnel killed during suspend.
fn tunnel_alive(local_port: u16) -> bool {
    ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(2))
        .timeout(std::time::Duration::from_secs(4))
        .build()
        .get(&format!("http://127.0.0.1:{local_port}/api/health"))
        .call()
        .is_ok()
}

impl RemoteControl for SshRemoteControl {
    fn list_hosts(&self) -> Result<Vec<RemoteHost>, String> {
        ssh::list_targets()
    }

    fn status(&self) -> RemoteStatus {
        self.state.lock().unwrap().status.clone()
    }

    fn active_target(&self) -> Option<RemoteTarget> {
        self.state.lock().unwrap().target.clone()
    }

    fn last_host(&self) -> Option<String> {
        self.state.lock().unwrap().last_host.clone()
    }

    fn connect(&self, host: &str) -> Result<(), String> {
        let mut s = self.state.lock().unwrap();
        if s.busy {
            return Err("A connection attempt is already in progress.".into());
        }
        if s.target.is_some() {
            return Err("Already connected — disconnect first.".into());
        }
        s.busy = true;
        s.generation += 1;
        let generation = s.generation;
        s.status = RemoteStatus {
            state: "detecting".into(),
            host: Some(host.to_string()),
            message: Some("Connecting…".into()),
        };
        drop(s);

        let state = self.state.clone();
        let host = host.to_string();
        let app_version = self.app_version.clone();
        let store = self.store.clone();
        std::thread::spawn(move || session::run_connect(state, host, generation, app_version, store, false));
        Ok(())
    }

    fn disconnect(&self, forget: bool) -> Result<(), String> {
        let mut s = self.state.lock().unwrap();
        // Bump the generation BEFORE clearing: this same lock both supersedes any
        // in-flight connect (so it won't re-persist the host) and clears last_host,
        // closing the race where a connect finishing mid-disconnect resurrects it.
        s.generation += 1;
        s.busy = false;
        s.target = None;
        if forget {
            s.last_host = None;
        }
        s.status = idle_status();
        let sess = s.session.take();
        drop(s);
        if forget {
            lastconn::forget();
        }
        if let Some(mut sess) = sess {
            sess.teardown();
        }
        Ok(())
    }
}
