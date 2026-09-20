//! The remote connection manager — the `RemoteControl` impl that a `skill-server`
//! exposes over `/api/remote/*`. It shells out to the system `ssh` (inheriting the
//! user's keys/config/ProxyJump) or, for a local WSL/WSL2 distro on Windows, to
//! `wsl.exe`; it discovers or starts the per-user host service on the target,
//! provisioning a version-pinned `skill-server` when needed, and reaches it through an
//! SSH or distro-specific WSL loopback forward; the local server proxies `/api/*` to it.
//!
//! Lives server-side so BOTH entry points get it identically: the desktop's
//! in-process server and the standalone `skill-server` binary (browser-local dev, or
//! a dev box). The durable host worker leaves `ServerConfig::remote = None`, so
//! it cannot switch another client's workspace or broker nested onward-SSH.
use std::sync::{Arc, Mutex};

use crate::{RemoteControl, RemoteHost, RemoteStatus, RemoteTarget, SecureStore};

// The transport seam (Remote/SessionHandle): both the ssh/wsl shell-out and russh plug in
// here, so one connect orchestration drives both.
mod conn;
mod lastconn;
mod provision;
mod reconnect;
// Pure-Rust SSH transport for the mobile switchboard (iOS can't spawn `ssh`). Desktop
// keeps `ssh.rs`; these compile only under the `russh-transport` feature.
#[cfg(feature = "russh-transport")]
pub mod keygen;
#[cfg(feature = "russh-transport")]
mod russh_tx;
mod session;
mod ssh;
mod wsl;

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
    /// Keep reconnect progress distinct from first-time setup for the client.
    recovering: bool,
    /// Native authentication suspension. Desktop controllers stay unlocked.
    app_unlocked: bool,
    /// Resume live connection intent after authentication, without reviving a
    /// failed selection or an explicit Disconnect. A connected tunnel recovers;
    /// an interrupted setup/Retry retains its original provisioning permission.
    unlock_intent: Option<session::ConnectIntent>,
}

fn idle_status() -> RemoteStatus {
    RemoteStatus { state: "idle".into(), host: None, message: None }
}

/// Update the live status during a connect, unless that connect has been superseded.
fn set_stage(state: &Mutex<State>, generation: u64, stage: &str, host: &str, msg: &str) {
    let mut s = state.lock().unwrap();
    if !s.app_unlocked || s.generation != generation {
        return; // a newer connect/disconnect won — don't clobber its status
    }
    let stage = if s.recovering { "reconnecting" } else { stage };
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
                recovering: false,
                app_unlocked: true,
                unlock_intent: None,
            })),
            app_version,
            store,
        }
    }

    /// Drop this client's tunnel on app exit; the host service keeps running.
    /// Retain the selection until exit, so polling clients cannot mistake
    /// teardown for switching to Local and reload their unsaved workspace.
    /// WSL pipe drains can outlive their launcher: cleanup is best-effort and
    /// bounded so the Windows updater can hand off to its installer promptly.
    pub fn shutdown(&self) {
        let session = {
            let mut state = self.state.lock().unwrap();
            state.generation += 1;
            state.busy = true;
            state.recovering = false;
            state.unlock_intent = None;
            state.target = None;
            if state.status.host.is_some() {
                state.status.state = "reconnecting".into();
                state.status.message = Some("VibeStudio is closing…".into());
            }
            state.session.take()
        };
        if let Some(mut session) = session {
            let (finished, completion) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                session.teardown();
                drop(session);
                let _ = finished.send(());
            });
            if completion.recv_timeout(std::time::Duration::from_secs(1)).is_err() {
                log::warn!("Client tunnel cleanup exceeded the exit budget; continuing app exit. Host services and tmux sessions remain running.");
            }
        }
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

    /// Suspend this phone's access after the native grace period, or reconnect
    /// its selected host after native authentication. The state change and
    /// generation cancellation are synchronous; network teardown/recovery are
    /// off-thread so neither blocks the OS lifecycle callback. This never stops
    /// the host service or its jobs, and never turns a selected remote into Local.
    pub fn set_app_unlocked(&self, unlocked: bool) {
        let (session, reconnect) = self.update_app_access(unlocked);
        if let Some(mut session) = session {
            std::thread::spawn(move || session.teardown());
        }
        if let Some((host, generation, intent)) = reconnect {
            let state = self.state.clone();
            let app_version = self.app_version.clone();
            let store = self.store.clone();
            std::thread::spawn(move || session::run_connect(state, host, generation, app_version, store, intent));
        }
    }

    fn update_app_access(&self, unlocked: bool) -> (Option<session::Session>, Option<(String, u64, session::ConnectIntent)>) {
        let mut s = self.state.lock().unwrap();
        if s.app_unlocked == unlocked {
            return (None, None);
        }
        s.app_unlocked = unlocked;
        s.generation += 1;
        s.busy = false;
        s.recovering = false;
        if !unlocked {
            s.target = None;
            let session = s.session.take();
            if s.unlock_intent.is_some() && s.status.host.is_some() {
                s.status.state = "locked".into();
                s.status.message = Some("Unlock VibeStudio to reconnect.".into());
            }
            return (session, None);
        }
        let reconnect = s.status.host.clone().zip(s.unlock_intent).map(|(host, intent)| {
            s.busy = true;
            s.recovering = intent != session::ConnectIntent::Setup;
            s.status.state = if s.recovering { "reconnecting" } else { "detecting" }.into();
            s.status.message = Some("Reconnecting…".into());
            (host, s.generation, intent)
        });
        (None, reconnect)
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
    let (generation, host, probe) = {
        let s = state.lock().unwrap();
        if !s.app_unlocked || s.busy || s.status.state != "connected" {
            // Automatic recovery already owns transient failures. A setup or
            // trust error awaits Retry; remembered history is not live intent.
            return;
        }
        let Some(host) = s.status.host.clone() else { return };
        let Some(session) = &s.session else { return };
        (s.generation, host, session.health_probe())
    };

    if probe.alive() {
        return; // the tunnel survived the suspend — nothing to do
    }

    // Reconnect. Re-take the lock and bail if anything moved while we probed (a
    // user connect/disconnect bumps `generation`), so we never stomp a newer intent.
    let mut s = state.lock().unwrap();
    if !s.app_unlocked || s.busy || s.generation != generation || s.status.host.as_deref() != Some(host.as_str()) {
        return;
    }
    s.busy = true;
    s.generation += 1;
    let generation = s.generation;
    s.target = None;
    let dead = s.session.take();
    s.recovering = true;
    s.unlock_intent = Some(session::ConnectIntent::Recover);
    s.status = RemoteStatus {
        state: "reconnecting".into(),
        host: Some(host.clone()),
        message: Some("Reconnecting…".into()),
    };
    drop(s);
    if let Some(mut dead) = dead {
        dead.teardown();
    }
    session::run_connect(state, host, generation, app_version, store, session::ConnectIntent::Recover);
}

impl RemoteControl for SshRemoteControl {
    fn list_hosts(&self) -> Result<Vec<RemoteHost>, String> {
        ssh::list_targets()
    }

    fn status(&self) -> RemoteStatus {
        self.state.lock().unwrap().status.clone()
    }

    fn active_target(&self) -> Option<RemoteTarget> {
        let s = self.state.lock().unwrap();
        s.app_unlocked.then(|| s.target.clone()).flatten()
    }

    fn route(&self) -> crate::RemoteRoute {
        let s = self.state.lock().unwrap();
        if !s.app_unlocked {
            return crate::RemoteRoute::Unavailable("Unlock VibeStudio to continue.".into());
        }
        if let Some(target) = &s.target {
            crate::RemoteRoute::Connected(target.clone())
        } else if s.status.host.is_some() {
            crate::RemoteRoute::Unavailable(s.status.message.clone().unwrap_or_else(|| "The selected host is unavailable.".into()))
        } else {
            crate::RemoteRoute::Local
        }
    }

    fn retry(&self) -> Result<(), String> {
        let (host, generation) = {
            let mut s = self.state.lock().unwrap();
            if !s.app_unlocked {
                return Err("Unlock VibeStudio to reconnect.".into());
            }
            if s.busy || s.target.is_some() {
                return Ok(());
            }
            let host = s.status.host.clone().ok_or("No remote connection to retry.")?;
            s.busy = true;
            s.recovering = true;
            s.unlock_intent = Some(session::ConnectIntent::Retry);
            s.generation += 1;
            s.status = RemoteStatus { state: "reconnecting".into(), host: Some(host.clone()), message: Some("Reconnecting…".into()) };
            (host, s.generation)
        };
        let state = self.state.clone();
        let app_version = self.app_version.clone();
        let store = self.store.clone();
        std::thread::spawn(move || session::run_connect(state, host, generation, app_version, store, session::ConnectIntent::Retry));
        Ok(())
    }

    fn last_host(&self) -> Option<String> {
        self.state.lock().unwrap().last_host.clone()
    }

    fn connect(&self, host: &str) -> Result<(), String> {
        let mut s = self.state.lock().unwrap();
        if !s.app_unlocked {
            return Err("Unlock VibeStudio to connect.".into());
        }
        if s.busy {
            return Err("A connection attempt is already in progress.".into());
        }
        if s.target.is_some() {
            return Err("Already connected — disconnect first.".into());
        }
        s.busy = true;
        s.recovering = false;
        s.unlock_intent = Some(session::ConnectIntent::Setup);
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
        std::thread::spawn(move || session::run_connect(state, host, generation, app_version, store, session::ConnectIntent::Setup));
        Ok(())
    }

    fn disconnect(&self, forget: bool) -> Result<(), String> {
        let mut s = self.state.lock().unwrap();
        // Bump the generation BEFORE clearing: this same lock both supersedes any
        // in-flight connect (so it won't re-persist the host) and clears last_host,
        // closing the race where a connect finishing mid-disconnect resurrects it.
        s.generation += 1;
        s.busy = false;
        s.recovering = false;
        s.unlock_intent = None;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn disconnected_controller() -> SshRemoteControl {
        SshRemoteControl {
            state: Arc::new(Mutex::new(State {
                status: RemoteStatus { state: "reconnecting".into(), host: Some("workbox".into()), message: Some("Reconnecting…".into()) },
                target: None, session: None, busy: true, generation: 10,
                last_host: Some("workbox".into()), recovering: true,
                app_unlocked: true, unlock_intent: Some(session::ConnectIntent::Recover),
            })),
            app_version: "test".into(), store: None,
        }
    }

    #[test]
    fn disconnect_cancels_recovery_without_erasing_app_exit_resume_memory() {
        let control = disconnected_controller();
        assert!(matches!(control.route(), crate::RemoteRoute::Unavailable(_)));
        control.disconnect(false).unwrap();
        assert!(matches!(control.route(), crate::RemoteRoute::Local));
        let state = control.state.lock().unwrap();
        assert_eq!(state.generation, 11);
        assert!(!state.busy);
        assert!(!state.recovering);
        assert_eq!(state.last_host.as_deref(), Some("workbox"));
    }

    #[test]
    fn app_exit_does_not_wait_for_blocked_pipe_cleanup_or_switch_to_local() {
        struct BlockedTunnel(std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>);
        impl conn::SessionHandle for BlockedTunnel {
            fn is_alive(&self) -> bool { true }
            fn teardown(&mut self) {
                self.0.recv().unwrap();
                self.1.send(()).unwrap();
            }
        }
        let control = disconnected_controller();
        let (release, blocked) = std::sync::mpsc::channel();
        let (finished, cleanup) = std::sync::mpsc::channel();
        {
            let mut state = control.state.lock().unwrap();
            state.status.state = "connected".into();
            state.session = Some(session::Session::with_test_handle(Box::new(BlockedTunnel(blocked, finished))));
            state.target = Some(RemoteTarget { base_url: "http://127.0.0.1:1".into(), token: String::new() });
        }
        // Keep the pipe drain blocked until shutdown returns: a synchronous
        // join would hang this test and prevent the installer from launching.
        let (returned, complete) = std::sync::mpsc::channel();
        let control = Arc::new(control);
        let exiting = control.clone();
        let shutdown = std::thread::spawn(move || { exiting.shutdown(); returned.send(()).unwrap(); });
        let completed = complete.recv_timeout(std::time::Duration::from_secs(3));
        release.send(()).unwrap();
        shutdown.join().unwrap();
        completed.expect("app exit must finish without waiting for blocked tunnel pipes");
        cleanup.recv_timeout(std::time::Duration::from_secs(1)).unwrap();
        assert!(matches!(control.route(), crate::RemoteRoute::Unavailable(_)));
        assert_eq!(control.status().host.as_deref(), Some("workbox"));
        let state = control.state.lock().unwrap();
        assert_eq!(state.generation, 11);
        assert_eq!(state.last_host.as_deref(), Some("workbox"));
        assert!(state.session.is_none());
        assert!(state.busy);
        assert!(!state.recovering);
    }

    #[test]
    fn superseded_connect_progress_cannot_resurrect_remote_selection() {
        let control = disconnected_controller();
        control.disconnect(false).unwrap();
        set_stage(&control.state, 10, "forwarding", "workbox", "Late result");
        assert_eq!(control.status().state, "idle");
        assert!(matches!(control.route(), crate::RemoteRoute::Local));
    }

    #[test]
    fn foreground_does_not_replace_failed_selection_with_a_previously_used_host() {
        let control = disconnected_controller();
        {
            let mut state = control.state.lock().unwrap();
            state.busy = false;
            state.recovering = false;
            state.status = RemoteStatus { state: "error".into(), host: Some("new-host".into()), message: Some("Host key changed".into()) };
        }
        resume_reconnect(control.state.clone(), "test".into(), None);
        let state = control.state.lock().unwrap();
        assert_eq!(state.generation, 10);
        assert_eq!(state.status.host.as_deref(), Some("new-host"));
        assert_eq!(state.status.state, "error");
        assert!(!state.busy);
    }

    #[test]
    fn foreground_does_not_reverse_an_explicit_disconnect() {
        let control = disconnected_controller();
        control.disconnect(false).unwrap();
        resume_reconnect(control.state.clone(), "test".into(), None);
        assert_eq!(control.status().state, "idle");
        assert_eq!(control.state.lock().unwrap().generation, 11);
    }

    #[test]
    fn app_lock_cancels_all_recovery_paths_and_unlock_retains_selected_host() {
        let control = disconnected_controller();
        control.set_app_unlocked(false);
        assert!(control.connect("another-host").is_err());
        assert!(control.retry().is_err());
        resume_reconnect(control.state.clone(), "test".into(), None);
        set_stage(&control.state, 10, "connected", "workbox", "Stale connect");
        assert_eq!(control.status().state, "locked");
        assert_eq!(control.status().host.as_deref(), Some("workbox"));
        assert!(matches!(control.route(), crate::RemoteRoute::Unavailable(_)));
        assert!(control.active_target().is_none());
        assert_eq!(control.last_host().as_deref(), Some("workbox"));
        assert_eq!(control.state.lock().unwrap().generation, 11);

        let (session, reconnect) = control.update_app_access(true);
        assert!(session.is_none());
        assert_eq!(reconnect, Some(("workbox".into(), 12, session::ConnectIntent::Recover)));
        assert_eq!(control.status().state, "reconnecting");
        assert!(matches!(control.route(), crate::RemoteRoute::Unavailable(_)));
        assert!(control.update_app_access(true).1.is_none(), "duplicate unlock must not start another tunnel");
    }

    #[test]
    fn unlock_does_not_resurrect_history_or_failed_connections() {
        let control = disconnected_controller();
        control.disconnect(false).unwrap();
        control.set_app_unlocked(false);
        assert!(control.update_app_access(true).1.is_none());
        assert_eq!(control.status().state, "idle");
        assert_eq!(control.last_host().as_deref(), Some("workbox"));
        {
            let mut state = control.state.lock().unwrap();
            state.status = RemoteStatus { state: "error".into(), host: Some("untrusted-host".into()), message: Some("Host key changed".into()) };
        }
        control.set_app_unlocked(false);
        assert!(control.update_app_access(true).1.is_none());
        assert_eq!(control.status().state, "error");
        assert_eq!(control.status().host.as_deref(), Some("untrusted-host"));
    }

    #[test]
    fn app_lock_preserves_interrupted_setup_and_explicit_retry_intent() {
        for intent in [session::ConnectIntent::Setup, session::ConnectIntent::Retry] {
            let control = disconnected_controller();
            control.state.lock().unwrap().unlock_intent = Some(intent);
            control.set_app_unlocked(false);
            control.set_app_unlocked(false);
            assert_eq!(control.update_app_access(true).1, Some(("workbox".into(), 12, intent)));
            control.set_app_unlocked(false);
            control.disconnect(false).unwrap();
            assert!(control.update_app_access(true).1.is_none(), "Disconnect cancels suspended connection intent");
        }
    }
}
