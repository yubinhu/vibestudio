//! iOS owns authentication and the privacy cover; the shared server owns access.
//! Resolve the native entry-point class at runtime because Cargo also links a
//! cdylib before Xcode links the Objective-C++ application sources.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use objc2::runtime::AnyClass;
use skill_server::{AppAccess, SshRemoteControl};
use tauri::Manager;

struct Bridge {
    app: tauri::AppHandle,
    access: Arc<AppAccess>,
    remote: Arc<SshRemoteControl>,
    local: Arc<super::LocalServer>,
    loaded: AtomicBool,
    pending_navigation: AtomicBool,
}

static BRIDGE: OnceLock<Bridge> = OnceLock::new();

extern "C" fn set_unlocked(unlocked: bool) {
    let Some(bridge) = BRIDGE.get() else { return };
    if !unlocked {
        // Close the HTTP gate before cancelling this phone's SSH work. The
        // detached computer-side service and its agents are never stopped.
        bridge.access.set_unlocked(false);
        bridge.remote.set_app_unlocked(false);
        return;
    }
    bridge.remote.set_app_unlocked(true);
    bridge.access.set_unlocked(true);
    if !bridge.loaded.load(Ordering::SeqCst) {
        // Don't load the SPA until initial authentication succeeds: its startup
        // probes and saved-host reconnect must not run behind the lock screen.
        navigate_workspace(bridge, true);
    } else {
        if bridge.pending_navigation.load(Ordering::SeqCst) {
            navigate_workspace(bridge, false);
        }
        // A quick app switch within the grace period keeps the workspace and
        // only checks whether iOS reclaimed its SSH connection.
        bridge.remote.resume_check();
    }
}

pub fn install(
    app: &tauri::App,
    access: Arc<AppAccess>,
    remote: Arc<SshRemoteControl>,
    local: Arc<super::LocalServer>,
) -> Result<(), std::io::Error> {
    let class = AnyClass::get(c"VibeStudioAppLock")
        .ok_or_else(|| std::io::Error::other("native app lock is unavailable"))?;
    BRIDGE.set(Bridge {
        app: app.handle().clone(), access, remote, local,
        loaded: AtomicBool::new(false),
        pending_navigation: AtomicBool::new(false),
    }).map_err(|_| std::io::Error::other("app lock is already installed"))?;
    let callback = set_unlocked as extern "C" fn(bool) as *const std::ffi::c_void;
    // SAFETY: +installWithCallback: takes a C function pointer represented by
    // void*, retains it for this process, and invokes it only on the main thread.
    unsafe { let _: () = objc2::msg_send![class, installWithCallback: callback]; }
    Ok(())
}

fn navigate_workspace(bridge: &Bridge, initial: bool) {
    let Some(window) = bridge.app.get_webview_window("main") else { return };
    let port = bridge.local.port.load(Ordering::SeqCst);
    let base = format!("http://127.0.0.1:{port}").parse().expect("loopback URL");
    let url = if initial { base } else {
        window.url().ok().filter(|url| url.scheme() == "http")
            .map(|mut url| { let _ = url.set_port(Some(port)); url })
            .unwrap_or(base)
    };
    match window.navigate(url) {
        Ok(()) => {
            bridge.loaded.store(true, Ordering::SeqCst);
            bridge.pending_navigation.store(false, Ordering::SeqCst);
        }
        Err(error) => log::error!("could not open the unlocked workspace: {error}"),
    }
}

pub fn listener_changed() {
    if let Some(bridge) = BRIDGE.get() {
        bridge.pending_navigation.store(true, Ordering::SeqCst);
        if bridge.access.is_unlocked() && bridge.loaded.load(Ordering::SeqCst) {
            navigate_workspace(bridge, false);
        }
    }
}

pub fn refresh() {
    if let Some(class) = AnyClass::get(c"VibeStudioAppLock") {
        // Evaluate elapsed background time before iOS's resume networking runs.
        // This executes on Tauri's main-thread window-event callback.
        unsafe { let _: () = objc2::msg_send![class, refresh]; }
    }
}
