//! Real detached-worker tests. Every child gets private config and tmux roots;
//! startup maintenance is disabled so this cannot reap/warm the user's engines.
#![cfg(all(feature = "local-backend", unix))]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
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

/// A versioned legacy worker on private loopback/config roots. The production
/// server binary remains unmodified; its release floor comes from its real
/// compiled version while this fixture can advertise an older health response.
struct LegacyWorker {
    record: HostServiceRecord,
    exit: Arc<AtomicBool>,
    stops: Arc<AtomicUsize>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl LegacyWorker {
    fn start(fixture: &Fixture, actual_version: &str, recorded_version: &str) -> Self {
        Self::start_with_failure(fixture, actual_version, recorded_version, false)
    }

    fn start_with_failure(
        fixture: &Fixture,
        actual_version: &str,
        recorded_version: &str,
        fail_next_launch: bool,
    ) -> Self {
        let dir = fixture.0.join("config/vibestudio");
        fs::create_dir_all(&dir).unwrap();
        let lifetime = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join("host-service.lock"))
            .unwrap();
        lifetime.lock().unwrap();
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let record = HostServiceRecord {
            protocol: 1,
            instance_id: format!("{:032x}", NEXT.fetch_add(1, Ordering::Relaxed)),
            pid: std::process::id(),
            port: server.server_addr().to_ip().unwrap().port(),
            version: recorded_version.into(),
            started_at: 0,
        };
        fs::write(
            dir.join("host-service.json"),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
        let identity =
            serde_json::json!({"protocol": record.protocol, "instanceId": record.instance_id});
        let health = serde_json::json!({"pid": record.pid, "version": actual_version, "hostService": identity});
        let exit = Arc::new(AtomicBool::new(false));
        let stops = Arc::new(AtomicUsize::new(0));
        let thread_exit = exit.clone();
        let thread_stops = stops.clone();
        let thread = std::thread::spawn(move || {
            while !thread_exit.load(Ordering::SeqCst) {
                let Some(mut request) = server.recv_timeout(Duration::from_millis(50)).unwrap()
                else {
                    continue;
                };
                match request.url() {
                    "/api/health" => {
                        request
                            .respond(tiny_http::Response::from_string(health.to_string()))
                            .unwrap();
                    }
                    "/api/host-service/stop" => {
                        let mut body = String::new();
                        request.as_reader().read_to_string(&mut body).unwrap();
                        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
                        assert_eq!(body["instanceId"], identity["instanceId"]);
                        // The real Stop route takes the startup lock. Holding it
                        // here catches a launcher that forgets to release it.
                        let startup = fs::OpenOptions::new()
                            .read(true)
                            .write(true)
                            .create(true)
                            .truncate(false)
                            .open(dir.join("host-service-startup.lock"))
                            .unwrap();
                        startup
                            .try_lock()
                            .expect("launcher must release startup lock before Stop");
                        fs::write(
                            dir.join("host-service-stopped.json"),
                            serde_json::to_vec(&identity).unwrap(),
                        )
                        .unwrap();
                        if fail_next_launch {
                            // Fail the replacement's log-open after verified
                            // Stop, proving an unsuccessful update never makes
                            // the client accept its old incompatible worker.
                            fs::create_dir(dir.join("host-service.log")).unwrap();
                        }
                        thread_stops.fetch_add(1, Ordering::SeqCst);
                        request
                            .respond(tiny_http::Response::from_string("{}"))
                            .unwrap();
                        break;
                    }
                    _ => {
                        request.respond(tiny_http::Response::empty(404)).unwrap();
                    }
                }
            }
            drop(server);
            drop(lifetime);
        });
        Self {
            record,
            exit,
            stops,
            thread: Some(thread),
        }
    }
}

impl Drop for LegacyWorker {
    fn drop(&mut self) {
        self.exit.store(true, Ordering::SeqCst);
        self.thread.take().unwrap().join().unwrap();
    }
}

#[test]
fn recovery_rejects_older_worker_then_explicit_launch_upgrades_without_trusting_record_version() {
    let fixture = Fixture::new();
    let legacy = LegacyWorker::start(&fixture, "0.0.0-alpha", "999.0.0");
    let recovery = fixture.command().arg("--recover").output().unwrap();
    assert!(!recovery.status.success());
    let error = String::from_utf8_lossy(&recovery.stderr);
    assert!(
        error.contains("incompatible host service version 0.0.0-alpha"),
        "{error}"
    );
    assert!(error.contains("Retry"), "{error}");
    assert_eq!(legacy.stops.load(Ordering::SeqCst), 0);
    assert!(healthy(&legacy.record));

    let upgrade = fixture.command().output().unwrap();
    assert!(
        upgrade.status.success(),
        "{}",
        String::from_utf8_lossy(&upgrade.stderr)
    );
    let current = fixture.record();
    assert_ne!(current.instance_id, legacy.record.instance_id);
    assert_eq!(current.port, legacy.record.port);
    assert_eq!(current.version, env!("CARGO_PKG_VERSION"));
    assert_eq!(legacy.stops.load(Ordering::SeqCst), 1);
    assert!(healthy(&current));
}

#[test]
fn failed_replacement_reports_retry_and_can_be_started_after_the_failure_is_fixed() {
    let fixture = Fixture::new();
    let legacy = LegacyWorker::start_with_failure(&fixture, "0.0.0-alpha", "0.0.0-alpha", true);
    let result = fixture.command().output().unwrap();
    assert!(!result.status.success());
    let error = String::from_utf8_lossy(&result.stderr);
    assert!(
        error.contains("host service update did not finish"),
        "{error}"
    );
    assert!(error.contains("Retry"), "{error}");
    assert!(error.contains("tmux sessions were left running"), "{error}");
    assert_eq!(legacy.stops.load(Ordering::SeqCst), 1);
    assert!(!healthy(&legacy.record));
    fs::remove_dir(fixture.0.join("config/vibestudio/host-service.log")).unwrap();
    let retry = fixture.command().output().unwrap();
    assert!(
        retry.status.success(),
        "{}",
        String::from_utf8_lossy(&retry.stderr)
    );
    assert!(healthy(&fixture.record()));
}

#[test]
fn newer_host_is_reused_and_unknown_health_version_is_not_accepted_or_stopped() {
    for (actual, recorded, accepted) in [
        ("999.0.0", "0.0.0-alpha", true),
        ("unknown", "999.0.0", false),
    ] {
        let fixture = Fixture::new();
        let legacy = LegacyWorker::start(&fixture, actual, recorded);
        for recover in [false, true] {
            let mut command = fixture.command();
            if recover {
                command.arg("--recover");
            }
            let result = command.output().unwrap();
            assert_eq!(
                result.status.success(),
                accepted,
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            if accepted {
                let output = String::from_utf8_lossy(&result.stdout);
                assert!(output.contains("\"version\":\"999.0.0\""), "{output}");
            }
            assert_eq!(legacy.stops.load(Ordering::SeqCst), 0);
            assert_eq!(fixture.record(), legacy.record);
        }
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
