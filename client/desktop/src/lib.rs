// Tauri shell — a thin CLIENT. It brings up `skill-server` in-process on a
// loopback port and points the webview at it, so the shell runs the EXACT same
// HTTP path as a browser or a remote host. There are no `#[tauri::command]`s:
// every capability is reached over `/api` (see `server/skill-server`). Two
// shapes from one crate (split by the target tables in Cargo.toml):
//
//   * **Desktop** — an in-process HTTP switchboard owns the UI and native
//     capabilities. A detached per-user host service owns the local backend,
//     agents, connector gateway and phone access. Closing or quitting the client
//     drops only its SSH tunnels; the host service and tmux sessions survive.
//   * **Mobile (iOS)** — a pure switchboard: the same loopback server, but with
//     no local backend; everything happens on the SSH remote it connects to via
//     the in-process russh transport, with credentials from the Keychain-backed
//     [`securestore`].
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};
#[cfg(desktop)]
use tauri::menu::{MenuBuilder, MenuItemBuilder};
#[cfg(desktop)]
use tauri::tray::TrayIconBuilder;
#[cfg(any(desktop, target_os = "ios"))]
use tauri_plugin_notification::NotificationExt;
#[cfg(desktop)]
use tauri_plugin_updater::UpdaterExt;

use skill_server::{init_logging, init_logging_to_file, ServerConfig, SshRemoteControl};

#[cfg(desktop)]
mod editor; // ShellEditor: the "Open in VS Code" control (client-side, pinned-local route)
#[cfg(any(desktop, target_os = "ios"))]
mod sound;
#[cfg(desktop)]
mod host;
// KeychainStore: the mobile switchboard's SSH credential store. Compiled on
// macOS too (same Security.framework path) so its tests run on a Mac; only the
// iOS setup path actually wires it in, hence the desktop dead_code allowance.
#[cfg(any(target_os = "macos", target_os = "ios"))]
#[cfg_attr(desktop, allow(dead_code))]
mod securestore;
#[cfg(target_os = "ios")]
mod app_lock;

/// Locate the bundled `llama-server` so the on-device commit-message generator
/// runs with zero setup. Checks the production bundle (resource dir) then the
/// dev-vendored tree (`client/desktop/binaries/<triple>/`, populated by
/// `scripts/fetch-engine.sh`). Returns the first match.
#[cfg(desktop)]
fn find_bundled_engine(app: &tauri::App) -> Option<std::path::PathBuf> {
    let exe = if cfg!(windows) { "llama-server.exe" } else { "llama-server" };
    let look = |base: std::path::PathBuf| -> Option<std::path::PathBuf> {
        // base/<triple>/<exe> (one platform subdir), or base/<exe> directly.
        if let Ok(entries) = std::fs::read_dir(&base) {
            for e in entries.flatten() {
                let c = e.path().join(exe);
                if c.is_file() {
                    return Some(c);
                }
            }
        }
        let direct = base.join(exe);
        direct.is_file().then_some(direct)
    };
    let resource = app.path().resource_dir().ok().map(|r| r.join("binaries"));
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("binaries");
    // Release: the bundled resource copy. Dev: the repo SOURCE first — Tauri re-copies
    // `binaries` into the target resource dir on rebuilds, and running the warm engine
    // from the source (not the copy) keeps that copy free to overwrite (else ETXTBSY).
    let order: Vec<std::path::PathBuf> = if cfg!(debug_assertions) {
        std::iter::once(source).chain(resource).collect()
    } else {
        resource.into_iter().chain(std::iter::once(source)).collect()
    };
    order.into_iter().find_map(look)
}

/// The shell's half of `skill_core::update`: the server module owns the
/// `/api/update/*` surface and the version check; only this process can replace
/// its own binary, so download/install runs here via `tauri-plugin-updater`.
#[cfg(desktop)]
struct ShellUpdater {
    app: tauri::AppHandle,
    remote_slot: std::sync::Arc<std::sync::OnceLock<std::sync::Arc<SshRemoteControl>>>,
    local_host: std::sync::Arc<host::LocalHost>,
}

#[cfg(desktop)]
impl skill_core::update::UpdateControl for ShellUpdater {
    fn can_install(&self) -> bool {
        // The plugin installs every target we ship (dmg-app, NSIS, deb/AppImage);
        // failures surface via report_error and the UI offers a manual download.
        true
    }

    fn begin_install(&self) {
        let app = self.app.clone();
        let remote_slot = self.remote_slot.clone();
        let local_host = self.local_host.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(msg) = install_update(app, remote_slot, local_host).await {
                skill_core::update::report_error(msg);
            }
        });
    }
}

/// The shell's half of `skill_server::NotifyControl`: the SPA decides WHEN a
/// agent transition deserves a toast or sound (it owns focus + seen state) and posts to the
/// pinned-local `/api/notify*` routes; only this process can talk to the OS
/// notification center/audio, so native delivery runs here.
#[cfg(any(desktop, target_os = "ios"))]
struct ShellNotifier {
    app: tauri::AppHandle,
}

#[cfg(any(desktop, target_os = "ios"))]
impl skill_server::NotifyControl for ShellNotifier {
    fn notify_while_visible(&self) -> bool {
        cfg!(target_os = "ios")
    }

    fn notify(&self, title: &str, body: &str) -> Result<(), String> {
        self.app
            .notification()
            .builder()
            .title(title)
            .body(body)
            .show()
            .map_err(|e| e.to_string())
    }

    fn prime(&self) {
        // Permission prompts can block until answered — never on a server worker.
        let app = self.app.clone();
        std::thread::spawn(move || {
            if let Err(e) = app.notification().request_permission() {
                log::warn!("notification permission request failed: {e}");
            }
        });
    }

    fn sound(&self, sound: skill_server::NotificationSound) -> Result<bool, String> {
        sound::play(sound).map(|()| true)
    }

    fn set_badge(&self, count: u32) {
        // Dock badge (macOS) / launcher count (some Linux DEs). Windows has no
        // numeric badge — the Err is deliberately dropped (quiet degradation).
        // iOS app-icon badges go through UNUserNotificationCenter rather than the
        // window API, and aren't wired yet (the toast is the signal that matters),
        // so it's a no-op there.
        #[cfg(desktop)]
        if let Some(w) = self.app.get_webview_window("main") {
            let _ = w.set_badge_count((count > 0).then_some(count as i64));
        }
        #[cfg(not(desktop))]
        let _ = count;
    }
}

/// Download → install → relaunch. On Windows the plugin hands off to the NSIS
/// installer and exits this process itself — `RunEvent::Exit` never fires — so
/// `on_before_exit` must repeat tunnel teardown. Stop the host only after a verified
/// download, before replacing its executable/resources (required on Windows).
/// macOS/Linux installs return, and we restart explicitly. tmux agents survive.
#[cfg(desktop)]
async fn install_update(
    app: tauri::AppHandle,
    remote_slot: std::sync::Arc<std::sync::OnceLock<std::sync::Arc<SshRemoteControl>>>,
    local_host: std::sync::Arc<host::LocalHost>,
) -> Result<(), String> {
    let updater = app
        .updater_builder()
        .on_before_exit(move || {
            if let Some(r) = remote_slot.get() {
                r.shutdown();
            }
        })
        .build()
        .map_err(|e| format!("The updater could not start: {e}"))?;
    let update = updater
        .check()
        .await
        .map_err(|e| format!("Couldn't check for the update: {e}"))?
        .ok_or_else(|| "The update is no longer available.".to_string())?;
    let mut received: u64 = 0;
    let bytes = update
        .download(
            move |chunk, total| {
                received += chunk as u64;
                let pct = total.filter(|t| *t > 0).map(|t| (received * 100 / t).min(100) as u8);
                skill_core::update::report_progress(pct);
            },
            // The plugin invokes this before verifying the package signature.
            // Only report ready after download() returns verified bytes.
            || {},
        )
        .await
        .map_err(|e| format!("Couldn't download the update: {e}"))?;
    skill_core::update::report_ready();
    let stop_started = std::time::Instant::now();
    let stopping = local_host.clone();
    let stopped = tauri::async_runtime::spawn_blocking(move || stopping.stop())
        .await
        .map_err(|e| e.to_string())
        .and_then(|result| result);
    if stop_started.elapsed() > std::time::Duration::from_secs(2) {
        log::warn!("Stopping the local host for the update took {:?}", stop_started.elapsed());
    }
    if let Err(error) = stopped {
        // Stop may have succeeded before its acknowledgement was lost. Clear
        // any durable stop intent and restore the host when aborting installation.
        return Err(restore_after_failed_update(local_host, format!("Couldn't stop the host for the update: {error}")).await);
    }
    // Extraction, platform installer startup and tunnel cleanup are synchronous.
    // Keep them off the async executor that drives the rest of the desktop.
    let installed = tauri::async_runtime::spawn_blocking(move || update.install(bytes).map_err(|error| error.to_string()))
        .await
        .map_err(|error| error.to_string())
        .and_then(|result| result);
    if let Err(error) = installed {
        return Err(restore_after_failed_update(local_host, format!("Couldn't install the update: {error}")).await);
    }
    // macOS/Linux: request event-loop shutdown without parking an async worker
    // indefinitely. Windows has already handed off to NSIS and exited above.
    app.request_restart();
    Ok(())
}

#[cfg(desktop)]
async fn restore_after_failed_update(local_host: std::sync::Arc<host::LocalHost>, message: String) -> String {
    match tauri::async_runtime::spawn_blocking(move || local_host.resume_after_failed_update()).await {
        Ok(Ok(())) => message,
        Ok(Err(error)) => format!("{message} The host could not restart: {error}"),
        Err(error) => format!("{message} The host restart task failed: {error}"),
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // The same shipped executable also hosts the background worker. Handle
    // this before initializing Tauri, its single-instance plugin or any window.
    #[cfg(desktop)]
    if let Some(result) = skill_server::host_service::run_from_args(&std::env::args().collect::<Vec<_>>()) {
        if let Err(error) = result {
            eprintln!("host service: {error}");
            std::process::exit(1);
        }
        return;
    }
    // The SSH connection manager is created in `setup` (it needs the app version to
    // provision the matching remote `skill-server`); this slot hands it to the exit
    // handler so this client's tunnels are closed on quit.
    let remote_slot: std::sync::Arc<std::sync::OnceLock<std::sync::Arc<SshRemoteControl>>> =
        std::sync::Arc::new(std::sync::OnceLock::new());
    let remote_slot_setup = remote_slot.clone();
    // iOS: the loopback server plus the means to respawn it — see [`LocalServer`].
    #[cfg(target_os = "ios")]
    let local_slot: std::sync::Arc<std::sync::OnceLock<std::sync::Arc<LocalServer>>> =
        std::sync::Arc::new(std::sync::OnceLock::new());
    #[cfg(target_os = "ios")]
    let local_slot_setup = local_slot.clone();
    #[allow(unused_mut)]
    let mut builder = tauri::Builder::default();
    // Single instance FIRST, so a second launch short-circuits before it spawns a
    // rival server or tray. This is the fix for the update seam: `app.restart()`
    // releases the lock (cleanup_before_exit → RunEvent::Exit → the plugin's
    // destroy) before exec, so the new build acquires it cleanly — but if the old
    // process lingers (a slow-exiting tray, or a manual relaunch of a hidden
    // window), the newcomer forwards its argv here and exits instead of adding a
    // second tray. The detached host service has its own singleton lock.
    // Release-only: dev shares the bundle id, so the guard would otherwise send
    // `npm run dev` straight to the installed tray app.
    #[cfg(all(desktop, not(debug_assertions)))]
    {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            show_main(app); // surface the already-running window (un-hides a tray-hidden one)
        }));
    }
    #[cfg(desktop)]
    {
        builder = builder.plugin(tauri_plugin_updater::Builder::new().build()); // self-update; driven from ShellUpdater, no JS API
    }
    // Notifications on desktop AND iOS (agent turn-finish toasts); driven from
    // ShellNotifier via the pinned-local /api/notify* routes, no JS API.
    #[cfg(any(desktop, target_os = "ios"))]
    {
        builder = builder.plugin(tauri_plugin_notification::init());
    }
    builder
        .setup(move |app| {
            #[cfg(desktop)]
            setup_desktop(app, &remote_slot_setup)?;
            #[cfg(target_os = "ios")]
            setup_mobile(app, &remote_slot_setup, &local_slot_setup)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            // Desktop: close = hide to tray; the tray's Quit is how you actually
            // leave. (Mobile has no window close — the OS suspends the app.)
            #[cfg(desktop)]
            if window.label() == "main" {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
            #[cfg(not(desktop))]
            let _ = (window, event);
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        // `_app` is used only by the macOS Reopen arm; the underscore keeps it
        // warning-free on the other platforms where that arm is compiled out.
        .run(move |_app, event| {
            match event {
                // Client exit drops this accessor's tunnels. Local and remote
                // host services, their gateways and agents remain available.
                tauri::RunEvent::Exit => {
                    if let Some(r) = remote_slot.get() {
                        r.shutdown();
                    }
                }
                // macOS: clicking the dock icon with the window hidden re-shows it.
                #[cfg(target_os = "macos")]
                tauri::RunEvent::Reopen { .. } => show_main(_app),
                // iOS tears the SSH tunnel down within minutes of the app
                // backgrounding — on return to foreground, reconnect to the
                // remembered host (a no-op if the tunnel actually survived; a
                // reattach, not a relaunch, if the remote server kept running).
                // Foregrounding arrives as a WINDOW event: tao turns
                // applicationWillEnterForeground into Event::Resumed, which
                // tauri-runtime-wry (mobile) forwards as WindowEvent::Resumed.
                // tauri::RunEvent::Resumed is never produced on iOS — it only
                // arises from ControlFlow::Poll, which the runner never uses.
                #[cfg(target_os = "ios")]
                tauri::RunEvent::WindowEvent {
                    event: tauri::WindowEvent::Resumed,
                    ..
                } => {
                    app_lock::refresh();
                    // The switchboard itself first: iOS may have reclaimed the
                    // loopback listener during the suspension, and with it gone
                    // nothing else is reachable — the tunnel check included.
                    if let Some(ls) = local_slot.get() {
                        ls.heal(_app.clone());
                    }
                    // The native lock restores/checks SSH access only after
                    // authentication or a return within its one-minute grace.
                }
                _ => {}
            }
        });
}

/// Desktop setup: durable local backend + thin tray-resident client.
#[cfg(desktop)]
fn setup_desktop(
    app: &tauri::App,
    remote_slot: &std::sync::Arc<std::sync::OnceLock<std::sync::Arc<SshRemoteControl>>>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Logger first, so even early startup is captured. Tee to a small on-disk
    // file (durable for the packaged app, where stderr goes nowhere) + stderr
    // (so `npm run dev` still shows logs). The in-process server shares this
    // process, so its `log::*` records land here too. RUST_LOG-gated; quiet by
    // default. Frontend warns/errors arrive via POST /api/logs/client.
    let log_path = app.path().app_log_dir().ok().map(|d| d.join("vibestudio.log"));
    match &log_path {
        Some(p) => init_logging_to_file(p),
        None => init_logging(),
    }
    if let Some(p) = &log_path {
        log::info!("on-disk log: {}", p.display());
    }

    // Point the on-device generator at the bundled/vendored llama-server so
    // it works with no config; an explicit env override still wins. The
    // detached worker inherits the env var.
    if std::env::var_os("VIBESTUDIO_LLAMA_SERVER").is_none() {
        if let Some(p) = find_bundled_engine(app) {
            std::env::set_var("VIBESTUDIO_LLAMA_SERVER", p);
        }
    }

    // SSH connection manager: provisions the release-matching `skill-server`
    // onto remotes, using the app's Cargo package version stamped from the release tag.
    let remote = std::sync::Arc::new(SshRemoteControl::new(
        app.package_info().version.to_string(),
    ));
    let _ = remote_slot.set(remote.clone());

    // ── bring up the loopback backend and point the webview at it ──
    let resource_dir = app.path().resource_dir().ok();
    let dist = resource_dir
        .clone()
        .map(|r| r.join("dist"))
        .unwrap_or_else(|| std::path::PathBuf::from("dist"));
    let dist = if dist.is_absolute() { dist } else { std::env::current_dir()?.join(dist) };
    // The worker outlives this process, serves the phone directly, and owns
    // terminals, discovery, attention watching, connectors and engine upkeep.
    let local_host = host::LocalHost::start(skill_server::host_service::HostServiceOptions {
        executable: std::env::current_exe()?,
        dist: dist.clone(),
        bundled_skills: resource_dir.clone().map(|r| r.join("skills")),
        examples_base: resource_dir.clone(),
        preferred_port: if tauri::is_dev() { 8766 } else { skill_server::PHONE_PORT },
        version: app.package_info().version.to_string(),
        ..Default::default()
    }).map_err(std::io::Error::other)?;
    let updater = std::sync::Arc::new(ShellUpdater {
        app: app.handle().clone(),
        remote_slot: remote_slot.clone(),
        local_host: local_host.clone(),
    }) as std::sync::Arc<dyn skill_core::update::UpdateControl>;
    let notifier = std::sync::Arc::new(ShellNotifier { app: app.handle().clone() })
        as std::sync::Arc<dyn skill_server::NotifyControl>;
    // "Open in VS Code" acts on this machine's screen — a client concern, so
    // it lives in the shell (see editor.rs), reached over the pinned-local route.
    let editor = std::sync::Arc::new(editor::ShellEditor)
        as std::sync::Arc<dyn skill_server::EditorControl>;
    let make_cfg = |port: u16| ServerConfig {
        host: "127.0.0.1".into(),
        port,
        dist: dist.clone(),
        bundled_skills: resource_dir.clone().map(|r| r.join("skills")),
        examples_base: resource_dir.clone(), // resolve bundled examples by relative path
        startup_maintenance: false,
        // Plug the SSH connection manager into the local switchboard.
        remote: Some(remote.clone() as std::sync::Arc<dyn skill_server::RemoteControl>),
        local_backend: Some(local_host.clone() as std::sync::Arc<dyn skill_server::LocalBackendControl>),
        // Hand the server's update module its installer (see ShellUpdater).
        updater: Some(updater.clone()),
        // OS toasts + dock badge for the SPA's turn-finish notifier.
        notifier: Some(notifier.clone()),
        // "Open in VS Code" on this machine (or the remote over Remote-SSH).
        editor: Some(editor.clone()),
        ..Default::default()
    };
    // Only the dev proxy needs a fixed client port. The phone uses the worker's
    // persistent port; production webviews get their own ephemeral switchboard.
    let preferred = std::env::var("VIBESTUDIO_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(if tauri::is_dev() { 8767 } else { 0 });
    let port = skill_server::spawn(make_cfg(preferred))?.addr.port();

    // Same-origin model: the webview's origin IS the server, so api.ts's
    // relative `/api` calls + the SSE EventSource pass CSP `default-src 'self'`.
    let url = if tauri::is_dev() {
        "http://localhost:1420".to_string() // native-mode Vite proxies /api → 8767
    } else {
        format!("http://127.0.0.1:{port}") // the in-process server serves UI + /api
    };
    WebviewWindowBuilder::new(app.handle(), "main", WebviewUrl::External(url.parse().unwrap()))
        .title("VibeStudio")
        .inner_size(1200.0, 800.0)
        .min_inner_size(720.0, 480.0)
        // Off by default in wry; enables Cmd/Ctrl +/-/0 whole-window zoom
        // (native page zoom on Windows, an injected CSS-zoom polyfill on
        // macOS/Linux). The terminal's DOM renderer stays crisp under it.
        .zoom_hotkeys_enabled(true)
        // `target="_blank"` links (release page, GitHub device flow) must
        // open in the SYSTEM browser — wry's default silently drops them.
        .on_new_window(|url, _features| {
            if matches!(url.scheme(), "http" | "https") {
                if let Err(e) = open::that_detached(url.as_str()) {
                    log::warn!("couldn't open {url} in the system browser: {e}");
                }
            }
            tauri::webview::NewWindowResponse::Deny
        })
        .build()?;

    // Closing or quitting the client leaves the host and agents available.
    let open_item = MenuItemBuilder::with_id("open", "Open VibeStudio").build(app)?;
    let phone_item = MenuItemBuilder::with_id("phone", "Open on your phone…").build(app)?;
    let quit_item = MenuItemBuilder::with_id("quit", "Quit VibeStudio").build(app)?;
    let stop_item = MenuItemBuilder::with_id("stop-host", "Stop local host service and quit").build(app)?;
    let menu = MenuBuilder::new(app)
        .item(&open_item)
        .item(&phone_item)
        .separator()
        .item(&stop_item)
        .item(&quit_item)
        .build()?;
    let remote_for_tray = remote.clone();
    let mut tray = TrayIconBuilder::with_id("main-tray")
        .tooltip("VibeStudio")
        .menu(&menu)
        .on_menu_event(move |app, event| match event.id().as_ref() {
            "open" => show_main(app),
            "phone" => {
                show_main(app);
                // The SPA opens the phone modal when it sees this param
                // (and strips it) — see RemoteMenu.tsx.
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.eval("window.location.hash = '#/?phone=1'");
                }
            }
            "quit" => {
                remote_for_tray.shutdown();
                app.exit(0);
            }
            "stop-host" => {
                let host = local_host.clone();
                let app = app.clone();
                std::thread::spawn(move || match host.stop() {
                    Ok(()) => app.exit(0),
                    Err(error) => log::error!("could not stop local host service: {error}"),
                });
            }
            _ => {}
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

/// iOS: the in-process loopback server plus the means to bring it back. When the
/// app is suspended, iOS may reclaim its sockets; tiny_http's single accept
/// thread exits for good on the first accept error, so a reclaimed listener
/// means the server is dead for the rest of the process — every webview fetch
/// fails ("Load failed") until relaunch. The workers are detached with nothing
/// to join or reuse; recovery is a fresh [`skill_server::spawn`] on foreground.
#[cfg(target_os = "ios")]
struct LocalServer {
    /// The currently bound port — the webview's origin embeds it.
    port: std::sync::atomic::AtomicU16,
    /// A heal is already probing/respawning; don't stack another.
    healing: std::sync::atomic::AtomicBool,
    /// Spawn a fresh server on the given port (0 = ephemeral); returns the bound port.
    respawn: Box<dyn Fn(u16) -> std::io::Result<u16> + Send + Sync>,
}

#[cfg(target_os = "ios")]
impl LocalServer {
    /// Foreground check, off-thread: probe the listener, and if iOS reclaimed it
    /// respawn — on the SAME port first, so the loaded SPA (whose origin is
    /// frozen at launch) keeps working with all its state; on a fresh port with
    /// a webview reload only if the old bind lingers.
    fn heal(self: &std::sync::Arc<Self>, app: tauri::AppHandle) {
        use std::sync::atomic::Ordering;
        if self.healing.swap(true, Ordering::SeqCst) {
            return;
        }
        let me = self.clone();
        std::thread::spawn(move || {
            let port = me.port.load(Ordering::SeqCst);
            if !skill_server::loopback_alive(port) {
                log::warn!("loopback server on {port} is gone after resume; respawning");
                match (me.respawn)(port).or_else(|_| (me.respawn)(0)) {
                    Ok(p) if p == port => log::info!("loopback server rebound on {p}"),
                    Ok(p) => {
                        me.port.store(p, Ordering::SeqCst);
                        log::warn!("loopback server moved to {p}; reloading the webview");
                        let _ = app.run_on_main_thread(move || {
                            // Keep the route, and defer loading until native
                            // unlock if this port moved behind the privacy cover.
                            app_lock::listener_changed();
                        });
                    }
                    Err(e) => log::error!("couldn't respawn the loopback server: {e}"),
                }
            }
            me.healing.store(false, Ordering::SeqCst);
        });
    }
}

/// Mobile setup (iOS): the pure switchboard. Same loopback server, no local backend
/// — no terminals/engine (`startup_maintenance` stays false and the local-backend
/// feature is compiled out), no tray, no updater (the App Store owns updates),
/// no phone hub (this IS the phone). Credentials come from the Keychain-backed
/// [`securestore::KeychainStore`]; connects run over the in-process russh
/// transport (see `skill-server`'s `russh-transport` feature).
#[cfg(target_os = "ios")]
fn setup_mobile(
    app: &tauri::App,
    remote_slot: &std::sync::Arc<std::sync::OnceLock<std::sync::Arc<SshRemoteControl>>>,
    local_slot: &std::sync::Arc<std::sync::OnceLock<std::sync::Arc<LocalServer>>>,
) -> Result<(), Box<dyn std::error::Error>> {
    // The Apple entry point owns the delegate class. Resolve it at runtime:
    // Rust also emits a cdylib, before Xcode links the entry point's object file.
    let notification_delegate = objc2::runtime::AnyClass::get(c"VibeStudioNotificationDelegate")
        .ok_or_else(|| std::io::Error::other("native notification delegate is unavailable"))?;
    // SAFETY: this class implements +install. Tauri initializes its plugins
    // before setup, which runs on the main thread during app launch.
    // Own tap handling so notifications from a previous process cannot trigger
    // the plugin's in-memory metadata lookup. Scheduling/permissions stay there.
    unsafe {
        let _: () = objc2::msg_send![notification_delegate, install];
    }

    // FIRST, before anything reads config: pin skill-core's config dir to the app
    // sandbox. skill-core resolves it from `~/.config` via `dirs::home_dir()`, but
    // an iOS app process has no `HOME` — `home_dir()` returns None there, so the
    // default resolution errors and `KeychainStore::new()` below (plus every other
    // config reader: last-host, known_hosts) would fail, panicking setup at launch.
    // Tauri resolves the OS-sandboxed dir correctly on device; hand it to skill-core.
    // (The Simulator hides this: it inherits the Mac's HOME, so the crash is
    // device-only — it's what took down the first TestFlight build.)
    if let Ok(cfg) = app.path().app_config_dir().or_else(|_| app.path().app_data_dir()) {
        let _ = std::fs::create_dir_all(&cfg);
        skill_core::paths::set_config_dir(cfg);
    }

    // Same durable logging as the desktop: on iOS stderr goes nowhere useful,
    // the app_log_dir file is what you'd pull from the device to debug.
    let log_path = app.path().app_log_dir().ok().map(|d| d.join("vibestudio.log"));
    match &log_path {
        Some(p) => init_logging_to_file(p),
        None => init_logging(),
    }

    // The credential store: connection profiles on disk (non-secret), private
    // keys in the iOS Keychain (or an app-private file store if the Keychain
    // won't open — resolve() degrades rather than crashing). Wired into BOTH
    // consumers — the server (for the /api/remote/profiles* routes the credential
    // UI drives) and the connection manager (so `connect(id)` resolves a saved
    // profile to russh credentials).
    let cfg_dir = app
        .path()
        .app_config_dir()
        .or_else(|_| app.path().app_data_dir())
        .map_err(std::io::Error::other)?;
    let store = skill_core::keystore::resolve(securestore::platform_native(), &cfg_dir)
        .map_err(std::io::Error::other)?;

    let remote = std::sync::Arc::new(SshRemoteControl::with_secure_store(
        app.package_info().version.to_string(),
        Some(store.clone()),
    ));
    let access = std::sync::Arc::new(skill_server::AppAccess::new_locked());
    remote.set_app_unlocked(false);
    let _ = remote_slot.set(remote.clone());

    let resource_dir = app.path().resource_dir().ok();
    let dist = resource_dir
        .clone()
        .map(|r| r.join("dist"))
        .unwrap_or_else(|| std::path::PathBuf::from("dist"));
    // A factory rather than a one-shot config: the same spawn must be repeatable
    // from LocalServer::heal after iOS reclaims the listener across a suspension.
    // Native notifications, the same control the desktop uses: turn-finish
    // events arrive from the remote hub over SSE, the SPA posts the pinned-local
    // /api/notify* routes, and this fires an iOS local notification on the phone
    // the user is holding. (Delivery while the app runs or is briefly
    // backgrounded; true closed-app push needs APNs — see the notifications
    // note in docs/plans.)
    let notifier = std::sync::Arc::new(ShellNotifier { app: app.handle().clone() })
        as std::sync::Arc<dyn skill_server::NotifyControl>;
    let make_config = {
        let bundled_skills = resource_dir.clone().map(|r| r.join("skills"));
        let remote = remote.clone() as std::sync::Arc<dyn skill_server::RemoteControl>;
        let access = access.clone();
        move |port: u16| ServerConfig {
            host: "127.0.0.1".into(),
            port, // 0 = ephemeral — nothing on the phone needs a stable port
            dist: dist.clone(),
            bundled_skills: bundled_skills.clone(),
            examples_base: resource_dir.clone(),
            startup_maintenance: false, // no local terminals/engine to maintain
            remote: Some(remote.clone()),
            app_access: Some(access.clone()),
            secure_store: Some(store.clone()),
            notifier: Some(notifier.clone()),
            ..Default::default()
        }
    };
    let handle = skill_server::spawn(make_config(0))?;
    let port = handle.addr.port();
    let local = std::sync::Arc::new(LocalServer {
        port: std::sync::atomic::AtomicU16::new(port),
        healing: std::sync::atomic::AtomicBool::new(false),
        respawn: Box::new(move |p| skill_server::spawn(make_config(p)).map(|h| h.addr.port())),
    });
    let _ = local_slot.set(local.clone());

    // Same-origin model as the desktop: the webview's origin IS the loopback
    // server (needs the ATS loopback exception in Info.plist).
    // Authentication owns initial navigation. No saved-host reconnect or data
    // fetch is allowed to run behind the native launch lock.
    WebviewWindowBuilder::new(app.handle(), "main", WebviewUrl::External("about:blank".parse().unwrap()))
        .build()?;
    app_lock::install(app, access, remote, local)?;
    Ok(())
}

/// Show + focus the main window (tray "Open", macOS dock reopen).
#[cfg(desktop)]
fn show_main(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}
