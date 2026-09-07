//! Connector discovery must cross the same HTTP boundary locally and remotely.
//! A configured command is metadata: a passive scan must never execute it or
//! return its arguments, headers, environment values, or URL credentials.
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use skill_server::{spawn, ServerConfig};

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn connector_discovery_preserves_scope_and_redacts_secrets_without_running_commands() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let fixture = Fixture(std::env::temp_dir().join(format!(
        "vibestudio-connectors-{}-{nonce}",
        std::process::id()
    )));
    std::fs::create_dir_all(&fixture.0).unwrap();
    let marker = fixture.0.join("must-not-exist");
    let secret = "connector-smoke-sensitive-value";
    std::fs::write(
        fixture.0.join(".mcp.json"),
        json!({
            "mcpServers": {
                "vibestudio-smoke-remote": {
                    "type": "http",
                    "url": format!("https://user:{secret}@example.test/mcp?token={secret}"),
                    "headers": {"Authorization": format!("Bearer {secret}")}
                },
                "vibestudio-smoke-local": {
                    "command": "sh",
                    "args": ["-c", format!("touch '{}'", marker.display()), secret],
                    "env": {"PRIVATE_KEY": secret}
                }
            }
        })
        .to_string(),
    )
    .unwrap();
    let server = spawn(ServerConfig {
        port: 0,
        startup_maintenance: false,
        ..Default::default()
    })
    .unwrap();
    let base = format!("http://127.0.0.1:{}", server.addr.port());
    let body = ureq::get(&format!(
        "{base}/api/connectors/discover?project={}",
        urlencoding::encode(&fixture.0.to_string_lossy())
    ))
    .call()
    .unwrap()
    .into_string()
    .unwrap();
    assert!(
        !body.contains(secret),
        "secret values must not cross the API"
    );
    assert!(
        !body.contains("must-not-exist"),
        "command arguments must not cross the API"
    );
    assert!(
        !marker.exists(),
        "a passive scan must not run a configured command"
    );
    let response: Value = serde_json::from_str(&body).unwrap();
    let connectors = response["connectors"]
        .as_array()
        .expect("inventory contract");
    let remote = connectors
        .iter()
        .find(|c| c["name"] == "vibestudio-smoke-remote")
        .expect("project connector");
    assert_eq!(remote["host"], "example.test");
    assert!(remote["availability"].as_array().unwrap().iter().any(|a| {
        a["agentId"] == "claude" && a["scope"] == "project" && a["state"] == "configured"
    }));

    // Validation occurs before the explicit runtime route could start any agent.
    let invalid = ureq::post(&format!("{base}/api/connectors/check"))
        .set("Content-Type", "application/json")
        .send_string(r#"{"project":"relative/path"}"#);
    assert!(
        matches!(invalid, Err(ureq::Error::Status(400, _))),
        "relative project must be rejected"
    );
}
