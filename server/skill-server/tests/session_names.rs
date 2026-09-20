//! Session name HTTP persistence on private tmux/config state. This binary has
//! one test; no user agent, startup file, or Codex store is read or changed.
#![cfg(all(unix, feature = "local-backend"))]

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use skill_server::{spawn, RemoteControl, RemoteHost, RemoteStatus, RemoteTarget, ServerConfig};

struct Fixture {
    root: PathBuf,
    socket: PathBuf,
}

impl Fixture {
    fn command(&self) -> Command {
        let mut command = skill_core::process::hidden_command("tmux");
        command
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .env("TMUX_TMPDIR", &self.root)
            .arg("-S")
            .arg(&self.socket);
        command
    }

    fn shell(&self, id: &str, label: &str) {
        // Supply normal registry metadata for a real, startup-free shell. The
        // title reader sees agent=shell, so it cannot inspect a live agent store.
        fs::write(
            self.root
                .join("config/terminals")
                .join(format!("{id}.json")),
            json!({"label":label,"agent":"shell","cwd":self.root,"created":"1","session_id":""})
                .to_string(),
        )
        .unwrap();
        let output = self
            .command()
            .args([
                "-f",
                "/dev/null",
                "new-session",
                "-d",
                "-s",
                id,
                "bash",
                "--noprofile",
                "--norc",
                "-i",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // An explicit private socket is essential: never kill the user's tmux.
        let _ = self.command().arg("kill-server").output();
        let _ = fs::remove_dir_all(&self.root);
        // Keep TMUX_TMPDIR isolated until this test process exits, including
        // the remaining lifetime of its background HTTP worker threads.
    }
}

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

fn get(client: &ureq::Agent, base: &str) -> Value {
    let body = client
        .get(&format!("{base}/api/terminal/list"))
        .call()
        .unwrap()
        .into_string()
        .unwrap();
    serde_json::from_str(&body).unwrap()
}

fn post(
    client: &ureq::Agent,
    base: &str,
    route: &str,
    body: Value,
) -> Result<Value, Box<ureq::Error>> {
    let body = client
        .post(&format!("{base}/api/terminal/{route}"))
        .set("Content-Type", "application/json")
        .send_string(&body.to_string())?
        .into_string()
        .unwrap();
    Ok(serde_json::from_str(&body).unwrap())
}

fn session<'a>(list: &'a Value, id: &str) -> &'a Value {
    list.as_array()
        .unwrap()
        .iter()
        .find(|session| session["id"] == id)
        .expect("fixture session in inventory")
}

#[test]
fn names_persist_across_http_servers_and_proxy_without_changing_other_sessions() {
    if skill_core::process::hidden_command("tmux")
        .arg("-V")
        .output()
        .is_err()
    {
        eprintln!("tmux unavailable — skipping session name HTTP test");
        return;
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = PathBuf::from(format!("/tmp/vs-name-{}-{stamp}", std::process::id()));
    fs::create_dir_all(root.join("config/terminals")).unwrap();
    let socket_dir = root.join(format!("tmux-{}", fs::metadata(&root).unwrap().uid()));
    fs::create_dir(&socket_dir).unwrap();
    fs::set_permissions(&socket_dir, fs::Permissions::from_mode(0o700)).unwrap();
    let fixture = Fixture {
        socket: socket_dir.join("default"),
        root,
    };
    std::env::set_var("TMUX_TMPDIR", &fixture.root);
    skill_core::paths::set_config_dir(fixture.root.join("config"));
    let first_id = format!("ass-{}-{stamp}-0", std::process::id());
    let second_id = format!("ass-{}-{stamp}-1", std::process::id());
    fixture.shell(&first_id, "First shell");
    fixture.shell(&second_id, "Second shell");

    let host = spawn(ServerConfig {
        port: 0,
        startup_maintenance: false,
        ..Default::default()
    })
    .unwrap();
    let base = host.url();
    let client = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(3))
        .build();
    let original = get(&client, &base);
    assert_eq!(original.as_array().unwrap().len(), 2);
    assert_eq!(session(&original, &first_id)["label"], "First shell");
    assert!(session(&original, &first_id).get("customTitle").is_none());

    assert_eq!(
        post(
            &client,
            &base,
            "rename",
            json!({"id":first_id,"title":"  Review naming 🔎  "})
        )
        .unwrap(),
        json!({"customTitle":"Review naming 🔎","savedIn":"vibestudio"})
    );
    post(
        &client,
        &base,
        "rename",
        json!({"id":second_id,"title":"Investigate another task"}),
    )
    .unwrap();
    let durable = fixture.root.join("config/session-titles.json");
    let stored: Value = serde_json::from_slice(&fs::read(&durable).unwrap()).unwrap();
    assert_eq!(stored[&first_id], "Review naming 🔎");
    assert_eq!(stored[&second_id], "Investigate another task");

    let another = spawn(ServerConfig {
        port: 0,
        startup_maintenance: false,
        ..Default::default()
    })
    .unwrap();
    let other = another.url();
    let loaded = get(&client, &other);
    assert_eq!(
        session(&loaded, &first_id)["customTitle"],
        stored[&first_id]
    );
    assert_eq!(
        session(&loaded, &second_id)["customTitle"],
        stored[&second_id]
    );

    let before = fs::read(&durable).unwrap();
    for body in [
        json!({"id":first_id,"title":""}),
        json!({"id":first_id,"title":"a\nb"}),
        json!({"id":first_id,"title":"🔎".repeat(201)}),
        json!({"id":first_id,"title":42}),
        json!({"id":first_id}),
        json!({"id":"ass-0-0-0","title":"Missing session"}),
    ] {
        assert!(matches!(
            *post(&client, &other, "rename", body).unwrap_err(),
            ureq::Error::Status(400, _)
        ));
        assert_eq!(fs::read(&durable).unwrap(), before);
    }
    assert_eq!(
        post(
            &client,
            &other,
            "rename",
            json!({"id":first_id,"title":null})
        )
        .unwrap(),
        json!({"customTitle":null,"savedIn":"vibestudio"})
    );
    let reset = get(&client, &base);
    assert!(session(&reset, &first_id).get("customTitle").is_none());
    assert_eq!(session(&reset, &first_id)["label"], "First shell");
    assert_eq!(
        session(&reset, &second_id)["customTitle"],
        stored[&second_id]
    );

    // Marked responses prove that both routes reach the remote host; a local
    // fall-through would either fail on this ID or mutate the private store.
    let upstream = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let upstream_addr = upstream.server_addr().to_ip().unwrap();
    let worker = std::thread::spawn(move || {
        for route in ["/api/terminal/rename", "/api/terminal/list"] {
            let mut request = upstream
                .recv_timeout(Duration::from_secs(3))
                .unwrap()
                .unwrap();
            assert_eq!(request.url(), route);
            if route.ends_with("rename") {
                assert_eq!(request.method(), &tiny_http::Method::Post);
                let mut body = String::new();
                request.as_reader().read_to_string(&mut body).unwrap();
                assert_eq!(
                    serde_json::from_str::<Value>(&body).unwrap(),
                    json!({"id":"remote-session","title":"Remote name"})
                );
            }
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
    let switched = switchboard.url();
    assert_eq!(
        post(
            &client,
            &switched,
            "rename",
            json!({"id":"remote-session","title":"Remote name"})
        )
        .unwrap()["remote"],
        true
    );
    assert_eq!(get(&client, &switched)["remote"], true);
    worker.join().unwrap();
    assert_eq!(
        session(&get(&client, &base), &second_id)["customTitle"],
        stored[&second_id]
    );

    let valid_store = fs::read(&durable).unwrap();
    fs::write(&durable, "broken json").unwrap();
    let inventory = get(&client, &base);
    assert_eq!(
        inventory.as_array().unwrap().len(),
        2,
        "corrupt names must not hide running terminals"
    );
    assert!(session(&inventory, &second_id).get("customTitle").is_none());
    assert!(matches!(
        *post(
            &client,
            &base,
            "rename",
            json!({"id":second_id,"title":"Replacement"})
        )
        .unwrap_err(),
        ureq::Error::Status(400, _)
    ));
    assert_eq!(fs::read_to_string(&durable).unwrap(), "broken json");
    fs::write(&durable, valid_store).unwrap();

    post(&client, &base, "kill", json!({"id":second_id})).unwrap();
    let stored: Value = serde_json::from_slice(&fs::read(&durable).unwrap()).unwrap();
    assert!(
        stored.get(&second_id).is_none(),
        "kill removes its custom name"
    );
    let remaining = get(&client, &other);
    assert_eq!(remaining.as_array().unwrap().len(), 1);
    assert_eq!(remaining[0]["id"], first_id);

    // Every registered agent can use a durable local name, even when its CLI
    // exposes no native rename API. Only change private metadata on our shell;
    // no agent process is launched and no native conversation ID is supplied.
    let metadata_file = fixture.root.join("config/terminals").join(format!("{first_id}.json"));
    let original_metadata = fs::read(&metadata_file).unwrap();
    let mut metadata: Value = serde_json::from_slice(&original_metadata).unwrap();
    for agent in skill_core::agents::AGENTS {
        metadata["agent"] = json!(agent.family);
        fs::write(&metadata_file, metadata.to_string()).unwrap();
        let title = format!("Review {} session", agent.label);
        assert_eq!(
            post(&client, &base, "rename", json!({"id":first_id,"title":title})).unwrap(),
            json!({"customTitle":title,"savedIn":"vibestudio"}),
            "{} must support local rename without a native API", agent.family
        );
        let persisted: Value = serde_json::from_slice(&fs::read(&durable).unwrap()).unwrap();
        assert_eq!(persisted[&first_id], title);
        assert_eq!(
            post(&client, &other, "rename", json!({"id":first_id,"title":null})).unwrap(),
            json!({"customTitle":null,"savedIn":"vibestudio"})
        );
    }
    // Restore shell metadata before inventory enrichment so this fixture never
    // opens the user's agent stores while checking the reset result.
    fs::write(metadata_file, original_metadata).unwrap();
    assert!(session(&get(&client, &other), &first_id).get("customTitle").is_none());
}
