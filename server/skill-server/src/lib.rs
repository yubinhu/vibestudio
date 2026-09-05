//! skill-server — the HTTP face of `skill-core` + `skill-term`.
//!
//! This crate IS the backend: it exposes every capability over `/api/*` (JSON,
//! plus SSE for terminal output) and serves the built UI (`dist/`) from the same
//! origin. It runs two ways from ONE serve loop:
//!   * **standalone** — the `skill-server` binary (`src/main.rs`), e.g. on a remote
//!     host reached over an `ssh -L` tunnel, or for browser-local dev.
//!   * **in-process** — the desktop shell ([client/desktop]) calls [`spawn`] on a
//!     background thread and points its webview at the returned loopback URL.
//!
//! The desktop and the remote host therefore run byte-identical request handling;
//! only the entry point differs. Configuration (bind addr, the `dist` directory,
//! the bundled-skills + examples bases, an optional bearer token) flows in via
//! [`ServerConfig`] — the library makes no CWD-relative guesses of its own.

use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;

use serde_json::{json, Value};
use skill_core::{
    commit_agent, commitmsg, connections, discover, engine, github, gitops, mining, preferences, recents,
    reveal, secrets, skill, sync, update,
};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

#[cfg(feature = "local-backend")]
mod events;
mod gateway;
mod phone;
mod proxy;
#[cfg(feature = "local-backend")]
mod push;
mod remote_api;
mod sshmgr;
mod tailscale;

pub use phone::{PhoneControl, PHONE_PORT};
pub use sshmgr::SshRemoteControl;

/// Install the process-wide logger: stderr sink, level via `RUST_LOG`. Idempotent
/// and a no-op if a global logger is already set, so it's safe to call from either
/// entry point — the standalone `skill-server` binary (`main.rs`) or the desktop
/// shell that hosts this server in-process.
///
/// Output is pinned to **stderr** so it never corrupts the `SKILL_SERVER_READY
/// port=N` line the desktop reads off this process's *stdout* (see `main.rs`). The
/// default is intentionally quiet (`warn`, with the `skill_*` crates at `info`), so
/// a packaged/remote build is near-silent; for development raise it with e.g.
/// `RUST_LOG=skill_server=debug,skill_core=debug,skill_term=debug`.
///
/// `log` + `env_logger` (not `tracing`) is deliberate: the server is synchronous
/// (`tiny_http` + `std::thread`, no tokio) and this is dev logging, not tracing.
/// The `log::*` call sites stay portable — an OpenTelemetry Logs exporter
/// (`opentelemetry-appender-log`) can replace this subscriber later with no
/// call-site changes.
pub fn init_logging() {
    let _ = base_log_builder().target(env_logger::Target::Stderr).try_init();
}

/// Like [`init_logging`], but ALSO append logs to `path` — for the packaged desktop,
/// where stderr goes nowhere (no attached terminal; on Windows the release build
/// detaches the console entirely). Tees every line to BOTH stderr (so `npm run dev`
/// keeps showing logs) and the file. Best-effort: if the file can't be opened it
/// falls back to stderr-only, so logging never breaks. The file is kept small by
/// rotating once at startup when it exceeds ~1 MiB (retaining a single `.log.1`); at
/// the default `warn` level it grows very slowly.
pub fn init_logging_to_file(path: &Path) {
    let target = match open_log_file(path) {
        Some(file) => env_logger::Target::Pipe(Box::new(TeeWriter { file })),
        None => env_logger::Target::Stderr,
    };
    let _ = base_log_builder().target(target).try_init();
}

const DEFAULT_LOG_FILTER: &str = "warn,skill_server=info,skill_core=info,skill_term=info";

/// Shared builder: read `RUST_LOG` (quiet default), millisecond timestamps. The
/// caller picks the sink via `.target(...)`.
fn base_log_builder() -> env_logger::Builder {
    let mut b =
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(DEFAULT_LOG_FILTER));
    b.format_timestamp_millis();
    b
}

/// Rotate the log if it's already large (keep one previous `.log.1`), then open it
/// for append. All steps best-effort — any failure yields `None` → stderr-only.
fn open_log_file(path: &Path) -> Option<std::fs::File> {
    const MAX_BYTES: u64 = 1024 * 1024; // 1 MiB — keep the on-disk log small.
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if std::fs::metadata(path).map(|m| m.len() > MAX_BYTES).unwrap_or(false) {
        let _ = std::fs::rename(path, path.with_extension("log.1"));
    }
    std::fs::OpenOptions::new().create(true).append(true).open(path).ok()
}

/// Fans each formatted log line out to the file (the durable sink for a packaged
/// app) AND stderr (keeps `npm run dev` working). The stderr write is best-effort so
/// a closed/invalid stderr in a bundled app never drops the file write. env_logger
/// serializes writes to this target internally, so no extra locking is needed.
struct TeeWriter {
    file: std::fs::File,
}
impl Write for TeeWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let _ = std::io::stderr().write_all(buf);
        self.file.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        let _ = std::io::stderr().flush();
        self.file.flush()
    }
}

// ───────────────────────────── public API ─────────────────────────────

// ── Remote-SSH connection manager (desktop only) ──
// The desktop's SSH controller plugs in here so the UI can drive it over
// `/api/remote/*`; while connected, the local server reverse-proxies the rest of
// `/api/*` to the remote. The standalone (remote) binary leaves
// `ServerConfig::remote` as `None`, so none of this is active there. The types live
// here so `ServerConfig` can name them; the impl lives in `client/desktop`
// (preserving the one-way `client/desktop` → `skill_server` crate dependency).

/// A host the user can connect to: an alias from `~/.ssh/config`, or free-form
/// `user@host`.
#[derive(serde::Serialize)]
pub struct RemoteHost {
    pub name: String,
    /// Resolved `user@hostname:port` for display, when known.
    pub detail: Option<String>,
}

/// Live connection state, polled by the UI via `GET /api/remote/status`.
#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RemoteStatus {
    /// `idle` | `detecting` | `installing` | `launching` | `forwarding` | `connected` | `error`.
    pub state: String,
    pub host: Option<String>,
    pub message: Option<String>,
}

/// Where the switchboard forwards `/api/*` while connected. The `token` is injected
/// into the upstream `Authorization` header by the proxy and never reaches the
/// browser (so the SSE `EventSource`, which can't set headers, needs no token).
#[derive(Clone)]
pub struct RemoteTarget {
    /// `http://127.0.0.1:<local-forwarded-port>`.
    pub base_url: String,
    pub token: String,
}

/// The desktop's SSH connection manager. `connect` kicks off provisioning/tunnelling
/// on a background thread and reports progress through `status`; `active_target`
/// returns `Some` only once fully connected — that's the proxy's signal to forward.
pub trait RemoteControl: Send + Sync {
    fn list_hosts(&self) -> Result<Vec<RemoteHost>, String>;
    fn connect(&self, host: &str) -> Result<(), String>;
    /// Tear down the live session. `forget` also clears the remembered resume host —
    /// atomically with invalidating any in-flight connect — so an explicit user
    /// disconnect starts Local next launch. App-exit teardown passes `false`, so
    /// quitting while connected still resumes next time.
    fn disconnect(&self, forget: bool) -> Result<(), String>;
    fn status(&self) -> RemoteStatus;
    fn active_target(&self) -> Option<RemoteTarget>;
    /// The host to auto-reconnect to on launch (the last one we connected to and never
    /// explicitly disconnected from), or `None` to start Local. The client reads this
    /// (`GET /api/remote/last`) and drives the resume through the normal connect path.
    fn last_host(&self) -> Option<String> {
        None
    }
}

/// The desktop shell's native-notification surface (OS toasts + dock/taskbar
/// badge), driven by the SPA over the pinned-local `/api/notify*` routes.
/// Implemented in `client/desktop` (same one-way dependency rule as
/// [`RemoteControl`]); a server without one (standalone binary, browser mode)
/// 404s those routes and the SPA falls back to the Web Notification API where
/// the platform has one.
pub trait NotifyControl: Send + Sync {
    /// Show an OS notification. Must not block the calling worker.
    fn notify(&self, title: &str, body: &str) -> Result<(), String>;
    /// Ask the OS for notification permission at a moment the user expects it
    /// (macOS prompts; Windows/Linux need nothing). Must not block.
    fn prime(&self) {}
    /// Unread-count badge on the dock/taskbar icon; 0 clears. Best-effort.
    fn set_badge(&self, _count: u32) {}
}

/// Opening a session's folder in the user's local editor (VS Code) — the "Open in
/// VS Code" affordance. A CLIENT-machine capability, not agent work: it acts on the
/// screen the user is at, so it's implemented in `client/desktop` (same one-way
/// dependency rule as [`NotifyControl`]) and reached only over the pinned-local
/// `/api/editor/*` route (never proxied). A server without one (standalone binary,
/// browser mode) 404s those routes and the SPA hides the button.
pub trait EditorControl: Send + Sync {
    /// The reachable editor's display name (`Some` → show the button), or `None`.
    fn detect(&self) -> Option<String>;
    /// Open `path` in the editor. `remote_host` set → the folder is on that SSH
    /// remote, so open it over VS Code Remote-SSH (a local window attached over the
    /// same SSH the tunnel uses); `None` → open the local path directly.
    fn open(&self, path: &str, remote_host: Option<&str>) -> Result<(), String>;
}

// The mobile switchboard's SSH credential store — the trait, the profile type,
// and the file-backed fallback all live in `skill-core` (see its `keystore`
// module) so both this crate and the standalone binary reach them; the native
// OS-keystore backends live in `client/desktop`. Re-exported so existing
// `skill_server::{SecureStore, SshProfile}` paths keep resolving.
pub use skill_core::keystore::{FileSecureStore, SecureStore, SshProfile};

/// How to run the server. Build with `..Default::default()` and override fields.
pub struct ServerConfig {
    /// Bind host. Desktop and CLI both use `127.0.0.1`.
    pub host: String,
    /// Requested port. `0` = ephemeral (the desktop uses this and reads the real
    /// port back from [`ServerHandle::addr`]).
    pub port: u16,
    /// Directory of the built UI (`index.html` + assets). Required — the library
    /// does NOT fall back to a CWD-relative `dist`.
    pub dist: PathBuf,
    /// Base dir of the bundled built-in skills (`load-secrets`, `skill-miner`),
    /// each a subfolder with a `SKILL.md`. `None` falls back to an env/CWD/dist
    /// probe.
    pub bundled_skills: Option<PathBuf>,
    /// Base dir for resolving a bundled example by relative path in
    /// `/api/read-skill`. `None` keeps absolute-path-only behaviour.
    pub examples_base: Option<PathBuf>,
    /// Optional bearer token. `None` = no auth (loopback). `Some` = require
    /// `Authorization: Bearer <token>` on every request (the SSH case).
    pub token: Option<String>,
    /// Worker-thread count.
    pub workers: usize,
    /// Run the startup chores: `skill_term::sweep_stale` (GC long-finished
    /// terminals — never live ones; sessions deliberately outlive backends),
    /// `engine::reap_orphans` (kill an inference engine whose backend died),
    /// and `prefetch_model`. The standalone binary owns this; an embedding
    /// process (the desktop) that does its own lifecycle sets it `false` so
    /// the chores don't run twice.
    pub startup_maintenance: bool,
    /// The SSH connection manager (desktop only). `None` = a plain server with no
    /// remoting (the standalone binary, or browser-local dev). When set, the server
    /// serves `/api/remote/*` and proxies the rest of `/api/*` to the connected remote.
    pub remote: Option<Arc<dyn RemoteControl>>,
    /// The app's installer (desktop only) — `spawn` hands it to
    /// `skill_core::update::init`, which runs the background release check.
    /// `None` (standalone/dev) = `/api/update/status` reports `canAuto: false`.
    pub updater: Option<Arc<dyn update::UpdateControl>>,
    /// "Open on your phone" (`/api/phone/*`): ensure a persistent daemon on the
    /// phone port + configure `tailscale serve`. `None` = feature hidden (404),
    /// e.g. a provisioned remote server.
    pub phone: Option<Arc<PhoneControl>>,
    /// Native notifications (desktop only): OS toasts + dock badge for agent
    /// turn-finish events. `None` (standalone/browser) = `/api/notify*` 404s
    /// and the SPA falls back to the Web Notification API.
    pub notifier: Option<Arc<dyn NotifyControl>>,
    /// "Open in VS Code" (desktop only): open a session's folder in the local
    /// editor, or on the connected remote over Remote-SSH. `None` (standalone/
    /// browser) = `/api/editor/*` 404s and the SPA hides the button.
    pub editor: Option<Arc<dyn EditorControl>>,
    /// SSH credential store (mobile only): saved connection profiles + Keychain-
    /// held private keys for the russh transport. `None` (desktop, standalone) =
    /// `/api/remote/profiles*` 404s and the SPA hides the credential UI.
    pub secure_store: Option<Arc<dyn SecureStore>>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 0,
            dist: PathBuf::from("dist"),
            bundled_skills: None,
            examples_base: None,
            token: None,
            workers: 4,
            startup_maintenance: true,
            remote: None,
            updater: None,
            phone: None,
            notifier: None,
            editor: None,
            secure_store: None,
        }
    }
}

/// A running server. The worker threads each hold the listener, so the server
/// keeps serving even if this handle is dropped; keep it to read [`addr`] or to
/// [`join`] (block) on it.
///
/// [`addr`]: ServerHandle::addr
/// [`join`]: ServerHandle::join
pub struct ServerHandle {
    /// The ACTUAL bound address; `.port()` is the kernel-assigned port when
    /// `ServerConfig::port` was `0`.
    pub addr: SocketAddr,
    workers: Vec<thread::JoinHandle<()>>,
}

impl ServerHandle {
    /// `http://<addr>` — point a webview here.
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }
    /// Block until the workers exit (they serve until the process ends).
    pub fn join(self) {
        for w in self.workers {
            let _ = w.join();
        }
    }
}

/// Probe a loopback server's `/api/health`. Tight timeouts: this backs the iOS
/// shell's foreground check for a listener the OS reclaimed during an app
/// suspension (tiny_http's accept thread dies for good on the first accept
/// error), so it must answer fast whether the server is live, gone, or bound
/// but no longer accepting.
pub fn loopback_alive(port: u16) -> bool {
    ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(1))
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .get(&format!("http://127.0.0.1:{port}/api/health"))
        .call()
        .is_ok()
}

/// Bind synchronously (so a bind error surfaces here), then serve on background
/// worker threads. Returns immediately with the bound address.
pub fn spawn(cfg: ServerConfig) -> std::io::Result<ServerHandle> {
    let server = Server::http(format!("{}:{}", cfg.host, cfg.port))
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    let addr = server
        .server_addr()
        .to_ip()
        .ok_or_else(|| std::io::Error::other("server bound to a non-IP address"))?;
    let server = Arc::new(server);

    if cfg.startup_maintenance {
        // GC terminals whose agent finished long ago (live ones persist across
        // backend restarts by design — see skill-term), reap an inference
        // engine orphaned by a dead backend, then warm the model so the first
        // commit draft is fast.
        #[cfg(feature = "local-backend")]
        skill_term::sweep_stale();
        engine::reap_orphans();
        engine::prefetch_model();
        // Repoint agent MCP configs at this boot's gateway port (the desktop may
        // bind an ephemeral port when its preferred one is taken, stranding a
        // prior boot's /gw URL). Off-thread so the claude shell-outs don't delay
        // serving; no-op when nothing changed.
        let gw_port = addr.port();
        thread::spawn(move || connections::resync_agent_configs(gw_port));
    }

    if let Some(control) = cfg.updater {
        // The workspace version is stamped to the release tag, same as the desktop.
        update::init(control, env!("CARGO_PKG_VERSION"));
    }

    // Bell watching starts with the server, not the first browser: Web Push must
    // fire exactly when no client is connected. Gated to phone-serving servers
    // (desktop, standalone, hub) so a test/ephemeral `spawn()` — which shares the
    // real tmux and config-dir push.json — never delivers a real push. A browser
    // that later opens `/api/events` still starts it on demand via subscribe().
    #[cfg(feature = "local-backend")]
    if cfg.phone.is_some() {
        events::start();
    }

    let ctx = Arc::new(ServerCtx {
        dist: cfg.dist,
        bundled_skills: cfg.bundled_skills,
        examples_base: cfg.examples_base,
        token: cfg.token,
        remote: cfg.remote,
        phone: cfg.phone,
        notifier: cfg.notifier,
        editor: cfg.editor,
        secure_store: cfg.secure_store,
        port: addr.port(),
    });

    let mut workers = Vec::with_capacity(cfg.workers);
    for _ in 0..cfg.workers {
        let server = Arc::clone(&server);
        let ctx = Arc::clone(&ctx);
        workers.push(thread::spawn(move || worker_loop(&server, &ctx)));
    }
    Ok(ServerHandle { addr, workers })
}

/// Resolved per-request context (config the handlers actually read).
struct ServerCtx {
    dist: PathBuf,
    bundled_skills: Option<PathBuf>,
    examples_base: Option<PathBuf>,
    token: Option<String>,
    remote: Option<Arc<dyn RemoteControl>>,
    phone: Option<Arc<PhoneControl>>,
    notifier: Option<Arc<dyn NotifyControl>>,
    editor: Option<Arc<dyn EditorControl>>,
    secure_store: Option<Arc<dyn SecureStore>>,
    /// The ACTUAL bound port — the MCP-connection flow bakes it into the
    /// `/gw/<id>/mcp` gateway URL written into agent configs.
    port: u16,
}

/// Bearer-token guard. `None` token ⇒ always authorized (loopback default).
///
/// NOTE for when a token is actually used (the SSH case): the SSE attach path
/// (`/api/terminal/attach`) is consumed with `EventSource`, which can't send an
/// `Authorization` header — so that route will need the token via a query param,
/// or to lean on the loopback-bound `ssh -L` tunnel for auth. See design.md.
fn authorized(token: &Option<String>, request: &Request) -> bool {
    match token {
        None => true,
        Some(t) => {
            let want = format!("Bearer {t}");
            request
                .headers()
                .iter()
                .any(|h| h.field.equiv("Authorization") && h.value.as_str() == want.as_str())
        }
    }
}

fn worker_loop(server: &Server, ctx: &ServerCtx) {
    for mut request in server.incoming_requests() {
        let method = request.method().clone();
        let url = request.url().to_string();
        let path = url.split('?').next().unwrap_or(url.as_str()).to_string();
        // ── MCP gateway (/gw/<id>/mcp) — deliberately BEFORE the bearer guard
        // and the remote proxy (it isn't /api, so the switchboard never forwards
        // it): the local agent CLIs calling it can't send our bearer, and the
        // route self-authenticates (loopback Host + unguessable connection id).
        // Responses can be lifetime-long SSE streams → each on its own thread.
        if path.starts_with("/gw/") {
            let (m, u) = (method.clone(), url.clone());
            thread::spawn(move || gateway::handle(request, &m, &u));
            continue;
        }
        // Auth at the single choke point (no-op when token is None). OPTIONS
        // preflight carries no Authorization, so it stays unauthenticated.
        if method != Method::Options && !authorized(&ctx.token, &request) {
            reply_status(request, 401, "Unauthorized");
            continue;
        }
        // Writes get the cross-site origin check at the same choke point.
        if method == Method::Post && !origin_allowed(&request) {
            reply_status(request, 403, "Cross-origin request rejected");
            continue;
        }

        // ── Remote-SSH switchboard (desktop only; `ctx.remote` is None on the
        //    standalone binary, so neither branch fires there) ──
        // The connection manager is ALWAYS handled locally, even while connected.
        if path.starts_with("/api/remote/") {
            let mut body = String::new();
            if method == Method::Post {
                let _ = request.as_reader().read_to_string(&mut body);
            }
            send_reply(request, remote_api::handle(&method, &path, &body, ctx));
            continue;
        }
        // Frontend log shipping — ALWAYS handled locally (never proxied), so the
        // UI's own warns/errors land in THIS machine's log file, which is where
        // you'd look to debug the desktop app. The client batches these, so it
        // only fires when the UI actually logs a warning/error.
        if path == "/api/logs/client" {
            let mut body = String::new();
            if method == Method::Post {
                let _ = request.as_reader().read_to_string(&mut body);
            }
            send_reply(request, client_log(&body));
            continue;
        }
        // App auto-update — ALWAYS handled locally (never proxied): it's THIS
        // desktop's own update state, not the connected remote's.
        if path.starts_with("/api/update/") {
            let mut body = String::new();
            if method == Method::Post {
                let _ = request.as_reader().read_to_string(&mut body);
            }
            send_reply(request, handle(&method, &url, &body, ctx));
            continue;
        }
        // On-device SSH keygen — ALWAYS handled locally (never proxied): the
        // generated private key must be born on the machine whose keystore will
        // hold it; forwarding it to a connected remote would mint (and expose)
        // the key over there instead. 404s on servers without `russh-transport`.
        if path == "/api/ssh/keygen" {
            let mut body = String::new();
            if method == Method::Post {
                let _ = request.as_reader().read_to_string(&mut body);
            }
            send_reply(request, handle(&method, &url, &body, ctx));
            continue;
        }
        // Native notifications — ALWAYS handled locally (never proxied): a toast
        // or dock badge belongs to the machine whose screen the user is looking
        // at, not to the connected remote. And only to its OWN webview/browser:
        // a tailscale-served phone client shares this origin, but its toast must
        // pop on the phone, not this desktop — fronted requests get the same 404
        // as a notifier-less server, which sends the SPA to the Web Notification
        // API fallback.
        if path == "/api/notify" || path.starts_with("/api/notify/") {
            if !from_this_machine(&request) {
                send_reply(request, notify_unavailable());
                continue;
            }
            let mut body = String::new();
            if method == Method::Post {
                let _ = request.as_reader().read_to_string(&mut body);
            }
            send_reply(request, handle(&method, &url, &body, ctx));
            continue;
        }
        // Opening a local editor (VS Code) belongs to the machine whose screen the
        // user is at — like notifications, pinned LOCAL (never proxied) and served
        // only to THIS machine's own webview/browser. A tailscale-fronted phone
        // client gets the same 404 as an editor-less server, so the SPA hides the
        // affordance rather than popping a window on the unattended host desktop.
        if path == "/api/editor" || path.starts_with("/api/editor/") {
            if !from_this_machine(&request) {
                send_reply(request, editor_unavailable());
                continue;
            }
            let mut body = String::new();
            if method == Method::Post {
                let _ = request.as_reader().read_to_string(&mut body);
            }
            send_reply(request, handle(&method, &url, &body, ctx));
            continue;
        }
        // Revealing a saved file in the OS file manager is likewise pinned LOCAL
        // (never proxied) and served only to THIS machine's own client — the file
        // it reveals is the one just written here. A fronted/phone client 404s;
        // the SPA never offers Reveal there (it has no local path).
        if path == "/api/reveal" {
            if !from_this_machine(&request) {
                send_reply(request, reveal_unavailable());
                continue;
            }
            let mut body = String::new();
            if method == Method::Post {
                let _ = request.as_reader().read_to_string(&mut body);
            }
            send_reply(request, handle(&method, &url, &body, ctx));
            continue;
        }
        // Saving a `.skill` straight to disk writes on the ATTENDED machine, so it
        // must not be proxied. Served only from this machine's own client AND only
        // when no remote is connected — a connected hub's skills live on the
        // remote, whose bytes reach the local webview via the proxied blob GET
        // instead. Otherwise 404 → the SPA falls back to that blob download.
        if path == "/api/download/skill/save" {
            let remote_active = ctx.remote.as_ref().and_then(|r| r.active_target()).is_some();
            if !from_this_machine(&request) || remote_active {
                send_reply(request, save_unavailable());
                continue;
            }
            let mut body = String::new();
            if method == Method::Post {
                let _ = request.as_reader().read_to_string(&mut body);
            }
            send_reply(request, handle(&method, &url, &body, ctx));
            continue;
        }
        // While a remote is connected, every other /api/* is reverse-proxied to it.
        // (Must precede the local attach branch so the SSE stream proxies too.) BOTH
        // proxy paths run on their OWN thread, so a slow/hung remote can never pin a
        // pooled worker — keeping the locally-handled connection manager (status,
        // disconnect) responsive even when every request is being proxied.
        if path.starts_with("/api/") {
            if let Some(target) = ctx.remote.as_ref().and_then(|r| r.active_target()) {
                let url = url.clone();
                // Both SSE routes must stream, not buffer — `proxy_buffered` would
                // block forever collecting an unending body.
                if method == Method::Get && (path == "/api/terminal/attach" || path == "/api/events") {
                    thread::spawn(move || proxy::proxy_sse(request, &url, &target));
                } else {
                    let method = method.clone();
                    thread::spawn(move || proxy::proxy_buffered(request, &method, &url, &target));
                }
                continue;
            }
        }

        // ── local handling ──
        // Terminal output streams (SSE) block for the session's lifetime — run
        // them on a dedicated thread so they never starve this worker. (Only a
        // local-backend build serves terminals; a switchboard reaches them via the
        // proxy branch above, so this is gated off there.)
        #[cfg(feature = "local-backend")]
        if method == Method::Get && path == "/api/terminal/attach" {
            thread::spawn(move || stream_terminal(request, &url));
            continue;
        }
        // The terminal-events stream (SSE) blocks for the subscription's lifetime —
        // same dedicated-thread treatment as the attach stream.
        #[cfg(feature = "local-backend")]
        if method == Method::Get && path == "/api/events" {
            thread::spawn(move || stream_events(request));
            continue;
        }
        let mut body = String::new();
        if method == Method::Post {
            let _ = request.as_reader().read_to_string(&mut body);
        }
        send_reply(request, handle(&method, &url, &body, ctx));
    }
}

/// One-line access/error log for an outgoing reply, the single point through which
/// every locally-handled and proxied request flows. `<400` logs at `debug` (a
/// per-request access trace, off by default — enable with `RUST_LOG=…=debug`);
/// `>=400` logs at `warn` with the error detail pulled from our uniform
/// `{"error": …}` body, so server-side failures that previously only reached the
/// client are now visible at the default level.
fn log_reply(request: &Request, status: u16, body: &[u8]) {
    if status < 400 {
        log::debug!("{} {} -> {}", request.method().as_str(), request.url(), status);
        return;
    }
    let detail = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_owned))
        .unwrap_or_default();
    log::warn!("{} {} -> {} {}", request.method().as_str(), request.url(), status, detail);
}

/// A request header's value, if present. (`equiv` needs the `'static` name.)
fn header_value(request: &Request, name: &'static str) -> Option<String> {
    request
        .headers()
        .iter()
        .find(|h| h.field.equiv(name))
        .map(|h| h.value.as_str().to_string())
}

/// The host part of an origin/authority string, lowercased, port stripped
/// (`https://Host:port` / `[::1]:port` / bare `host:port` all accepted).
fn host_part(s: &str) -> String {
    let rest = s
        .strip_prefix("http://")
        .or_else(|| s.strip_prefix("https://"))
        .unwrap_or(s);
    let host = if let Some(v6) = rest.strip_prefix('[') {
        v6.split(']').next().unwrap_or("")
    } else {
        rest.split(':').next().unwrap_or(rest)
    };
    host.to_ascii_lowercase()
}

/// True for the loopback origins the browser-local dev split uses
/// (SPA on `localhost:1420`, API here) — the only legit cross-origin callers.
fn is_loopback_origin(origin: &str) -> bool {
    matches!(host_part(origin).as_str(), "localhost" | "127.0.0.1" | "::1")
}

/// True when the request reached this server's loopback bind directly — i.e.
/// from THIS machine's webview or browser. A `tailscale serve`-fronted request
/// arrives with forwarding headers and/or a ts.net `Host`, and must not count:
/// tailscaled proxies from 127.0.0.1, so the peer address can't tell them apart.
pub(crate) fn from_this_machine(request: &Request) -> bool {
    header_value(request, "X-Forwarded-Host").is_none()
        && header_value(request, "X-Forwarded-For").is_none()
        && header_value(request, "Host").as_deref().map(is_loopback_origin).unwrap_or(false)
}

/// Cross-site write guard: browsers attach `Origin` to POSTs. Accept requests
/// without one (curl, the desktop proxy), same-origin ones (Origin host matches
/// Host / X-Forwarded-Host, however the server is fronted — tailscale serve, a
/// LAN IP), and the loopback dev origins. Everything else is some other website
/// driving a browser at this API — CORS already hides responses, but "simple"
/// POSTs would still execute server-side, so refuse them outright.
fn origin_allowed(request: &Request) -> bool {
    let Some(origin) = header_value(request, "Origin") else { return true };
    if is_loopback_origin(&origin) {
        return true;
    }
    let origin_host = host_part(&origin);
    [header_value(request, "X-Forwarded-Host"), header_value(request, "Host")]
        .into_iter()
        .flatten()
        .any(|h| host_part(&h) == origin_host)
}

/// Serialize a `Reply` onto the wire with the standard headers, then any
/// reply-specific `extra` headers (an extra `Cache-Control` replaces the default
/// `no-store`, so static assets can opt into caching). CORS headers are emitted
/// only for loopback origins (the dev split); same-origin traffic never needs
/// them, and reflecting arbitrary origins would hand the API to any website open
/// in a browser that can reach this server. Shared by local handlers and the proxy.
pub(crate) fn send_reply(request: Request, reply: Reply) {
    log_reply(&request, reply.status, &reply.body);
    let mut response = Response::from_data(reply.body).with_status_code(reply.status);
    let mut headers: Vec<(&str, &str)> = vec![("Content-Type", reply.content_type.as_str())];
    let origin = header_value(&request, "Origin");
    if let Some(origin) = origin.as_deref().filter(|o| is_loopback_origin(o)) {
        headers.push(("Access-Control-Allow-Origin", origin));
        headers.push(("Access-Control-Allow-Methods", "GET, POST, OPTIONS"));
        headers.push(("Access-Control-Allow-Headers", "Content-Type, Authorization"));
        headers.push(("Vary", "Origin"));
    }
    let has_cache = reply.extra.iter().any(|(k, _)| k.eq_ignore_ascii_case("cache-control"));
    if !has_cache {
        headers.push(("Cache-Control", "no-store"));
    }
    for (k, val) in headers {
        if let Ok(h) = Header::from_bytes(k.as_bytes(), val.as_bytes()) {
            response.add_header(h);
        }
    }
    for (k, val) in &reply.extra {
        if let Ok(h) = Header::from_bytes(k.as_bytes(), val.as_bytes()) {
            response.add_header(h);
        }
    }
    let _ = request.respond(response);
}

// ───────────────────────────── request handling ─────────────────────────────

struct Reply {
    status: u16,
    body: Vec<u8>,
    content_type: String,
    extra: Vec<(String, String)>,
}

fn json_reply<T: serde::Serialize>(result: Result<T, String>) -> Reply {
    match result {
        Ok(v) => Reply {
            status: 200,
            body: serde_json::to_vec(&v).unwrap_or_default(),
            content_type: "application/json".into(),
            extra: vec![],
        },
        Err(e) => Reply {
            status: 400,
            body: serde_json::to_vec(&json!({ "error": e })).unwrap_or_default(),
            content_type: "application/json".into(),
            extra: vec![],
        },
    }
}

/// connection-begin/-reconnect wire shapes: 200 serializes `BeginOk`; 400 is
/// the typed `{"error": <code>, "message": …}` contract (not the uniform
/// `json_reply` error, whose `error` field is the human message).
fn begin_reply(result: Result<connections::BeginOk, connections::BeginError>) -> Reply {
    match result {
        Ok(ok) => json_reply(Ok(ok)),
        Err(e) => Reply {
            status: 400,
            body: serde_json::to_vec(&json!({ "error": e.code, "message": e.message }))
                .unwrap_or_default(),
            content_type: "application/json".into(),
            extra: vec![],
        },
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// The OAuth callback lands a real browser tab here — a tiny self-contained
/// page (inline CSS, no assets), one variant per outcome.
fn callback_page(outcome: connections::CallbackOutcome) -> Reply {
    use connections::CallbackOutcome as O;
    let (title, detail) = match &outcome {
        O::Success { label } => (
            "You’re connected".to_string(),
            format!("{} is now connected — return to VibeStudio.", html_escape(label)),
        ),
        O::Denied => {
            ("Authorization declined".into(), "No changes made — you can close this tab.".into())
        }
        O::Failed { message } => ("Connection failed".into(), html_escape(message)),
    };
    let body = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>VibeStudio</title>\
         <style>body{{margin:0;min-height:100vh;display:flex;align-items:center;justify-content:center;\
         font-family:system-ui,-apple-system,sans-serif;background:#f5f6f8;color:#1d2733}}\
         main{{max-width:26rem;padding:2.5rem;text-align:center}}\
         h1{{font-size:1.25rem;margin:0 0 .5rem}}p{{margin:0;color:#51606f;line-height:1.5}}</style>\
         </head><body><main><h1>{title}</h1><p>{detail}</p></main></body></html>"
    );
    Reply {
        status: 200,
        body: body.into_bytes(),
        content_type: "text/html; charset=utf-8".into(),
        extra: vec![],
    }
}

/// Re-emit a batch of frontend log entries through the server logger, so they land
/// in the same sink (stderr + the on-disk file) as backend logs. Only `warn`/`error`
/// are forwarded by the client, under the `skill_client` target; malformed batches
/// are ignored (never let client input crash the route).
fn client_log(body: &str) -> Reply {
    #[derive(serde::Deserialize)]
    struct Entry {
        level: String,
        scope: Option<String>,
        msg: String,
    }
    #[derive(serde::Deserialize)]
    struct Batch {
        entries: Vec<Entry>,
    }
    if let Ok(batch) = serde_json::from_str::<Batch>(body) {
        for e in batch.entries.into_iter().take(200) {
            let scope = e.scope.as_deref().unwrap_or("client");
            if e.level == "error" {
                log::error!(target: "skill_client", "[{scope}] {}", e.msg);
            } else {
                log::warn!(target: "skill_client", "[{scope}] {}", e.msg);
            }
        }
    }
    json_reply(Ok(json!({ "ok": true })))
}

fn web_mime(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("").to_lowercase();
    match ext.as_str() {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "webmanifest" => "application/manifest+json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "wasm" => "application/wasm",
        "map" => "application/json",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// The SPA compiled into the binary (`embed-ui` builds; see Cargo.toml). Disk
/// `dist` wins when present, so a dev checkout still serves fresh builds.
#[cfg(feature = "embed-ui")]
static EMBEDDED_UI: include_dir::Dir<'_> =
    include_dir::include_dir!("$CARGO_MANIFEST_DIR/../../dist");

/// Serve `url_path` from the embedded SPA, mirroring `serve_static` semantics
/// (SPA fallback, extension-miss 404, immutable assets/).
#[cfg(feature = "embed-ui")]
fn serve_embedded(url_path: &str) -> Reply {
    let rel = url_path.trim_start_matches('/');
    let lookup = if rel.is_empty() || rel.contains("..") { "index.html" } else { rel };
    let looks_like_file = lookup.rsplit('/').next().unwrap_or("").contains('.');
    let (path, file) = match EMBEDDED_UI.get_file(lookup) {
        Some(f) => (lookup, f),
        None if looks_like_file => {
            return Reply {
                status: 404,
                body: b"Not found.".to_vec(),
                content_type: "text/plain; charset=utf-8".into(),
                extra: vec![],
            };
        }
        None => match EMBEDDED_UI.get_file("index.html") {
            Some(f) => ("index.html", f),
            None => {
                return Reply {
                    status: 404,
                    body: b"This build embeds no UI.".to_vec(),
                    content_type: "text/plain; charset=utf-8".into(),
                    extra: vec![],
                };
            }
        },
    };
    let cache = if path.starts_with("assets/") && path == lookup {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    Reply {
        status: 200,
        content_type: web_mime(path).into(),
        body: file.contents().to_vec(),
        extra: vec![("Cache-Control".into(), cache.into())],
    }
}

/// Serve a static asset from `dist`, falling back to index.html (SPA). Misses
/// that look like a file (extension in the last segment) 404 instead — an SPA
/// fallback there hands HTML to probes like iOS's GET /apple-touch-icon.png.
/// With no usable `dist` on disk, an `embed-ui` build serves the compiled-in SPA.
fn serve_static(dist: &Path, url_path: &str) -> Reply {
    #[cfg(feature = "embed-ui")]
    if !dist.join("index.html").is_file() {
        return serve_embedded(url_path);
    }
    let rel = url_path.trim_start_matches('/');
    // Empty path or a traversal attempt → fall back to the SPA index; only ever
    // serve within dist.
    let candidate = if rel.is_empty() || rel.contains("..") {
        dist.join("index.html")
    } else {
        dist.join(rel)
    };
    let looks_like_file = rel.rsplit('/').next().unwrap_or("").contains('.');
    let hit = candidate.is_file();
    let target = if hit {
        candidate
    } else if looks_like_file {
        return Reply {
            status: 404,
            body: b"Not found.".to_vec(),
            content_type: "text/plain; charset=utf-8".into(),
            extra: vec![],
        };
    } else {
        dist.join("index.html")
    };
    // Vite content-hashes everything under assets/, so those can cache forever;
    // index.html (and the few root files) must revalidate so new builds land.
    let cache = if hit && rel.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    match std::fs::read(&target) {
        Ok(body) => Reply {
            status: 200,
            content_type: web_mime(target.to_str().unwrap_or("")).into(),
            body,
            extra: vec![("Cache-Control".into(), cache.into())],
        },
        Err(_) => Reply {
            status: 404,
            body: b"Not found. Build the UI first (npm run build) or pass --dist.".to_vec(),
            content_type: "text/plain; charset=utf-8".into(),
            extra: vec![],
        },
    }
}

/// 404 for `/api/phone/*` on a server with no `PhoneControl` — the UI reads
/// this as "hide the feature".
fn phone_unavailable() -> Reply {
    Reply {
        status: 404,
        body: serde_json::to_vec(&json!({ "error": "phone access not available on this server" }))
            .unwrap_or_default(),
        content_type: "application/json".into(),
        extra: vec![],
    }
}

/// 404 for `/api/notify*` on a server with no `NotifyControl` — the SPA reads
/// this as "no native surface, use the Web Notification API".
fn notify_unavailable() -> Reply {
    Reply {
        status: 404,
        body: serde_json::to_vec(&json!({ "error": "native notifications not available on this server" }))
            .unwrap_or_default(),
        content_type: "application/json".into(),
        extra: vec![],
    }
}

/// 404 for `/api/editor/*` reached from anywhere but this machine's own client —
/// the SPA reads it as "no local editor here" and hides the button.
fn editor_unavailable() -> Reply {
    Reply {
        status: 404,
        body: serde_json::to_vec(&json!({ "error": "opening a local editor is not available from here" }))
            .unwrap_or_default(),
        content_type: "application/json".into(),
        extra: vec![],
    }
}

/// 404 for `/api/reveal` reached from anywhere but this machine's own client.
fn reveal_unavailable() -> Reply {
    Reply {
        status: 404,
        body: serde_json::to_vec(&json!({ "error": "revealing files is not available from here" }))
            .unwrap_or_default(),
        content_type: "application/json".into(),
        extra: vec![],
    }
}

/// 404 for `/api/download/skill/save` when not from this machine or a remote is
/// connected — the SPA reads it as "save-to-disk unavailable" and falls back to
/// the blob download (`/api/download/skill`).
fn save_unavailable() -> Reply {
    Reply {
        status: 404,
        body: serde_json::to_vec(&json!({ "error": "saving to disk is not available from here" }))
            .unwrap_or_default(),
        content_type: "application/json".into(),
        extra: vec![],
    }
}

fn query_param(url: &str, key: &str) -> Option<String> {
    let q = url.split_once('?')?.1;
    for pair in q.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            if k == key {
                return Some(urlencoding::decode(v).map(|c| c.into_owned()).unwrap_or_else(|_| v.to_string()));
            }
        }
    }
    None
}

/// Locate the base dir of the bundled built-in skills (`load-secrets`,
/// `skill-miner`). Honors `VIBESTUDIO_BUNDLED_SKILLS`, else looks relative to
/// CWD and the dist dir. A candidate counts if it contains the activation skill.
fn bundled_skills_dir(dist: &Path) -> Option<PathBuf> {
    let has_skills = |p: &Path| p.join("load-secrets").join("SKILL.md").exists();
    if let Ok(p) = std::env::var("VIBESTUDIO_BUNDLED_SKILLS") {
        let pb = PathBuf::from(p);
        if has_skills(&pb) {
            return Some(pb);
        }
    }
    let candidates = [PathBuf::from("skills"), dist.join("../skills"), dist.join("skills")];
    candidates.into_iter().find(|c| has_skills(c))
}

/// A bundled skill's folder, by dir name.
fn bundled_skill(ctx: &ServerCtx, name: &str) -> Option<PathBuf> {
    let base = ctx.bundled_skills.clone().or_else(|| bundled_skills_dir(&ctx.dist))?;
    let dir = base.join(name);
    dir.join("SKILL.md").exists().then_some(dir)
}

fn handle(method: &Method, url: &str, body: &str, ctx: &ServerCtx) -> Reply {
    let path = url.split('?').next().unwrap_or(url);
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();

    match (method, path) {
        (Method::Options, _) => Reply {
            status: 204,
            body: vec![],
            content_type: "text/plain".into(),
            extra: vec![],
        },
        (Method::Get, "/api/skills/discover") => json_reply(discover::discover_and_autotrack()),
        (Method::Post, "/api/skills/read") => {
            let root = skill::resolve_skill_input(&s("path"), ctx.examples_base.as_deref());
            json_reply(skill::build_raw_skill(&root))
        }
        // Generate an SSH keypair on THIS machine (mobile switchboard): the private half is
        // returned for the client to store in the OS keystore (iOS Keychain), the public half
        // to paste into the remote's authorized_keys. Feature-gated; pin this local in the
        // proxy so it's never forwarded to a connected remote.
        #[cfg(feature = "russh-transport")]
        (Method::Post, "/api/ssh/keygen") => {
            let comment = { let c = s("comment"); if c.is_empty() { "vibestudio".into() } else { c } };
            json_reply(crate::sshmgr::keygen::generate_ed25519(&comment).map(|k| json!({
                "privateKey": k.private_openssh,
                "publicKey": k.public_openssh,
                "fingerprint": k.fingerprint,
            })))
        }
        (Method::Post, "/api/fs/read") => json_reply(skill::read_file_impl(&s("root"), &s("rel"))),
        (Method::Post, "/api/fs/stat") => json_reply(skill::stat_file_impl(&s("root"), &s("rel"))),
        (Method::Post, "/api/fs/write") => {
            // `expectedEtag` (the tag the editor loaded) turns this into a
            // compare-and-swap; absent → legacy unconditional overwrite.
            let expected = v.get("expectedEtag").and_then(|x| x.as_str()).filter(|s| !s.is_empty());
            json_reply(skill::write_file_impl(&s("root"), &s("rel"), &s("content"), expected))
        }
        (Method::Post, "/api/fs/delete") => {
            json_reply(skill::delete_path_impl(&s("root"), &s("rel")).map(|_| json!({ "ok": true })))
        }
        (Method::Post, "/api/fs/read-image") => json_reply(skill::read_image_impl(&s("root"), &s("rel"))),
        (Method::Post, "/api/fs/write-asset") => {
            // `data` is the media file base64-encoded (the JSON body must stay
            // UTF-8 text — same convention as /api/import-zip). The server picks a
            // non-clobbering name under `dir` and returns the path it wrote,
            // relative to `root`, for the markdown link.
            json_reply(
                skill::write_asset_impl(&s("root"), &s("dir"), &s("name"), &s("data"))
                    .map(|rel| json!({ "rel": rel })),
            )
        }
        (Method::Post, "/api/fs/list-dir") => json_reply(skill::list_dir_impl(
            &s("path"),
            v.get("includeFiles").and_then(|x| x.as_bool()).unwrap_or(false),
        )),
        (Method::Post, "/api/sync/targets") => json_reply(sync::sync_targets(&s("root"))),
        (Method::Post, "/api/sync/skill") => {
            let overwrite = v.get("overwrite").and_then(|x| x.as_bool()).unwrap_or(false);
            let link = v.get("link").and_then(|x| x.as_bool()).unwrap_or(false);
            json_reply(sync::sync_skill(&s("root"), &s("target"), overwrite, link))
        }
        (Method::Post, "/api/skills/delete") => json_reply(sync::delete_skill(&s("root"))),
        (Method::Post, "/api/skills/promote") => json_reply(sync::promote_skill(&s("root"))),
        (Method::Get, "/api/skills/homes") => json_reply(sync::skill_homes()),
        (Method::Post, "/api/skills/create") => {
            json_reply(sync::create_skill(&s("target"), &s("name"), &s("content")))
        }
        (Method::Post, "/api/import/folder") => {
            let overwrite = v.get("overwrite").and_then(|x| x.as_bool()).unwrap_or(false);
            json_reply(sync::import_skill_folder(&s("source"), &s("target"), overwrite))
        }
        (Method::Post, "/api/import/zip") => {
            // `data` is the .zip base64-encoded (the JSON body must stay UTF-8 text).
            let overwrite = v.get("overwrite").and_then(|x| x.as_bool()).unwrap_or(false);
            json_reply(sync::import_skill_zip_base64(&s("data"), &s("target"), overwrite))
        }
        (Method::Post, "/api/import/remote") => {
            // Clone a skill repository (GitHub/GitLab/any git URL) into a home;
            // the clone keeps its origin, so the skill arrives sync-connected.
            let overwrite = v.get("overwrite").and_then(|x| x.as_bool()).unwrap_or(false);
            json_reply(github::import_skill_from_remote(&s("url"), &s("target"), overwrite))
        }
        // --- app-managed agent terminals (tmux-backed) --- (local-backend only;
        // a switchboard proxies every terminal route to the connected remote)
        #[cfg(feature = "local-backend")]
        (Method::Get, "/api/terminal/agents") => json_reply(Ok(skill_term::detect_agents())),
        #[cfg(feature = "local-backend")]
        (Method::Get, "/api/terminal/list") => json_reply(skill_term::list_sessions().map(|list| {
            // Enrich each session with a human title read from the agent's own
            // session store (falls back to the cwd client-side when absent).
            list.into_iter()
                .map(|s| {
                    let mut v = serde_json::to_value(&s).unwrap_or_default();
                    let sid = s.session_id.trim();
                    let title = skill_core::agents::session_title_for(
                        &s.agent,
                        &s.cwd,
                        s.created.parse().unwrap_or(0),
                        (!sid.is_empty()).then_some(sid),
                    );
                    if let (Some(t), Some(obj)) = (title, v.as_object_mut()) {
                        obj.insert("title".into(), serde_json::Value::String(t));
                    }
                    v
                })
                .collect::<Vec<_>>()
        })),
        #[cfg(feature = "local-backend")]
        (Method::Post, "/api/terminal/create") => {
            let u16f = |k: &str, d: u16| v.get(k).and_then(|x| x.as_u64()).map(|n| n as u16).unwrap_or(d);
            let boolf = |k: &str| v.get(k).and_then(|x| x.as_bool()).unwrap_or(false);
            let extra: Vec<String> = v
                .get("extraArgs")
                .and_then(|x| x.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                .unwrap_or_default();
            // `resume` (API-only, no dialog control): reopen the agent's
            // recorded session in cwd instead of starting a fresh one.
            if boolf("resume") {
                let model = s("model");
                let effort = s("effort");
                json_reply(skill_term::create_session_resume(
                    &s("agent"),
                    &s("cwd"),
                    u16f("cols", 80),
                    u16f("rows", 24),
                    Some(model.trim()).filter(|m| !m.is_empty()),
                    Some(effort.trim()).filter(|e| !e.is_empty()),
                ))
            } else {
                json_reply(skill_term::create_session(
                    &s("agent"),
                    &s("cwd"),
                    u16f("cols", 80),
                    u16f("rows", 24),
                    boolf("ide"),
                    boolf("skipPermissions"),
                    boolf("autoMode"),
                    &extra,
                ))
            }
        }
        #[cfg(feature = "local-backend")]
        (Method::Post, "/api/terminal/kill") => {
            json_reply(skill_term::kill_session(&s("id")).map(|_| json!({ "ok": true })))
        }
        #[cfg(feature = "local-backend")]
        (Method::Post, "/api/terminal/input") => {
            let data = skill_term::b64_decode(&s("data"));
            json_reply(skill_term::write(&s("id"), &data).map(|_| json!({ "ok": true })))
        }
        #[cfg(feature = "local-backend")]
        (Method::Post, "/api/terminal/resize") => {
            let u16f = |k: &str, d: u16| v.get(k).and_then(|x| x.as_u64()).map(|n| n as u16).unwrap_or(d);
            json_reply(
                skill_term::resize(&s("id"), u16f("cols", 80), u16f("rows", 24)).map(|_| json!({ "ok": true })),
            )
        }
        #[cfg(feature = "local-backend")]
        (Method::Post, "/api/terminal/paste-image") => {
            // `data` is the image base64-encoded (the JSON body must stay UTF-8
            // text — same convention as /api/import-zip). Returns the temp-file
            // path on THIS machine, which is where the agent runs.
            json_reply(skill_term::save_pasted_image(&s("data"), &s("mime")).map(|p| json!({ "path": p })))
        }
        // --- skill mining (a skill-miner run in an agent terminal) ---
        (Method::Get, "/api/mine/sources") => {
            let days = query_param(url, "days").and_then(|d| d.parse().ok()).unwrap_or(35);
            json_reply(Ok(mining::sources(days)))
        }
        // The active run dir's files (the history archive is excluded) — the
        // mining page's artifacts listing.
        (Method::Get, "/api/mine/files") => json_reply(mining::files()),
        // Past runs archived under history/<id>/ — the mining page's "Past
        // runs" list (display-only: agent, id, when).
        (Method::Get, "/api/mine/history") => json_reply(mining::history()),
        // Whether the installed skill-miner copies differ from the bundled
        // official version — the dialog only offers "reinstall" when they do.
        (Method::Get, "/api/mine/miner-status") => {
            let bundled = bundled_skill(ctx, "skill-miner");
            json_reply(Ok(mining::miner_status(bundled.as_deref())))
        }
        // Restore every installed copy of the skill-miner to the official
        // bundled version (any .git the user created is preserved, so the
        // refresh lands as ordinary reviewable uncommitted changes).
        (Method::Post, "/api/mine/reinstall-miner") => {
            let bundled = bundled_skill(ctx, "skill-miner");
            json_reply(mining::reinstall_miner(bundled.as_deref()).map(|roots| json!({ "roots": roots })))
        }
        // The prompt a run with these settings would send — the dialog shows
        // it for review/editing; an edited prompt comes back via mine/start.
        (Method::Post, "/api/mine/prompt") => {
            let days = v.get("days").and_then(|x| x.as_u64()).unwrap_or(35);
            let improve = v.get("improve").and_then(|x| x.as_bool()).unwrap_or(true);
            json_reply(mining::preview_prompt(days, improve).map(|p| json!({ "prompt": p })))
        }
        #[cfg(feature = "local-backend")]
        (Method::Post, "/api/mine/start") => {
            let days = v.get("days").and_then(|x| x.as_u64()).unwrap_or(35);
            let sources: Vec<String> = v
                .get("sources")
                .and_then(|x| x.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                .unwrap_or_default();
            let improve = v.get("improve").and_then(|x| x.as_bool()).unwrap_or(true);
            let agent = s("agent");
            let model = s("model");
            let effort = s("effort");
            let prompt = s("prompt");
            let bundled = bundled_skill(ctx, "skill-miner");
            json_reply((|| {
                let opt = skill_term::detect_agents()
                    .into_iter()
                    .find(|a| a.id == agent)
                    .ok_or_else(|| format!("Unknown agent option: {agent}"))?;
                let prompt_override = Some(prompt.trim()).filter(|p| !p.is_empty());
                let prep = mining::prepare_run(
                    &opt.agent,
                    days,
                    &sources,
                    improve,
                    prompt_override,
                    bundled.as_deref(),
                )?;
                // The agent registry's interactive launch: the TUI with the
                // run prompt pre-submitted — an ordinary agent session. The
                // client navigates the user to this terminal, where any
                // first-run trust dialog or approval prompt is answered.
                let cmd = mining::launch_cmd(
                    &opt.agent,
                    &opt.bin,
                    &prep.prompt,
                    Some(model.trim()).filter(|m| !m.is_empty()),
                    Some(effort.trim()).filter(|e| !e.is_empty()),
                )
                .ok_or_else(|| format!("{} can't run skill mining yet.", opt.label))?;
                let sess = skill_term::create_session_cmd(&agent, &prep.run_dir, 200, 50, &cmd)?;
                mining::record_run(prep, &agent, model.trim(), effort.trim(), &sess.id)
            })())
        }
        // The run's conversation: the recorded terminal while the agent is
        // live in it, else revived — via the terminal API's resume path
        // (create_session_resume), the same one `terminal/create {resume:true}`
        // takes.
        #[cfg(feature = "local-backend")]
        (Method::Post, "/api/mine/continue") => {
            let exists = |id: &str| {
                skill_term::list_sessions()
                    .map(|ss| ss.iter().any(|s| s.id == id))
                    .unwrap_or(false)
            };
            let running = |id: &str| !skill_term::agent_exited(id);
            let spawn = |agent_id: &str, cwd: &str, model: Option<&str>, effort: Option<&str>| {
                skill_term::create_session_resume(agent_id, cwd, 200, 50, model, effort)
                    .map(|s| s.id)
            };
            json_reply(
                mining::continue_run(exists, running, spawn)
                    .map(|id| json!({ "terminalId": id })),
            )
        }
        #[cfg(feature = "local-backend")]
        (Method::Get, "/api/mine/state") => {
            let exists = |id: &str| {
                skill_term::list_sessions()
                    .map(|ss| ss.iter().any(|s| s.id == id))
                    .unwrap_or(false)
            };
            let running = |id: &str| !skill_term::agent_exited(id);
            json_reply(mining::state(exists, running))
        }
        #[cfg(feature = "local-backend")]
        (Method::Post, "/api/mine/stop") => {
            json_reply(mining::stop(skill_term::kill_session).map(|_| json!({ "ok": true })))
        }
        (Method::Post, "/api/skills/detect-env") => {
            let root = s("root");
            json_reply(secrets::secret_keys().map(|keys| skill::scan_for_env_vars(Path::new(&root), &keys)))
        }
        (Method::Post, "/api/git/info") => json_reply(gitops::git_info(&s("root"))),
        (Method::Post, "/api/git/track") => json_reply(gitops::git_track(&s("root"))),
        (Method::Post, "/api/git/untrack") => {
            json_reply(gitops::git_untrack(&s("root")).map(|_| json!({ "ok": true })))
        }
        (Method::Post, "/api/git/dirty-many") => {
            let roots: Vec<String> = v
                .get("roots")
                .and_then(|x| x.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                .unwrap_or_default();
            json_reply(Ok(gitops::git_dirty_many(&roots)))
        }
        (Method::Post, "/api/git/commit") => json_reply(gitops::git_commit(&s("root"), &s("message"))),
        (Method::Post, "/api/commit-message/generate") => json_reply(commitmsg::generate(&s("root"))),
        (Method::Post, "/api/commit-message/regenerate") => json_reply(commitmsg::regenerate(&s("root"))),
        (Method::Post, "/api/commit-message/peek") => json_reply(commitmsg::peek(&s("root"))),
        (Method::Get, "/api/commit-message/model-status") => json_reply(Ok(commit_agent::status())),
        (Method::Post, "/api/git/log") => {
            let limit = v.get("limit").and_then(|x| x.as_u64()).unwrap_or(20) as usize;
            json_reply(gitops::git_log(&s("root"), limit))
        }
        (Method::Post, "/api/git/status") => json_reply(gitops::git_status(&s("root"))),
        (Method::Post, "/api/git/worktree-diff") => json_reply(gitops::git_worktree_diff(&s("root"))),
        (Method::Post, "/api/git/commit-diff") => json_reply(gitops::git_commit_diff(&s("root"), &s("sha"))),
        (Method::Post, "/api/git/file-at") => json_reply(gitops::git_file_at(&s("root"), &s("rev"), &s("path"))),
        (Method::Post, "/api/git/files-at") => json_reply(gitops::git_files_at(&s("root"), &s("rev"))),
        (Method::Post, "/api/git/discard") => {
            json_reply(gitops::git_discard(&s("root"), &s("path")).map(|_| json!({ "ok": true })))
        }
        (Method::Post, "/api/git/discard-all") => {
            json_reply(gitops::git_discard_all(&s("root")).map(|_| json!({ "ok": true })))
        }
        (Method::Post, "/api/git/enter-version") => json_reply(gitops::git_enter_version(&s("root"), &s("sha"))),
        (Method::Post, "/api/git/exit-version") => json_reply(gitops::git_exit_version(&s("root"))),
        (Method::Post, "/api/git/keep-version") => json_reply(gitops::git_keep_version(&s("root"), &s("message"))),
        // --- publish a skill to GitHub (its own repo; remote = source of truth) ---
        (Method::Post, "/api/github/status") => {
            let check_remote = v.get("checkRemote").and_then(|x| x.as_bool()).unwrap_or(false);
            json_reply(github::github_status(&s("root"), check_remote))
        }
        (Method::Post, "/api/github/owners") => json_reply(github::list_owners()),
        (Method::Post, "/api/github/connect-token") => json_reply(github::connect_token(&s("token"))),
        (Method::Post, "/api/github/disconnect") => {
            json_reply(github::disconnect().map(|_| json!({ "ok": true })))
        }
        (Method::Post, "/api/github/device-start") => json_reply(github::device_start()),
        (Method::Post, "/api/github/device-poll") => json_reply(github::device_poll()),
        (Method::Post, "/api/github/publish") => {
            let private = v.get("private").and_then(|x| x.as_bool()).unwrap_or(true);
            json_reply(github::publish(&s("root"), &s("owner"), &s("repo"), private))
        }
        // Provider-free: connect any existing git remote by URL (GitLab,
        // Bitbucket, self-hosted, …) — only github.com URLs get token sugar.
        (Method::Post, "/api/github/connect-remote") => {
            json_reply(github::connect_remote(&s("root"), &s("url")))
        }
        (Method::Post, "/api/github/sync") => json_reply(github::sync_now(&s("root"))),
        (Method::Post, "/api/github/auto-pull") => json_reply(github::auto_pull(&s("root"))),
        (Method::Post, "/api/github/unlink") => {
            json_reply(github::unlink(&s("root")).map(|_| json!({ "ok": true })))
        }
        // --- app auto-update (always served locally — see worker_loop) ---
        (Method::Get, "/api/update/status") => json_reply(Ok(update::status())),
        (Method::Post, "/api/update/apply") => {
            json_reply(update::apply().map(|_| json!({ "ok": true })))
        }
        // Recently opened skills/markdown. A NORMAL /api/* route (not short-circuited
        // like /api/remote/*): while connected it proxies to the remote, so recents
        // belong to whichever machine you're working on — same list whether you reach
        // it locally or over SSH.
        (Method::Get, "/api/recents/list") => json_reply(recents::list()),
        (Method::Post, "/api/recents/add") => {
            json_reply(recents::add(&s("root"), &s("name"), v.get("kind").and_then(|x| x.as_str())))
        }
        (Method::Post, "/api/recents/remove") => json_reply(recents::remove(&s("root"))),
        // Workspace defaults follow the active server, just like recents.
        (Method::Get, "/api/preferences") => json_reply(preferences::get()),
        (Method::Post, "/api/preferences/picker") => {
            json_reply(preferences::remember_picker(&s("context"), &s("path")))
        }
        (Method::Post, "/api/preferences/terminal") => {
            let prefs = serde_json::from_value(v.get("prefs").cloned().unwrap_or(Value::Null));
            json_reply(prefs.map_err(|e| format!("Invalid terminal preferences: {e}"))
                .and_then(|p| preferences::remember_terminal(&s("agentId"), p)))
        }
        (Method::Get, "/api/secrets/status") => json_reply(secrets::secrets_status()),
        (Method::Get, "/api/secrets/list") => json_reply(secrets::secrets_list()),
        (Method::Post, "/api/secrets/set") => {
            json_reply(secrets::secret_set(&s("key"), &s("value")).map(|_| json!({ "ok": true })))
        }
        (Method::Post, "/api/secrets/delete") => {
            json_reply(secrets::secret_delete(&s("key")).map(|_| json!({ "ok": true })))
        }
        // --- MCP connections: Studio holds the OAuth tokens; agents reach the
        // MCP through the loopback /gw/<id>/mcp gateway (see gateway.rs) ---
        (Method::Post, "/api/connections/begin") => {
            let label = v.get("label").and_then(|x| x.as_str());
            begin_reply(connections::begin(&s("url"), &s("origin"), label))
        }
        // The AS redirects a real browser tab here (also reachable through the
        // switchboard proxy, which injects the upstream bearer) — text/html out.
        (Method::Get, "/api/connections/callback") => {
            let q = |k: &str| query_param(url, k).unwrap_or_default();
            callback_page(connections::finish_callback(&q("state"), &q("code"), &q("error"), ctx.port))
        }
        (Method::Get, "/api/connections/pending") => {
            json_reply(Ok(connections::pending_status(&query_param(url, "state").unwrap_or_default())))
        }
        (Method::Get, "/api/connections/list") => json_reply(connections::list()),
        (Method::Post, "/api/connections/reconnect") => {
            begin_reply(connections::reconnect(&s("id"), &s("origin")))
        }
        (Method::Post, "/api/connections/delete") => {
            json_reply(connections::delete(&s("id")).map(|_| json!({ "ok": true })))
        }
        (Method::Post, "/api/secrets/preview-env") => json_reply(Ok(secrets::preview_dotenv(&s("data")))),
        (Method::Post, "/api/secrets/setup") => {
            let bootstrap = bundled_skill(ctx, "load-secrets");
            json_reply(secrets::secrets_setup(bootstrap.as_deref()))
        }
        // The skill's secrets as a plain-text .env download — for handing a
        // collaborator the values over a channel of the user's choosing (the
        // values deliberately never travel in the repo; see remotesync).
        (Method::Get, "/api/download/env") => {
            let root = query_param(url, "root").unwrap_or_default();
            let vars: Vec<String> = query_param(url, "vars")
                .map(|s| s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect())
                .unwrap_or_default();
            let name = Path::new(&root)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "skill".into());
            match secrets::render_dotenv(&vars) {
                Ok(body) if body.is_empty() => {
                    json_reply::<()>(Err("None of this skill's secrets are in your store yet.".into()))
                }
                Ok(body) => Reply {
                    status: 200,
                    body: body.into_bytes(),
                    content_type: "text/plain; charset=utf-8".into(),
                    extra: vec![(
                        "Content-Disposition".into(),
                        format!("attachment; filename=\"{name}.env\""),
                    )],
                },
                Err(e) => json_reply::<()>(Err(e)),
            }
        }
        (Method::Get, "/api/download/skill") => {
            let root = query_param(url, "root").unwrap_or_default();
            // Optional `vars=A,B` → bundle those managed secrets' values as a .env.
            let env_vars: Vec<String> = query_param(url, "vars")
                .map(|s| s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect())
                .unwrap_or_default();
            match skill::zip_skill_bytes(&root, &env_vars) {
                Ok((filename, bytes)) => Reply {
                    status: 200,
                    body: bytes,
                    content_type: "application/zip".into(),
                    extra: vec![(
                        "Content-Disposition".into(),
                        format!("attachment; filename=\"{filename}\""),
                    )],
                },
                Err(e) => json_reply::<()>(Err(e)),
            }
        }
        // Desktop-only sibling of the blob GET above: write the packaged `.skill`
        // straight to this machine's Downloads folder and report the path, so the
        // UI can confirm the save (the webview's blob download is silent) and
        // reveal it. Pinned local + no-remote by the dispatch loop; a browser/
        // phone or connected-hub client 404s and falls back to the blob GET.
        (Method::Post, "/api/download/skill/save") => {
            let env_vars: Vec<String> = v
                .get("vars")
                .and_then(|x| x.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            json_reply(
                skill::save_skill_to_downloads(&s("root"), &env_vars)
                    .map(|p| json!({ "path": p.to_string_lossy() })),
            )
        }
        // The answering server's identity (informational; proxies like any route,
        // so a connected switchboard reports the hub's version, not its own).
        (Method::Get, "/api/health") => json_reply(Ok(json!({
            "version": env!("CARGO_PKG_VERSION"),
            "pid": std::process::id(),
        }))),
        // "Open on your phone" — answered by the HUB: proxied to the connected
        // remote (whose PhoneControl runs tailscale on ITS machine and serves
        // ITS port), or handled here when Local. The stable server is what the
        // phone reaches; this client machine is never a relay.
        (Method::Get, "/api/phone/status") => match &ctx.phone {
            Some(p) => json_reply(Ok(p.status())),
            None => phone_unavailable(),
        },
        (Method::Post, "/api/phone/enable") => match &ctx.phone {
            Some(p) => json_reply(Ok(p.enable())),
            None => phone_unavailable(),
        },
        (Method::Post, "/api/phone/disable") => match &ctx.phone {
            Some(p) => json_reply(Ok(p.disable())),
            None => phone_unavailable(),
        },
        (Method::Post, "/api/phone/login") => match &ctx.phone {
            Some(p) => json_reply(Ok(p.login())),
            None => phone_unavailable(),
        },
        // Web Push — deliberately NOT pinned local (unlike /api/notify*): when a
        // remote hub is connected these proxy to it, so subscriptions live next
        // to the watcher that fires them. The phone reaches these through the
        // tailscale-served origin like any /api route.
        #[cfg(feature = "local-backend")]
        (Method::Get, "/api/push/key") => json_reply(push::public_key().map(|k| json!({ "key": k }))),
        #[cfg(feature = "local-backend")]
        (Method::Post, "/api/push/subscribe") => {
            let key = |k: &str| {
                v.get("keys").and_then(|x| x.get(k)).and_then(|x| x.as_str()).unwrap_or("").to_string()
            };
            let sub = push::Subscription { endpoint: s("endpoint"), p256dh: key("p256dh"), auth: key("auth") };
            json_reply(push::add_subscription(sub).map(|n| json!({ "ok": true, "count": n })))
        }
        #[cfg(feature = "local-backend")]
        (Method::Post, "/api/push/unsubscribe") => {
            json_reply(push::remove_subscription(&s("endpoint")).map(|n| json!({ "ok": true, "count": n })))
        }
        #[cfg(feature = "local-backend")]
        (Method::Post, "/api/push/attention") => {
            push::set_attention(&s("client"), v.get("focused").and_then(|x| x.as_bool()).unwrap_or(false));
            json_reply(Ok(json!({ "ok": true })))
        }
        // Native notifications — pinned LOCAL by worker_loop (never proxied): the
        // toast/badge belongs to this machine's screen. No notifier (standalone
        // binary, browser mode) → 404, which the SPA reads as "fall back to the
        // Web Notification API".
        (Method::Get, "/api/notify/status") => match &ctx.notifier {
            Some(_) => json_reply(Ok(json!({ "native": true }))),
            None => notify_unavailable(),
        },
        (Method::Post, "/api/notify") => match &ctx.notifier {
            Some(n) => json_reply(n.notify(&s("title"), &s("body")).map(|_| json!({ "ok": true }))),
            None => notify_unavailable(),
        },
        (Method::Post, "/api/notify/prime") => match &ctx.notifier {
            Some(n) => {
                n.prime();
                json_reply(Ok(json!({ "ok": true })))
            }
            None => notify_unavailable(),
        },
        (Method::Post, "/api/notify/badge") => match &ctx.notifier {
            Some(n) => {
                n.set_badge(v.get("count").and_then(|x| x.as_u64()).unwrap_or(0).min(9999) as u32);
                json_reply(Ok(json!({ "ok": true })))
            }
            None => notify_unavailable(),
        },
        // Open a session's folder in VS Code. The control lives in the desktop shell
        // (client-side); pinned local + gated on from_this_machine by the dispatch
        // loop above (never proxied), so it opens on the screen the user is at.
        // `status` drives the button's visibility. `None` (standalone/browser) 404s.
        (Method::Get, "/api/editor/status") => match &ctx.editor {
            Some(ed) => {
                let name = ed.detect();
                json_reply(Ok(json!({ "available": name.is_some(), "name": name })))
            }
            None => editor_unavailable(),
        },
        (Method::Post, "/api/editor/open") => match &ctx.editor {
            Some(ed) => {
                // A connected remote → the folder lives there; open it over Remote-SSH
                // rather than on this machine. The ssh destination comes from the
                // switchboard (authoritative + already validated), NEVER the request
                // body, so a caller can't smuggle ssh options in via a fake host.
                let host = ctx
                    .remote
                    .as_ref()
                    .filter(|r| r.active_target().is_some())
                    .and_then(|r| r.status().host);
                json_reply(ed.open(&s("path"), host.as_deref()).map(|()| json!({ "ok": true })))
            }
            None => editor_unavailable(),
        },
        // Reveal a saved file in the OS file manager (backs "Reveal in folder" on
        // the export confirmation). Pinned local + gated on from_this_machine by
        // the dispatch loop, so the window opens on the screen the user is at.
        (Method::Post, "/api/reveal") => {
            json_reply(reveal::reveal(&s("path")).map(|()| json!({ "ok": true })))
        }
        // Unknown /api routes must not fall through to the SPA fallback — a JSON
        // client probing an endpoint would get index.html with a 200.
        (_, p) if p.starts_with("/api/") => Reply {
            status: 404,
            body: serde_json::to_vec(&json!({ "error": format!("unknown API route {p}") }))
                .unwrap_or_default(),
            content_type: "application/json".into(),
            extra: vec![],
        },
        (Method::Get, _) => serve_static(&ctx.dist, path),
        _ => Reply {
            status: 404,
            body: serde_json::to_vec(&json!({ "error": "Not found" })).unwrap_or_default(),
            content_type: "application/json".into(),
            extra: vec![],
        },
    }
}

// ───────────────── terminal output streaming (Server-Sent Events) ─────────────────
// We take over the raw socket (`into_writer`) and hand-roll a chunked
// `text/event-stream`: PTY output → `data: <base64>\n\n` frames, each flushed
// immediately. (tiny_http's normal `respond` streams a reader via a
// never-returning `io::copy` and only flushes at the very end, so it can't do
// SSE.) Browser input/resize ride the POST routes above. Holding the
// `Attachment` Arc keeps the tmux-attach client alive; returning from this fn
// drops it → detaches, leaving the tmux session running (nohup w.r.t. the UI).

/// Write one HTTP/1.1 chunk and flush it to the socket immediately.
pub(crate) fn write_chunk(w: &mut dyn Write, frame: &[u8]) -> std::io::Result<()> {
    write!(w, "{:x}\r\n", frame.len())?;
    w.write_all(frame)?;
    w.write_all(b"\r\n")?;
    w.flush()
}

// Bound concurrent attach streams so a flood of `/api/terminal/attach` requests
// can't spawn unbounded threads. Terminals are few in practice; this is a backstop.
static ACTIVE_STREAMS: AtomicUsize = AtomicUsize::new(0);
const MAX_STREAMS: usize = 256;

/// RAII guard for one streaming slot; releases the count on drop (every exit path).
pub(crate) struct StreamSlot;
impl Drop for StreamSlot {
    fn drop(&mut self) {
        ACTIVE_STREAMS.fetch_sub(1, Ordering::Relaxed);
    }
}
/// Reserve a streaming slot, or `None` if `MAX_STREAMS` are already open. Shared by the
/// local terminal stream and the proxied (remote) one so the cap covers both paths.
pub(crate) fn acquire_stream_slot() -> Option<StreamSlot> {
    if ACTIVE_STREAMS.fetch_add(1, Ordering::Relaxed) >= MAX_STREAMS {
        ACTIVE_STREAMS.fetch_sub(1, Ordering::Relaxed);
        None
    } else {
        Some(StreamSlot)
    }
}

pub(crate) fn reply_status(request: Request, status: u16, error: &str) {
    // Status-only replies (401/404/503/proxy 502, …) bypass `send_reply`, so log
    // them here too — `error` is already the human detail, no body parse needed.
    if status >= 400 {
        log::warn!("{} {} -> {} {}", request.method().as_str(), request.url(), status, error);
    } else {
        log::debug!("{} {} -> {}", request.method().as_str(), request.url(), status);
    }
    let body = serde_json::to_vec(&json!({ "error": error })).unwrap_or_default();
    let mut resp = Response::from_data(body).with_status_code(StatusCode(status));
    let origin = header_value(&request, "Origin");
    let mut headers: Vec<(&str, &str)> = vec![("Content-Type", "application/json")];
    if let Some(origin) = origin.as_deref().filter(|o| is_loopback_origin(o)) {
        headers.push(("Access-Control-Allow-Origin", origin));
        headers.push(("Vary", "Origin"));
    }
    for (k, val) in headers {
        if let Ok(h) = Header::from_bytes(k.as_bytes(), val.as_bytes()) {
            resp.add_header(h);
        }
    }
    let _ = request.respond(resp);
}

/// Response head for the hand-rolled SSE paths (`stream_terminal` / `proxy_sse`
/// take over the socket, bypassing `send_reply`) — same CORS policy as there:
/// echo loopback dev origins only, so a foreign website can never read a
/// terminal stream cross-origin.
pub(crate) fn sse_head(request: &Request) -> String {
    let cors = header_value(request, "Origin")
        .filter(|o| is_loopback_origin(o))
        .map(|o| format!("Access-Control-Allow-Origin: {o}\r\nVary: Origin\r\n"))
        .unwrap_or_default();
    format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: text/event-stream\r\n\
         Cache-Control: no-store\r\n\
         Transfer-Encoding: chunked\r\n\
         {cors}X-Accel-Buffering: no\r\n\r\n"
    )
}

/// Handle `GET /api/terminal/attach?id=&cols=&rows=` on its own thread (it blocks
/// for the session's lifetime, so it must not occupy a pooled worker). Local-backend
/// only — a switchboard proxies the attach SSE to the remote (`proxy::proxy_sse`).
#[cfg(feature = "local-backend")]
fn stream_terminal(request: Request, url: &str) {
    let _slot = match acquire_stream_slot() {
        Some(s) => s,
        None => return reply_status(request, 503, "Too many terminal streams are open."),
    };

    let id = query_param(url, "id").unwrap_or_default();
    let cols = query_param(url, "cols").and_then(|s| s.parse().ok()).unwrap_or(80u16);
    let rows = query_param(url, "rows").and_then(|s| s.parse().ok()).unwrap_or(24u16);

    let (att, rx) = match skill_term::attach(&id, cols, rows) {
        Ok(pair) => pair,
        Err(e) => return reply_status(request, 400, &e),
    };

    let head = sse_head(&request);
    let mut w = request.into_writer();
    if w.write_all(head.as_bytes()).is_err() || w.flush().is_err() {
        return; // drops `att` → detaches
    }

    use std::sync::mpsc::RecvTimeoutError;
    loop {
        // The 15s keepalive comment doubles as a disconnect probe: the write
        // fails once the client is gone, so we stop and detach.
        let frame = match rx.recv_timeout(std::time::Duration::from_secs(15)) {
            Ok(bytes) => format!("data: {}\n\n", skill_term::b64_encode(&bytes)),
            Err(RecvTimeoutError::Timeout) => ": ping\n\n".to_string(),
            Err(RecvTimeoutError::Disconnected) => break, // PTY closed
        };
        if write_chunk(w.as_mut(), frame.as_bytes()).is_err() {
            break; // client gone
        }
    }
    let _ = write_chunk(w.as_mut(), b""); // terminating 0-length chunk
    drop(att); // detach (the tmux session keeps running)
}

/// Handle `GET /api/events` on its own thread (it blocks for the subscription's
/// lifetime). Local-backend only — a connected switchboard proxies this stream
/// to the remote (whose terminals are the ones on screen), like the attach SSE.
#[cfg(feature = "local-backend")]
fn stream_events(request: Request) {
    let _slot = match acquire_stream_slot() {
        Some(s) => s,
        None => return reply_status(request, 503, "Too many event streams are open."),
    };
    let rx = events::subscribe();
    let head = sse_head(&request);
    let mut w = request.into_writer();
    if w.write_all(head.as_bytes()).is_err() || w.flush().is_err() {
        return;
    }
    use std::sync::mpsc::RecvTimeoutError;
    loop {
        let frame = match rx.recv_timeout(std::time::Duration::from_secs(15)) {
            Ok(f) => f,
            Err(RecvTimeoutError::Timeout) => ": ping\n\n".to_string(),
            Err(RecvTimeoutError::Disconnected) => break, // registry pruned us
        };
        if write_chunk(w.as_mut(), frame.as_bytes()).is_err() {
            break; // client gone — dropping rx lets the next emit prune our sender
        }
    }
    let _ = write_chunk(w.as_mut(), b"");
}
