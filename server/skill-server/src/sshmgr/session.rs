//! The remote session lifecycle, transport-agnostic. A [`Remote`] (the user's `ssh`/`wsl`
//! on desktop, or russh on the mobile switchboard) attaches a durable host service
//! through a fresh local port: SSH forwards `-L L:127.0.0.1:R`, while WSL relays
//! TCP over `wsl.exe` pipes into the selected distro. Windows localhost can belong to
//! a different service at the same port, so WSL must not connect to it directly. Tearing
//! it down closes only the forward. The host service outlives every accessor.
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::{RemoteStatus, RemoteTarget};

use super::conn::{self, LaunchError, Remote};
use super::{provision, set_stage, State};

/// A live connection: the transport session handle (tunnel + lifeline) and the forwarded
/// local port.
pub struct Session {
    pub local_port: u16,
    pub token: String,
    /// The transport-specific handle (an `ssh` child, or the russh forward + lifeline).
    handle: Box<dyn super::conn::SessionHandle>,
    record: ServiceRecord,
}

impl Session {
    pub fn health_probe(&self) -> HealthProbe {
        HealthProbe { local_port: self.local_port, record: self.record.clone() }
    }

    /// Close the tunnel/keepalive channel, leaving the durable worker and agents running.
    pub fn teardown(&mut self) {
        self.handle.teardown();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectIntent {
    Setup,
    Recover,
    Retry,
}

/// Connect off-thread unless superseded by a newer generation. Recovery retries
/// transient failures with capped backoff; setup/trust failures await explicit
/// Retry. Both retain the selected host, keeping workspace requests off Local.
pub fn run_connect(
    state: Arc<Mutex<State>>,
    host: String,
    generation: u64,
    app_version: String,
    store: Option<Arc<dyn crate::SecureStore>>,
    intent: ConnectIntent,
) {
    let mut attempt = 0u32;
    loop {
        if current(&state, generation).is_err() {
            return;
        }
        let result = build_and_connect(&state, &host, generation, &app_version, store.as_deref(), intent);
        let mut s = state.lock().unwrap();
        if !s.app_unlocked || s.generation != generation {
            drop(s);
            if let Ok(mut sess) = result {
                sess.teardown();
            }
            return;
        }
        match result {
            Ok(sess) => {
                s.busy = false;
                s.recovering = false;
                s.unlock_intent = Some(ConnectIntent::Recover);
                s.target = Some(RemoteTarget {
                    base_url: format!("http://127.0.0.1:{}", sess.local_port),
                    token: sess.token.clone(),
                });
                s.status = RemoteStatus { state: "connected".into(), host: Some(host.clone()), message: None };
                s.session = Some(sess);
                s.last_host = Some(host.clone());
                super::lastconn::remember(&host);
                drop(s);
                spawn_monitor(state, generation, host, app_version, store);
                return;
            }
            Err(error) => {
                s.target = None;
                s.session = None;
                // A radio outage must not forget the host or rebind requests
                // to Local. Trust/auth/setup errors remain visible for Retry.
                let retry = intent != ConnectIntent::Setup && super::reconnect::transient(&error);
                s.busy = retry;
                s.recovering = retry;
                s.unlock_intent = retry.then_some(intent);
                s.status = RemoteStatus {
                    state: if retry { "reconnecting" } else { "error" }.into(),
                    host: Some(host.clone()),
                    message: Some(if retry { format!("Reconnecting to {host}… {error}") } else { error }),
                };
                drop(s);
                if !retry || !wait_current(&state, generation, super::reconnect::delay(attempt)) {
                    return;
                }
                attempt = attempt.saturating_add(1);
            }
        }
    }
}

/// Sleeps in short slices so Disconnect cancels a backoff promptly.
fn wait_current(state: &Mutex<State>, generation: u64, duration: Duration) -> bool {
    let deadline = std::time::Instant::now() + duration;
    loop {
        if current(state, generation).is_err() {
            return false;
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return true;
        }
        std::thread::sleep(remaining.min(Duration::from_millis(100)));
    }
}

/// Pick the transport for `host` (may itself connect+auth, e.g. russh), then run the connect
/// flow over it.
fn build_and_connect(
    state: &Arc<Mutex<State>>,
    host: &str,
    generation: u64,
    app_version: &str,
    store: Option<&dyn crate::SecureStore>,
    intent: ConnectIntent,
) -> Result<Session, String> {
    current(state, generation)?;
    let remote = conn::build_remote(host, store)?;
    connect_flow(state, remote.as_ref(), host, generation, app_version, intent == ConnectIntent::Recover)
}

/// Detect dead tunnels and recover the same host. Generation guards prevent an
/// old monitor from replacing a newer connection or reversing Disconnect.
fn spawn_monitor(state: Arc<Mutex<State>>, generation: u64, host: String, app_version: String, store: Option<Arc<dyn crate::SecureStore>>) {
    std::thread::spawn(move || loop {
        if !wait_current(&state, generation, Duration::from_secs(3)) {
            return;
        }
        let (probe, alive) = {
            let s = state.lock().unwrap();
            if !s.app_unlocked || s.generation != generation { return; }
            let Some(session) = &s.session else { return };
            (session.health_probe(), session.handle.is_alive())
        };
        // Probe outside the state lock: a hung tunnel must not stall status or
        // Disconnect. Keepalive flags alone can lag after phone suspension.
        if alive && probe.alive() {
            continue;
        }
        let mut s = state.lock().unwrap();
        if !s.app_unlocked || s.generation != generation || s.busy {
            return;
        }
        s.generation += 1;
        let next = s.generation;
        s.busy = true;
        s.recovering = true;
        s.unlock_intent = Some(ConnectIntent::Recover);
        s.target = None;
        let dead = s.session.take();
        s.status = RemoteStatus {
            state: "reconnecting".into(),
            host: Some(host.clone()),
            message: Some(format!("Reconnecting to {host}…")),
        };
        drop(s);
        if let Some(mut dead) = dead { dead.teardown(); }
        run_connect(state, host, next, app_version, store, ConnectIntent::Recover);
        return;
    });
}

/// The same on-wire record is understood by mobile builds without linking the
/// local worker module. A service protocol version is distinct from app version.
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServiceRecord {
    protocol: u32,
    instance_id: String,
    pid: u32,
    port: u16,
    #[serde(default)]
    version: String,
    #[serde(skip)]
    explicitly_stopped: bool,
}

#[derive(Clone)]
pub struct HealthProbe {
    local_port: u16,
    record: ServiceRecord,
}

impl HealthProbe {
    pub fn alive(&self) -> bool { verify_tunnel(self.local_port, &self.record).is_ok() }
}

fn current(state: &Mutex<State>, generation: u64) -> Result<(), String> {
    let s = state.lock().unwrap();
    if s.app_unlocked && s.generation == generation { Ok(()) }
    else { Err("Connection attempt cancelled.".into()) }
}

fn connect_flow(
    state: &Arc<Mutex<State>>,
    remote: &dyn Remote,
    host: &str,
    generation: u64,
    app_version: &str,
    reuse_installed: bool,
) -> Result<Session, String> {
    current(state, generation)?;
    let version = provision::server_version(app_version)?;
    let minimum = crate::server_version::minimum(app_version, &version)?;
    set_stage(state, generation, "launching", host, "Looking for the host service…");
    // Reconnect/second-client discovery happens BEFORE platform detection or
    // provisioning. Older clients reuse newer compatible workers; newer clients
    // must upgrade an older worker before routing workspace requests to it.
    if let Some(record) = probe_running(remote)? {
        current(state, generation)?;
        if record.explicitly_stopped {
            if reuse_installed { return Err("The host service was explicitly stopped. Choose Retry or reconnect to start it.".into()); }
            // Explicit setup/Retry goes through ensure: it waits for the old
            // worker to stop before clearing intent, never returns a doomed port.
        } else {
            match reattach(remote, host, record) {
                Ok(mut session) => {
                    match crate::server_version::require_at_least(&session.record.version, &minimum) {
                        Ok(()) => return Ok(session),
                        Err(error) => {
                            // Only this temporary tunnel closes. The verified
                            // old worker stays live while a replacement stages.
                            session.teardown();
                            if reuse_installed {
                                return Err(format!("{error} Running terminal sessions were left untouched."));
                            }
                        }
                    }
                }
                Err(error) if error.contains("incompatible host service") => return Err(error),
                Err(error) => log::debug!("Existing host-service endpoint unavailable: {error}"),
            }
        }
    }
    current(state, generation)?;
    // During recovery reuse the already installed binary if possible. The
    // supervisor must not repeatedly download releases while a radio is offline.
    if !version.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte)) || version.is_empty() {
        return Err("Invalid skill-server release version.".into());
    }
    let installed = format!("$HOME/.vibestudio/server/{version}/skill-server");
    let bin = if reuse_installed {
        let output = remote.capture(&format!("[ -x \"{installed}\" ] && \"{installed}\" --version"))?;
        let actual = crate::server_version::from_banner(&output)?;
        crate::server_version::require_at_least(&actual, &minimum)?;
        installed
    } else {
        set_stage(state, generation, "detecting", host, "Detecting the remote platform…");
        let platform = provision::detect(remote)?;
        current(state, generation)?;
        set_stage(state, generation, "installing", host, "Installing skill-server on the remote…");
        let bin = provision::ensure_installed(remote, &platform, app_version)?;
        current(state, generation)?;
        let output = remote.capture(&format!("\"{bin}\" --version"))?;
        let actual = crate::server_version::from_banner(&output)?;
        crate::server_version::require_at_least(&actual, &minimum)?;
        bin
    };
    current(state, generation)?;
    set_stage(state, generation, "launching", host, "Starting the durable host service…");
    let output = remote.capture(&launch_script(&bin, reuse_installed))?;
    let record = output.lines().find_map(|line| line.strip_prefix("SKILL_HOST_SERVICE_READY ").and_then(parse_record))
        .ok_or("The host service did not return a valid ready record.")?;
    current(state, generation)?;
    let mut session = reattach(remote, host, record)?;
    if let Err(error) = crate::server_version::require_at_least(&session.record.version, &minimum)
        .and_then(|()| current(state, generation)) {
        session.teardown();
        return Err(error);
    }
    // A failed download/upgrade must retain the previous executable for rollback.
    // Pruning happens only after the replacement's identity/version are verified.
    // A concurrent launcher may have started a newer worker that --daemon
    // reused. Its installation is not the cache we just provisioned.
    if !reuse_installed && session.record.version == version {
        if let Err(error) = current(state, generation) {
            session.teardown();
            return Err(error);
        }
        provision::prune_old_versions(remote, &version);
    }
    Ok(session)
}

fn parse_record(text: &str) -> Option<ServiceRecord> {
    if text.len() > 16 * 1024 { return None; }
    let record: ServiceRecord = serde_json::from_str(text).ok()?;
    (record.port > 0 && record.pid > 0 && record.instance_id.len() == 32
        && record.instance_id.bytes().all(|byte| byte.is_ascii_hexdigit())).then_some(record)
}

/// Opening/closing this channel only controls the tunnel. Worker startup already
/// completed in a separate one-shot command and has no SSH-owned file descriptor.
fn reattach(remote: &dyn Remote, host: &str, record: ServiceRecord) -> Result<Session, String> {
    let mut last_error = String::new();
    for _ in 0..4 {
        let local_port = if remote.same_port() { record.port } else { free_local_port()? };
        let handle = match remote.open_session(&reattach_script(record.port), local_port, record.port, host) {
            Ok(handle) => handle,
            Err(LaunchError::PortConflict(error)) => { last_error = error; continue; }
            Err(LaunchError::Fatal(error)) => return Err(error),
        };
        let mut session = Session { local_port, token: String::new(), handle, record: record.clone() };
        if let Err(error) = verify_tunnel(local_port, &record) { session.teardown(); return Err(error); }
        if record.protocol != 1 {
            session.teardown();
            return Err(format!("incompatible host service protocol {} (client supports 1). Update the client or explicitly restart the host service; running agents were left untouched.", record.protocol));
        }
        return Ok(session);
    }
    Err(format!("Could not bind the remote forward after several attempts. {last_error}"))
}

fn verify_tunnel(local_port: u16, record: &ServiceRecord) -> Result<(), String> {
    use std::io::Read;
    let response = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(1)).timeout(Duration::from_secs(3)).build()
        .get(&format!("http://127.0.0.1:{local_port}/api/health")).call()
        .map_err(|error| format!("Connection to the host service failed: {error}"))?;
    let mut body = String::new();
    response.into_reader().take(16 * 1024).read_to_string(&mut body)
        .map_err(|error| format!("Connection closed while reading host-service health: {error}"))?;
    let value: serde_json::Value = serde_json::from_str(&body).map_err(|_| "The host service returned an invalid health response.")?;
    if value["pid"].as_u64() != Some(record.pid as u64)
        || value["hostService"]["protocol"].as_u64() != Some(record.protocol as u64)
        || value["hostService"]["instanceId"].as_str() != Some(record.instance_id.as_str()) {
        return Err("The host service identity changed; reconnect to discover its current endpoint.".into());
    }
    let actual = value["version"].as_str().unwrap_or_default();
    if actual != record.version {
        return Err("incompatible host service version: its health response does not match the service record. Reconnect after updating the host service; running agents were left untouched.".into());
    }
    crate::server_version::require_at_least(actual, actual)?;
    Ok(())
}

fn probe_running(remote: &dyn Remote) -> Result<Option<ServiceRecord>, String> {
    let output = remote.capture(probe_script())?;
    let (record_text, marker) = match output.split_once("\nHOST_SERVICE_STOPPED ") {
        Some((record, marker)) => (record, Some(marker)),
        None => (output.as_str(), None),
    };
    let Some(mut record) = parse_record(record_text.trim()) else { return Ok(None); };
    if let Some(marker) = marker {
        let identity: serde_json::Value = serde_json::from_str(marker.trim())
            .map_err(|_| "Could not read the host-service stop intent.")?;
        record.explicitly_stopped = identity["instanceId"].as_str() == Some(record.instance_id.as_str())
            && identity["protocol"].as_u64() == Some(record.protocol as u64);
    }
    Ok(Some(record))
}

fn launch_script(bin: &str, recovery: bool) -> String {
    let recover = if recovery { " --recover" } else { "" };
    format!("unset VIBESTUDIO_SERVER_TOKEN; exec \"{bin}\" --daemon{recover} --host 127.0.0.1 --port 8765")
}

fn reattach_script(remote_port: u16) -> String {
    format!("echo SKILL_SERVER_READY port={remote_port}; exec cat")
}

fn probe_script() -> &'static str {
    // Do not adopt per-version legacy running files: their processes still
    // belong to an older client's stdin. Leave them and all tmux agents alone.
    "d=\"${XDG_CONFIG_HOME:-$HOME/.config}/vibestudio\"; f=\"$d/host-service.json\"; [ -f \"$f\" ] || exit 0; head -c 16384 \"$f\"; if [ -f \"$d/host-service-stopped.json\" ]; then printf '\\nHOST_SERVICE_STOPPED '; head -c 16384 \"$d/host-service-stopped.json\"; fi"
}

fn free_local_port() -> Result<u16, String> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|error| format!("could not allocate a local port: {error}"))?;
    listener.local_addr().map(|address| address.port()).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn wsl_attaches_to_distro_even_when_windows_port_belongs_to_another_host() {
        struct WslFixture { record: String, distro_port: u16 }
        impl Remote for WslFixture {
            fn capture(&self, command: &str) -> Result<String, String> {
                assert_eq!(command, probe_script(), "existing worker must not be reprovisioned");
                Ok(self.record.clone())
            }
            fn run(&self, _: &str) -> Result<String, super::super::ssh::RunError> { panic!("must not provision") }
            fn run_with_stdin(&self, _: &str, _: &[u8]) -> Result<(), super::super::ssh::RunError> { panic!("must not upload") }
            fn same_port(&self) -> bool { conn::SshRemote::new("wsl:Ubuntu").same_port() }
            fn open_session(&self, _: &str, local_port: u16, remote_port: u16, _: &str) -> Result<Box<dyn conn::SessionHandle>, LaunchError> {
                assert_ne!(local_port, remote_port, "WSL must not reuse Windows localhost");
                // Separate listeners model the two OS network namespaces: the
                // distro command reaches its own service at the logical port.
                Ok(super::super::wsl::test_forward(local_port, self.distro_port))
            }
        }
        let windows = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let windows_port = windows.server_addr().to_ip().unwrap().port();
        let distro = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let distro_port = distro.server_addr().to_ip().unwrap().port();
        let identity = "0123456789abcdef0123456789abcdef";
        let record = serde_json::json!({"protocol":1,"instanceId":identity,"pid":42,"port":windows_port,"version":"1.2.4"}).to_string();
        let windows_worker = std::thread::spawn(move || {
            let request = windows.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
            request.respond(tiny_http::Response::from_string(serde_json::json!({
                "pid":99,"hostService":{"protocol":1,"instanceId":"f".repeat(32)}
            }).to_string())).unwrap();
            assert!(windows.recv_timeout(Duration::from_millis(500)).unwrap().is_none(),
                "forward must never send workspace requests to the Windows worker");
        });
        // Reproduce the screenshot's error with the old direct-localhost path.
        assert!(verify_tunnel(windows_port, &parse_record(&record).unwrap()).unwrap_err().contains("identity changed"));
        let distro_worker = std::thread::spawn(move || {
            for _ in 0..2 {
                let request = distro.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
                request.respond(tiny_http::Response::from_string(serde_json::json!({
                    "pid":42,"version":"1.2.4","hostService":{"protocol":1,"instanceId":identity}
                }).to_string())).unwrap();
            }
        });
        let remote = WslFixture { record, distro_port };
        let state = Arc::new(Mutex::new(State { status: super::super::idle_status(), target: None,
            session: None, busy: true, generation: 1, last_host: None, recovering: true,
            app_unlocked: true, unlock_intent: Some(ConnectIntent::Recover) }));
        let mut session = connect_flow(&state, &remote, "wsl:Ubuntu", 1, "1.2.4", true)
            .unwrap_or_else(|error| panic!("{error}"));
        assert!(session.health_probe().alive());
        session.teardown();
        windows_worker.join().unwrap();
        distro_worker.join().unwrap();
    }

    #[test]
    fn lock_detaches_the_phone_tunnel_off_the_state_lock_and_cancels_stale_connects() {
        struct Handle {
            state: Arc<Mutex<State>>,
            closed: std::sync::mpsc::Sender<bool>,
        }
        impl conn::SessionHandle for Handle {
            fn is_alive(&self) -> bool { true }
            fn teardown(&mut self) {
                self.closed.send(self.state.try_lock().is_ok()).unwrap();
            }
        }
        let state = Arc::new(Mutex::new(State {
            status: RemoteStatus { state: "connected".into(), host: Some("workbox".into()), message: None },
            target: Some(RemoteTarget { base_url: "http://127.0.0.1:42".into(), token: String::new() }),
            session: None, busy: false, generation: 5, last_host: Some("workbox".into()),
            recovering: false, app_unlocked: true, unlock_intent: Some(ConnectIntent::Recover),
        }));
        let (closed, observed) = std::sync::mpsc::channel();
        state.lock().unwrap().session = Some(Session {
            local_port: 42, token: String::new(), handle: Box::new(Handle { state: state.clone(), closed }),
            record: ServiceRecord { protocol: 1, instance_id: "fixture".into(), pid: 42, port: 42, version: "1.2.4".into(), explicitly_stopped: false },
        });
        let control = super::super::SshRemoteControl { state: state.clone(), app_version: "test".into(), store: None };
        control.set_app_unlocked(false);
        assert!(observed.recv_timeout(Duration::from_secs(2)).unwrap(), "teardown cannot hold the connection state lock");
        assert!(current(&state, 5).is_err());
        assert!(!wait_current(&state, 5, Duration::ZERO));
        // An already queued recovery must exit without opening SSH or changing
        // the selected host, even if it starts running after the app is locked.
        run_connect(state.clone(), "must-not-open-ssh".into(), 5, "test".into(), None, ConnectIntent::Recover);
        let locked = state.lock().unwrap();
        assert!(locked.target.is_none());
        assert!(locked.session.is_none());
        assert_eq!(locked.status.state, "locked");
        assert_eq!(locked.status.host.as_deref(), Some("workbox"));
        assert_eq!(locked.last_host.as_deref(), Some("workbox"));
    }

    #[test]
    fn existing_workers_enforce_protocol_and_version_before_any_provisioning() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Handle(Arc<AtomicUsize>);
        impl conn::SessionHandle for Handle {
            fn is_alive(&self) -> bool { true }
            fn teardown(&mut self) { self.0.fetch_add(1, Ordering::SeqCst); }
        }
        struct Existing { record: String, closed: Arc<AtomicUsize> }
        impl Remote for Existing {
            fn capture(&self, command: &str) -> Result<String, String> {
                assert_eq!(command, probe_script(), "healthy shared worker must bypass detect/install/launch");
                Ok(self.record.clone())
            }
            fn run(&self, _: &str) -> Result<String, super::super::ssh::RunError> { panic!("must not provision") }
            fn run_with_stdin(&self, _: &str, _: &[u8]) -> Result<(), super::super::ssh::RunError> { panic!("must not upload") }
            fn same_port(&self) -> bool { true }
            fn open_session(&self, command: &str, _: u16, port: u16, _: &str) -> Result<Box<dyn conn::SessionHandle>, LaunchError> {
                assert_eq!(command, reattach_script(port));
                Ok(Box::new(Handle(self.closed.clone())))
            }
        }
        for (protocol, recorded, actual, minimum, error) in [
            (1u32, "1.2.4", "1.2.4", "1.2.4", None),
            (1, "1.2.9", "1.2.9", "1.2.4", None),
            (2, "1.2.4", "1.2.4", "1.2.4", Some("incompatible host service")),
            (1, "1.2.4", "1.2.4", "1.2.8", Some("Choose Retry")),
            (1, "1.2.8", "1.2.4", "1.2.8", Some("incompatible host service version")),
            (1, "", "", "1.2.8", Some("incompatible host service version")),
            (1, "unknown", "unknown", "1.2.8", Some("incompatible host service version")),
        ] {
            let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
            let port = server.server_addr().to_ip().unwrap().port();
            let identity = "0123456789abcdef0123456789abcdef";
            let response = serde_json::json!({"pid":42,"version":actual,"hostService":{"protocol":protocol,"instanceId":identity}}).to_string();
            let thread = std::thread::spawn(move || {
                let request = server.recv().unwrap();
                assert_eq!(request.url(), "/api/health");
                request.respond(tiny_http::Response::from_string(response)).unwrap();
            });
            let closed = Arc::new(AtomicUsize::new(0));
            let remote = Existing {
                record: serde_json::json!({"protocol":protocol,"instanceId":identity,"pid":42,"port":port,"version":recorded}).to_string(),
                closed: closed.clone(),
            };
            let state = Arc::new(Mutex::new(State { status: super::super::idle_status(), target: None,
                session: None, busy: true, generation: 1, last_host: None, recovering: true, app_unlocked: true, unlock_intent: Some(ConnectIntent::Recover) }));
            let result = connect_flow(&state, &remote, "fixture", 1, minimum, true);
            match error {
                None => { let mut session = result.unwrap_or_else(|error| panic!("{error}")); session.teardown(); }
                Some(expected) => assert!(result.err().unwrap().contains(expected)),
            }
            assert_eq!(closed.load(Ordering::SeqCst), 1);
            thread.join().unwrap();
        }
    }

    #[test]
    fn explicit_upgrade_verifies_download_before_launch_and_prunes_only_a_verified_replacement() {
        use std::sync::atomic::{AtomicBool, Ordering};
        struct Handle(Arc<Mutex<Vec<&'static str>>>);
        impl conn::SessionHandle for Handle {
            fn is_alive(&self) -> bool { true }
            fn teardown(&mut self) { self.0.lock().unwrap().push("close tunnel"); }
        }
        struct Upgrade {
            old: String,
            replacement: String,
            banner: &'static str,
            download_fails: bool,
            cancel: Option<Arc<Mutex<State>>>,
            launched: Arc<AtomicBool>,
            events: Arc<Mutex<Vec<&'static str>>>,
        }
        impl Remote for Upgrade {
            fn capture(&self, command: &str) -> Result<String, String> {
                if command == probe_script() {
                    self.events.lock().unwrap().push("probe");
                    return Ok(self.old.clone());
                }
                if command == "uname -sm" {
                    self.events.lock().unwrap().push("detect");
                    return Ok("Linux x86_64".into());
                }
                if command.ends_with("--version") {
                    self.events.lock().unwrap().push("verify binary");
                    return Ok(self.banner.into());
                }
                assert_eq!(command, launch_script("$HOME/.vibestudio/server/1.2.8/skill-server", false));
                self.events.lock().unwrap().push("launch");
                self.launched.store(true, Ordering::SeqCst);
                Ok(format!("SKILL_HOST_SERVICE_READY {}", self.replacement))
            }
            fn run(&self, command: &str) -> Result<String, super::super::ssh::RunError> {
                if command.contains("url=") {
                    self.events.lock().unwrap().push("download");
                    if self.download_fails {
                        return Err(super::super::ssh::RunError { code: Some(4), message: "checksum mismatch".into() });
                    }
                    if let Some(state) = &self.cancel {
                        state.lock().unwrap().generation += 1;
                    }
                } else {
                    assert!(self.launched.load(Ordering::SeqCst), "cleanup must follow a verified launch");
                    self.events.lock().unwrap().push("prune");
                }
                Ok("INSTALLED".into())
            }
            fn run_with_stdin(&self, _: &str, _: &[u8]) -> Result<(), super::super::ssh::RunError> { panic!("no upload needed") }
            fn same_port(&self) -> bool { true }
            fn open_session(&self, _: &str, _: u16, _: u16, _: &str) -> Result<Box<dyn conn::SessionHandle>, LaunchError> {
                self.events.lock().unwrap().push("open tunnel");
                Ok(Box::new(Handle(self.events.clone())))
            }
        }
        for (download_fails, banner, after, success, cancelled) in [
            (false, "skill-server 1.2.8 host-service=1", "1.2.8", true, false),
            (false, "skill-server 1.2.8 host-service=1", "1.2.9", true, false),
            (true, "skill-server 1.2.8 host-service=1", "1.2.8", false, false),
            (false, "skill-server 1.2.4 host-service=1", "1.2.8", false, false),
            (false, "skill-server 1.2.8 host-service=1", "1.2.4", false, false),
            (false, "skill-server 1.2.8 host-service=1", "1.2.8", false, true),
        ] {
            let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
            let port = server.server_addr().to_ip().unwrap().port();
            let old_id = "a".repeat(32);
            let new_id = "b".repeat(32);
            let events = Arc::new(Mutex::new(Vec::new()));
            let observed = events.clone();
            let launched = Arc::new(AtomicBool::new(false));
            let serving_new = launched.clone();
            let expected_launch = !download_fails && !cancelled && banner.contains("1.2.8");
            let old_identity = old_id.clone();
            let new_identity = new_id.clone();
            let responder = std::thread::spawn(move || {
                for _ in 0..if expected_launch { 2 } else { 1 } {
                    let request = server.recv_timeout(Duration::from_secs(5)).unwrap().expect("expected health probe");
                    assert_eq!(request.url(), "/api/health");
                    observed.lock().unwrap().push("verify health");
                    let replacing = serving_new.load(Ordering::SeqCst);
                    request.respond(tiny_http::Response::from_string(serde_json::json!({
                        "pid": if replacing { 43 } else { 42 },
                        "version": if replacing { after } else { "1.2.4" },
                        "hostService": { "protocol": 1, "instanceId": if replacing { &new_identity } else { &old_identity } },
                    }).to_string())).unwrap();
                }
            });
            let state = Arc::new(Mutex::new(State { status: super::super::idle_status(), target: None,
                session: None, busy: true, generation: 1, last_host: None, recovering: false,
                app_unlocked: true, unlock_intent: Some(ConnectIntent::Setup) }));
            let remote = Upgrade {
                old: serde_json::json!({"protocol":1,"instanceId":old_id,"pid":42,"port":port,"version":"1.2.4"}).to_string(),
                replacement: serde_json::json!({"protocol":1,"instanceId":new_id,"pid":43,"port":port,"version":after}).to_string(),
                banner, download_fails, launched: launched.clone(), events: events.clone(),
                cancel: cancelled.then(|| state.clone()),
            };
            let result = connect_flow(&state, &remote, "fixture", 1, "1.2.8", false);
            if success {
                let mut session = result.unwrap_or_else(|error| panic!("{error}"));
                assert_eq!(session.record.version, after);
                session.teardown();
            } else {
                assert!(result.is_err(), "an unverified replacement must not become the active target");
            }
            responder.join().unwrap();
            let events = events.lock().unwrap();
            assert_eq!(&events[..5], &["probe", "open tunnel", "verify health", "close tunnel", "detect"]);
            assert_eq!(events.contains(&"launch"), expected_launch);
            let pruned = success && after == "1.2.8";
            assert_eq!(events.contains(&"prune"), pruned);
            if expected_launch {
                assert!(events.iter().position(|event| *event == "verify binary") < events.iter().position(|event| *event == "launch"));
            }
            if pruned {
                assert!(events.iter().rposition(|event| *event == "verify health") < events.iter().position(|event| *event == "prune"));
            }
        }
    }

    #[test]
    fn explicit_retry_can_provision_even_while_status_is_reconnecting() {
        use std::sync::atomic::{AtomicBool, Ordering};
        struct Handle;
        impl conn::SessionHandle for Handle {
            fn is_alive(&self) -> bool { true }
            fn teardown(&mut self) {}
        }
        struct Setup { ready: String, installed: AtomicBool }
        impl Remote for Setup {
            fn capture(&self, command: &str) -> Result<String, String> {
                if command == probe_script() { return Ok(String::new()); }
                if command == "uname -sm" { return Ok("Linux x86_64".into()); }
                assert!(!command.starts_with("[ -x"), "Retry must run setup instead of requiring a cached binary");
                if command.ends_with("--version") { return Ok("skill-server 1.2.3 host-service=1".into()); }
                assert!(command.contains("--daemon"));
                assert!(self.installed.load(Ordering::SeqCst));
                Ok(self.ready.clone())
            }
            fn run(&self, command: &str) -> Result<String, super::super::ssh::RunError> {
                if command.contains("url=") { self.installed.store(true, Ordering::SeqCst); }
                Ok("INSTALLED".into())
            }
            fn run_with_stdin(&self, _: &str, _: &[u8]) -> Result<(), super::super::ssh::RunError> { panic!("no upload needed") }
            fn same_port(&self) -> bool { true }
            fn open_session(&self, _: &str, _: u16, _: u16, _: &str) -> Result<Box<dyn conn::SessionHandle>, LaunchError> { Ok(Box::new(Handle)) }
        }
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        let identity = "0123456789abcdef0123456789abcdef";
        let response = serde_json::json!({"pid":42,"version":"1.2.3","hostService":{"protocol":1,"instanceId":identity}}).to_string();
        let thread = std::thread::spawn(move || {
            server.recv().unwrap().respond(tiny_http::Response::from_string(response)).unwrap();
        });
        let remote = Setup { installed: AtomicBool::new(false), ready: format!("SKILL_HOST_SERVICE_READY {}", serde_json::json!({"protocol":1,"instanceId":identity,"pid":42,"port":port,"version":"1.2.3"})) };
        let state = Arc::new(Mutex::new(State { status: super::super::idle_status(), target: None,
            session: None, busy: true, generation: 1, last_host: None, recovering: true, app_unlocked: true, unlock_intent: Some(ConnectIntent::Recover) }));
        let mut session = connect_flow(&state, &remote, "fixture", 1, "1.2.3", false).unwrap_or_else(|error| panic!("{error}"));
        assert!(remote.installed.load(Ordering::SeqCst));
        session.teardown();
        thread.join().unwrap();
    }

    #[test]
    fn remote_recovery_never_reattaches_a_matching_explicit_stop_marker() {
        struct Stopped;
        impl Remote for Stopped {
            fn capture(&self, command: &str) -> Result<String, String> {
                assert_eq!(command, probe_script());
                Ok(concat!(r#"{"protocol":1,"instanceId":"0123456789abcdef0123456789abcdef","pid":42,"port":8765}"#,
                    "\nHOST_SERVICE_STOPPED ", r#"{"protocol":1,"instanceId":"0123456789abcdef0123456789abcdef"}"#).into())
            }
            fn run(&self, _: &str) -> Result<String, super::super::ssh::RunError> { panic!("must not provision") }
            fn run_with_stdin(&self, _: &str, _: &[u8]) -> Result<(), super::super::ssh::RunError> { panic!("must not upload") }
            fn same_port(&self) -> bool { true }
            fn open_session(&self, _: &str, _: u16, _: u16, _: &str) -> Result<Box<dyn conn::SessionHandle>, LaunchError> { panic!("must not reattach dying worker") }
        }
        let state = Arc::new(Mutex::new(State { status: super::super::idle_status(), target: None,
            session: None, busy: true, generation: 1, last_host: None, recovering: true, app_unlocked: true, unlock_intent: Some(ConnectIntent::Recover) }));
        let error = connect_flow(&state, &Stopped, "fixture", 1, "1.2.3", true).err().unwrap();
        assert!(error.contains("explicitly stopped"));
    }

    #[test]
    fn launch_uses_detached_host_service_without_lifeline_or_legacy_record() {
        let script = launch_script("$HOME/.vibestudio/server/1.2.3/skill-server", false);
        assert!(script.contains("--daemon"));
        assert!(!script.contains("--lifeline-stdin"));
        assert!(!script.contains("/running"));
        assert!(script.contains("unset VIBESTUDIO_SERVER_TOKEN"));
        assert!(launch_script("$HOME/server", true).contains("--daemon --recover"));
    }

    #[test]
    fn reattach_is_tunnel_only_and_record_is_shared_across_versions() {
        let script = reattach_script(39544);
        assert!(script.contains("SKILL_SERVER_READY port=39544"));
        assert!(script.contains("exec cat"));
        assert!(!script.contains("skill-server"));
        assert!(probe_script().contains("host-service.json"));
        assert!(!probe_script().contains("/server/"));
    }

    #[test]
    fn records_require_pid_port_and_unambiguous_instance_identity() {
        let record = r#"{"protocol":1,"instanceId":"0123456789abcdef0123456789abcdef","pid":42,"port":8765}"#;
        assert!(parse_record(record).is_some());
        assert!(parse_record(&record.replace("8765", "0")).is_none());
        assert!(parse_record(&record.replace("0123456789abcdef0123456789abcdef", "oops")).is_none());
        assert!(parse_record("42 8765 legacytoken").is_none());
    }
}
