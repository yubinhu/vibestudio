//! Real HTTP persistence checks, isolated from the user's configuration and
//! without startup maintenance, agents, SSH, or discovery.
use serde_json::{json, Value};
use skill_server::{spawn, RemoteControl, RemoteHost, RemoteStatus, RemoteTarget, ServerConfig};
use std::sync::Arc;

struct FixedRemote(RemoteTarget);
impl RemoteControl for FixedRemote {
    fn list_hosts(&self) -> Result<Vec<RemoteHost>, String> {
        Ok(vec![])
    }
    fn connect(&self, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn disconnect(&self, _: bool) -> Result<(), String> {
        Ok(())
    }
    fn status(&self) -> RemoteStatus {
        RemoteStatus {
            state: "connected".into(),
            host: Some("test-remote".into()),
            message: None,
        }
    }
    fn active_target(&self) -> Option<RemoteTarget> {
        Some(self.0.clone())
    }
}

fn get(base: &str, route: &str) -> Value {
    let text = ureq::get(&format!("{base}/api/{route}"))
        .call()
        .unwrap()
        .into_string()
        .unwrap();
    serde_json::from_str(&text).unwrap()
}

fn post(base: &str, route: &str, body: Value) -> Value {
    let text = ureq::post(&format!("{base}/api/{route}"))
        .set("Content-Type", "application/json")
        .send_string(&body.to_string())
        .unwrap()
        .into_string()
        .unwrap();
    serde_json::from_str(&text).unwrap()
}

#[test]
fn defaults_and_history_survive_another_server_instance() {
    let dir =
        std::env::temp_dir().join(format!("vibestudio-workspace-http-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    skill_core::paths::set_config_dir(dir.join("config"));
    let first = spawn(ServerConfig {
        port: 0,
        startup_maintenance: false,
        ..Default::default()
    })
    .unwrap();
    let base = format!("http://{}", first.addr);
    assert_eq!(get(&base, "preferences")["pickerLocations"], json!({}));

    let open = dir.join("documents");
    let import = dir.join("downloads");
    std::fs::create_dir(&open).unwrap();
    std::fs::create_dir(&import).unwrap();
    post(
        &base,
        "preferences/picker",
        json!({"context":"open", "path":open}),
    );
    post(
        &base,
        "preferences/picker",
        json!({"context":"import", "path":import}),
    );
    post(
        &base,
        "preferences/terminal",
        json!({"agentId":"shell:cli", "prefs":{"cwd":open, "auto":true, "skip":true}}),
    );

    // Invalid selection must leave the previous good default intact.
    let invalid = ureq::post(&format!("{base}/api/preferences/picker"))
        .set("Content-Type", "application/json")
        .send_string(&json!({"context":"open", "path":dir.join("missing")}).to_string());
    assert!(matches!(invalid, Err(ureq::Error::Status(400, _))));

    let skill = open.join("example-skill");
    let markdown = open.join("notes.md");
    post(
        &base,
        "recents/add",
        json!({"root":skill,"name":"Example skill"}),
    );
    post(
        &base,
        "recents/add",
        json!({"root":markdown,"name":"notes.md","kind":"markdown"}),
    );
    post(
        &base,
        "recents/add",
        json!({"root":skill,"name":"Example renamed","kind":"skill"}),
    );

    // No per-server in-memory cache: another server reads the same on-disk state.
    let second = spawn(ServerConfig {
        port: 0,
        startup_maintenance: false,
        ..Default::default()
    })
    .unwrap();
    let other = format!("http://{}", second.addr);
    let prefs = get(&other, "preferences");
    assert_eq!(prefs["pickerLocations"]["open"], open.to_str().unwrap());
    assert_eq!(prefs["pickerLocations"]["import"], import.to_str().unwrap());
    assert_eq!(prefs["lastAgent"], "shell:cli");
    assert_eq!(
        prefs["terminal"]["shell:cli"]["cwd"],
        open.to_str().unwrap()
    );
    assert_eq!(prefs["terminal"]["shell:cli"]["skip"], false);
    let recents = get(&other, "recents/list");
    assert_eq!(recents.as_array().unwrap().len(), 2);
    assert_eq!(recents[0]["name"], "Example renamed");
    assert!(recents[0]["openedAt"].as_u64().unwrap() > 0);
    assert_eq!(recents[1]["kind"], "markdown");
    post(&other, "recents/remove", json!({"root":skill}));
    assert_eq!(get(&base, "recents/list").as_array().unwrap().len(), 1);

    // These routes must reach the answering remote, not the connecting client's
    // preferences file. A marked fake upstream proves both reads and writes route.
    let upstream = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let upstream_addr = upstream.server_addr().to_ip().unwrap();
    let worker = std::thread::spawn(move || {
        for _ in 0..4 {
            let request = upstream.recv().unwrap();
            assert!(
                request.url().starts_with("/api/preferences")
                    || request.url().starts_with("/api/recents")
            );
            request
                .respond(tiny_http::Response::from_string(r#"{"remote":true}"#))
                .unwrap();
        }
    });
    let switchboard = spawn(ServerConfig {
        port: 0,
        startup_maintenance: false,
        remote: Some(Arc::new(FixedRemote(RemoteTarget {
            base_url: format!("http://{upstream_addr}"),
            token: "test".into(),
        }))),
        ..Default::default()
    })
    .unwrap();
    let switched = format!("http://{}", switchboard.addr);
    assert_eq!(get(&switched, "preferences")["remote"], true);
    assert_eq!(
        post(
            &switched,
            "preferences/picker",
            json!({"context":"open","path":"/remote/docs"})
        )["remote"],
        true
    );
    assert_eq!(get(&switched, "recents/list")["remote"], true);
    assert_eq!(
        post(
            &switched,
            "recents/add",
            json!({"root":"/remote/notes.md","name":"notes.md","kind":"markdown"})
        )["remote"],
        true
    );
    worker.join().unwrap();
    assert_eq!(
        get(&base, "preferences")["pickerLocations"]["open"],
        open.to_str().unwrap()
    );

    // HTTP callers receive an error instead of overwriting a corrupt store.
    let file = dir.join("config/preferences.json");
    std::fs::write(&file, "broken").unwrap();
    let bad = ureq::post(&format!("{base}/api/preferences/picker"))
        .set("Content-Type", "application/json")
        .send_string(&json!({"context":"open", "path":open}).to_string());
    assert!(matches!(bad, Err(ureq::Error::Status(400, _))));
    assert_eq!(std::fs::read_to_string(file).unwrap(), "broken");
    std::fs::remove_dir_all(dir).unwrap();
}
