//! HTTP routing is exercised without a live host or native desktop window.
use serde_json::{json, Value};
use skill_core::comparison::ComparisonManager;
use skill_server::{spawn, RemoteControl, RemoteHost, RemoteStatus, RemoteTarget, ServerConfig};
use std::sync::Arc;

struct UnavailableWorkspace;
impl RemoteControl for UnavailableWorkspace {
    fn list_hosts(&self) -> Result<Vec<RemoteHost>, String> { Ok(vec![]) }
    fn connect(&self, _: &str) -> Result<(), String> { Ok(()) }
    fn disconnect(&self, _: bool) -> Result<(), String> { Ok(()) }
    fn status(&self) -> RemoteStatus {
        RemoteStatus { state: "error".into(), host: Some("offline".into()), message: Some("offline".into()) }
    }
    fn active_target(&self) -> Option<RemoteTarget> { None }
}

fn url(port: u16, path: &str) -> String { format!("http://127.0.0.1:{port}/api/comparison/{path}") }

#[test]
fn comparison_stays_local_and_rejects_fronted_clients() {
    let server = spawn(ServerConfig {
        comparison: Some(ComparisonManager::default()),
        remote: Some(Arc::new(UnavailableWorkspace)),
        startup_maintenance: false,
        ..Default::default()
    }).unwrap();
    let port = server.addr.port();
    let capabilities: Value = serde_json::from_reader(ureq::get(&url(port, "capabilities")).call().unwrap().into_reader()).unwrap();
    assert_eq!(capabilities["protocol"], 1);
    let list: Value = serde_json::from_reader(ureq::get(&url(port, "list")).call().unwrap().into_reader()).unwrap();
    assert_eq!(list, json!([]), "offline selected workspace must not intercept desktop controls");
    let devices: Value = serde_json::from_reader(ureq::get(&url(port, "devices")).call().unwrap().into_reader()).unwrap();
    assert_eq!(devices["sourceUrl"], skill_core::comparison_devices::SOURCE_URL);
    assert!(!devices["devices"].as_array().unwrap().is_empty());
    assert_eq!(devices["refreshing"], false, "ordinary reads must not start background network work");
    let refreshed: Value = serde_json::from_reader(ureq::post(&url(port, "devices/refresh")).send_string("{}").unwrap().into_reader()).unwrap();
    assert!(!refreshed["devices"].as_array().unwrap().is_empty());
    let fronted_devices = ureq::get(&url(port, "devices")).set("X-Forwarded-For", "192.0.2.1").call();
    assert!(matches!(fronted_devices, Err(ureq::Error::Status(404, _))));
    let fronted = ureq::get(&url(port, "list")).set("X-Forwarded-For", "192.0.2.1").call();
    assert!(matches!(fronted, Err(ureq::Error::Status(404, _))));
    let foreign = ureq::post(&url(port, "start")).set("Origin", "https://example.com").send_string("{}");
    assert!(matches!(foreign, Err(ureq::Error::Status(403, _))));
    let malformed = ureq::post(&url(port, "start")).send_string("not json");
    assert!(matches!(malformed, Err(ureq::Error::Status(400, _))));
}

#[test]
fn standalone_listener_reports_comparison_unavailable() {
    let server = spawn(ServerConfig { startup_maintenance: false, ..Default::default() }).unwrap();
    let result = ureq::get(&url(server.addr.port(), "capabilities")).call();
    assert!(matches!(result, Err(ureq::Error::Status(404, _))));
    let devices = ureq::get(&url(server.addr.port(), "devices")).call();
    assert!(matches!(devices, Err(ureq::Error::Status(404, _))));
}

#[test]
fn artifact_metadata_and_window_lifecycle_are_available_over_http() {
    use std::fs;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    struct Fixture {
        root: std::path::PathBuf,
        manager: ComparisonManager,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.manager.shutdown(Duration::from_secs(5));
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    let root = std::env::temp_dir().join(format!("vs-comparison-http-{}-{}", std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
    fs::create_dir(&root).unwrap();
    let initialized = skill_core::process::hidden_command("git").args(["init", "--quiet"]).arg(&root).output().unwrap();
    assert!(initialized.status.success());
    let fixture = Fixture { manager: ComparisonManager::with_store(root.join("artifacts.json")).unwrap(), root };
    let server = spawn(ServerConfig {
        comparison: Some(fixture.manager.clone()),
        startup_maintenance: false,
        ..Default::default()
    }).unwrap();
    let port = server.addr.port();
    let post = |path: &str, body: Value| -> Value {
        let response = ureq::post(&url(port, path)).set("Content-Type", "application/json").send_string(&body.to_string()).unwrap();
        serde_json::from_reader(response.into_reader()).unwrap()
    };
    let metadata = json!({"title":"Account layout", "description":"Review navigation spacing", "owner":{"hostId":"local","terminalId":"ass-fixture","provider":"codex","conversationId":"conversation-fixture"}});
    let started = post("start", json!({
        "repository": fixture.root,
        "artifact": metadata,
        "baseline": {"url": url(port, "capabilities")},
        "working": {"url": url(port, "capabilities")}
    }));
    let id = started["id"].as_str().unwrap();
    assert_eq!(started["config"]["artifact"], metadata);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let state = fixture.manager.get(id).unwrap();
        if state.state == skill_core::comparison::ComparisonState::Ready { break; }
        assert!(Instant::now() < deadline, "preview startup failed: {:?}", state.error);
        std::thread::sleep(Duration::from_millis(20));
    }
    fixture.manager.mark_window_open(id).unwrap();
    let closed = post("close", json!({"id":id}));
    assert_eq!(closed["windowRequested"], false);
    assert_eq!(closed["state"], "ready");
    let opened = post("open", json!({"id":id}));
    assert_eq!(opened["windowRequested"], true);
    assert!(opened["presentationRevision"].as_u64() > started["presentationRevision"].as_u64());
    let renamed = post("update", json!({"id":id,"artifact":{"title":"Review spacing", "owner": metadata["owner"]}}));
    assert_eq!(renamed["config"]["artifact"]["title"], "Review spacing");
    assert_eq!(renamed["config"]["artifact"]["owner"], metadata["owner"]);
    post("stop", json!({"id":id}));
    assert!(fixture.manager.shutdown(Duration::from_secs(5)));
    let restored = ComparisonManager::with_store(fixture.root.join("artifacts.json")).unwrap();
    assert_eq!(restored.get(id).unwrap().config.artifact.unwrap().title, "Review spacing");
    assert!(!restored.get(id).unwrap().window_requested);
}


#[test]
fn scroll_relay_requires_exact_origin_capability_and_stays_local() {
    use skill_core::comparison::ComparisonScrollRelay;
    use std::sync::Mutex;
    let relay = ComparisonScrollRelay::default();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let callback_seen = seen.clone();
    let registration = relay.register("https://preview.example".into(), Arc::new(move |payload| {
        callback_seen.lock().unwrap().push(payload.to_owned());
        Ok(())
    })).unwrap();
    let server = spawn(ServerConfig {
        comparison_scroll: Some(relay),
        token: Some("workspace-bearer".into()),
        remote: Some(Arc::new(UnavailableWorkspace)),
        startup_maintenance: false,
        ..Default::default()
    }).unwrap();
    let endpoint = url(server.addr.port(), &format!("scroll/{}", registration.token()));
    let payload = r#"[{"root":true,"key":"","id":"","path":[],"index":-1,"x":0,"y":0.5,"script":"untrusted-extra"}]"#;
    let response = ureq::post(&endpoint).set("Origin", "https://preview.example").set("Content-Type", "text/plain").send_string(payload).unwrap();
    assert_eq!(response.status(), 204);
    assert_eq!(response.header("Access-Control-Allow-Origin"), Some("https://preview.example"));
    assert_eq!(response.header("Access-Control-Allow-Credentials"), None);
    assert_eq!(response.header("Cache-Control"), Some("no-store"));
    assert_eq!(seen.lock().unwrap().len(), 1);
    assert!(!seen.lock().unwrap()[0].contains("untrusted-extra"));
    for origin in ["https://preview.example:8443", "https://evil.example", "http://localhost:1420", "null"] {
        let error = ureq::post(&endpoint).set("Origin", origin).set("Content-Type", "text/plain").send_string(payload).unwrap_err();
        let ureq::Error::Status(status, response) = error else { panic!("unexpected transport error") };
        assert_eq!(status, 403, "{origin}");
        assert_eq!(response.header("Access-Control-Allow-Origin"), None);
    }
    assert!(matches!(ureq::post(&endpoint).set("Content-Type", "text/plain").send_string(payload), Err(ureq::Error::Status(403, _))));
    for header in ["X-Forwarded-For", "Forwarded", "X-Forwarded-Host"] {
        assert!(matches!(ureq::post(&endpoint).set("Origin", "https://preview.example").set("Content-Type", "text/plain").set(header, "remote-client").send_string(payload), Err(ureq::Error::Status(404, _))));
    }
    let preflight = ureq::request("OPTIONS", &endpoint).set("Origin", "https://preview.example").set("Access-Control-Request-Method", "POST").set("Access-Control-Request-Headers", "content-type").call().unwrap();
    assert_eq!(preflight.status(), 204);
    assert_eq!(preflight.header("Access-Control-Allow-Origin"), Some("https://preview.example"));
    assert!(matches!(ureq::request("OPTIONS", &endpoint).set("Origin", "https://evil.example").set("Access-Control-Request-Method", "POST").call(), Err(ureq::Error::Status(403, _))));
    assert!(matches!(ureq::request("OPTIONS", &endpoint).set("Origin", "https://preview.example").set("Access-Control-Request-Method", "POST").set("Access-Control-Request-Headers", "authorization").call(), Err(ureq::Error::Status(403, _))));
    assert!(matches!(ureq::post(&url(server.addr.port(), "start")).set("Origin", "https://preview.example").send_string("{}"), Err(ureq::Error::Status(401, _))), "scroll capability must not bypass general API bearer checks");
    drop(registration);
    assert!(matches!(ureq::post(&endpoint).set("Origin", "https://preview.example").set("Content-Type", "text/plain").send_string(payload), Err(ureq::Error::Status(403, _))));
    assert_eq!(seen.lock().unwrap().len(), 1, "rejected or preflight requests must not invoke native callbacks");
}

#[test]
fn scroll_relay_bounds_payloads_and_honors_native_lock() {
    use skill_core::comparison::{ComparisonScrollRelay, MAX_COMPARISON_SCROLL_BYTES};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let relay = ComparisonScrollRelay::default();
    let calls = Arc::new(AtomicUsize::new(0));
    let callback_calls = calls.clone();
    let registration = relay.register("http://127.0.0.1:4500".into(), Arc::new(move |_| {
        callback_calls.fetch_add(1, Ordering::Relaxed);
        Ok(())
    })).unwrap();
    let server = spawn(ServerConfig { comparison_scroll: Some(relay.clone()), startup_maintenance: false, ..Default::default() }).unwrap();
    let endpoint = url(server.addr.port(), &format!("scroll/{}", registration.token()));
    let payload = r#"[{"root":true,"key":"","id":"","path":[],"index":-1,"x":0,"y":0.5}]"#;
    let post = |body: &str| ureq::post(&endpoint).set("Origin", "http://127.0.0.1:4500").set("Content-Type", "text/plain; charset=UTF-8").send_string(body).map_err(|error| match error {
        ureq::Error::Status(status, _) => status,
        error => panic!("unexpected transport error: {error}"),
    });
    assert!(matches!(post(&" ".repeat(MAX_COMPARISON_SCROLL_BYTES + 1)), Err(413)));
    for body in ["not json".to_owned(), "[]".into(), payload.replace("0.5", "1.1"), payload.replace("[]", "[65536]"), payload.replace("-1", "-2")] {
        assert!(matches!(post(&body), Err(400)));
    }
    let nine = format!("[{}]", [payload.trim_start_matches('[').trim_end_matches(']'); 9].join(","));
    assert!(matches!(post(&nine), Err(400)));
    assert!(matches!(ureq::post(&endpoint).set("Origin", "http://127.0.0.1:4500").set("Content-Type", "application/json").send_string(payload), Err(ureq::Error::Status(415, _))));
    let access = Arc::new(skill_server::AppAccess::new_locked());
    let locked = spawn(ServerConfig { comparison_scroll: Some(relay), app_access: Some(access), startup_maintenance: false, ..Default::default() }).unwrap();
    let locked_url = url(locked.addr.port(), &format!("scroll/{}", registration.token()));
    assert!(matches!(ureq::post(&locked_url).set("Origin", "http://127.0.0.1:4500").set("Content-Type", "text/plain").send_string(payload), Err(ureq::Error::Status(423, _))));
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert_eq!(post(payload).unwrap().status(), 204);
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}
