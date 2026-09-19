//! Real terminal-link HTTP contract on a private tmux server. No live user
//! session, configuration, shell startup file, or working tree is modified.
#![cfg(all(unix, feature = "local-backend"))]

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use skill_server::{spawn, RemoteControl, RemoteHost, RemoteStatus, RemoteTarget, ServerConfig};

struct Fixture {
    root: PathBuf,
    id: String,
    original_tmpdir: Option<std::ffi::OsString>,
}

impl Fixture {
    fn command(&self) -> Command {
        let mut command = skill_core::process::hidden_command("tmux");
        command.env_remove("TMUX").env("TMUX_TMPDIR", &self.root);
        command
    }

    fn run(&self, args: &[&str]) {
        let output = self.command().args(args).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.command().arg("kill-server").output();
        if let Some(original) = &self.original_tmpdir {
            std::env::set_var("TMUX_TMPDIR", original);
        } else {
            std::env::remove_var("TMUX_TMPDIR");
        }
        let _ = fs::remove_dir_all(&self.root);
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
            host: Some("terminal-host".into()),
            message: None,
        }
    }
    fn active_target(&self) -> Option<RemoteTarget> {
        Some(self.0.clone())
    }
}

fn resolve(base: &str, id: &str, path: &str) -> Result<Value, Box<ureq::Error>> {
    let response = ureq::post(&format!("{base}/api/terminal/resolve-link"))
        .set("Content-Type", "application/json")
        .send_string(&json!({"id":id,"path":path}).to_string())?;
    Ok(serde_json::from_str(&response.into_string().unwrap()).unwrap())
}

#[test]
fn terminal_links_resolve_current_host_files_through_http_and_remote_proxy() {
    if skill_core::process::hidden_command("tmux")
        .arg("-V")
        .output()
        .is_err()
    {
        eprintln!("tmux unavailable — skipping terminal link HTTP test");
        return;
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = PathBuf::from(format!("/tmp/vs-link-http-{}-{stamp}", std::process::id()));
    fs::create_dir_all(root.join("first")).unwrap();
    fs::create_dir_all(root.join("second")).unwrap();
    let fixture = Fixture {
        root,
        id: format!("ass-{}-{stamp}-10", std::process::id()),
        original_tmpdir: std::env::var_os("TMUX_TMPDIR"),
    };
    // This integration-test binary has one test. Environment setup precedes all
    // server threads and isolates the library's normal tmux socket discovery.
    std::env::set_var("TMUX_TMPDIR", &fixture.root);
    skill_core::paths::set_config_dir(fixture.root.join("config"));
    let first = fs::canonicalize(fixture.root.join("first")).unwrap();
    let second = fs::canonicalize(fixture.root.join("second")).unwrap();
    fs::write(first.join("file with spaces.rs"), "first").unwrap();
    fs::write(second.join("file with spaces.rs"), "second").unwrap();
    fixture.run(&[
        "-f",
        "/dev/null",
        "new-session",
        "-d",
        "-s",
        &fixture.id,
        "-c",
        first.to_str().unwrap(),
        "bash",
        "--noprofile",
        "--norc",
        "-i",
    ]);
    let host = spawn(ServerConfig {
        port: 0,
        startup_maintenance: false,
        ..Default::default()
    })
    .unwrap();
    let base = format!("http://{}", host.addr);
    let link = resolve(&base, &fixture.id, "file with spaces.rs").unwrap();
    assert_eq!(
        link,
        json!({
            "path": first.join("file with spaces.rs"),
            "root": first,
            "rel": "file with spaces.rs",
        })
    );
    for (id, path) in [
        (fixture.id.as_str(), "missing.rs"),
        (fixture.id.as_str(), "."),
        (fixture.id.as_str(), "bad\nname"),
        (fixture.id.strip_suffix('0').unwrap(), "file with spaces.rs"),
        ("user-session", "file with spaces.rs"),
    ] {
        assert!(matches!(
            *resolve(&base, id, path).unwrap_err(),
            ureq::Error::Status(400, _)
        ));
    }

    fixture.run(&[
        "send-keys",
        "-t",
        &fixture.id,
        &format!("cd -- '{}'", second.to_str().unwrap()),
        "Enter",
    ]);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if resolve(&base, &fixture.id, "file with spaces.rs").unwrap()["root"] == json!(second) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "HTTP resolver did not follow live cd"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    let switchboard = spawn(ServerConfig {
        port: 0,
        startup_maintenance: false,
        remote: Some(Arc::new(FixedRemote(RemoteTarget {
            base_url: base,
            token: String::new(),
        }))),
        ..Default::default()
    })
    .unwrap();
    let remote = resolve(
        &format!("http://{}", switchboard.addr),
        &fixture.id,
        "file with spaces.rs",
    )
    .unwrap();
    assert_eq!(remote["root"], json!(second));
    assert_eq!(remote["rel"], "file with spaces.rs");
}
