//! Durable per-user backend. Accessors own tunnels and windows; this process owns
//! the host's terminals, gateway, phone endpoint and attention watcher.
//!
//! The stable record is shared across executable paths and compatible versions.
//! A startup lock serializes launchers; a separate lifetime lock prevents even a
//! directly invoked worker from starting a second backend. Neither lock is ever
//! broken by deleting a file, and no stale record is trusted without HTTP identity.
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;
pub const STOPPED_MESSAGE: &str =
    "The host service was explicitly stopped. Choose Retry or reconnect to start it.";
const READY_TIMEOUT: Duration = Duration::from_secs(25);
const POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Clone, Debug)]
pub struct HostServiceOptions {
    pub executable: PathBuf,
    pub dist: PathBuf,
    pub bundled_skills: Option<PathBuf>,
    pub examples_base: Option<PathBuf>,
    pub preferred_port: u16,
    pub version: String,
    pub startup_maintenance: bool,
    /// Automatic repair respects a prior explicit Stop; user starts clear it.
    pub recovery: bool,
}

impl Default for HostServiceOptions {
    fn default() -> Self {
        Self {
            executable: std::env::current_exe().unwrap_or_default(),
            dist: std::env::var_os("SKILL_DIST")
                .map(PathBuf::from)
                .unwrap_or_else(|| "dist".into()),
            bundled_skills: None,
            examples_base: None,
            preferred_port: crate::phone::PHONE_PORT,
            version: env!("CARGO_PKG_VERSION").into(),
            startup_maintenance: true,
            recovery: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HostServiceIdentity {
    pub protocol: u32,
    pub instance_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HostServiceRecord {
    pub protocol: u32,
    pub instance_id: String,
    pub pid: u32,
    pub port: u16,
    pub version: String,
    pub started_at: u64,
}

impl HostServiceRecord {
    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn identity(&self) -> HostServiceIdentity {
        HostServiceIdentity {
            protocol: self.protocol,
            instance_id: self.instance_id.clone(),
        }
    }
}

fn private_file(path: &Path) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .map_err(|error| format!("Could not open host-service state: {error}"))
}

fn lock_until(file: &File, deadline: Instant) -> Result<(), String> {
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(()),
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(POLL_INTERVAL)
            }
            Err(TryLockError::WouldBlock) => {
                return Err(
                    "The host service is still starting. Try connecting again shortly.".into(),
                )
            }
            Err(TryLockError::Error(error)) => {
                return Err(format!("Could not lock host-service state: {error}"))
            }
        }
    }
}

fn read_record(dir: &Path) -> Option<HostServiceRecord> {
    let bytes = fs::read(dir.join("host-service.json")).ok()?;
    if bytes.len() > 16 * 1024 {
        return None;
    }
    let record: HostServiceRecord = serde_json::from_slice(&bytes).ok()?;
    (record.port > 0 && record.pid > 0 && !record.instance_id.is_empty()).then_some(record)
}

/// Prove the record belongs to the answering worker, not a recycled PID/port or
/// a legacy switchboard. A version string alone is not sufficient identity.
pub fn healthy(record: &HostServiceRecord) -> bool {
    let response = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_millis(500))
        .timeout(Duration::from_secs(1))
        .build()
        .get(&format!("{}/api/health", record.base_url()))
        .call();
    let Ok(response) = response else {
        return false;
    };
    let mut body = String::new();
    if response
        .into_reader()
        .take(16 * 1024)
        .read_to_string(&mut body)
        .is_err()
    {
        return false;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&body) else {
        return false;
    };
    value.get("pid").and_then(|pid| pid.as_u64()) == Some(record.pid as u64)
        && value
            .get("hostService")
            .and_then(|value| serde_json::from_value::<HostServiceIdentity>(value.clone()).ok())
            == Some(record.identity())
}

fn compatible(record: HostServiceRecord) -> Result<HostServiceRecord, String> {
    if record.protocol != PROTOCOL_VERSION {
        return Err(format!("The running host service uses protocol {} (this client supports {}). Update the client or restart that host service explicitly; its running agents were left untouched.", record.protocol, PROTOCOL_VERSION));
    }
    Ok(record)
}

/// Reuse the healthy per-user worker or detach a new copy of `options.executable`.
/// Desktop passes its own bundled executable; no separate sidecar is required.
pub fn ensure(options: &HostServiceOptions) -> Result<HostServiceRecord, String> {
    let dir = skill_core::paths::ensure_config_dir()?;
    ensure_in(&dir, options)
}

// Keep detached children reaped if the accessor remains open when a worker is
// explicitly stopped. Dropping this guard never terminates the worker.
struct ReapOnDrop(Option<Child>);
impl Drop for ReapOnDrop {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
    }
}

fn stop_intent(dir: &Path, record: Option<&HostServiceRecord>) -> Result<bool, String> {
    let bytes = match fs::read(dir.join("host-service-stopped.json")) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("Could not read host-service stop intent: {error}")),
    };
    let identity: HostServiceIdentity = serde_json::from_slice(&bytes)
        .map_err(|error| format!("Could not read host-service stop intent: {error}"))?;
    Ok(record.is_none_or(|record| record.identity() == identity))
}

fn ensure_in(dir: &Path, options: &HostServiceOptions) -> Result<HostServiceRecord, String> {
    let deadline = Instant::now() + READY_TIMEOUT;
    let startup = private_file(&dir.join("host-service-startup.lock"))?;
    lock_until(&startup, deadline)?;
    // Serialize even the healthy case against Stop, so a user Retry during
    // shutdown cannot return a healthy-but-doomed instance before clearing intent.
    let record = read_record(dir);
    let stopped = stop_intent(dir, record.as_ref())?;
    if stopped && options.recovery {
        return Err(STOPPED_MESSAGE.into());
    }
    if stopped {
        let lifetime = private_file(&dir.join("host-service.lock"))?;
        lock_until(&lifetime, deadline)?;
        fs::remove_file(dir.join("host-service-stopped.json"))
            .map_err(|error| error.to_string())?;
        // Explicit user intent only clears Stop once the old worker is gone.
        drop(lifetime);
    }
    if let Some(record) = read_record(dir).filter(healthy) {
        return compatible(record);
    }
    let lifetime = private_file(&dir.join("host-service.lock"))?;
    let mut child = ReapOnDrop(match lifetime.try_lock() {
        Ok(()) => {
            lifetime.unlock().map_err(|error| error.to_string())?;
            Some(launch_worker(dir, options)?)
        }
        Err(TryLockError::WouldBlock) => None,
        Err(TryLockError::Error(error)) => {
            return Err(format!("Could not inspect the host-service lock: {error}"))
        }
    });
    loop {
        if let Some(record) = read_record(dir).filter(healthy) {
            return compatible(record);
        }
        if let Some(child) = child.0.as_mut() {
            if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
                if !status.success() {
                    return Err(format!(
                        "The host service could not start. See {}.",
                        dir.join("host-service.log").display()
                    ));
                }
            }
        }
        if Instant::now() >= deadline {
            return Err(format!("The host service is running or starting but did not answer its health check. See {}. Running agents were left untouched.", dir.join("host-service.log").display()));
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn absolute(path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .map_err(|error| error.to_string())
    }
}

fn launch_worker(dir: &Path, options: &HostServiceOptions) -> Result<Child, String> {
    let log_path = dir.join("host-service.log");
    if fs::metadata(&log_path).is_ok_and(|metadata| metadata.len() > 10 * 1024 * 1024) {
        let _ = fs::rename(&log_path, dir.join("host-service.previous.log"));
    }
    let log = OpenOptions::new()
        .append(true)
        .create(true)
        .open(&log_path)
        .map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        log.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|error| error.to_string())?;
    }
    let mut command = skill_core::process::hidden_command(absolute(&options.executable)?);
    command
        .arg("--host-service")
        .arg("--host-service-config")
        .arg(absolute(dir)?)
        .arg("--port")
        .arg(options.preferred_port.to_string())
        .arg("--dist")
        .arg(absolute(&options.dist)?)
        .arg("--host-service-version")
        .arg(&options.version)
        .env_remove("VIBESTUDIO_SERVER_TOKEN")
        .stdin(Stdio::null())
        .stdout(log.try_clone().map_err(|error| error.to_string())?)
        .stderr(log);
    if let Some(path) = &options.bundled_skills {
        command.arg("--bundled-skills").arg(absolute(path)?);
    }
    if let Some(path) = &options.examples_base {
        command.arg("--examples-base").arg(absolute(path)?);
    }
    if !options.startup_maintenance {
        command.arg("--no-startup-maintenance");
    }
    if options.recovery {
        command.arg("--recover");
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // The detached worker can outlive or lose access to its launch folder.
        // All resource/config arguments above are absolute before changing cwd.
        command.current_dir("/");
        // setsid is async-signal-safe and detaches the worker from SSH's session
        // and terminal. All inherited streams have already been redirected.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0000_0008 | 0x0000_0200); // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP
    }
    command
        .spawn()
        .map_err(|error| format!("Could not launch the host service: {error}"))
}

fn write_record(dir: &Path, record: &HostServiceRecord) -> Result<(), String> {
    let path = dir.join("host-service.json.tmp");
    let mut file = private_file(&path)?;
    file.set_len(0).map_err(|error| error.to_string())?;
    serde_json::to_writer(&mut file, record).map_err(|error| error.to_string())?;
    file.write_all(b"\n").map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())?;
    drop(file);
    fs::rename(path, dir.join("host-service.json")).map_err(|error| error.to_string())
}

/// Run a worker in this process. Returns immediately if another worker already
/// owns the per-user lifetime lock. It never becomes an SSH switchboard.
pub fn run(options: HostServiceOptions) -> Result<(), String> {
    // Native headless entry bypasses Tauri setup; stdout/stderr are already
    // redirected by ensure. Standalone may have installed this logger already.
    crate::init_logging();
    let dir = skill_core::paths::ensure_config_dir()?;
    if options.recovery && stop_intent(&dir, read_record(&dir).as_ref())? {
        return Err(STOPPED_MESSAGE.into());
    }
    let lifetime = private_file(&dir.join("host-service.lock"))?;
    match lifetime.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Ok(()),
        Err(TryLockError::Error(error)) => {
            return Err(format!("Could not lock the host service: {error}"))
        }
    }
    // An older compatible implementation may expose a verified record without
    // this lock. Respect it too instead of stealing its record/phone endpoint.
    if let Some(record) = read_record(&dir).filter(healthy) {
        compatible(record)?;
        return Ok(());
    }
    let mut random = [0u8; 16];
    getrandom::getrandom(&mut random).map_err(|error| error.to_string())?;
    let identity = HostServiceIdentity {
        protocol: PROTOCOL_VERSION,
        instance_id: random.iter().map(|byte| format!("{byte:02x}")).collect(),
    };
    let phone = Arc::new(crate::PhoneControl::new(options.version.clone()));
    let config = |port| crate::ServerConfig {
        host: "127.0.0.1".into(),
        port,
        dist: options.dist.clone(),
        bundled_skills: options.bundled_skills.clone(),
        examples_base: options.examples_base.clone(),
        startup_maintenance: options.startup_maintenance,
        phone: Some(phone.clone()),
        host_service_identity: Some(identity.clone()),
        remote: None,
        token: None,
        ..Default::default()
    };
    // Preserve an earlier fallback port across restarts, keeping phone and MCP
    // gateway URLs stable whenever that port is still available.
    let preferred_port = read_record(&dir)
        .map(|record| record.port)
        .unwrap_or(options.preferred_port);
    let server = match crate::spawn(config(preferred_port)) {
        Ok(server) => server,
        Err(_) if preferred_port != 0 => {
            crate::spawn(config(0)).map_err(|error| error.to_string())?
        }
        Err(error) => return Err(format!("Could not bind the host service: {error}")),
    };
    let record = HostServiceRecord {
        protocol: identity.protocol,
        instance_id: identity.instance_id,
        pid: std::process::id(),
        port: server.addr.port(),
        version: options.version,
        started_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    };
    write_record(&dir, &record)?;
    phone.set_port(record.port);
    // Tailscale Serve is machine-wide, even when this worker's config directory
    // is isolated. Respect the same maintenance opt-out as the standalone host.
    if options.startup_maintenance {
        phone.resync_on_start();
    }
    log::info!(
        "host service ready on 127.0.0.1:{} (protocol {})",
        record.port,
        record.protocol
    );
    server.join();
    drop(lifetime);
    Ok(())
}

/// Explicitly stop this verified service instance, leaving every tmux agent
/// running. Disconnecting a client never calls this operation.
pub fn stop(record: &HostServiceRecord) -> Result<(), String> {
    if !healthy(record) {
        return Err(
            "The recorded host service is no longer answering with the expected identity.".into(),
        );
    }
    let dir = skill_core::paths::config_dir()?;
    if !read_record(&dir)
        .is_some_and(|local| local.identity() == record.identity() && local.pid == record.pid)
    {
        return Err("The record does not belong to this local host-service configuration.".into());
    }
    let lifetime = private_file(&dir.join("host-service.lock"))?;
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(3))
        .build()
        .post(&format!("{}/api/host-service/stop", record.base_url()))
        .set("Content-Type", "application/json")
        .send_string(&serde_json::json!({ "instanceId": record.instance_id }).to_string())
        .map_err(|error| {
            let detail = match error {
                ureq::Error::Status(status, response) => {
                    let mut body = String::new();
                    let _ = response.into_reader().take(16 * 1024).read_to_string(&mut body);
                    serde_json::from_str::<serde_json::Value>(&body).ok()
                        .and_then(|value| value["error"].as_str().map(str::to_owned))
                        .unwrap_or_else(|| format!("HTTP {status}"))
                }
                other => other.to_string(),
            };
            format!("Could not stop the host service: {detail}")
        })?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let alive = healthy(record);
        let unlocked = match lifetime.try_lock() {
            Ok(()) => {
                lifetime.unlock().map_err(|error| error.to_string())?;
                true
            }
            Err(TryLockError::WouldBlock) => false,
            Err(TryLockError::Error(error)) => return Err(error.to_string()),
        };
        if !alive && unlocked {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(
                "The host service accepted Stop but has not released its process lock yet.".into(),
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Persist explicit operator intent before acknowledging Stop. Automatic local
/// and SSH recovery must respect this same marker, including other accessors.
pub fn schedule_stop(identity: &HostServiceIdentity) -> Result<(), String> {
    let dir = skill_core::paths::ensure_config_dir()?;
    let startup = private_file(&dir.join("host-service-startup.lock"))?;
    lock_until(&startup, Instant::now() + Duration::from_secs(2))?;
    if !read_record(&dir)
        .is_some_and(|record| record.identity() == *identity && record.pid == std::process::id())
    {
        return Err("This process no longer owns the host-service record.".into());
    }
    // A busy offline engine must reject Stop before recording intent or queuing
    // an exit. Keep the idle lifecycle reserved through persistence so a request
    // cannot start inference between the check and the shutdown grace period.
    let shutdown = skill_core::engine::prepare_shutdown()?;
    let temporary = dir.join("host-service-stopped.json.tmp");
    let mut file = private_file(&temporary)?;
    file.set_len(0).map_err(|error| error.to_string())?;
    serde_json::to_writer(&mut file, identity).map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())?;
    drop(file);
    fs::rename(temporary, dir.join("host-service-stopped.json"))
        .map_err(|error| error.to_string())?;
    shutdown.commit();
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_millis(250));
        skill_core::engine::shutdown();
        std::process::exit(0);
    });
    Ok(())
}

/// Headless hook shared by the standalone and desktop executable. Invoke before
/// starting Tauri so a detached desktop copy never creates windows/tray state.
pub fn run_from_args(args: &[String]) -> Option<Result<(), String>> {
    let stop_requested = args.iter().any(|arg| arg == "--stop-host-service");
    if !stop_requested && !args.iter().any(|arg| arg == "--host-service") {
        return None;
    }
    let mut options = HostServiceOptions::default();
    let value = |name: &str| {
        args.windows(2)
            .find(|pair| pair[0] == name)
            .map(|pair| pair[1].clone())
    };
    if let Some(dir) = value("--host-service-config") {
        skill_core::paths::set_config_dir(dir.into());
    }
    if stop_requested {
        return Some(skill_core::paths::ensure_config_dir().and_then(|dir| {
            match read_record(&dir) {
                Some(record) => stop(&record),
                None => Ok(()),
            }
        }));
    }
    if let Some(path) = value("--dist") {
        options.dist = path.into();
    }
    options.bundled_skills = value("--bundled-skills").map(PathBuf::from);
    options.examples_base = value("--examples-base").map(PathBuf::from);
    if let Some(version) = value("--host-service-version") {
        options.version = version;
    }
    if let Some(port) = value("--port") {
        match port.parse() {
            Ok(port) => options.preferred_port = port,
            Err(_) => return Some(Err("Invalid host-service port.".into())),
        }
    }
    options.startup_maintenance = !args.iter().any(|arg| arg == "--no-startup-maintenance");
    options.recovery = args.iter().any(|arg| arg == "--recover");
    Some(run(options))
}
