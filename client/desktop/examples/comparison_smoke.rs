//! Isolated native comparison harness. Run with private XDG_CONFIG_HOME and
//! TMUX_TMPDIR, and a path to a ComparisonConfig JSON fixture as its argument:
//!
//! cargo run --manifest-path client/desktop/Cargo.toml --example comparison_smoke -- /tmp/fixture/config.json
//!
//! No host service, tray, phone mapping, startup maintenance, or endpoint
//! discovery record is created. The fixture's manager and child webviews are
//! identical to the desktop's. An optional VIBESTUDIO_COMPARISON_SMOKE_SCRIPT
//! path contains JavaScript evaluated in both panes after creation, useful for
//! recording viewport/scroll results to the fixture's own HTTP server.

#[cfg(desktop)]
#[path = "../src/comparison.rs"]
mod comparison;

#[cfg(desktop)]
fn main() {
    use skill_core::comparison::{ComparisonConfig, ComparisonManager, ComparisonState};
    use std::time::Duration;
    use tauri::Manager;

    for name in ["XDG_CONFIG_HOME", "TMUX_TMPDIR"] {
        assert!(
            std::env::var_os(name).is_some(),
            "Set private {name} before running this harness"
        );
    }
    let config_path = std::env::args()
        .nth(1)
        .expect("Pass a ComparisonConfig JSON fixture path");
    let config: ComparisonConfig =
        serde_json::from_slice(&std::fs::read(config_path).expect("Read fixture config"))
            .expect("Parse fixture config");
    let script = std::env::var_os("VIBESTUDIO_COMPARISON_SMOKE_SCRIPT")
        .map(|path| std::fs::read_to_string(path).expect("Read smoke script"));
    let window_state =
        std::env::var_os("VIBESTUDIO_COMPARISON_SMOKE_WINDOW_STATE").map(std::path::PathBuf::from);
    let window_command = std::env::var_os("VIBESTUDIO_COMPARISON_SMOKE_WINDOW_COMMAND")
        .map(std::path::PathBuf::from);
    let manager = ComparisonManager::default();
    let scroll = skill_core::comparison::ComparisonScrollRelay::default();
    let exit_manager = manager.clone();
    skill_server::init_logging();
    tauri::Builder::default()
        .setup(move |app| {
            let server = skill_server::spawn(skill_server::ServerConfig {
                port: 0,
                dist: std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../dist"),
                comparison: Some(manager.clone()),
                comparison_scroll: Some(scroll.clone()),
                startup_maintenance: false,
                ..Default::default()
            })?;
            let origin = server.url();
            comparison::ShellComparison::new(app.handle().clone(), manager.clone(), scroll)
                .start(origin.clone());
            let session = manager.start(config).map_err(std::io::Error::other)?;
            println!("COMPARISON_SMOKE_READY url={origin} id={}", session.id);
            let app = app.handle().clone();
            std::thread::spawn(move || {
                let mut handled = String::new();
                let mut first_visible = false;
                loop {
                    if let Some(path) = &window_command {
                        if let Some(command) = std::fs::read(path)
                            .ok()
                            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
                        {
                            let token = command["id"].as_str().unwrap_or_default();
                            if !token.is_empty() && token != handled {
                                let id = command["comparisonId"].as_str().unwrap_or_default();
                                if let Some(window) = app.get_window(&format!("comparison-{id}")) {
                                    if command["action"] == "close" {
                                        let _ = window.close();
                                    }
                                    if command["action"] == "resize" {
                                        if let (Some(width), Some(height)) = (command["width"].as_f64(), command["height"].as_f64()) {
                                            let _ = window.set_size(tauri::LogicalSize::new(width, height));
                                        }
                                    }
                                }
                                if command["action"] == "switch" {
                                    if let Some(view) = app.get_webview(&format!("comparison-{id}-toolbar")) {
                                        let target = serde_json::to_string(command["targetId"].as_str().unwrap_or_default()).unwrap();
                                        let script = r#"(() => {
                                          const target = __TARGET__; let attempts = 0;
                                          const timer = setInterval(() => {
                                            const select = document.querySelector('select[aria-label="Switch UI diff"]');
                                            if (select && !select.disabled && [...select.options].some(option => option.value === target)) {
                                              select.value = target;
                                              select.dispatchEvent(new Event('change', {bubbles: true}));
                                              clearInterval(timer);
                                            } else if (++attempts > 50) clearInterval(timer);
                                          }, 100);
                                        })();"#.replace("__TARGET__", &target);
                                        let _ = view.eval(script);
                                    }
                                }
                                handled = token.into();
                            }
                        }
                    }
                    let mut observed = Vec::new();
                    for snapshot in manager.list() {
                        if snapshot.state == ComparisonState::Failed {
                            eprintln!(
                                "COMPARISON_SMOKE_FAILED {}",
                                snapshot.error.unwrap_or_default()
                            );
                            app.exit(1);
                            return;
                        }
                        let label = format!("comparison-{}", snapshot.id);
                        let window = app.get_window(&label);
                        let visible = window.as_ref().is_some_and(|window| {
                            window.webviews().len() == 3 && window.is_visible().unwrap_or(false)
                        });
                        let scale = window.as_ref().and_then(|window| window.scale_factor().ok()).unwrap_or(1.0);
                        let size = window.as_ref().and_then(|window| window.inner_size().ok()).map(|size| size.to_logical::<f64>(scale));
                        observed.push(serde_json::json!({
                            "id": snapshot.id,
                            "present": window.is_some(),
                            "visible": visible,
                            "focused": window.as_ref().is_some_and(|window| window.is_focused().unwrap_or(false)),
                            "children": window.as_ref().map(|window| window.webviews().len()).unwrap_or(0),
                            "title": window.as_ref().and_then(|window| window.title().ok()),
                            "width": size.map(|size| size.width),
                            "height": size.map(|size| size.height),
                            "scale": scale,
                        }));
                        if snapshot.id == session.id && visible {
                            if !first_visible {
                                println!("COMPARISON_SMOKE_VISIBLE children=3");
                                first_visible = true;
                            }
                            // The guard inside the fixture script makes repeated
                            // eval safe, including after route loads/reopening.
                            if let Some(script) = &script {
                                for pane in ["baseline", "working"] {
                                    if let Some(view) = app.get_webview(&format!("{label}-{pane}")) {
                                        let _ = view.eval(script);
                                    }
                                }
                            }
                        }
                    }
                    if let Some(path) = &window_state {
                        let _ = std::fs::write(path, serde_json::json!({
                            "windows": observed, "handled": handled,
                        }).to_string());
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("Build isolated native fixture")
        .run(move |_, event| {
            match event {
                // The production app retains its main window/tray. This fixture
                // stays alive without either while testing Close and reopen.
                tauri::RunEvent::ExitRequested { code: None, api, .. } => api.prevent_exit(),
                tauri::RunEvent::Exit => {
                    let _ = exit_manager.shutdown(Duration::from_secs(5));
                }
                _ => {}
            }
        });
}

#[cfg(not(desktop))]
fn main() {
    panic!("Live comparison is a desktop feature");
}
