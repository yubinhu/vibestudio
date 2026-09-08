//! Real detached-worker tests. Every child gets private config and tmux roots;
//! startup maintenance is disabled so this cannot reap/warm the user's engines.
#![cfg(all(feature = "local-backend", unix))]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use skill_server::host_service::{healthy, stop, HostServiceRecord};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = PathBuf::from("/tmp").join(format!(
            "vs-host-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        fs::create_dir(path.join("tmux")).unwrap();
        fs::create_dir(path.join("dist")).unwrap();
        fs::write(path.join("dist/index.html"), "host service fixture").unwrap();
        Self(path)
    }

    fn base_command(&self) -> Command {
        let mut command = skill_core::process::hidden_command(env!("CARGO_BIN_EXE_skill-server"));
        command
            .env("XDG_CONFIG_HOME", self.0.join("config"))
            .env("TMUX_TMPDIR", self.0.join("tmux"))
            .env_remove("VIBESTUDIO_SERVER_TOKEN")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn command(&self) -> Command {
        let mut command = self.base_command();
        command
            .arg("--daemon")
            .arg("--port")
            .arg("0")
            .arg("--dist")
            .arg(self.0.join("dist"))
            .arg("--no-startup-maintenance");
        command
    }

    fn stop(&self) -> std::process::Output {
        self.base_command()
            .arg("--stop-host-service")
            .output()
            .unwrap()
    }

    fn record(&self) -> HostServiceRecord {
        serde_json::from_slice(
            &fs::read(self.0.join("config/vibestudio/host-service.json")).unwrap(),
        )
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Ok(bytes) = fs::read(self.0.join("config/vibestudio/host-service.json")) {
            if let Ok(record) = serde_json::from_slice::<HostServiceRecord>(&bytes) {
                if healthy(&record) {
                    let _ = self.stop();
                    if healthy(&record) {
                        eprintln!(
                            "Retaining live fixture state for cleanup: {}",
                            self.0.display()
                        );
                        return;
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(350));
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn wait_stopped(record: &HostServiceRecord) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while healthy(record) {
        assert!(Instant::now() < deadline, "host service did not stop");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn concurrent_accessors_share_worker_that_outlives_launchers_and_reuses_port_after_stop() {
    let fixture = Fixture::new();
    let children: Vec<_> = (0..3).map(|_| fixture.command().spawn().unwrap()).collect();
    let mut records = Vec::new();
    for child in children {
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8(output.stdout).unwrap();
        let record: HostServiceRecord = text
            .lines()
            .find_map(|line| {
                line.strip_prefix("SKILL_HOST_SERVICE_READY ")
                    .and_then(|json| serde_json::from_str(json).ok())
            })
            .expect("daemon launcher ready record");
        records.push(record);
    }
    let first = fixture.record();
    assert!(records.iter().all(|record| record == &first));
    assert!(
        healthy(&first),
        "worker must survive all launching processes exiting and stdin EOF"
    );
    assert_eq!(
        ureq::get(&format!("{}/", first.base_url()))
            .call()
            .unwrap()
            .into_string()
            .unwrap(),
        "host service fixture"
    );
    // The shared worker never exposes an onward SSH connection manager.
    assert!(matches!(
        ureq::get(&format!("{}/api/remote/status", first.base_url())).call(),
        Err(ureq::Error::Status(404, _))
    ));
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::metadata(fixture.0.join("config/vibestudio/host-service.log"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);

    // Stop is an explicit HTTP operation and verifies the generation. A caller
    // holding an unrelated/stale instance ID cannot terminate this worker.
    let bad = HostServiceRecord {
        instance_id: "f".repeat(32),
        ..first.clone()
    };
    assert!(stop(&bad).is_err());
    assert!(healthy(&first));
    let stopped = fixture.stop();
    assert!(
        stopped.status.success(),
        "{}",
        String::from_utf8_lossy(&stopped.stderr)
    );
    wait_stopped(&first);
    // Another accessor's automatic repair cannot undo the operator's Stop.
    for _ in 0..2 {
        let recovery = fixture.command().arg("--recover").output().unwrap();
        assert!(!recovery.status.success());
        assert!(String::from_utf8_lossy(&recovery.stderr).contains("explicitly stopped"));
        assert!(!healthy(&first));
        assert_eq!(fixture.record(), first);
    }

    let output = fixture.command().output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let second = fixture.record();
    assert_ne!(first.instance_id, second.instance_id);
    assert_eq!(
        first.port, second.port,
        "restart should keep gateway and phone URLs stable"
    );
    assert!(healthy(&second));
    assert!(!fixture
        .0
        .join("config/vibestudio/host-service-stopped.json")
        .exists());
    let recovery = fixture.command().arg("--recover").output().unwrap();
    assert!(recovery.status.success());
    assert_eq!(fixture.record(), second);

    // Retry can race the 250ms Stop acknowledgement window. It must wait and
    // launch a new generation instead of returning the still-healthy dying one.
    ureq::post(&format!("{}/api/host-service/stop", second.base_url()))
        .set("Content-Type", "application/json")
        .send_string(&serde_json::json!({"instanceId": second.instance_id}).to_string())
        .unwrap();
    let output = fixture.command().output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let third = fixture.record();
    assert_ne!(third.instance_id, second.instance_id);
    assert_eq!(third.port, second.port);
    assert!(healthy(&third));
}
