//! The native app lock protects the listener, not just the visible webview.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use skill_server::{AppAccess, RemoteControl, RemoteHost, RemoteStatus, RemoteTarget, ServerConfig};

struct RemoteCalls(AtomicUsize);
impl RemoteControl for RemoteCalls {
    fn list_hosts(&self) -> Result<Vec<RemoteHost>, String> { Ok(vec![]) }
    fn connect(&self, _: &str) -> Result<(), String> { self.0.fetch_add(1, Ordering::SeqCst); Ok(()) }
    fn disconnect(&self, _: bool) -> Result<(), String> { self.0.fetch_add(1, Ordering::SeqCst); Ok(()) }
    fn status(&self) -> RemoteStatus {
        self.0.fetch_add(1, Ordering::SeqCst);
        RemoteStatus { state: "idle".into(), host: None, message: None }
    }
    fn active_target(&self) -> Option<RemoteTarget> { None }
}

fn status(result: Result<ureq::Response, ureq::Error>) -> u16 {
    match result {
        Ok(response) | Err(ureq::Error::Status(_, response)) => response.status(),
        Err(error) => panic!("request failed: {error}"),
    }
}

#[test]
fn native_lock_denies_sensitive_routes_and_http_cannot_unlock_it() {
    let access = Arc::new(AppAccess::new_locked());
    let remote = Arc::new(RemoteCalls(AtomicUsize::new(0)));
    let server = skill_server::spawn(ServerConfig {
        startup_maintenance: false,
        app_access: Some(access.clone()),
        remote: Some(remote.clone()),
        ..Default::default()
    }).unwrap();
    let url = format!("http://{}", server.addr);
    for path in ["/api/remote/status", "/api/remote/profiles", "/api/remote/last", "/api/secrets/status",
        "/api/terminal/attach?id=fixture", "/api/events", "/gw/fixture/mcp", "/api/health/", "/api"] {
        assert_eq!(status(ureq::get(&format!("{url}{path}")).call()), 423, "{path}");
    }
    for path in ["/api/remote/connect", "/api/remote/disconnect", "/api/remote/profiles", "/api/ssh/keygen",
        "/api/app/unlock", "/api/unlock", "/gw/fixture/mcp", "/api/health"] {
        assert_eq!(status(ureq::post(&format!("{url}{path}")).send_string(r#"{"host":"fixture","unlocked":true}"#)), 423, "{path}");
        assert!(!access.is_unlocked());
    }
    assert_eq!(remote.0.load(Ordering::SeqCst), 0, "locked requests must not reach the connection manager");
    assert_eq!(status(ureq::get(&format!("{url}/api/health")).call()), 200);
    assert_ne!(status(ureq::get(&format!("{url}/")).call()), 423, "static resources can load behind the native shield");
    assert_ne!(status(ureq::request("OPTIONS", &format!("{url}/api/remote/status")).call()), 423);

    access.set_unlocked(true);
    assert_eq!(status(ureq::get(&format!("{url}/api/remote/status")).call()), 200);
    assert_eq!(remote.0.load(Ordering::SeqCst), 1);
    for path in ["/api/app/unlock", "/api/unlock"] {
        assert_eq!(status(ureq::post(&format!("{url}{path}")).send_string(r#"{"unlocked":true}"#)), 404);
    }
    let before_lock = remote.0.load(Ordering::SeqCst);
    access.set_unlocked(false);
    assert_eq!(status(ureq::get(&format!("{url}/api/remote/status")).call()), 423);
    assert_eq!(remote.0.load(Ordering::SeqCst), before_lock, "locked requests must not resolve remote routing");
}

#[test]
fn ordinary_desktop_listener_has_no_native_gate() {
    let server = skill_server::spawn(ServerConfig { startup_maintenance: false, ..Default::default() }).unwrap();
    assert_eq!(status(ureq::get(&format!("http://{}/api/secrets/status", server.addr)).call()), 200);
}
