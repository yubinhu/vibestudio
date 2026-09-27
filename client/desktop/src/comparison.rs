//! Desktop rendering adapter for the HTTP comparison manager. All repository,
//! process and lifecycle work belongs to skill-core. These child webviews have
//! no Tauri capabilities; their only native bridge relays bounded scroll data
//! to the other preview in the same comparison window.
//!
//! `Window::add_child` is desktop-only and requires Tauri's `unstable` feature.
//! Verified against our lockfile's Tauri 2.11.2 / wry 0.55.1. The renderer is
//! WKWebView on macOS, WebView2 on Windows, and WebKitGTK on Linux.
//! Comparison requires macOS 11+ because native page zoom was added there:
//! https://docs.rs/tauri/2.11.2/tauri/webview/struct.Webview.html#method.set_zoom

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use skill_core::comparison::{
    ComparisonManager, ComparisonScrollRegistration, ComparisonScrollRelay, ComparisonSession,
    ComparisonState,
};
use tauri::webview::{NewWindowResponse, WebviewBuilder};
use tauri::window::WindowBuilder;
use tauri::{AppHandle, LogicalPosition, LogicalSize, Manager, Url, Webview, WebviewUrl, Window};

// Keep aligned with ComparisonPage's fixed toolbar height. The toolbar is its
// own native child; previews are never embedded in it or in HTML iframes.
const TOOLBAR_HEIGHT: f64 = 176.0;
const PADDING: f64 = 12.0;
// WebView2 normalizes host zoom to its supported range. Keep at least 25%,
// enlarging the minimum window instead of silently changing the CSS viewport.
// https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2controller#put_zoomfactor
const MIN_ZOOM: f64 = 0.25;
const SCROLL_SCRIPT: &str = include_str!("comparison-scroll.js");
const MAX_SCROLL_PAYLOAD: usize = 16_384;

pub struct ShellComparison {
    app: AppHandle,
    manager: ComparisonManager,
    scroll: ComparisonScrollRelay,
}

impl ShellComparison {
    pub fn new(app: AppHandle, manager: ComparisonManager, scroll: ComparisonScrollRelay) -> Self {
        Self {
            app,
            manager,
            scroll,
        }
    }

    /// Start once, after the switchboard has bound its toolbar origin. Polling
    /// keeps HTTP and backend workers independent of the platform event loop.
    pub fn start(self, toolbar_origin: String) {
        let windows = Arc::new(Mutex::new(BTreeMap::<String, NativeSession>::new()));
        let scheduled = Arc::new(AtomicBool::new(false));
        std::thread::spawn(move || loop {
            if !scheduled.swap(true, Ordering::AcqRel) {
                let snapshots = self.manager.list();
                let manager = self.manager.clone();
                let scroll = self.scroll.clone();
                let windows = windows.clone();
                let scheduled_task = scheduled.clone();
                let app = self.app.clone();
                let origin = toolbar_origin.clone();
                // add_child synchronously awaits its platform result. Schedule
                // construction on the main thread (Tauri dispatches inline there)
                // rather than waiting on the main thread for a worker to do it.
                if self
                    .app
                    .run_on_main_thread(move || {
                        reconcile(&app, &manager, &scroll, &origin, &windows, snapshots);
                        scheduled_task.store(false, Ordering::Release);
                    })
                    .is_err()
                {
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(200));
        });
    }
}

struct NativeSession {
    window: Window,
    toolbar: Webview,
    baseline: Webview,
    working: Webview,
    viewport: Arc<Mutex<(u32, u32)>>,
    sync: Arc<AtomicBool>,
    baseline_url: String,
    working_url: String,
    presentation_revision: u64,
    title: String,
    active: Arc<AtomicBool>,
    _scroll: [ComparisonScrollRegistration; 2],
}

impl Drop for NativeSession {
    fn drop(&mut self) {
        self.active.store(false, Ordering::Release);
    }
}

fn reconcile(
    app: &AppHandle,
    manager: &ComparisonManager,
    scroll: &ComparisonScrollRelay,
    toolbar_origin: &str,
    windows: &Mutex<BTreeMap<String, NativeSession>>,
    snapshots: Vec<ComparisonSession>,
) {
    let Ok(mut windows) = windows.lock() else {
        return;
    };
    windows.retain(|id, native| {
        // HTTP mutations may have happened since this main-thread tick was
        // queued. Presentation always checks the manager's current intent.
        if manager.get(id).is_some_and(|snapshot| {
            snapshot.window_requested
                && matches!(
                    snapshot.state,
                    ComparisonState::Starting | ComparisonState::Ready
                )
        }) {
            return true;
        }
        // destroy skips CloseRequested, which belongs to user-initiated closes.
        let _ = native.window.destroy();
        false
    });
    for queued in snapshots {
        let Some(snapshot) = manager.get(&queued.id) else {
            continue;
        };
        if snapshot.state != ComparisonState::Ready || !snapshot.window_requested {
            continue;
        }
        let outcome = if let Some(native) = windows.get_mut(&snapshot.id) {
            native.update(&snapshot).and_then(|_| {
                // Close followed immediately by Open may retain this existing
                // window while clearing its previous presentation receipt.
                if !snapshot.window_open {
                    manager.mark_window_open(&snapshot.id)?;
                }
                Ok(())
            })
        } else {
            create(app, manager, scroll, toolbar_origin, &snapshot).map(|native| {
                windows.insert(snapshot.id.clone(), native);
            })
        };
        if let Err(error) = outcome {
            if let Some(native) = windows.remove(&snapshot.id) {
                let _ = native.window.destroy();
            }
            // An HTTP close/stop can race a queued main-thread creation. Its
            // cancellation must not turn a healthy artifact into a failure.
            if manager.get(&snapshot.id).is_some_and(|current| {
                current.state == ComparisonState::Ready && current.window_requested
            }) {
                let _ = manager.fail(&snapshot.id, format!("Comparison window: {error}"));
            }
        }
    }
}

fn create(
    app: &AppHandle,
    manager: &ComparisonManager,
    scroll: &ComparisonScrollRelay,
    toolbar_origin: &str,
    snapshot: &ComparisonSession,
) -> Result<NativeSession, String> {
    ensure_platform_support()?;
    let baseline_url = snapshot
        .baseline_url
        .as_deref()
        .ok_or("baseline URL is not ready")?;
    let working_url = snapshot
        .working_url
        .as_deref()
        .ok_or("working URL is not ready")?;
    let prefix = format!("comparison-{}", snapshot.id);
    let minimum = minimum_window((
        snapshot.config.viewport.width,
        snapshot.config.viewport.height,
    ));
    let initial = initial_window_size(app, minimum);
    let title = comparison_title(snapshot);
    let window = WindowBuilder::new(app, &prefix)
        .title(&title)
        .inner_size(initial.0, initial.1)
        .min_inner_size(minimum.0, minimum.1)
        .visible(false)
        .build()
        .map_err(|error| error.to_string())?;
    let result = (|| {
        install_native_container(&window)?;
        let toolbar_url: Url = format!(
            "{}/#/comparison/{}",
            toolbar_origin.trim_end_matches('/'),
            snapshot.id
        )
        .parse()
        .map_err(|error| format!("Invalid comparison toolbar URL: {error}"))?;
        let toolbar_allowed = toolbar_url.origin();
        let toolbar = window
            .add_child(
                WebviewBuilder::new(
                    format!("{prefix}-toolbar"),
                    WebviewUrl::External(toolbar_url),
                )
                .on_navigation(move |url| url.origin() == toolbar_allowed)
                .on_new_window(|_, _| NewWindowResponse::Deny),
                LogicalPosition::new(0.0, 0.0),
                LogicalSize::new(1400.0, TOOLBAR_HEIGHT),
            )
            .map_err(|error| error.to_string())?;
        attach_native_child(&toolbar)?;
        let sync = Arc::new(AtomicBool::new(snapshot.config.sync_scroll));
        let active = Arc::new(AtomicBool::new(true));
        let bridge = PreviewBridge {
            app,
            manager,
            scroll,
            id: &snapshot.id,
            server_origin: toolbar_origin,
            enabled: sync.clone(),
            active: active.clone(),
        };
        let (baseline, baseline_scroll) = preview(
            &bridge,
            &window,
            &format!("{prefix}-baseline"),
            &format!("{prefix}-working"),
            baseline_url,
        )?;
        let (working, working_scroll) = preview(
            &bridge,
            &window,
            &format!("{prefix}-working"),
            &format!("{prefix}-baseline"),
            working_url,
        )?;
        attach_native_child(&baseline)?;
        attach_native_child(&working)?;
        let viewport = Arc::new(Mutex::new((
            snapshot.config.viewport.width,
            snapshot.config.viewport.height,
        )));
        let native = NativeSession {
            window: window.clone(),
            toolbar,
            baseline,
            working,
            viewport,
            sync,
            baseline_url: baseline_url.into(),
            working_url: working_url.into(),
            presentation_revision: snapshot.presentation_revision,
            title,
            active,
            _scroll: [baseline_scroll, working_scroll],
        };
        native.layout()?;
        observe_native_allocation(&native)?;

        let resize_window = window.clone();
        let toolbar = native.toolbar.clone();
        let baseline = native.baseline.clone();
        let working = native.working.clone();
        let viewport = native.viewport.clone();
        let close_manager = manager.clone();
        let id = snapshot.id.clone();
        window.on_window_event(move |event| match event {
            tauri::WindowEvent::CloseRequested { api, .. } => {
                // Closing the viewer preserves its pinned worktree and servers.
                // Reconciliation destroys only the window; explicit Stop owns
                // resource cleanup and leaves the artifact available to reopen.
                api.prevent_close();
                let _ = close_manager.close(&id);
            }
            tauri::WindowEvent::Resized(_) | tauri::WindowEvent::ScaleFactorChanged { .. } => {
                if let Ok(viewport) = viewport.lock().map(|viewport| *viewport) {
                    if let Err(error) =
                        layout(&resize_window, &toolbar, &baseline, &working, viewport)
                    {
                        log::warn!("Comparison layout failed: {error}");
                    }
                }
            }
            _ => {}
        });
        if !manager.get(&snapshot.id).is_some_and(|current| {
            current.state == ComparisonState::Ready && current.window_requested
        }) {
            return Err("Comparison window request was withdrawn".into());
        }
        window.show().map_err(|error| error.to_string())?;
        let _ = window.set_focus();
        manager.mark_window_open(&snapshot.id)?;
        Ok(native)
    })();
    if result.is_err() {
        let _ = window.destroy();
    }
    result
}

fn comparison_title(snapshot: &ComparisonSession) -> String {
    snapshot
        .config
        .artifact
        .as_ref()
        .map(|artifact| format!("{} — UI diff — VibeStudio", artifact.title))
        .unwrap_or_else(|| "Live UI comparison — VibeStudio".into())
}

struct PreviewBridge<'a> {
    app: &'a AppHandle,
    manager: &'a ComparisonManager,
    scroll: &'a ComparisonScrollRelay,
    id: &'a str,
    server_origin: &'a str,
    enabled: Arc<AtomicBool>,
    active: Arc<AtomicBool>,
}

/// A single pending slot bounds native dispatch even when its main thread is
/// busy. The next available turn sends the newest positions, never a backlog.
struct ScrollDelivery {
    app: AppHandle,
    manager: ComparisonManager,
    id: String,
    peer: String,
    enabled: Arc<AtomicBool>,
    active: Arc<AtomicBool>,
    pending: Mutex<(bool, Option<String>)>,
}
impl ScrollDelivery {
    fn send(self: &Arc<Self>, payload: String) -> Result<(), String> {
        if !self.active.load(Ordering::Acquire)
            || !self.enabled.load(Ordering::Acquire)
            || !self.manager.accepts_scroll(&self.id)
        {
            return Ok(());
        }
        {
            let mut pending = self
                .pending
                .lock()
                .map_err(|_| "Scroll relay lock failed")?;
            pending.1 = Some(payload);
            if pending.0 {
                return Ok(());
            }
            pending.0 = true;
        }
        let delivery = self.clone();
        if let Err(error) = self.app.run_on_main_thread(move || {
            let payload = delivery.pending.lock().ok().and_then(|mut pending| {
                pending.0 = false;
                pending.1.take()
            });
            if delivery.active.load(Ordering::Acquire)
                && delivery.enabled.load(Ordering::Acquire)
                && delivery.manager.accepts_scroll(&delivery.id)
            {
                if let (Some(payload), Some(view)) =
                    (payload, delivery.app.get_webview(&delivery.peer))
                {
                    let _ = view.eval(format!(
                        "window.__VIBESTUDIO_COMPARE_SCROLL__?.receive({payload});"
                    ));
                }
            }
        }) {
            if let Ok(mut pending) = self.pending.lock() {
                *pending = (false, None);
            }
            return Err(error.to_string());
        }
        Ok(())
    }
}

fn preview(
    bridge: &PreviewBridge<'_>,
    window: &Window,
    label: &str,
    peer: &str,
    url: &str,
) -> Result<(Webview, ComparisonScrollRegistration), String> {
    let url: Url = url
        .parse()
        .map_err(|error| format!("Invalid preview URL: {error}"))?;
    let origin = url.origin();
    let mut channel = url.clone();
    channel.set_path(&format!("/__vibestudio_comparison_scroll/{label}"));
    channel.set_query(None);
    channel.set_fragment(None);
    let channel_path = channel.path().to_string();
    let enabled = bridge.enabled.clone();
    let delivery = Arc::new(ScrollDelivery {
        app: bridge.app.clone(),
        manager: bridge.manager.clone(),
        id: bridge.id.to_string(),
        peer: peer.into(),
        enabled: enabled.clone(),
        active: bridge.active.clone(),
        pending: Mutex::new((false, None)),
    });
    let http_delivery = delivery.clone();
    let registration = bridge.scroll.register(
        origin.ascii_serialization(),
        Arc::new(move |payload| http_delivery.send(payload.to_string())),
    )?;
    let relay_url = format!(
        "{}/api/comparison/scroll/{}",
        bridge.server_origin.trim_end_matches('/'),
        registration.token()
    );
    let script = format!(
        "{SCROLL_SCRIPT}({});",
        json!({
            "origin": origin.ascii_serialization(), "channel": channel.as_str(),
            "enabled": enabled.load(Ordering::Acquire), "relayUrl": relay_url,
        })
    );
    let sync_on_load = enabled.clone();
    let view = window
        .add_child(
            WebviewBuilder::new(label, WebviewUrl::External(url))
                .incognito(true)
                .zoom_hotkeys_enabled(false)
                .focused(false)
                .disable_drag_drop_handler()
                .initialization_script(script)
                .on_new_window(|_, _| NewWindowResponse::Deny)
                .on_download(|_, _| false)
                .on_page_load(move |view, _| {
                    let _ = view.eval(format!(
                        "window.__VIBESTUDIO_COMPARE_SCROLL__?.setEnabled({});",
                        sync_on_load.load(Ordering::Acquire)
                    ));
                })
                .on_navigation(move |url| {
                    if url.origin() != origin {
                        return false;
                    }
                    if url.path() != channel_path {
                        return true;
                    }
                    if enabled.load(Ordering::Acquire) && url.as_str().len() <= MAX_SCROLL_PAYLOAD {
                        if let Some((_, payload)) =
                            url.query_pairs().find(|(key, _)| key == "payload")
                        {
                            if let Some(updates) = validated_scroll(&payload) {
                                let _ = delivery.send(updates.to_string());
                            }
                        }
                    }
                    false
                }),
            LogicalPosition::new(PADDING, TOOLBAR_HEIGHT + PADDING),
            LogicalSize::new(1.0, 1.0),
        )
        .map_err(|error| error.to_string())?;
    Ok((view, registration))
}

impl NativeSession {
    fn update(&mut self, snapshot: &ComparisonSession) -> Result<(), String> {
        let title = comparison_title(snapshot);
        if self.title != title {
            self.window
                .set_title(&title)
                .map_err(|error| error.to_string())?;
            self.title = title;
        }
        // Each explicit Open is also a request to bring an existing viewer
        // forward. Ordinary polling and HMR never steal focus.
        if self.presentation_revision != snapshot.presentation_revision {
            self.window
                .unminimize()
                .map_err(|error| error.to_string())?;
            self.window.show().map_err(|error| error.to_string())?;
            let _ = self.window.set_focus();
            self.presentation_revision = snapshot.presentation_revision;
        }
        let viewport = (
            snapshot.config.viewport.width,
            snapshot.config.viewport.height,
        );
        let resized = {
            let mut current = self
                .viewport
                .lock()
                .map_err(|_| "Comparison viewport lock failed")?;
            let resized = *current != viewport;
            *current = viewport;
            resized
        };
        if resized {
            let minimum = minimum_window(viewport);
            self.window
                .set_min_size(Some(LogicalSize::new(minimum.0, minimum.1)))
                .map_err(|error| error.to_string())?;
            let scale = self
                .window
                .scale_factor()
                .map_err(|error| error.to_string())?;
            let size = self
                .window
                .inner_size()
                .map_err(|error| error.to_string())?
                .to_logical::<f64>(scale);
            if size.width < minimum.0 || size.height < minimum.1 {
                self.window
                    .set_size(LogicalSize::new(
                        size.width.max(minimum.0),
                        size.height.max(minimum.1),
                    ))
                    .map_err(|error| error.to_string())?;
            }
            self.layout()?;
        }
        if self
            .sync
            .swap(snapshot.config.sync_scroll, Ordering::AcqRel)
            != snapshot.config.sync_scroll
        {
            let script = format!(
                "window.__VIBESTUDIO_COMPARE_SCROLL__?.setEnabled({});",
                snapshot.config.sync_scroll
            );
            self.baseline
                .eval(&script)
                .map_err(|error| error.to_string())?;
            self.working
                .eval(&script)
                .map_err(|error| error.to_string())?;
        }
        for (view, current, next) in [
            (
                &self.baseline,
                &mut self.baseline_url,
                &snapshot.baseline_url,
            ),
            (&self.working, &mut self.working_url, &snapshot.working_url),
        ] {
            if let Some(next) = next {
                if current != next {
                    let url = next
                        .parse()
                        .map_err(|error| format!("Invalid comparison URL: {error}"))?;
                    view.navigate(url).map_err(|error| error.to_string())?;
                    current.clone_from(next);
                }
            }
        }
        Ok(())
    }

    fn layout(&self) -> Result<(), String> {
        let viewport = *self
            .viewport
            .lock()
            .map_err(|_| "Comparison viewport lock failed")?;
        layout(
            &self.window,
            &self.toolbar,
            &self.baseline,
            &self.working,
            viewport,
        )
    }
}

#[derive(Debug)]
struct PaneLayout {
    zoom: f64,
    width: f64,
    height: f64,
    left: f64,
    right: f64,
    top: f64,
}

fn initial_window_size(app: &AppHandle, minimum: (f64, f64)) -> (f64, f64) {
    let monitor = app
        .get_window("main")
        .and_then(|window| window.current_monitor().ok().flatten())
        .or_else(|| app.primary_monitor().ok().flatten());
    let available = monitor
        .map(|monitor| {
            let size = monitor.size().to_logical::<f64>(monitor.scale_factor());
            (size.width - 64.0, size.height - 64.0)
        })
        .unwrap_or((1400.0, 980.0));
    // Leave room for window decorations and the desktop's panels. Very large
    // custom viewports still take priority over fitting a small display.
    (
        1400.0_f64.min(available.0).max(minimum.0),
        980.0_f64.min(available.1).max(minimum.1),
    )
}

fn minimum_window(viewport: (u32, u32)) -> (f64, f64) {
    (
        (2.0 * (f64::from(viewport.0) * MIN_ZOOM + 2.0 * PADDING)).max(900.0),
        (f64::from(viewport.1) * MIN_ZOOM + TOOLBAR_HEIGHT + 2.0 * PADDING).max(440.0),
    )
}

fn pane_layout(window_width: f64, window_height: f64, viewport: (u32, u32)) -> PaneLayout {
    let half = window_width / 2.0;
    let available_width = (half - 2.0 * PADDING).max(1.0);
    let available_height = (window_height - TOOLBAR_HEIGHT - 2.0 * PADDING).max(1.0);
    let width_fit = available_width / f64::from(viewport.0.max(1));
    let height_fit = available_height / f64::from(viewport.1.max(1));
    let initial_zoom = width_fit.min(height_fit).clamp(MIN_ZOOM, 1.0);
    let limiting_dimension = if width_fit <= height_fit {
        viewport.0
    } else {
        viewport.1
    };
    let zoom = pixel_aligned_zoom(initial_zoom, viewport, limiting_dimension);
    let width = f64::from(viewport.0.max(1)) * zoom;
    let height = f64::from(viewport.1.max(1)) * zoom;
    let left = (half - width) / 2.0;
    PaneLayout {
        zoom,
        width,
        height,
        left,
        right: half + left,
        top: TOOLBAR_HEIGHT + PADDING,
    }
}

/// Native GTK child bounds are whole logical pixels and WebKit floors CSS
/// viewport measurements after zoom. Try at most eight slightly smaller native
/// bounds so common presets keep their exact responsive breakpoints. Arbitrary
/// custom sizes can still have quantization error; both panes use the same fit.
fn pixel_aligned_zoom(initial: f64, viewport: (u32, u32), limiting_dimension: u32) -> f64 {
    let matches = |zoom: f64| {
        [viewport.0, viewport.1]
            .iter()
            .all(|size| ((f64::from(*size) * zoom).ceil() / zoom).floor() == f64::from(*size))
    };
    if matches(initial) {
        return initial;
    }
    let limiting = f64::from(limiting_dimension.max(1));
    let pixels = (limiting * initial).ceil();
    for adjustment in 1..=8 {
        let zoom = (pixels - f64::from(adjustment)) / limiting;
        if zoom < MIN_ZOOM {
            break;
        }
        if matches(zoom) {
            return zoom;
        }
    }
    initial
}

fn layout(
    window: &Window,
    toolbar: &Webview,
    baseline: &Webview,
    working: &Webview,
    viewport: (u32, u32),
) -> Result<(), String> {
    let size = native_content_size(window)?;
    layout_content(toolbar, baseline, working, viewport, size.0, size.1)
}

fn layout_content(
    toolbar: &Webview,
    baseline: &Webview,
    working: &Webview,
    viewport: (u32, u32),
    width: f64,
    height: f64,
) -> Result<(), String> {
    let panes = pane_layout(width, height, viewport);
    set_native_bounds(toolbar, 0.0, 0.0, width, TOOLBAR_HEIGHT)?;
    for (view, x) in [(baseline, panes.left), (working, panes.right)] {
        // Native page zoom scales the requested CSS viewport into the available
        // area. Both panes retain identical layout dimensions while resizing.
        view.set_zoom(panes.zoom)
            .map_err(|error| error.to_string())?;
        set_native_bounds(view, x, panes.top, panes.width, panes.height)?;
    }
    Ok(())
}

// Tauri 2.11.2 puts Linux children into its default GtkBox, whose packing
// overrides child bounds. GtkLayout positions children without deriving its
// minimum size from their size requests. GtkFixed would feed the toolbar's
// width back into the toplevel minimum and prevent shrinking. With GTK client
// decorations, Tao's inner_size also includes frame extents, producing endless
// growth when that width is requested inside the smaller content allocation.
// GtkLayout reports zero preferred size; our explicit window minimum owns it.
// https://github.com/GNOME/gtk/blob/gtk-3-24/gtk/gtklayout.c
// Wry remembers the original parent kind, so bounds use GTK directly on Linux.
fn native_content_size(window: &Window) -> Result<(f64, f64), String> {
    #[cfg(target_os = "linux")]
    {
        use gtk::prelude::*;
        let canvas = native_container(window)?;
        let allocation = canvas.allocation();
        if allocation.width() > 1 && allocation.height() > 1 {
            return Ok((
                f64::from(allocation.width()),
                f64::from(allocation.height()),
            ));
        }
    }
    // Before the Linux container is first allocated, initialize child bounds
    // from the requested window size. Other platforms use their client size.
    let scale = window.scale_factor().map_err(|error| error.to_string())?;
    let size = window
        .inner_size()
        .map_err(|error| error.to_string())?
        .to_logical::<f64>(scale);
    Ok((size.width, size.height))
}

#[cfg(target_os = "linux")]
fn native_container(window: &Window) -> Result<gtk::Layout, String> {
    use gtk::prelude::*;
    window
        .default_vbox()
        .map_err(|error| error.to_string())?
        .children()
        .into_iter()
        .find_map(|child| child.downcast::<gtk::Layout>().ok())
        .ok_or_else(|| "Comparison GTK layout container is missing".into())
}

fn observe_native_allocation(_native: &NativeSession) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        use gtk::prelude::*;
        let canvas = native_container(&_native.window)?;
        let toolbar = _native.toolbar.clone();
        let baseline = _native.baseline.clone();
        let working = _native.working.clone();
        let viewport = _native.viewport.clone();
        let previous = std::cell::Cell::new((0, 0));
        canvas.connect_size_allocate(move |_, allocation| {
            let size = (allocation.width(), allocation.height());
            if size.0 <= 1 || size.1 <= 1 || previous.replace(size) == size {
                return;
            }
            // Window configure events can precede GTK's content allocation.
            // Apply that final allocation, not the earlier decorated size.
            if let Ok(viewport) = viewport.lock().map(|viewport| *viewport) {
                if let Err(error) = layout_content(
                    &toolbar,
                    &baseline,
                    &working,
                    viewport,
                    f64::from(size.0),
                    f64::from(size.1),
                ) {
                    log::warn!("Comparison content layout failed: {error}");
                }
            }
        });
    }
    Ok(())
}

fn install_native_container(_window: &Window) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        use gtk::prelude::*;
        let container = _window.default_vbox().map_err(|error| error.to_string())?;
        let canvas = gtk::Layout::new(gtk::Adjustment::NONE, gtk::Adjustment::NONE);
        canvas.set_hexpand(true);
        canvas.set_vexpand(true);
        container.pack_start(&canvas, true, true, 0);
        canvas.show();
    }
    Ok(())
}

fn attach_native_child(_view: &Webview) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        use gtk::prelude::*;
        let (tx, rx) = std::sync::mpsc::channel();
        _view
            .with_webview(move |platform| {
                let raw = platform.inner();
                let result = (|| {
                    let parent = raw
                        .parent()
                        .ok_or("Comparison child has no GTK parent")?
                        .downcast::<gtk::Container>()
                        .map_err(|_| "Comparison GTK parent is not a container")?;
                    let canvas = parent
                        .children()
                        .into_iter()
                        .find_map(|child| child.downcast::<gtk::Layout>().ok())
                        .ok_or("Comparison GTK layout container is missing")?;
                    parent.remove(&raw);
                    raw.set_hexpand(false);
                    raw.set_vexpand(false);
                    canvas.put(&raw, 0, 0);
                    raw.show();
                    Ok::<_, &str>(())
                })();
                let _ = tx.send(result);
            })
            .map_err(|error| error.to_string())?;
        rx.recv_timeout(Duration::from_secs(1))
            .map_err(|error| error.to_string())?
            .map_err(str::to_string)?;
    }
    Ok(())
}

fn set_native_bounds(
    view: &Webview,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        use gtk::prelude::*;
        view.with_webview(move |platform| {
            let raw = platform.inner();
            if let Some(canvas) = raw
                .parent()
                .and_then(|parent| parent.downcast::<gtk::Layout>().ok())
            {
                // WebKit floors the resulting CSS viewport after page zoom.
                // Round native size up so standard presets do not undershoot a
                // breakpoint (e.g.390 CSS px becoming389). Arbitrary custom
                // dimensions can differ by native-pixel quantization; both
                // panes always receive identical bounds and zoom.
                let (x, y) = (x.round() as i32, y.round() as i32);
                let (width, height) = (width.ceil() as i32, height.ceil() as i32);
                raw.set_size_request(width, height);
                canvas.move_(&raw, x, y);
                // GtkLayout's preferred size remains zero, so a viewport-only
                // change need not cause another parent allocation. Apply the
                // child allocation now as Wry does for positioned GTK views.
                raw.size_allocate(&gtk::Allocation::new(x, y, width, height));
            }
        })
        .map_err(|error| error.to_string())?;
    }
    #[cfg(not(target_os = "linux"))]
    {
        view.set_size(LogicalSize::new(width, height))
            .map_err(|error| error.to_string())?;
        view.set_position(LogicalPosition::new(x, y))
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// Parse only position data, never script, selectors or arbitrary commands.
/// Re-serialization guarantees that untrusted preview text cannot escape eval.
fn validated_scroll(payload: &str) -> Option<Value> {
    if payload.len() > MAX_SCROLL_PAYLOAD {
        return None;
    }
    let value: Value = serde_json::from_str(payload).ok()?;
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
                .any(|part| part.as_u64().map_or(true, |part| part > 65_535))
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
        result.push(json!({ "root": root, "key": key, "id": id, "path": path, "index": index, "x": x, "y": y }));
    }
    Some(Value::Array(result))
}

/// WKWebView's setPageZoom selector is unavailable on macOS 10.15 (which the
/// rest of VibeStudio supports). Check before creating any preview webviews.
fn ensure_platform_support() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        static SUPPORTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let supported = SUPPORTED.get_or_init(|| {
            skill_core::process::hidden_command("/usr/bin/sw_vers")
                .arg("-productVersion")
                .output()
                .ok()
                .filter(|output| output.status.success())
                .and_then(|output| String::from_utf8(output.stdout).ok())
                .and_then(|version| version.trim().split('.').next()?.parse::<u32>().ok())
                .is_some_and(|major| major >= 11)
        });
        if !supported {
            return Err(
                "Live UI comparison requires macOS 11 or newer for native viewport scaling.".into(),
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fitting_preserves_matching_css_viewports() {
        for viewport in [(390, 844), (844, 390), (768, 1024), (1440, 900)] {
            let panes = pane_layout(1400.0, 980.0, viewport);
            assert!((panes.width / panes.zoom - f64::from(viewport.0)).abs() < 0.001);
            assert!((panes.height / panes.zoom - f64::from(viewport.1)).abs() < 0.001);
            assert!(panes.left >= PADDING);
            assert!(panes.right + panes.width <= 1400.0 - PADDING);
            assert!(panes.top + panes.height <= 980.0 - PADDING);
        }
    }

    #[test]
    fn standard_presets_keep_css_dimensions_after_native_pixel_rounding() {
        for viewport in [
            (375, 667),
            (390, 844),
            (430, 932),
            (412, 915),
            (1280, 800),
            (844, 390),
            (768, 1024),
            (1024, 768),
            (1440, 900),
        ] {
            let minimum = minimum_window(viewport);
            for (width, height) in [(1400.0_f64, 980.0_f64), (900.0, 440.0), (1600.0, 1000.0)] {
                let panes = pane_layout(width.max(minimum.0), height.max(minimum.1), viewport);
                assert_eq!(
                    (panes.width.ceil() / panes.zoom).floor() as u32,
                    viewport.0,
                    "width at {width}x{height}"
                );
                assert_eq!(
                    (panes.height.ceil() / panes.zoom).floor() as u32,
                    viewport.1,
                    "height at {width}x{height}"
                );
            }
        }
    }

    #[test]
    fn large_viewports_grow_the_minimum_window_instead_of_clamping_css_size() {
        for viewport in [(3840, 3840), (768, 1024), (240, 3840)] {
            let minimum = minimum_window(viewport);
            let panes = pane_layout(minimum.0, minimum.1, viewport);
            assert!(panes.zoom >= MIN_ZOOM);
            assert!(panes.left >= PADDING);
            assert!(panes.right + panes.width <= minimum.0 - PADDING);
            assert!(panes.top + panes.height <= minimum.1 - PADDING);
            assert!((panes.width / panes.zoom - f64::from(viewport.0)).abs() < 0.001);
            assert!((panes.height / panes.zoom - f64::from(viewport.1)).abs() < 0.001);
        }
    }

    #[test]
    fn bridge_accepts_only_bounded_scroll_positions() {
        let packet = json!([{ "root": false, "key": "sidebar", "id": "", "path": [1, 2], "index": 0, "x": 0.2, "y": 0.8 }]);
        assert_eq!(validated_scroll(&packet.to_string()), Some(packet.clone()));
        for bad in [
            "null".to_owned(),
            "[]".to_owned(),
            "alert(1)".to_owned(),
            "x".repeat(MAX_SCROLL_PAYLOAD + 1),
        ] {
            assert!(validated_scroll(&bad).is_none());
        }
        let mut bad = packet.clone();
        bad[0]["y"] = json!(1.2);
        assert!(validated_scroll(&bad.to_string()).is_none());
        bad = packet.clone();
        bad[0]["path"] = json!(["document.cookie"]);
        assert!(validated_scroll(&bad.to_string()).is_none());
        bad = packet;
        bad[0]["script"] = json!("alert(1)");
        assert!(validated_scroll(&bad.to_string()).unwrap()[0]
            .get("script")
            .is_none());
    }
}
