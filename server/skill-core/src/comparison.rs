//! Transport-agnostic lifecycle for pinned-baseline/live-working UI comparisons.
//! Each worker owns its worktree and child process groups, never the working
//! directory or servers supplied by URL. Terminal sessions remain queryable.
use crate::process::hidden_command;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::net::TcpListener;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComparisonConfig {
    pub repository: String,
    /// Identifies the agent conversation that produced this review artifact.
    #[serde(default)]
    pub artifact: Option<ComparisonArtifact>,
    #[serde(default)]
    pub working_directory: Option<String>,
    #[serde(default)]
    pub baseline_ref: Option<String>,
    #[serde(default)]
    pub baseline_worktree: Option<String>,
    #[serde(default)]
    pub baseline: PreviewConfig,
    #[serde(default)]
    pub working: PreviewConfig,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default = "default_route")]
    pub route: String,
    #[serde(default)]
    pub viewport: Viewport,
    #[serde(default = "yes")]
    pub sync_scroll: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComparisonArtifact {
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    pub owner: ComparisonOwner,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComparisonOwner {
    /// "local", or the exact selected SSH workspace identifier. Paths and
    /// processes still belong to the desktop machine, not this owner's host.
    pub host_id: String,
    #[serde(default)]
    pub terminal_id: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub conversation_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreviewConfig {
    /// Shell command, with {port} substitution and PORT/HOST environment.
    #[serde(default)]
    pub command: Option<String>,
    /// Borrow an existing server, without starting or terminating it.
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Relative directory inside this pane's root.
    #[serde(default)]
    pub directory: Option<String>,
    #[serde(default = "default_timeout")]
    pub ready_timeout_seconds: u64,
}
impl Default for PreviewConfig {
    fn default() -> Self {
        Self {
            command: None,
            url: None,
            port: None,
            env: BTreeMap::new(),
            directory: None,
            ready_timeout_seconds: default_timeout(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Viewport {
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub preset: Option<String>,
    #[serde(default)]
    pub orientation: Orientation,
}
impl Default for Viewport {
    fn default() -> Self {
        Self {
            width: 390,
            height: 844,
            preset: Some("custom".into()),
            orientation: Orientation::Portrait,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Orientation {
    #[default]
    Portrait,
    Landscape,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComparisonUpdate {
    pub artifact: Option<ComparisonArtifact>,
    pub route: Option<String>,
    pub viewport: Option<Viewport>,
    pub sync_scroll: Option<bool>,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ComparisonState {
    Starting,
    Ready,
    Stopping,
    Stopped,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComparisonSession {
    pub id: String,
    pub state: ComparisonState,
    /// The desktop presenter acknowledges successful native window construction.
    /// Server readiness alone does not imply that a comparison is visible.
    #[serde(default)]
    pub window_open: bool,
    /// Desired native visibility, separate from server readiness and acknowledgement.
    #[serde(default)]
    pub window_requested: bool,
    /// A new open request also focuses an already visible window exactly once.
    #[serde(default)]
    pub presentation_revision: u64,
    /// Environment values are deliberately omitted from the private artifact file.
    /// A restored artifact that used them requires an explicit replacement config.
    #[serde(default)]
    pub restore_required: bool,
    pub config: ComparisonConfig,
    pub baseline_sha: Option<String>,
    pub baseline_worktree: Option<String>,
    pub baseline_url: Option<String>,
    pub working_url: Option<String>,
    pub baseline_external: bool,
    pub working_external: bool,
    pub baseline_log: Option<String>,
    pub working_log: Option<String>,
    pub error: Option<String>,
    pub created_at: u64,
    #[serde(default)]
    pub updated_at: u64,
}

#[derive(Clone, Default)]
pub struct ComparisonManager {
    inner: Arc<ManagerInner>,
}
#[derive(Default)]
struct ManagerInner {
    sessions: Mutex<BTreeMap<String, Arc<SessionControl>>>,
    store: Option<PathBuf>,
}
struct SessionControl {
    snapshot: Mutex<ComparisonSession>,
    cancelled: AtomicBool,
    failure: Mutex<Option<String>>,
    roots: Mutex<Option<(String, String)>>,
    store: Option<PathBuf>,
}
impl Drop for ManagerInner {
    fn drop(&mut self) {
        for control in self.sessions.get_mut().unwrap().values() {
            control.cancelled.store(true, Ordering::Release);
        }
    }
}
impl ComparisonManager {
    /// Restore review metadata only. No process, checkout or native window is
    /// revived until an explicit open request. `default()` is memory-only.
    pub fn with_store(path: PathBuf) -> Result<Self, String> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(|e| format!("Create artifact directory: {e}"))?;
        }
        let records: BTreeMap<String, ComparisonSession> = crate::state_store::read(&path)?;
        let mut sessions = BTreeMap::new();
        for (id, mut snapshot) in records {
            if snapshot.id != id || id.len() != 24 || !id.bytes().all(|c| c.is_ascii_hexdigit()) {
                return Err("Comparison artifact has invalid or inconsistent identity".into());
            }
            validate_config(&snapshot.config)?;
            snapshot.state = ComparisonState::Stopped;
            snapshot.window_open = false;
            snapshot.window_requested = false;
            snapshot.baseline_url = None;
            snapshot.working_url = None;
            snapshot.baseline_log = None;
            snapshot.working_log = None;
            snapshot.error = None;
            sessions.insert(
                id,
                Arc::new(SessionControl::new(snapshot, Some(path.clone()))),
            );
        }
        Ok(Self {
            inner: Arc::new(ManagerInner {
                sessions: Mutex::new(sessions),
                store: Some(path),
            }),
        })
    }

    /// Start asynchronously. Query the returned id until ready or failed.
    pub fn start(&self, config: ComparisonConfig) -> Result<ComparisonSession, String> {
        validate_config(&config)?;
        let mut random = [0_u8; 12];
        getrandom::getrandom(&mut random).map_err(|e| format!("Create session id: {e}"))?;
        let id: String = random.iter().map(|b| format!("{b:02x}")).collect();
        let now = timestamp();
        let snapshot = ComparisonSession {
            id: id.clone(),
            state: ComparisonState::Starting,
            window_open: false,
            window_requested: true,
            presentation_revision: 1,
            restore_required: false,
            baseline_external: config.baseline.url.is_some(),
            working_external: config.working.url.is_some(),
            config,
            baseline_sha: None,
            baseline_worktree: None,
            baseline_url: None,
            working_url: None,
            baseline_log: None,
            working_log: None,
            error: None,
            created_at: now,
            updated_at: now,
        };
        let control = Arc::new(SessionControl::new(
            snapshot.clone(),
            self.inner.store.clone(),
        ));
        control.persist(&snapshot)?;
        self.inner
            .sessions
            .lock()
            .unwrap()
            .insert(id, control.clone());
        spawn_session(control)?;
        Ok(snapshot)
    }
    pub fn list(&self) -> Vec<ComparisonSession> {
        let controls: Vec<_> = self
            .inner
            .sessions
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect();
        let mut result: Vec<_> = controls
            .iter()
            .map(|c| c.snapshot.lock().unwrap().clone())
            .collect();
        result.sort_by_key(|s| s.created_at);
        result
    }
    pub fn get(&self, id: &str) -> Option<ComparisonSession> {
        self.control(id)
            .ok()
            .map(|c| c.snapshot.lock().unwrap().clone())
    }
    /// Cheap runtime gate for scroll relays, including already queued updates.
    /// Read current intent without cloning preview commands or environment values.
    pub fn accepts_scroll(&self, id: &str) -> bool {
        self.control(id).is_ok_and(|control| {
            let snapshot = control.snapshot.lock().unwrap();
            snapshot.state == ComparisonState::Ready
                && snapshot.window_requested
                && snapshot.config.sync_scroll
                && !control.cancelled.load(Ordering::Acquire)
        })
    }
    pub fn update(&self, id: &str, update: ComparisonUpdate) -> Result<ComparisonSession, String> {
        if let Some(artifact) = &update.artifact {
            validate_artifact(artifact)?;
        }
        if let Some(viewport) = &update.viewport {
            validate_viewport(viewport)?;
        }
        if let Some(route) = &update.route {
            validate_route(route)?;
        }
        let control = self.control(id)?;
        // Match worker lock ordering: roots, then snapshot.
        let roots = control.roots.lock().unwrap();
        let mut snapshot = control.snapshot.lock().unwrap();
        if snapshot.state == ComparisonState::Stopping {
            return Err("A stopping comparison cannot be updated".into());
        }
        let mut changed = snapshot.clone();
        if let Some(artifact) = update.artifact {
            changed.config.artifact = Some(artifact);
        }
        if let Some(viewport) = update.viewport {
            changed.config.viewport = viewport;
        }
        if let Some(sync_scroll) = update.sync_scroll {
            changed.config.sync_scroll = sync_scroll;
        }
        if let Some(route) = update.route {
            changed.config.route = route;
            if let Some((baseline, working)) = roots.as_ref() {
                changed.baseline_url = Some(preview_url(baseline, &changed.config.route)?);
                changed.working_url = Some(preview_url(working, &changed.config.route)?);
            }
        }
        changed.updated_at = timestamp();
        control.persist(&changed)?;
        *snapshot = changed.clone();
        Ok(changed)
    }
    /// Show/focus an active review, or restart a stopped artifact with its pinned
    /// commit and stable identity. A restored environment is never guessed.
    pub fn open(
        &self,
        id: &str,
        restore_config: Option<ComparisonConfig>,
    ) -> Result<ComparisonSession, String> {
        let control = self.control(id)?;
        let mut failure = control.failure.lock().unwrap();
        let mut roots = control.roots.lock().unwrap();
        let mut snapshot = control.snapshot.lock().unwrap();
        if snapshot.state == ComparisonState::Stopping {
            return Err("Wait for comparison cleanup before reopening".into());
        }
        let restart = matches!(
            snapshot.state,
            ComparisonState::Stopped | ComparisonState::Failed
        );
        let mut changed = snapshot.clone();
        if restart {
            if let Some(mut config) = restore_config {
                validate_config(&config)?;
                if config.repository != snapshot.config.repository
                    || config.working_directory != snapshot.config.working_directory
                    || config.baseline.url != snapshot.config.baseline.url
                {
                    return Err(
                        "Reopening an artifact must keep its repository, working directory and baseline URL"
                            .into(),
                    );
                }
                // Ownership changes are explicit updates, never an accidental
                // consequence of supplying replacement runtime credentials.
                config.artifact = snapshot.config.artifact.clone();
                changed.config = config;
                changed.restore_required = false;
            } else if snapshot.restore_required {
                return Err("This artifact used environment values that were not saved; provide restoreConfig to reopen it".into());
            }
            changed.state = ComparisonState::Starting;
            changed.window_open = false;
            changed.baseline_external = changed.config.baseline.url.is_some();
            changed.working_external = changed.config.working.url.is_some();
            changed.baseline_url = None;
            changed.working_url = None;
            changed.baseline_log = None;
            changed.working_log = None;
            changed.error = None;
        } else if restore_config.is_some() {
            return Err("Stop the comparison before replacing its runtime configuration".into());
        }
        changed.window_requested = true;
        changed.presentation_revision = changed.presentation_revision.saturating_add(1);
        changed.updated_at = timestamp();
        control.persist(&changed)?;
        *snapshot = changed.clone();
        if restart {
            *failure = None;
            *roots = None;
            control.cancelled.store(false, Ordering::Release);
        }
        drop(snapshot);
        drop(roots);
        drop(failure);
        if restart {
            spawn_session(control)?;
        }
        Ok(changed)
    }
    /// Close the native window while keeping this review's servers and baseline.
    pub fn close(&self, id: &str) -> Result<ComparisonSession, String> {
        let control = self.control(id)?;
        let mut snapshot = control.snapshot.lock().unwrap();
        let mut changed = snapshot.clone();
        changed.window_requested = false;
        changed.window_open = false;
        changed.updated_at = timestamp();
        // A full/unavailable metadata disk must not trap an OS window open.
        *snapshot = changed.clone();
        control.persist(&changed)?;
        Ok(changed)
    }
    /// Acknowledge presentation only after both native preview views exist.
    pub fn mark_window_open(&self, id: &str) -> Result<(), String> {
        let control = self.control(id)?;
        let mut snapshot = control.snapshot.lock().unwrap();
        if snapshot.state != ComparisonState::Ready
            || !snapshot.window_requested
            || control.cancelled.load(Ordering::Acquire)
        {
            return Err(
                "Only a ready comparison requesting a window can acknowledge presentation".into(),
            );
        }
        snapshot.window_open = true;
        Ok(())
    }
    pub fn stop(&self, id: &str) -> Result<ComparisonSession, String> {
        self.cancel(id, None)
    }
    /// The native presenter reports presentation failures through this path.
    pub fn fail(&self, id: &str, error: String) -> Result<ComparisonSession, String> {
        self.cancel(id, Some(error))
    }
    pub fn stop_all(&self) {
        for session in self.list() {
            let _ = self.stop(&session.id);
        }
    }
    /// Bounded shutdown for a process exit; returns false if cleanup is ongoing.
    pub fn shutdown(&self, timeout: Duration) -> bool {
        self.stop_all();
        let deadline = Instant::now() + timeout;
        while self
            .list()
            .iter()
            .any(|s| !matches!(s.state, ComparisonState::Stopped | ComparisonState::Failed))
        {
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        true
    }
    fn control(&self, id: &str) -> Result<Arc<SessionControl>, String> {
        self.inner
            .sessions
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| "Comparison session not found".into())
    }
    fn cancel(&self, id: &str, error: Option<String>) -> Result<ComparisonSession, String> {
        let control = self.control(id)?;
        let mut failure = control.failure.lock().unwrap();
        let mut snapshot = control.snapshot.lock().unwrap();
        if matches!(
            snapshot.state,
            ComparisonState::Stopped | ComparisonState::Failed
        ) {
            return Ok(snapshot.clone());
        }
        if let Some(error) = error {
            *failure = Some(error.clone());
            snapshot.error = Some(error);
        }
        snapshot.state = ComparisonState::Stopping;
        snapshot.window_requested = false;
        snapshot.window_open = false;
        snapshot.updated_at = timestamp();
        // Cleanup must still run if the artifact disk is unavailable.
        control.cancelled.store(true, Ordering::Release);
        control.persist(&snapshot)?;
        Ok(snapshot.clone())
    }
}
impl SessionControl {
    fn new(snapshot: ComparisonSession, store: Option<PathBuf>) -> Self {
        Self {
            snapshot: Mutex::new(snapshot),
            cancelled: AtomicBool::new(false),
            failure: Mutex::new(None),
            roots: Mutex::new(None),
            store,
        }
    }
    /// Call while holding the snapshot lock so concurrent metadata writes cannot
    /// persist an older snapshot after a newer one.
    fn persist(&self, snapshot: &ComparisonSession) -> Result<(), String> {
        let Some(path) = &self.store else {
            return Ok(());
        };
        let mut saved = snapshot.clone();
        saved.restore_required |= !saved.config.env.is_empty()
            || !saved.config.baseline.env.is_empty()
            || !saved.config.working.env.is_empty();
        saved.config.env.clear();
        saved.config.baseline.env.clear();
        saved.config.working.env.clear();
        // Dev-server output/errors may contain environment values or tokens.
        saved.baseline_log = None;
        saved.working_log = None;
        saved.error = None;
        crate::state_store::update::<BTreeMap<String, ComparisonSession>>(path, |records| {
            records.insert(saved.id.clone(), saved);
            Ok(())
        })?;
        Ok(())
    }
}
fn spawn_session(control: Arc<SessionControl>) -> Result<(), String> {
    let id = control.snapshot.lock().unwrap().id.clone();
    let worker_control = control.clone();
    if let Err(error) = std::thread::Builder::new()
        .name(format!("comparison-{id}"))
        .spawn(move || run_session(worker_control))
    {
        let message = format!("Start comparison worker: {error}");
        let mut snapshot = control.snapshot.lock().unwrap();
        snapshot.state = ComparisonState::Failed;
        snapshot.window_requested = false;
        snapshot.error = Some(message.clone());
        let _ = control.persist(&snapshot);
        return Err(message);
    }
    Ok(())
}
fn timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Ephemeral, per-pane scroll capabilities. This registry is deliberately separate
/// from serialized comparison artifacts and exposes no general native commands.
#[derive(Clone, Default)]
pub struct ComparisonScrollRelay {
    registrations: Arc<Mutex<BTreeMap<String, ScrollRegistration>>>,
}
type ScrollCallback = dyn Fn(&str) -> Result<(), String> + Send + Sync;
#[derive(Clone)]
struct ScrollRegistration {
    origin: String,
    callback: Arc<ScrollCallback>,
}
/// The native viewer owns this guard. Closing/dropping it revokes the capability.
pub struct ComparisonScrollRegistration {
    token: String,
    relay: ComparisonScrollRelay,
}
pub const MAX_COMPARISON_SCROLL_BYTES: usize = 16_384;
impl ComparisonScrollRelay {
    pub fn register(
        &self,
        origin: String,
        callback: Arc<ScrollCallback>,
    ) -> Result<ComparisonScrollRegistration, String> {
        let parsed = validate_url(&origin)?;
        if parsed.origin().ascii_serialization() != origin {
            return Err("Scroll relay requires an exact HTTP(S) origin".into());
        }
        let mut random = [0_u8; 32];
        getrandom::getrandom(&mut random).map_err(|e| format!("Create scroll capability: {e}"))?;
        let token: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        self.registrations
            .lock()
            .unwrap()
            .insert(token.clone(), ScrollRegistration { origin, callback });
        Ok(ComparisonScrollRegistration {
            token,
            relay: self.clone(),
        })
    }
    /// Authenticate before reading a request body, including CORS preflights.
    pub fn authorized(&self, token: &str, origin: &str) -> bool {
        self.registration(token, origin).is_some()
    }
    pub fn dispatch(&self, token: &str, origin: &str, payload: &str) -> Result<(), String> {
        let registration = self
            .registration(token, origin)
            .ok_or("Unknown scroll capability or origin")?;
        let updates = validated_comparison_scroll(payload).ok_or("Invalid scroll positions")?;
        (registration.callback)(&updates.to_string())
    }
    fn registration(&self, token: &str, origin: &str) -> Option<ScrollRegistration> {
        if token.len() != 64 || !token.bytes().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        self.registrations
            .lock()
            .unwrap()
            .get(token)
            .filter(|entry| entry.origin == origin)
            .cloned()
    }
}
impl ComparisonScrollRegistration {
    pub fn token(&self) -> &str {
        &self.token
    }
}
impl Drop for ComparisonScrollRegistration {
    fn drop(&mut self) {
        self.relay.registrations.lock().unwrap().remove(&self.token);
    }
}
/// Parse and rebuild only bounded scroll-position data. Unknown fields, scripts
/// and arbitrary native commands never reach the native callback.
fn validated_comparison_scroll(payload: &str) -> Option<serde_json::Value> {
    use serde_json::json;
    if payload.len() > MAX_COMPARISON_SCROLL_BYTES {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(payload).ok()?;
    let updates = value.as_array()?;
    if updates.is_empty() || updates.len() > 8 {
        return None;
    }
    let mut result = Vec::with_capacity(updates.len());
    for update in updates {
        let root = update.get("root")?.as_bool()?;
        let key = update.get("key")?.as_str()?;
        let id = update.get("id")?.as_str()?;
        if key.len() > 1024 || id.len() > 1024 {
            return None;
        }
        let path = update.get("path")?.as_array()?;
        if path.len() > 24
            || path
                .iter()
                .any(|part| part.as_u64().is_none_or(|part| part > 65_535))
        {
            return None;
        }
        let index = update.get("index")?.as_i64()?;
        if !(-1..=65_535).contains(&index) {
            return None;
        }
        let x = update.get("x")?.as_f64()?;
        let y = update.get("y")?.as_f64()?;
        if !(0.0..=1.0).contains(&x) || !(0.0..=1.0).contains(&y) {
            return None;
        }
        result.push(json!({"root":root,"key":key,"id":id,"path":path,"index":index,"x":x,"y":y}));
    }
    Some(serde_json::Value::Array(result))
}

fn yes() -> bool {
    true
}
fn default_route() -> String {
    "/".into()
}
fn default_timeout() -> u64 {
    120
}
fn validate_viewport(viewport: &Viewport) -> Result<(), String> {
    if !(240..=3840).contains(&viewport.width) || !(240..=3840).contains(&viewport.height) {
        return Err(
            "Viewport width and height must each be between 240 and 3840 CSS pixels".into(),
        );
    }
    Ok(())
}
fn validate_route(route: &str) -> Result<(), String> {
    if !route.starts_with('/')
        || route.starts_with("//")
        || route.contains('\\')
        || route.chars().any(char::is_control)
    {
        return Err("Comparison route must be a same-origin path beginning with /".into());
    }
    Ok(())
}
fn validate_url(raw: &str) -> Result<url::Url, String> {
    let url = url::Url::parse(raw).map_err(|e| format!("Invalid preview URL: {e}"))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("Preview URLs must be HTTP(S) URLs without embedded credentials".into());
    }
    Ok(url)
}
fn validate_artifact(artifact: &ComparisonArtifact) -> Result<(), String> {
    fn identifier(value: &str, name: &str, limit: usize) -> Result<(), String> {
        if value.trim().is_empty()
            || value.chars().count() > limit
            || value.chars().any(char::is_control)
        {
            return Err(format!("Artifact {name} must be nonempty, at most {limit} characters, and contain no control characters"));
        }
        Ok(())
    }
    identifier(&artifact.title, "title", 200)?;
    identifier(&artifact.owner.host_id, "hostId", 1024)?;
    for (value, name) in [
        (&artifact.owner.terminal_id, "terminalId"),
        (&artifact.owner.provider, "provider"),
        (&artifact.owner.conversation_id, "conversationId"),
    ] {
        if let Some(value) = value {
            identifier(value, name, 1024)?;
        }
    }
    if artifact.owner.conversation_id.is_some() && artifact.owner.provider.is_none() {
        return Err("Artifact conversationId requires provider".into());
    }
    if artifact.owner.terminal_id.is_none() && artifact.owner.conversation_id.is_none() {
        return Err("Artifact owner requires terminalId or provider and conversationId".into());
    }
    if let Some(description) = &artifact.description {
        if description.chars().count() > 4000 || description.contains('\0') {
            return Err("Artifact description must be at most 4000 characters without NUL".into());
        }
    }
    Ok(())
}
fn validate_config(config: &ComparisonConfig) -> Result<(), String> {
    if let Some(artifact) = &config.artifact {
        validate_artifact(artifact)?;
    }
    if config.repository.trim().is_empty() {
        return Err("A repository path is required".into());
    }
    validate_viewport(&config.viewport)?;
    validate_route(&config.route)?;
    for preview in [&config.baseline, &config.working] {
        if preview.command.is_some() && preview.url.is_some() {
            return Err("A preview accepts either a command or an existing URL".into());
        }
        if let Some(url) = &preview.url {
            validate_url(url)?;
        }
        if preview.port == Some(0) {
            return Err("Preview ports must be nonzero".into());
        }
        if !(1..=1800).contains(&preview.ready_timeout_seconds) {
            return Err("Preview readiness timeout must be between 1 and 1800 seconds".into());
        }
        if let Some(directory) = &preview.directory {
            validate_relative(directory)?;
        }
        if preview
            .command
            .as_ref()
            .is_some_and(|command| command.trim().is_empty())
        {
            return Err("Preview commands cannot be empty".into());
        }
    }
    if config.baseline.url.is_none()
        && config.working.url.is_none()
        && config.baseline.port.is_some()
        && config.baseline.port == config.working.port
    {
        return Err("Baseline and working dev servers require different ports".into());
    }
    for (key, value) in config
        .env
        .iter()
        .chain(&config.baseline.env)
        .chain(&config.working.env)
    {
        if key.is_empty() || key.contains(['=', '\0']) || value.contains('\0') {
            return Err("Invalid comparison environment variable".into());
        }
    }
    Ok(())
}
fn validate_relative(path: &str) -> Result<(), String> {
    if Path::new(path).components().any(|part| {
        matches!(
            part,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(
            "Preview directory must be relative and cannot traverse outside its pane root".into(),
        );
    }
    Ok(())
}
fn preview_url(root: &str, route: &str) -> Result<String, String> {
    let url = validate_url(root)?;
    // Preserve an explicit URL's useful path/hash under the default route.
    if route == "/" {
        return Ok(url.to_string());
    }
    url.join(route)
        .map(|u| u.to_string())
        .map_err(|e| format!("Invalid preview route: {e}"))
}
fn run_session(control: Arc<SessionControl>) {
    let config = control.snapshot.lock().unwrap().config.clone();
    let id = control.snapshot.lock().unwrap().id.clone();
    let mut owned = OwnedResources::default();
    let result = prepare(&control, &config, &id, &mut owned).and_then(|()| loop {
        if control.cancelled.load(Ordering::Acquire) {
            return Ok(());
        }
        for process in &mut owned.processes {
            if process.has_exited()? {
                return Err(format!(
                    "{} dev server exited; check its command and dependencies",
                    process.side
                ));
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    });
    let result = result.map_err(|error| owned.with_log_tail(error));
    let cleanup_error = owned.cleanup().err();
    let failure = control.failure.lock().unwrap().clone();
    let cancelled = control.cancelled.load(Ordering::Acquire);
    let error = failure.or_else(|| if cancelled { None } else { result.err() });
    let mut snapshot = control.snapshot.lock().unwrap();
    snapshot.error = match (error, cleanup_error) {
        (Some(error), Some(cleanup)) => Some(format!("{error}; cleanup: {cleanup}")),
        (Some(error), None) | (None, Some(error)) => Some(error),
        (None, None) => None,
    };
    snapshot.baseline_log = None;
    snapshot.working_log = None;
    snapshot.window_open = false;
    snapshot.window_requested = false;
    snapshot.updated_at = timestamp();
    snapshot.state = if snapshot.error.is_some() {
        ComparisonState::Failed
    } else {
        ComparisonState::Stopped
    };
    if let Err(error) = control.persist(&snapshot) {
        snapshot.error = Some(format!("Could not save comparison artifact: {error}"));
    }
}
fn check_cancelled(control: &SessionControl) -> Result<(), String> {
    if control.cancelled.load(Ordering::Acquire) {
        Err("Comparison stopped".into())
    } else {
        Ok(())
    }
}
fn prepare(
    control: &SessionControl,
    config: &ComparisonConfig,
    id: &str,
    owned: &mut OwnedResources,
) -> Result<(), String> {
    check_cancelled(control)?;
    let source =
        fs::canonicalize(&config.repository).map_err(|e| format!("Open repository: {e}"))?;
    let repo = PathBuf::from(git(&source, &["rev-parse", "--show-toplevel"])?);
    let working = fs::canonicalize(
        config
            .working_directory
            .as_ref()
            .map(Path::new)
            .unwrap_or(&source),
    )
    .map_err(|e| format!("Open working directory: {e}"))?;
    if !working.is_dir() {
        return Err("Working directory must be a directory".into());
    }
    // An artifact can be reopened after a crash. Use a fresh runtime directory
    // rather than adopting/removing a previous process's unverified resources.
    let mut run_nonce = [0_u8; 8];
    getrandom::getrandom(&mut run_nonce).map_err(|e| format!("Create comparison run id: {e}"))?;
    let temp = std::env::temp_dir().join(format!(
        "vibestudio-comparison-{id}-{:016x}",
        u64::from_le_bytes(run_nonce)
    ));
    fs::create_dir(&temp).map_err(|e| format!("Create comparison directory: {e}"))?;
    owned.temp = Some(temp.clone());
    let baseline = if config.baseline.url.is_none() {
        let pinned_sha = control.snapshot.lock().unwrap().baseline_sha.clone();
        let reference = match pinned_sha.or_else(|| config.baseline_ref.clone()) {
            Some(reference) => reference,
            None => default_baseline_ref(&repo)?,
        };
        let sha = git(
            &repo,
            &[
                "rev-parse",
                "--verify",
                "--end-of-options",
                &format!("{reference}^{{commit}}"),
            ],
        )?;
        check_cancelled(control)?;
        let path = config
            .baseline_worktree
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| temp.join("baseline"));
        if !path.is_absolute() {
            return Err("Baseline worktree location must be an absolute path".into());
        }
        // Atomic no-clobber reservation, including empty dirs and dangling symlinks.
        fs::create_dir(&path).map_err(|e| {
            format!(
                "Reserve baseline worktree {} (must not exist): {e}",
                path.display()
            )
        })?;
        owned.worktree = Some((repo.clone(), path.clone(), false));
        let checkout = checkout_baseline(
            control,
            &repo,
            &path,
            &sha,
            &temp,
            config.baseline.ready_timeout_seconds,
        );
        // Checkout filters/hooks can fail or be cancelled after registration.
        // Capture ownership even on failure so partial worktrees are removed.
        if let Ok(marker) = fs::read(path.join(".git")) {
            owned.worktree.as_mut().unwrap().2 = true;
            owned.worktree_marker = Some(marker);
        }
        checkout?;
        if owned.worktree_marker.is_none() {
            return Err("Baseline checkout did not create a Git ownership marker".into());
        }
        {
            let mut snapshot = control.snapshot.lock().unwrap();
            snapshot.baseline_sha = Some(sha);
            snapshot.baseline_worktree = Some(path.to_string_lossy().into_owned());
            snapshot.updated_at = timestamp();
            control.persist(&snapshot)?;
        }
        Some(path)
    } else {
        None
    };
    check_cancelled(control)?;
    let package_relative =
        PathBuf::from(git(&working, &["rev-parse", "--show-prefix"]).unwrap_or_default())
            .join(config.working.directory.as_deref().unwrap_or(""));
    let package_relative = package_relative.to_string_lossy();
    let working_dir = pane_directory(&working, config.working.directory.as_deref().unwrap_or(""))?;
    let baseline_dir = baseline
        .as_ref()
        .map(|root| {
            pane_directory(
                root,
                config
                    .baseline
                    .directory
                    .as_deref()
                    .unwrap_or(&package_relative),
            )
        })
        .transpose()?;
    if config.baseline.command.is_none() {
        if let (Some(root), Some(directory)) = (&baseline, &baseline_dir) {
            // Default inference borrows existing dependencies. Explicit commands
            // have full control of setup, including independent package installs.
            let working_root = git(&working, &["rev-parse", "--show-toplevel"])
                .map(PathBuf::from)
                .unwrap_or_else(|_| working.clone());
            link_dependencies(&working_root, root)?;
            if directory != root {
                link_dependencies(&working_dir, directory)?;
            }
        }
    }
    let baseline_url = start_preview(
        "baseline",
        &config.baseline,
        &config.env,
        baseline_dir.as_deref(),
        &temp,
        owned,
    )?;
    check_cancelled(control)?;
    let working_url = start_preview(
        "working",
        &config.working,
        &config.env,
        Some(&working_dir),
        &temp,
        owned,
    )?;
    {
        let mut roots = control.roots.lock().unwrap();
        let mut snapshot = control.snapshot.lock().unwrap();
        snapshot.baseline_url = Some(preview_url(&baseline_url, &snapshot.config.route)?);
        snapshot.working_url = Some(preview_url(&working_url, &snapshot.config.route)?);
        snapshot.baseline_log = config
            .baseline
            .url
            .is_none()
            .then(|| temp.join("baseline.log").to_string_lossy().into_owned());
        snapshot.working_log = config
            .working
            .url
            .is_none()
            .then(|| temp.join("working.log").to_string_lossy().into_owned());
        *roots = Some((baseline_url.clone(), working_url.clone()));
    }
    wait_ready(
        control,
        &baseline_url,
        config.baseline.ready_timeout_seconds,
        owned,
        "baseline",
    )?;
    wait_ready(
        control,
        &working_url,
        config.working.ready_timeout_seconds,
        owned,
        "working",
    )?;
    let mut snapshot = control.snapshot.lock().unwrap();
    if !control.cancelled.load(Ordering::Acquire) {
        snapshot.state = ComparisonState::Ready;
        snapshot.updated_at = timestamp();
        control.persist(&snapshot)?;
    }
    Ok(())
}
/// Git checkout may run user-defined filters/hooks, so it needs the same process
/// ownership and cancellation guarantees as a dev server.
fn checkout_baseline(
    control: &SessionControl,
    repo: &Path,
    path: &Path,
    sha: &str,
    temp: &Path,
    timeout: u64,
) -> Result<(), String> {
    let log = fs::File::create(temp.join("checkout.log")).map_err(|e| e.to_string())?;
    let mut command = hidden_command("git");
    command
        .arg("-C")
        .arg(repo)
        .args(["worktree", "add", "--detach"])
        .arg(path)
        .arg(sha)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone().map_err(|e| e.to_string())?))
        .stderr(Stdio::from(log));
    let mut process = OwnedProcess::spawn(command, "baseline checkout")?;
    let deadline = Instant::now() + Duration::from_secs(timeout);
    loop {
        check_cancelled(control)?;
        if process.has_exited()? {
            return if process.exit_success == Some(true) {
                Ok(())
            } else {
                Err("Baseline Git checkout failed".into())
            };
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "Baseline Git checkout did not finish within {timeout}s"
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn git(directory: &Path, args: &[&str]) -> Result<String, String> {
    let output = hidden_command("git")
        .arg("-C")
        .arg(directory)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("Run git: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "git {}: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
fn default_baseline_ref(repo: &Path) -> Result<String, String> {
    if let Ok(reference) = git(repo, &["symbolic-ref", "refs/remotes/origin/HEAD"]) {
        return Ok(reference);
    }
    for reference in [
        "refs/heads/main",
        "refs/heads/master",
        "refs/remotes/origin/main",
        "refs/remotes/origin/master",
    ] {
        if git(repo, &["rev-parse", "--verify", reference]).is_ok() {
            return Ok(reference.into());
        }
    }
    Err("No mainline branch found; configure baselineRef explicitly".into())
}
fn pane_directory(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let path = fs::canonicalize(root.join(relative))
        .map_err(|e| format!("Open preview directory: {e}"))?;
    let root = fs::canonicalize(root).map_err(|e| format!("Open preview root: {e}"))?;
    if !path.starts_with(root) || !path.is_dir() {
        return Err("Preview directory must be within its pane root".into());
    }
    Ok(path)
}
fn link_dependencies(working: &Path, baseline: &Path) -> Result<(), String> {
    let source = working.join("node_modules");
    let target = baseline.join("node_modules");
    if !source.is_dir() || target.symlink_metadata().is_ok() {
        return Ok(());
    }
    // A real node_modules directory gives each Vite root its own optimization
    // and config caches. Borrow packages shallowly, never the cache directories.
    fs::create_dir(&target).map_err(|e| format!("Create baseline dependencies directory: {e}"))?;
    for entry in fs::read_dir(&source).map_err(|e| format!("Read working dependencies: {e}"))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name();
        if name.to_string_lossy().starts_with(".vite") || name == ".cache" {
            continue;
        }
        let dest = target.join(name);
        #[cfg(unix)]
        std::os::unix::fs::symlink(entry.path(), &dest)
            .map_err(|e| format!("Link baseline dependency: {e}"))?;
        #[cfg(windows)]
        {
            let result = if entry.path().is_dir() {
                std::os::windows::fs::symlink_dir(entry.path(), &dest)
            } else {
                std::os::windows::fs::symlink_file(entry.path(), &dest)
            };
            result.map_err(|e| format!("Link baseline dependency: {e}; enable Developer Mode or supply a command that installs dependencies"))?;
        }
    }
    Ok(())
}
fn infer_command(directory: &Path) -> Result<String, String> {
    let package: serde_json::Value = fs::read(directory.join("package.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .ok_or("Specify a dev command or an existing URL for this project")?;
    let scripts = package["scripts"]
        .as_object()
        .ok_or("Specify a dev command: package.json has no scripts")?;
    for name in ["dev:vite", "dev:web", "dev", "start"] {
        if scripts
            .get(name)
            .and_then(|value| value.as_str())
            .is_some_and(|script| {
                script.split_whitespace().any(|word| word == "vite") && !script.contains("tauri")
            })
        {
            return Ok(format!(
                "npm run {name} -- --host 127.0.0.1 --port {{port}} --strictPort"
            ));
        }
    }
    Err("Cannot infer a web dev server; specify command (using PORT or {port}) or url for each preview".into())
}
fn start_preview(
    side: &'static str,
    preview: &PreviewConfig,
    env: &BTreeMap<String, String>,
    directory: Option<&Path>,
    temp: &Path,
    owned: &mut OwnedResources,
) -> Result<String, String> {
    if let Some(url) = &preview.url {
        return Ok(validate_url(url)?.to_string());
    }
    let directory = directory.ok_or("Missing baseline worktree")?;
    let command_text = preview
        .command
        .clone()
        .map(Ok)
        .unwrap_or_else(|| infer_command(directory))?;
    let listener = loop {
        let listener =
            TcpListener::bind(("127.0.0.1", preview.port.unwrap_or(0))).map_err(|e| {
                format!("Reserve {side} port (existing services are never displaced): {e}")
            })?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        if !owned.ports.contains(&port) {
            break listener;
        }
        if preview.port.is_some() {
            return Err("Baseline and working dev servers require different ports".into());
        }
    };
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    owned.ports.push(port);
    let command_text = command_text.replace("{port}", &port.to_string());
    let log = fs::File::create(temp.join(format!("{side}.log")))
        .map_err(|e| format!("Create preview log: {e}"))?;
    #[cfg(windows)]
    let mut command = {
        let mut command = hidden_command("cmd");
        command.args(["/D", "/S", "/C", &command_text]);
        command
    };
    #[cfg(not(windows))]
    let mut command = {
        let mut command = hidden_command("sh");
        command.args(["-c", &command_text]);
        command
    };
    command
        .current_dir(directory)
        .envs(env)
        .envs(&preview.env)
        .env("PORT", port.to_string())
        .env("HOST", "127.0.0.1")
        .env("VIBESTUDIO_COMPARISON_SIDE", side)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone().map_err(|e| e.to_string())?))
        .stderr(Stdio::from(log));
    // Tools cannot inherit a listening socket: release immediately before spawn.
    drop(listener);
    owned.processes.push(OwnedProcess::spawn(command, side)?);
    Ok(format!("http://127.0.0.1:{port}/"))
}
fn wait_ready(
    control: &SessionControl,
    url: &str,
    timeout: u64,
    owned: &mut OwnedResources,
    side: &str,
) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(timeout);
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_millis(500))
        .redirects(0)
        .build();
    loop {
        check_cancelled(control)?;
        for process in &mut owned.processes {
            if process.has_exited()? {
                return Err(format!(
                    "{} dev server exited before readiness; check command and dependencies",
                    process.side
                ));
            }
        }
        match agent.get(url).call() {
            Ok(_) | Err(ureq::Error::Status(_, _)) => return Ok(()),
            Err(_) => {}
        }
        if Instant::now() >= deadline {
            return Err(format!("{side} preview did not become ready within {timeout}s at {url}; commands must listen on PORT"));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
#[derive(Default)]
struct OwnedResources {
    processes: Vec<OwnedProcess>,
    worktree: Option<(PathBuf, PathBuf, bool)>,
    worktree_marker: Option<Vec<u8>>,
    ports: Vec<u16>,
    temp: Option<PathBuf>,
}
impl OwnedResources {
    fn with_log_tail(&self, mut error: String) -> String {
        use std::io::{Read, Seek, SeekFrom};
        if let Some(temp) = &self.temp {
            for side in ["baseline", "working", "checkout"] {
                if let Ok(mut file) = fs::File::open(temp.join(format!("{side}.log"))) {
                    let length = file.metadata().map(|m| m.len()).unwrap_or(0);
                    let _ = file.seek(SeekFrom::Start(length.saturating_sub(2048)));
                    let mut bytes = Vec::new();
                    let _ = file.take(2048).read_to_end(&mut bytes);
                    let tail = String::from_utf8_lossy(&bytes);
                    if !tail.trim().is_empty() {
                        error.push_str(&format!("\n{side} output:\n{}", tail.trim()));
                    }
                }
            }
        }
        error
    }
    fn cleanup(&mut self) -> Result<(), String> {
        self.processes.clear();
        let mut errors = Vec::new();
        if let Some((repo, path, registered)) = self.worktree.take() {
            let ownership_matches = !path
                .symlink_metadata()
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(true)
                && self.worktree_marker.as_ref().is_some_and(|marker| {
                    fs::read(path.join(".git")).ok().as_ref() == Some(marker)
                });
            let result = if registered && !ownership_matches {
                Err(
                    "Baseline ownership marker changed; preserving the path for manual review"
                        .into(),
                )
            } else if registered {
                git(
                    &repo,
                    &[
                        "worktree",
                        "remove",
                        "--force",
                        path.to_str().unwrap_or_default(),
                    ],
                )
                .map(|_| ())
            } else {
                fs::remove_dir(&path).map_err(|e| e.to_string())
            };
            if let Err(error) = result {
                errors.push(format!("Remove owned worktree {}: {error}", path.display()));
            }
        }
        if let Some(path) = self.temp.take() {
            // Keep a failed worktree removal registered and recoverable.
            if errors.is_empty() {
                if let Err(error) = fs::remove_dir_all(&path) {
                    errors.push(format!("Remove comparison directory: {error}"));
                }
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }
}
impl Drop for OwnedResources {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}
struct OwnedProcess {
    child: Child,
    side: &'static str,
    exit_success: Option<bool>,
    #[cfg(windows)]
    job: Option<windows_job::Handle>,
}
impl OwnedProcess {
    fn spawn(mut command: Command, side: &'static str) -> Result<Self, String> {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            use windows_sys::Win32::System::Threading::{CREATE_NO_WINDOW, CREATE_SUSPENDED};
            command.creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED);
        }
        let child = command
            .spawn()
            .map_err(|e| format!("Start {side} process: {e}"))?;
        #[cfg(windows)]
        let (child, job) = {
            let mut child = child;
            match windows_job::attach_and_resume(&child) {
                Ok(job) => (child, job),
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("Own {side} process tree: {error}"));
                }
            }
        };
        Ok(Self {
            child,
            side,
            exit_success: None,
            #[cfg(windows)]
            job: Some(job),
        })
    }
    fn has_exited(&mut self) -> Result<bool, String> {
        #[cfg(unix)]
        {
            // Do not reap: the leader PID must remain reserved until group cleanup,
            // including after a shell exits while descendants are still serving.
            let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    self.child.id() as _,
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if result != 0 {
                return Err(format!(
                    "Inspect {} server: {}",
                    self.side,
                    std::io::Error::last_os_error()
                ));
            }
            let exited = unsafe { info.si_pid() } != 0;
            if exited {
                self.exit_success =
                    Some(info.si_code == libc::CLD_EXITED && unsafe { info.si_status() } == 0);
            }
            Ok(exited)
        }
        #[cfg(not(unix))]
        {
            let status = self.child.try_wait().map_err(|e| e.to_string())?;
            if let Some(status) = status {
                self.exit_success = Some(status.success());
            }
            Ok(status.is_some())
        }
    }
}
impl Drop for OwnedProcess {
    fn drop(&mut self) {
        #[cfg(unix)]
        unsafe {
            // The unreaped leader PID guarantees this group cannot be unrelated.
            libc::kill(-(self.child.id() as libc::pid_t), libc::SIGTERM);
            std::thread::sleep(Duration::from_millis(100));
            libc::kill(-(self.child.id() as libc::pid_t), libc::SIGKILL);
        }
        #[cfg(windows)]
        {
            // Closing our private kill-on-close job terminates every descendant,
            // even when the original shell has already exited.
            self.job.take();
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(windows)]
mod windows_job {
    use std::os::windows::io::AsRawHandle;
    use std::process::Child;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

    // Stored as an integer to keep the owning resource Send; handles have no
    // thread affinity. Only this type closes them, and it is intentionally !Clone.
    pub(super) struct Handle(usize);
    impl Handle {
        fn new(handle: HANDLE) -> Result<Self, String> {
            if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                Err(std::io::Error::last_os_error().to_string())
            } else {
                Ok(Self(handle as usize))
            }
        }
        fn raw(&self) -> HANDLE {
            self.0 as HANDLE
        }
    }
    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.raw());
            }
        }
    }
    pub(super) fn attach_and_resume(child: &Child) -> Result<Handle, String> {
        // The process was created suspended. Job inheritance is established before
        // resuming its initial thread, so no child can escape the owned job.
        // https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects
        unsafe {
            let job = Handle::new(CreateJobObjectW(std::ptr::null(), std::ptr::null()))?;
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                job.raw(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
            ) == 0
                || AssignProcessToJobObject(job.raw(), child.as_raw_handle()) == 0
            {
                return Err(std::io::Error::last_os_error().to_string());
            }
            let snapshot = Handle::new(CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0))?;
            let mut entry: THREADENTRY32 = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
            let mut found = Thread32First(snapshot.raw(), &mut entry) != 0;
            while found {
                if entry.th32OwnerProcessID == child.id() {
                    let thread =
                        Handle::new(OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID))?;
                    if ResumeThread(thread.raw()) == u32::MAX {
                        return Err(std::io::Error::last_os_error().to_string());
                    }
                    return Ok(job);
                }
                found = Thread32Next(snapshot.raw(), &mut entry) != 0;
            }
            Err("Cannot locate the suspended dev-server thread".into())
        }
    }
}

/// Install missing bundled agent guidance, or migrate exact unchanged known
/// shipped versions. Unknown/customized copies are preserved.
pub fn ensure_agent_skill(bundled: &Path) -> Result<Vec<PathBuf>, String> {
    let home = dirs::home_dir().ok_or("Cannot locate home directory")?;
    ensure_agent_skill_in(bundled, &home)
}
fn ensure_agent_skill_in(bundled: &Path, home: &Path) -> Result<Vec<PathBuf>, String> {
    // Explicit migrations from known shipped trees, not a general overwrite
    // policy. Git-clean does not mean uncustomized; match every byte.
    let initial = BTreeMap::from([
        (
            "SKILL.md".into(),
            "6e2e97d719dd7d398869e2b5b82b28eee36c8520094a200ee61fbbf2e60a4b13".into(),
        ),
        (
            "references/configuration.md".into(),
            "dc349a115861cbb1296b497d5c4a20141e28850c0aa1e1885c182d71932060f5".into(),
        ),
        (
            "scripts/compare.py".into(),
            "8514f95579a7188d489b118cff102fb36dbce665a3602ebd6baf2555d47120fd".into(),
        ),
    ]);
    let session_artifacts = BTreeMap::from([
        (
            "SKILL.md".into(),
            "133459c50357380f1188e89ab72c6d2b8030eef6b1c18e009c2dc8726c27b420".into(),
        ),
        (
            "references/configuration.md".into(),
            "0d1b4925a772cee9b799a3c7a93ca765c645944d19da8a5362b575bd84fb92a6".into(),
        ),
        (
            "scripts/compare.py".into(),
            "0ba3cf152c410c580ddae0c0a497f447ae98e00f9b2125319ed4c246d409f3c6".into(),
        ),
    ]);
    let mut named_devices = session_artifacts.clone();
    named_devices.insert(
        "references/configuration.md".into(),
        "3242c200f40768b5020d1edc7e5ba762e65e9e7817d66d0223612d35ec81e668".into(),
    );
    let mut cached_catalog = session_artifacts.clone();
    cached_catalog.insert(
        "references/configuration.md".into(),
        "a30bd5507c7a4cf81021515de9abae004af513828409f4fcb3b9ac829e587e82".into(),
    );
    ensure_agent_skill_with_previous(
        bundled,
        home,
        &[initial, session_artifacts, named_devices, cached_catalog],
    )
}

fn ensure_agent_skill_with_previous(
    bundled: &Path,
    home: &Path,
    previous: &[BTreeMap<String, String>],
) -> Result<Vec<PathBuf>, String> {
    if !bundled.join("SKILL.md").is_file() {
        return Err("Bundled ui-compare skill is missing SKILL.md".into());
    }
    let mut destinations = crate::agents::install_dirs(home);
    destinations.push(home.join(".agents/skills"));
    destinations.sort();
    destinations.dedup();
    let mut installed = Vec::new();
    for directory in destinations {
        let target = directory.join("ui-compare");
        if target.symlink_metadata().is_ok() {
            if previous
                .iter()
                .any(|version| unchanged_skill_tree(&target, version))
            {
                // install_skill keeps the skill's Git history. The update stays
                // uncommitted so the user can review/revert the official changes.
                crate::sync::install_skill(bundled, &target)?;
                installed.push(target);
            }
            continue;
        }
        fs::create_dir_all(&directory)
            .map_err(|e| format!("Create agent skills directory: {e}"))?;
        // Reserve the final directory before copying; another concurrent installer
        // cannot win an existence check and overwrite a newly customized copy.
        match fs::create_dir(&target) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("Reserve comparison skill: {error}")),
        }
        // install_skill would remove existing contents. We reserved an empty dir,
        // but copy directly so no overwrite primitive ever touches this location.
        for entry in walkdir::WalkDir::new(bundled)
            .min_depth(1)
            .into_iter()
            .filter_entry(|entry| entry.file_name() != "__pycache__" && entry.file_name() != ".git")
        {
            let entry = entry.map_err(|e| format!("Read bundled comparison skill: {e}"))?;
            let relative = entry
                .path()
                .strip_prefix(bundled)
                .map_err(|e| e.to_string())?;
            let dest = target.join(relative);
            if entry.file_type().is_dir() {
                fs::create_dir(&dest).map_err(|e| e.to_string())?;
            } else if entry.file_type().is_file() {
                use std::io::Write;
                let mut file = fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(dest)
                    .map_err(|e| e.to_string())?;
                file.write_all(&fs::read(entry.path()).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
            }
        }
        installed.push(target);
    }
    Ok(installed)
}

/// Fail closed on missing files, extra authored entries, unreadable paths and
/// links. Only generated Python caches and the root Git metadata are ignored.
fn unchanged_skill_tree(root: &Path, expected: &BTreeMap<String, String>) -> bool {
    use sha2::{Digest, Sha256};
    let Ok(meta) = root.symlink_metadata() else {
        return false;
    };
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return false;
    }
    let mut found = BTreeMap::new();
    let entries = walkdir::WalkDir::new(root)
        .min_depth(1)
        .into_iter()
        .filter_entry(|entry| {
            let ignored = (entry.depth() == 1 && entry.file_name() == ".git")
                || (entry.file_name() == "__pycache__" && entry.file_type().is_dir());
            // A link is never an eligible authored entry or ignored directory.
            !ignored || entry.file_type().is_symlink()
        });
    for entry in entries {
        let Ok(entry) = entry else { return false };
        if entry.file_type().is_symlink() {
            return false;
        }
        let Ok(relative) = entry.path().strip_prefix(root) else {
            return false;
        };
        let relative = relative.to_string_lossy().replace('\\', "/");
        if entry.file_type().is_dir() {
            let prefix = format!("{relative}/");
            if !expected.keys().any(|name| name.starts_with(&prefix)) {
                return false;
            }
        } else if entry.file_type().is_file() && expected.contains_key(&relative) {
            let Ok(bytes) = fs::read(entry.path()) else {
                return false;
            };
            found.insert(relative, format!("{:x}", Sha256::digest(bytes)));
        } else {
            return false;
        }
    }
    &found == expected
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpStream;

    struct Fixture {
        root: PathBuf,
        manager: ComparisonManager,
    }
    impl Fixture {
        fn new() -> Self {
            let mut random = [0; 8];
            getrandom::getrandom(&mut random).unwrap();
            let root = std::env::temp_dir().join(format!(
                "vs-comparison-test-{:x}",
                u64::from_le_bytes(random)
            ));
            fs::create_dir(&root).unwrap();
            let repo = root.join("repo");
            fs::create_dir(&repo).unwrap();
            git(&repo, &["init", "-b", "main"]).unwrap();
            git(&repo, &["config", "user.email", "fixture@example.invalid"]).unwrap();
            git(&repo, &["config", "user.name", "Comparison fixture"]).unwrap();
            fs::write(repo.join("marker"), "baseline").unwrap();
            fs::write(repo.join("server.cjs"), r#"
const http = require('node:http');
const fs = require('node:fs');
http.createServer((req, res) => { res.end(fs.readFileSync('marker')); }).listen(Number(process.env.PORT), '127.0.0.1');
"#).unwrap();
            git(&repo, &["add", "."]).unwrap();
            git(&repo, &["commit", "-m", "baseline"]).unwrap();
            Self {
                root,
                manager: ComparisonManager::default(),
            }
        }
        fn repo(&self) -> PathBuf {
            self.root.join("repo")
        }
        fn config(&self) -> ComparisonConfig {
            serde_json::from_value(serde_json::json!({
                "repository": self.repo(), "baselineWorktree": self.root.join("baseline"),
                "baseline": { "command": "node server.cjs", "readyTimeoutSeconds": 10 },
                "working": { "command": "node server.cjs", "readyTimeoutSeconds": 10 }
            }))
            .unwrap()
        }
        fn wait(&self, id: &str, wanted: ComparisonState) -> ComparisonSession {
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                let session = self.manager.get(id).unwrap();
                if session.state == wanted {
                    return session;
                }
                assert!(
                    !matches!(
                        session.state,
                        ComparisonState::Failed | ComparisonState::Stopped
                    ),
                    "Unexpected terminal state: {session:?}"
                );
                assert!(Instant::now() < deadline, "Timed out: {session:?}");
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.manager.shutdown(Duration::from_secs(5));
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    fn read_url(url: &str) -> String {
        ureq::get(url)
            .timeout(Duration::from_secs(2))
            .call()
            .unwrap()
            .into_string()
            .unwrap()
    }

    #[test]
    fn pinned_baseline_live_uncommitted_worktree_and_owned_cleanup() {
        let fixture = Fixture::new();
        let original_sha = git(&fixture.repo(), &["rev-parse", "HEAD"]).unwrap();
        let working = fixture.root.join("agent-worktree");
        git(
            &fixture.repo(),
            &[
                "worktree",
                "add",
                "--detach",
                working.to_str().unwrap(),
                "HEAD",
            ],
        )
        .unwrap();
        fs::write(working.join("marker"), "uncommitted edit").unwrap();
        let mut config = fixture.config();
        config.working_directory = Some(working.to_string_lossy().into_owned());
        let session = fixture.manager.start(config).unwrap();
        let ready = fixture.wait(&session.id, ComparisonState::Ready);
        assert!(
            !ready.window_open,
            "Server readiness does not acknowledge presentation"
        );
        fixture.manager.mark_window_open(&session.id).unwrap();
        assert!(fixture.manager.get(&session.id).unwrap().window_open);
        assert_eq!(ready.baseline_sha.as_deref(), Some(original_sha.as_str()));
        assert_eq!(read_url(ready.baseline_url.as_ref().unwrap()), "baseline");
        assert_eq!(
            read_url(ready.working_url.as_ref().unwrap()),
            "uncommitted edit"
        );
        fs::write(working.join("marker"), "next saved edit").unwrap();
        fs::write(fixture.repo().join("marker"), "mainline advanced").unwrap();
        git(&fixture.repo(), &["commit", "-am", "advance mainline"]).unwrap();
        assert_eq!(read_url(ready.baseline_url.as_ref().unwrap()), "baseline");
        assert_eq!(
            read_url(ready.working_url.as_ref().unwrap()),
            "next saved edit"
        );
        let changed = fixture
            .manager
            .update(
                &session.id,
                ComparisonUpdate {
                    artifact: None,
                    route: Some("/screen?review=1#section".into()),
                    viewport: Some(Viewport {
                        width: 900,
                        height: 1440,
                        preset: None,
                        orientation: Orientation::Portrait,
                    }),
                    sync_scroll: Some(false),
                },
            )
            .unwrap();
        assert!(changed
            .baseline_url
            .unwrap()
            .ends_with("/screen?review=1#section"));
        assert_eq!(changed.config.viewport.width, 900);
        assert!(!changed.config.sync_scroll);
        assert!(fixture.manager.shutdown(Duration::from_secs(5)));
        assert!(!fixture.manager.get(&session.id).unwrap().window_open);
        assert!(fixture.manager.mark_window_open(&session.id).is_err());
        assert!(!fixture.root.join("baseline").exists());
        assert_eq!(
            fs::read_to_string(working.join("marker")).unwrap(),
            "next saved edit"
        );
        assert_eq!(
            fs::read_to_string(fixture.repo().join("marker")).unwrap(),
            "mainline advanced"
        );
        assert!(ureq::get(ready.baseline_url.as_ref().unwrap())
            .timeout(Duration::from_millis(500))
            .call()
            .is_err());
        assert!(ureq::get(ready.working_url.as_ref().unwrap())
            .timeout(Duration::from_millis(500))
            .call()
            .is_err());
        assert_eq!(
            git(&fixture.repo(), &["worktree", "list", "--porcelain"])
                .unwrap()
                .matches("worktree ")
                .count(),
            2
        );
    }

    #[test]
    fn scroll_capabilities_bind_origin_revoke_on_drop_and_forward_only_positions() {
        let relay = ComparisonScrollRelay::default();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let callback_seen = seen.clone();
        let registration = relay
            .register(
                "https://preview.example".into(),
                Arc::new(move |payload| {
                    callback_seen.lock().unwrap().push(payload.to_owned());
                    Ok(())
                }),
            )
            .unwrap();
        let token = registration.token().to_owned();
        let payload = r#"[{"root":true,"key":"","id":"","path":[],"index":-1,"x":0.2,"y":0.75,"script":"never-forward-this"}]"#;
        assert!(!relay.authorized(&token, "https://preview.example:8443"));
        assert!(!relay.authorized(&token, "https://other.example"));
        assert!(relay
            .dispatch(&token, "https://other.example", payload)
            .is_err());
        assert!(relay
            .dispatch(&"0".repeat(64), "https://preview.example", payload)
            .is_err());
        relay
            .dispatch(&token, "https://preview.example", payload)
            .unwrap();
        assert_eq!(seen.lock().unwrap().len(), 1);
        assert!(!seen.lock().unwrap()[0].contains("script"));
        assert!(!seen.lock().unwrap()[0].contains("never-forward"));
        assert!(relay
            .dispatch(
                &token,
                "https://preview.example",
                &" ".repeat(MAX_COMPARISON_SCROLL_BYTES + 1)
            )
            .is_err());
        assert!(relay
            .dispatch(
                &token,
                "https://preview.example",
                &payload.replace("0.75", "2")
            )
            .is_err());
        drop(registration);
        assert!(!relay.authorized(&token, "https://preview.example"));
        assert!(relay
            .dispatch(&token, "https://preview.example", payload)
            .is_err());
        assert_eq!(seen.lock().unwrap().len(), 1);
        assert!(relay
            .register("https://preview.example/path".into(), Arc::new(|_| Ok(())))
            .is_err());
    }

    fn artifact(title: &str, host_id: &str, terminal_id: &str) -> ComparisonArtifact {
        ComparisonArtifact {
            title: title.into(),
            description: Some("Review the uncommitted component changes".into()),
            owner: ComparisonOwner {
                host_id: host_id.into(),
                terminal_id: Some(terminal_id.into()),
                provider: Some("codex".into()),
                conversation_id: Some("conversation-1".into()),
            },
        }
    }

    #[test]
    fn close_keeps_resources_and_reopen_focuses_the_same_review() {
        let fixture = Fixture::new();
        let session = fixture.manager.start(fixture.config()).unwrap();
        let ready = fixture.wait(&session.id, ComparisonState::Ready);
        fixture.manager.mark_window_open(&session.id).unwrap();
        assert!(fixture.manager.accepts_scroll(&session.id));
        assert!(!fixture.manager.accepts_scroll("unknown-artifact"));
        fixture
            .manager
            .update(
                &session.id,
                ComparisonUpdate {
                    sync_scroll: Some(false),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(!fixture.manager.accepts_scroll(&session.id));
        fixture
            .manager
            .update(
                &session.id,
                ComparisonUpdate {
                    sync_scroll: Some(true),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(fixture.manager.accepts_scroll(&session.id));
        let closed = fixture.manager.close(&session.id).unwrap();
        assert!(!fixture.manager.accepts_scroll(&session.id));
        assert_eq!(closed.state, ComparisonState::Ready);
        assert!(!closed.window_requested && !closed.window_open);
        assert!(fixture.manager.mark_window_open(&session.id).is_err());
        assert!(fixture.root.join("baseline/marker").exists());
        assert_eq!(read_url(ready.baseline_url.as_ref().unwrap()), "baseline");
        fs::write(fixture.repo().join("marker"), "saved while closed").unwrap();
        assert_eq!(
            read_url(ready.working_url.as_ref().unwrap()),
            "saved while closed"
        );
        let reopened = fixture.manager.open(&session.id, None).unwrap();
        assert!(reopened.window_requested);
        assert_eq!(reopened.state, ComparisonState::Ready);
        assert_eq!(reopened.baseline_url, ready.baseline_url);
        assert_eq!(
            reopened.presentation_revision,
            ready.presentation_revision + 1
        );
        fixture.manager.mark_window_open(&session.id).unwrap();
        let focused = fixture.manager.open(&session.id, None).unwrap();
        assert!(focused.window_open);
        assert_eq!(
            focused.presentation_revision,
            reopened.presentation_revision + 1
        );
        assert_eq!(fixture.manager.list().len(), 1);
        assert!(fixture.manager.accepts_scroll(&session.id));
        fixture.manager.stop(&session.id).unwrap();
        assert!(!fixture.manager.accepts_scroll(&session.id));
    }

    #[test]
    fn stopped_artifact_reopens_at_original_commit_after_mainline_moves_and_is_renamed() {
        let fixture = Fixture::new();
        let mut config = fixture.config();
        config.artifact = Some(artifact("Navigation", "local", "terminal-1"));
        let session = fixture.manager.start(config).unwrap();
        let ready = fixture.wait(&session.id, ComparisonState::Ready);
        assert!(fixture.manager.shutdown(Duration::from_secs(5)));
        fs::write(fixture.repo().join("marker"), "new mainline").unwrap();
        git(&fixture.repo(), &["commit", "-am", "advance mainline"]).unwrap();
        git(&fixture.repo(), &["branch", "-m", "review"]).unwrap();
        let renamed = artifact("Navigation revision", "local", "terminal-1");
        fixture
            .manager
            .update(
                &session.id,
                ComparisonUpdate {
                    artifact: Some(renamed.clone()),
                    ..Default::default()
                },
            )
            .unwrap();
        fixture.manager.open(&session.id, None).unwrap();
        let reopened = fixture.wait(&session.id, ComparisonState::Ready);
        assert_eq!(reopened.id, ready.id);
        assert_eq!(reopened.created_at, ready.created_at);
        assert_eq!(reopened.baseline_sha, ready.baseline_sha);
        assert_eq!(reopened.config.artifact, Some(renamed));
        assert_eq!(
            read_url(reopened.baseline_url.as_ref().unwrap()),
            "baseline"
        );
        assert_eq!(
            read_url(reopened.working_url.as_ref().unwrap()),
            "new mainline"
        );
    }

    #[test]
    fn artifact_metadata_survives_restart_without_starting_processes_or_windows() {
        let mut fixture = Fixture::new();
        let path = fixture.root.join("artifacts.json");
        fixture.manager = ComparisonManager::with_store(path.clone()).unwrap();
        let mut config = fixture.config();
        let owner = artifact(
            "Responsive navigation",
            "ssh-review-host",
            "terminal-remote",
        );
        config.artifact = Some(owner.clone());
        let session = fixture.manager.start(config).unwrap();
        let ready = fixture.wait(&session.id, ComparisonState::Ready);
        assert!(fixture.manager.shutdown(Duration::from_secs(5)));
        let restored = ComparisonManager::with_store(path.clone()).unwrap();
        let loaded = restored.get(&session.id).unwrap();
        assert_eq!(loaded.state, ComparisonState::Stopped);
        assert!(!loaded.window_requested && !loaded.window_open);
        assert!(!loaded.restore_required);
        assert_eq!(loaded.config.artifact, Some(owner));
        assert_eq!(loaded.baseline_sha, ready.baseline_sha);
        assert_eq!(loaded.created_at, ready.created_at);
        assert_eq!(
            loaded.config.baseline.command.as_deref(),
            Some("node server.cjs")
        );
        assert!(!fixture.root.join("baseline").exists());
        assert!(loaded.baseline_url.is_none() && loaded.working_url.is_none());
        assert!(ureq::get(ready.baseline_url.as_ref().unwrap())
            .timeout(Duration::from_millis(300))
            .call()
            .is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        fs::write(
            fixture.repo().join("marker"),
            "mainline advanced after restart",
        )
        .unwrap();
        git(&fixture.repo(), &["commit", "-am", "advance mainline"]).unwrap();
        fixture.manager = restored;
        fixture.manager.open(&session.id, None).unwrap();
        let reopened = fixture.wait(&session.id, ComparisonState::Ready);
        assert_eq!(reopened.baseline_sha, ready.baseline_sha);
        assert_eq!(
            read_url(reopened.baseline_url.as_ref().unwrap()),
            "baseline"
        );
    }

    #[test]
    fn failed_metadata_write_does_not_prevent_window_close_or_owned_cleanup() {
        let mut fixture = Fixture::new();
        let path = fixture.root.join("artifacts.json");
        fixture.manager = ComparisonManager::with_store(path.clone()).unwrap();
        let session = fixture.manager.start(fixture.config()).unwrap();
        let ready = fixture.wait(&session.id, ComparisonState::Ready);
        fixture.manager.mark_window_open(&session.id).unwrap();
        fs::write(&path, "preserve this invalid store").unwrap();
        assert!(fixture.manager.close(&session.id).is_err());
        assert!(!fixture.manager.get(&session.id).unwrap().window_requested);
        assert!(fixture.manager.stop(&session.id).is_err());
        assert!(fixture.manager.shutdown(Duration::from_secs(5)));
        assert!(!fixture.root.join("baseline").exists());
        assert!(ureq::get(ready.working_url.as_ref().unwrap())
            .timeout(Duration::from_millis(300))
            .call()
            .is_err());
        assert_eq!(
            fs::read_to_string(path).unwrap(),
            "preserve this invalid store"
        );
    }

    #[test]
    fn persistent_artifacts_omit_environment_and_require_explicit_restore() {
        let mut fixture = Fixture::new();
        let path = fixture.root.join("artifacts.json");
        fixture.manager = ComparisonManager::with_store(path.clone()).unwrap();
        let mut config = fixture.config();
        config.artifact = Some(artifact("Settings", "local", "terminal-1"));
        config
            .env
            .insert("SHARED_TOKEN".into(), "secret-shared-value".into());
        config
            .baseline
            .env
            .insert("BASELINE_TOKEN".into(), "secret-baseline-value".into());
        config
            .working
            .env
            .insert("WORKING_TOKEN".into(), "secret-working-value".into());
        let session = fixture.manager.start(config.clone()).unwrap();
        fixture.wait(&session.id, ComparisonState::Ready);
        assert!(fixture.manager.shutdown(Duration::from_secs(5)));
        // Within this process the original environment is still usable.
        assert!(!fixture.manager.get(&session.id).unwrap().restore_required);
        let stored = fs::read_to_string(&path).unwrap();
        for secret in [
            "secret-shared-value",
            "secret-baseline-value",
            "secret-working-value",
            "SHARED_TOKEN",
            "BASELINE_TOKEN",
            "WORKING_TOKEN",
        ] {
            assert!(!stored.contains(secret), "persisted {secret}");
        }
        fixture.manager = ComparisonManager::with_store(path).unwrap();
        let loaded = fixture.manager.get(&session.id).unwrap();
        assert!(loaded.restore_required);
        assert!(
            loaded.config.env.is_empty()
                && loaded.config.baseline.env.is_empty()
                && loaded.config.working.env.is_empty()
        );
        assert!(fixture
            .manager
            .open(&session.id, None)
            .unwrap_err()
            .contains("restoreConfig"));
        assert_eq!(
            fixture.manager.get(&session.id).unwrap().state,
            ComparisonState::Stopped
        );
        let owner = config.artifact.clone();
        config.artifact = None;
        fixture.manager.open(&session.id, Some(config)).unwrap();
        let reopened = fixture.wait(&session.id, ComparisonState::Ready);
        assert!(!reopened.restore_required);
        assert_eq!(reopened.config.artifact, owner);
        assert_eq!(
            reopened.config.env.get("SHARED_TOKEN").map(String::as_str),
            Some("secret-shared-value")
        );
    }

    #[test]
    fn owner_identity_is_explicit_and_corrupt_artifact_storage_is_preserved() {
        let fixture = Fixture::new();
        let mut config = fixture.config();
        let mut metadata = artifact("One review", "local", "terminal-1");
        metadata.owner.conversation_id = None;
        config.artifact = Some(metadata.clone());
        assert!(
            validate_config(&config).is_ok(),
            "new terminals may not have a native conversation yet"
        );
        metadata.owner.terminal_id = None;
        config.artifact = Some(metadata.clone());
        assert!(validate_config(&config).is_err());
        metadata.owner.conversation_id = Some("native-id".into());
        config.artifact = Some(metadata.clone());
        assert!(validate_config(&config).is_ok());
        metadata.owner.provider = None;
        config.artifact = Some(metadata);
        assert!(validate_config(&config).is_err());
        let path = fixture.root.join("artifacts.json");
        fs::write(&path, "malformed artifact store").unwrap();
        assert!(ComparisonManager::with_store(path.clone()).is_err());
        assert_eq!(
            fs::read_to_string(path).unwrap(),
            "malformed artifact store"
        );
    }

    #[test]
    fn stop_during_startup_cancels_children_and_removes_owned_worktree() {
        let fixture = Fixture::new();
        let mut config = fixture.config();
        config.baseline.command = Some("node -e \"setInterval(() => {}, 1000)\"".into());
        let session = fixture.manager.start(config).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while fixture
            .manager
            .get(&session.id)
            .unwrap()
            .working_url
            .is_none()
        {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(20));
        }
        let start = Instant::now();
        assert!(fixture.manager.shutdown(Duration::from_secs(5)));
        assert!(start.elapsed() < Duration::from_secs(3));
        assert_eq!(
            fixture.manager.get(&session.id).unwrap().state,
            ComparisonState::Stopped
        );
        assert!(!fixture.root.join("baseline").exists());
        assert!(fixture.repo().join("marker").is_file());
    }

    #[cfg(unix)]
    #[test]
    fn cancellation_terminates_checkout_hooks_and_removes_partial_worktree() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = Fixture::new();
        let hook = fixture.repo().join(".git/hooks/post-checkout");
        let marker = fixture.root.join("checkout-hook-started");
        fs::write(
            &hook,
            format!("#!/bin/sh\ntouch '{}'\nsleep 120\n", marker.display()),
        )
        .unwrap();
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
        let session = fixture.manager.start(fixture.config()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !marker.is_file() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(fixture.manager.shutdown(Duration::from_secs(5)));
        assert_eq!(
            fixture.manager.get(&session.id).unwrap().state,
            ComparisonState::Stopped
        );
        assert!(!fixture.root.join("baseline").exists());
        assert_eq!(
            git(&fixture.repo(), &["worktree", "list", "--porcelain"])
                .unwrap()
                .matches("worktree ")
                .count(),
            1
        );
    }

    #[test]
    fn existing_baseline_directory_is_never_adopted_or_removed() {
        let fixture = Fixture::new();
        let baseline = fixture.root.join("baseline");
        fs::create_dir(&baseline).unwrap();
        fs::write(baseline.join("user-file"), "preserve me").unwrap();
        let session = fixture.manager.start(fixture.config()).unwrap();
        let failed = fixture.wait(&session.id, ComparisonState::Failed);
        assert!(failed.error.unwrap().contains("must not exist"));
        assert_eq!(
            fs::read_to_string(baseline.join("user-file")).unwrap(),
            "preserve me"
        );
    }

    #[test]
    fn failed_command_cleans_worktree_and_other_server() {
        let fixture = Fixture::new();
        let mut config = fixture.config();
        config.working.command = Some("exit 42".into());
        let session = fixture.manager.start(config).unwrap();
        let failed = fixture.wait(&session.id, ComparisonState::Failed);
        assert!(failed.error.unwrap().contains("exited"));
        assert!(!fixture.root.join("baseline").exists());
        assert!(fixture.repo().join("marker").is_file());
    }

    #[test]
    fn externally_owned_urls_are_preserved_and_not_claimed_as_pinned() {
        let fixture = Fixture::new();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let done = Arc::new(AtomicBool::new(false));
        let worker_done = done.clone();
        let worker = std::thread::spawn(move || {
            while !worker_done.load(Ordering::Acquire) {
                if let Ok((mut stream, _)) = listener.accept() {
                    stream
                        .set_read_timeout(Some(Duration::from_millis(100)))
                        .unwrap();
                    let mut request = [0; 2048];
                    let _ = stream.read(&mut request);
                    let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\nborrowed");
                } else {
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        });
        let url = format!("http://127.0.0.1:{port}/original#page");
        let mut config = fixture.config();
        config.baseline = PreviewConfig {
            url: Some(url.clone()),
            ..Default::default()
        };
        config.working = config.baseline.clone();
        let session = fixture.manager.start(config).unwrap();
        let ready = fixture.wait(&session.id, ComparisonState::Ready);
        assert!(ready.baseline_external && ready.working_external);
        assert_eq!(ready.baseline_url.as_deref(), Some(url.as_str()));
        assert!(ready.baseline_sha.is_none() && ready.baseline_worktree.is_none());
        assert!(fixture.manager.shutdown(Duration::from_secs(5)));
        assert_eq!(read_url(&url), "borrowed");
        done.store(true, Ordering::Release);
        let _ = TcpStream::connect(("127.0.0.1", port));
        worker.join().unwrap();
    }

    #[test]
    fn baseline_inherits_working_project_subdirectory() {
        let fixture = Fixture::new();
        let app = fixture.repo().join("frontend");
        fs::create_dir(&app).unwrap();
        fs::rename(fixture.repo().join("server.cjs"), app.join("server.cjs")).unwrap();
        fs::rename(fixture.repo().join("marker"), app.join("marker")).unwrap();
        git(&fixture.repo(), &["add", "-A"]).unwrap();
        git(&fixture.repo(), &["commit", "-m", "move frontend"]).unwrap();
        let mut config = fixture.config();
        config.working.directory = Some("frontend".into());
        let session = fixture.manager.start(config).unwrap();
        let ready = fixture.wait(&session.id, ComparisonState::Ready);
        assert_eq!(read_url(ready.baseline_url.as_ref().unwrap()), "baseline");
        assert_eq!(read_url(ready.working_url.as_ref().unwrap()), "baseline");
    }

    #[test]
    fn cleanup_preserves_worktree_if_ownership_marker_changed() {
        let fixture = Fixture::new();
        let session = fixture.manager.start(fixture.config()).unwrap();
        let ready = fixture.wait(&session.id, ComparisonState::Ready);
        let baseline = fixture.root.join("baseline");
        fs::write(baseline.join(".git"), "gitdir: /a/different/worktree").unwrap();
        assert!(fixture.manager.shutdown(Duration::from_secs(5)));
        let failed = fixture.manager.get(&session.id).unwrap();
        assert_eq!(failed.state, ComparisonState::Failed);
        assert!(failed.error.unwrap().contains("ownership marker changed"));
        assert!(baseline.join("marker").is_file());
        // This deliberately invalid fixture belongs entirely to this test.
        if let Some(log) = ready.baseline_log {
            let _ = fs::remove_dir_all(Path::new(&log).parent().unwrap());
        }
    }

    #[cfg(unix)]
    #[test]
    fn exited_shell_does_not_leave_its_dev_server_descendant_running() {
        let fixture = Fixture::new();
        let mut config = fixture.config();
        config.baseline.command =
            Some("node server.cjs & sleep 0.3; echo fixture-failure >&2; exit 42".into());
        let session = fixture.manager.start(config).unwrap();
        let failed = fixture.wait(&session.id, ComparisonState::Failed);
        assert!(failed.error.unwrap().contains("fixture-failure"));
        assert!(!fixture.root.join("baseline").exists());
        assert!(ureq::get(failed.baseline_url.as_ref().unwrap())
            .timeout(Duration::from_millis(500))
            .call()
            .is_err());
        assert!(ureq::get(failed.working_url.as_ref().unwrap())
            .timeout(Duration::from_millis(500))
            .call()
            .is_err());
    }

    #[test]
    fn rejects_origin_changing_routes_and_invalid_resource_options() {
        let fixture = Fixture::new();
        for route in [
            "//example.com",
            "/\\example.com",
            "https://example.com",
            "/hello\n",
        ] {
            let mut config = fixture.config();
            config.route = route.into();
            assert!(fixture.manager.start(config).is_err());
        }
        let mut config = fixture.config();
        config.working.url = Some("http://localhost:4000".into());
        assert!(fixture.manager.start(config).is_err());
        let mut config = fixture.config();
        config.baseline.directory = Some("../outside".into());
        assert!(fixture.manager.start(config).is_err());
        assert!(validate_url("file:///tmp/file").is_err());
        assert!(validate_url("http://user:password@localhost:4000").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn inferred_dependency_borrowing_keeps_vite_caches_independent() {
        let fixture = Fixture::new();
        let modules = fixture.repo().join("node_modules");
        fs::create_dir_all(modules.join(".vite")).unwrap();
        fs::create_dir_all(modules.join(".vite-temp")).unwrap();
        fs::create_dir_all(modules.join(".cache")).unwrap();
        fs::create_dir_all(modules.join(".bin")).unwrap();
        fs::create_dir_all(modules.join("vite")).unwrap();
        fs::write(modules.join(".vite/metadata"), "working cache").unwrap();
        fs::write(modules.join("vite/package.json"), "borrowed package").unwrap();
        let baseline = fixture.root.join("dependencies-test");
        fs::create_dir(&baseline).unwrap();
        link_dependencies(&fixture.repo(), &baseline).unwrap();
        let borrowed = baseline.join("node_modules");
        assert!(!borrowed
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(!borrowed.join(".vite").exists());
        assert!(!borrowed.join(".vite-temp").exists());
        assert!(!borrowed.join(".cache").exists());
        assert!(borrowed.join(".bin").is_dir());
        assert_eq!(
            fs::read_to_string(borrowed.join("vite/package.json")).unwrap(),
            "borrowed package"
        );
        fs::create_dir(borrowed.join(".vite")).unwrap();
        fs::write(borrowed.join(".vite/metadata"), "baseline cache").unwrap();
        assert_eq!(
            fs::read_to_string(modules.join(".vite/metadata")).unwrap(),
            "working cache"
        );
    }

    #[test]
    fn known_shipped_skill_migration_preserves_git_and_skips_customized_trees() {
        use sha2::{Digest, Sha256};
        let fixture = Fixture::new();
        let bundled = fixture.root.join("bundled");
        let previous_files = [
            ("SKILL.md", "prior bundled instructions"),
            ("references/configuration.md", "prior bundled configuration"),
            ("scripts/compare.py", "prior bundled helper"),
        ];
        let previous: BTreeMap<String, String> = previous_files
            .iter()
            .map(|(path, content)| {
                (
                    (*path).into(),
                    format!("{:x}", Sha256::digest(content.as_bytes())),
                )
            })
            .collect();
        let previous = [
            BTreeMap::from([("SKILL.md".into(), "another-known-version".into())]),
            previous,
        ];
        for (path, _) in previous_files {
            let dest = bundled.join(path);
            fs::create_dir_all(dest.parent().unwrap()).unwrap();
            fs::write(dest, format!("new bundled {path}")).unwrap();
        }
        fs::create_dir_all(bundled.join("scripts/__pycache__")).unwrap();
        fs::write(
            bundled.join("scripts/__pycache__/compare.pyc"),
            "generated bytecode",
        )
        .unwrap();
        for variant in [
            "pristine",
            "modified",
            "extra-file",
            "extra-directory",
            "cache-named-file",
            "missing-file",
        ] {
            let home = fixture.root.join(variant);
            let target = home.join(".agents/skills/ui-compare");
            for (path, content) in previous_files {
                let dest = target.join(path);
                fs::create_dir_all(dest.parent().unwrap()).unwrap();
                fs::write(dest, content).unwrap();
            }
            fs::create_dir(target.join(".git")).unwrap();
            fs::write(target.join(".git/HEAD"), "preserve my history").unwrap();
            fs::create_dir_all(target.join("scripts/__pycache__")).unwrap();
            fs::write(
                target.join("scripts/__pycache__/compare.pyc"),
                "old generated bytecode",
            )
            .unwrap();
            match variant {
                "modified" => {
                    fs::write(target.join("scripts/compare.py"), "custom helper").unwrap()
                }
                "extra-file" => fs::write(target.join("notes.md"), "my notes").unwrap(),
                "extra-directory" => fs::create_dir(target.join("my-resources")).unwrap(),
                "cache-named-file" => {
                    fs::write(target.join("__pycache__"), "authored file").unwrap()
                }
                "missing-file" => {
                    fs::remove_file(target.join("references/configuration.md")).unwrap()
                }
                _ => {}
            }
            let installed = ensure_agent_skill_with_previous(&bundled, &home, &previous).unwrap();
            assert_eq!(
                installed.contains(&target),
                variant == "pristine",
                "{variant}"
            );
            assert_eq!(
                fs::read_to_string(target.join(".git/HEAD")).unwrap(),
                "preserve my history"
            );
            let wanted = if variant == "pristine" {
                "new bundled SKILL.md"
            } else {
                "prior bundled instructions"
            };
            assert_eq!(fs::read_to_string(target.join("SKILL.md")).unwrap(), wanted);
            if variant == "pristine" {
                assert!(!target.join("scripts/__pycache__").exists());
                assert!(ensure_agent_skill_with_previous(&bundled, &home, &previous)
                    .unwrap()
                    .is_empty());
            }
            if variant == "modified" {
                assert_eq!(
                    fs::read_to_string(target.join("scripts/compare.py")).unwrap(),
                    "custom helper"
                );
            }
        }
        let fresh = fixture.root.join("fresh");
        ensure_agent_skill_with_previous(&bundled, &fresh, &previous).unwrap();
        assert!(!fresh
            .join(".agents/skills/ui-compare/scripts/__pycache__")
            .exists());
    }

    #[cfg(unix)]
    #[test]
    fn known_shipped_skill_migration_never_follows_symlinks() {
        use sha2::{Digest, Sha256};
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let root = fixture.root.join("skill");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("SKILL.md"), "original").unwrap();
        let previous = BTreeMap::from([(
            "SKILL.md".into(),
            format!("{:x}", Sha256::digest(b"original")),
        )]);
        assert!(unchanged_skill_tree(&root, &previous));
        let linked = fixture.root.join("linked-skill");
        symlink(&root, &linked).unwrap();
        assert!(!unchanged_skill_tree(&linked, &previous));
        fs::rename(root.join("SKILL.md"), fixture.root.join("outside.md")).unwrap();
        symlink(fixture.root.join("outside.md"), root.join("SKILL.md")).unwrap();
        assert!(!unchanged_skill_tree(&root, &previous));
        assert_eq!(
            fs::read_to_string(fixture.root.join("outside.md")).unwrap(),
            "original"
        );
    }

    #[test]
    fn skill_installation_preserves_customized_and_existing_copies() {
        let fixture = Fixture::new();
        let bundled = fixture.root.join("bundled");
        fs::create_dir(&bundled).unwrap();
        fs::write(bundled.join("SKILL.md"), "original guidance").unwrap();
        let home = fixture.root.join("home");
        fs::create_dir(&home).unwrap();
        let installed = ensure_agent_skill_in(&bundled, &home).unwrap();
        let shared = home.join(".agents/skills/ui-compare");
        assert!(installed.contains(&shared));
        fs::write(shared.join("SKILL.md"), "my customized instructions").unwrap();
        assert!(ensure_agent_skill_in(&bundled, &home).unwrap().is_empty());
        assert_eq!(
            fs::read_to_string(shared.join("SKILL.md")).unwrap(),
            "my customized instructions"
        );
    }
}
