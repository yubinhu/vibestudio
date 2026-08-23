//! App auto-update state + release checker.
//!
//! This module is the shared ledger between the HTTP API and whoever can
//! actually install (the desktop shell, via [`UpdateControl`]): a background
//! loop polls the release feed (`latest.json`, the tauri-updater manifest) and
//! records a strictly-newer version; `/api/update/status` reads the ledger;
//! `/api/update/apply` hands off to the control, whose download callbacks
//! report back through the `report_*` fns (same process — the shell links
//! skill-core). A standalone server has no control: status still works
//! (`canAuto: false`), and the UI falls back to the release page link.

use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

const RELEASE_URL: &str = "https://github.com/yubinhu/vibestudio/releases/latest";
const LATEST_JSON_URL: &str =
    "https://github.com/yubinhu/vibestudio/releases/latest/download/latest.json";
/// First check shortly after startup, then a slow steady cadence.
const FIRST_CHECK: Duration = Duration::from_secs(5);
const CHECK_EVERY: Duration = Duration::from_secs(4 * 60 * 60);

/// The installer half, implemented by the desktop shell (tauri-updater).
/// `begin_install` must NOT block — the shell downloads on its own task and
/// reports progress through [`report_progress`] / [`report_ready`] /
/// [`report_error`].
pub trait UpdateControl: Send + Sync {
    fn can_install(&self) -> bool;
    fn begin_install(&self);
}

#[derive(Serialize, Clone)]
pub struct AvailableUpdate {
    pub version: String,
    pub notes: Option<String>,
    pub date: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    pub current: String,
    pub available: Option<AvailableUpdate>,
    pub can_auto: bool,
    pub phase: &'static str,
    pub progress: Option<u8>,
    pub error: Option<String>,
    pub release_url: &'static str,
}

#[derive(Clone, Copy, Default)]
enum Phase {
    #[default]
    Idle,
    Downloading,
    Ready,
    Error,
}

impl Phase {
    fn as_str(self) -> &'static str {
        match self {
            Phase::Idle => "idle",
            Phase::Downloading => "downloading",
            Phase::Ready => "ready",
            Phase::Error => "error",
        }
    }
}

#[derive(Default)]
struct State {
    available: Option<AvailableUpdate>,
    phase: Phase,
    progress: Option<u8>,
    error: Option<String>,
}

static CONTROL: OnceLock<Arc<dyn UpdateControl>> = OnceLock::new();
static CURRENT: OnceLock<String> = OnceLock::new();

/// The ledger is plain data, valid even after a panic elsewhere — recover from
/// a poisoned lock instead of cascading the panic into every status request.
fn state() -> MutexGuard<'static, State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(Mutex::default).lock().unwrap_or_else(|p| p.into_inner())
}

/// QA knob: `VIBESTUDIO_UPDATE_FAKE=<version>` pretends that version is
/// available, no network check. Read at status time so it works without
/// [`init`] too (a standalone server reports it, with `canAuto: false`).
fn fake_available() -> Option<AvailableUpdate> {
    let v = std::env::var("VIBESTUDIO_UPDATE_FAKE").ok()?;
    let v = v.trim().trim_start_matches('v');
    (!v.is_empty()).then(|| AvailableUpdate { version: v.to_string(), notes: None, date: None })
}

/// Store the installer control + the running version and start the background
/// check loop. Called once by the server when an updater is configured;
/// "0.0.0" (the committed dev placeholder) disables real checking entirely.
/// No-op while [`switchboard::AUTO_UPDATE`](crate::switchboard::AUTO_UPDATE)
/// is off: no control is registered, so the whole feature stays dormant.
pub fn init(control: Arc<dyn UpdateControl>, current_version: &str) {
    if !crate::switchboard::AUTO_UPDATE {
        return;
    }
    let _ = CONTROL.set(control);
    let _ = CURRENT.set(current_version.to_string());
    if std::env::var("VIBESTUDIO_UPDATE_FAKE").is_ok() || current_version == "0.0.0" {
        return;
    }
    std::thread::spawn(|| {
        std::thread::sleep(FIRST_CHECK);
        loop {
            if let Err(e) = check_once() {
                log::warn!("update check failed: {e}");
            }
            std::thread::sleep(CHECK_EVERY);
        }
    });
}

pub fn status() -> UpdateStatus {
    let current =
        CURRENT.get().cloned().unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
    let s = state();
    UpdateStatus {
        current,
        available: fake_available().or_else(|| s.available.clone()),
        can_auto: CONTROL.get().map(|c| c.can_install()).unwrap_or(false),
        phase: s.phase.as_str(),
        progress: s.progress,
        error: s.error.clone(),
        release_url: RELEASE_URL,
    }
}

/// Kick off the install: validate, mark `downloading`, hand off to the shell.
/// Idempotent while an install is in flight: a second client's click (or an
/// HTTP retry) must not spawn a concurrent install over the same bundle.
pub fn apply() -> Result<(), String> {
    if fake_available().is_some() {
        // The shell would check the REAL feed and install whatever it finds —
        // on a 0.0.0 dev build that's any published release, over the dev tree.
        return Err("VIBESTUDIO_UPDATE_FAKE is set — install is disabled for fake updates.".to_string());
    }
    let control = CONTROL.get().filter(|c| c.can_install()).ok_or_else(|| {
        "Automatic install isn't available for this build — download the update from GitHub."
            .to_string()
    })?;
    {
        let mut s = state();
        if matches!(s.phase, Phase::Downloading | Phase::Ready) {
            return Ok(()); // already in flight
        }
        if s.available.is_none() {
            return Err("No update is available.".to_string());
        }
        s.phase = Phase::Downloading;
        s.progress = None;
        s.error = None;
    }
    control.begin_install();
    Ok(())
}

// ── reported by the shell's download callbacks ──

pub fn report_progress(pct: Option<u8>) {
    state().progress = pct;
}

pub fn report_ready() {
    let mut s = state();
    s.phase = Phase::Ready;
    s.error = None;
}

pub fn report_error(msg: String) {
    let mut s = state();
    s.phase = Phase::Error;
    s.progress = None;
    s.error = Some(msg);
}

// ───────────────────────────── release checker ─────────────────────────────

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(30))
        .build()
}

/// Fetch `latest.json` and record the version if it's strictly newer than the
/// running one. Missing `notes`/`pub_date` are fine — only `version` is required.
fn check_once() -> Result<(), String> {
    // QA knob: `VIBESTUDIO_UPDATE_URL` overrides the release-feed URL.
    let url = std::env::var("VIBESTUDIO_UPDATE_URL").unwrap_or_else(|_| LATEST_JSON_URL.into());
    let resp = match agent().get(&url).set("User-Agent", "vibestudio").call() {
        Ok(r) => r,
        Err(ureq::Error::Status(code, _)) => return Err(format!("release feed returned {code}")),
        Err(e) => return Err(format!("release feed is unreachable: {e}")),
    };
    let text = resp.into_string().map_err(|e| format!("couldn't read the release feed: {e}"))?;
    let v: Value =
        serde_json::from_str(&text).map_err(|e| format!("release feed isn't valid JSON: {e}"))?;
    let version = v
        .get("version")
        .and_then(|x| x.as_str())
        .ok_or_else(|| "release feed has no version field".to_string())?;
    let current = CURRENT.get().map(String::as_str).unwrap_or(env!("CARGO_PKG_VERSION"));
    let new = is_newer(version, current).then(|| AvailableUpdate {
        version: version.trim_start_matches('v').to_string(),
        notes: v.get("notes").and_then(|x| x.as_str()).map(str::to_string),
        date: v.get("pub_date").and_then(|x| x.as_str()).map(str::to_string),
    });
    // Assign unconditionally: a regressed feed (yanked release) must clear a
    // stale offer. A CHANGED version also resets a stale error/ready left by a
    // previous attempt — but never an in-flight download, whose report
    // callbacks own the phase.
    let mut s = state();
    let changed = s.available.as_ref().map(|a| a.version.as_str())
        != new.as_ref().map(|a| a.version.as_str());
    s.available = new;
    if changed && !matches!(s.phase, Phase::Downloading) {
        s.phase = Phase::Idle;
        s.progress = None;
        s.error = None;
    }
    Ok(())
}

/// Strictly-newer comparison of `x.y.z` numeric triples (leading `v` tolerated).
/// Any parse failure on either side counts as not-newer — a malformed feed must
/// never produce an update prompt.
fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_triple(candidate), parse_triple(current)) {
        (Some(c), Some(cur)) => c > cur,
        _ => false,
    }
}

fn parse_triple(s: &str) -> Option<(u64, u64, u64)> {
    let mut parts = s.trim().trim_start_matches('v').split('.');
    let triple =
        (parts.next()?.parse().ok()?, parts.next()?.parse().ok()?, parts.next()?.parse().ok()?);
    parts.next().is_none().then_some(triple)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_versions_are_detected() {
        assert!(is_newer("0.1.18", "0.1.17"));
        assert!(is_newer("0.2.0", "0.1.99"));
        assert!(is_newer("1.0.0", "0.99.99"));
        assert!(is_newer("v0.1.18", "0.1.17"), "leading v on the feed side");
        assert!(is_newer("0.1.18", "v0.1.17"), "leading v on the current side");
    }

    #[test]
    fn equal_and_older_are_not_newer() {
        assert!(!is_newer("0.1.17", "0.1.17"));
        assert!(!is_newer("0.1.16", "0.1.17"));
        assert!(!is_newer("0.0.9", "0.1.0"));
    }

    #[test]
    fn garbage_is_never_newer() {
        assert!(!is_newer("garbage", "0.1.17"));
        assert!(!is_newer("0.1.18", "garbage"));
        assert!(!is_newer("0.1", "0.1.17"), "two components");
        assert!(!is_newer("0.1.18.1", "0.1.17"), "four components");
        assert!(!is_newer("0.1.18-beta", "0.1.17"), "prerelease suffix stays conservative");
        assert!(!is_newer("", ""));
    }
}
