//! Loopback smoke test for the pinned-local `/api/notify*` surface. Stands up a
//! server with a capturing `NotifyControl` and checks the two boundaries that
//! matter: no notifier → 404 (the SPA's cue to use the Web Notification API),
//! and a `tailscale serve`-fronted request (forwarding headers / foreign Host)
//! → 404 even WITH a notifier — the phone's toast must not pop on this desktop.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use skill_server::{spawn, NotificationSound, NotifyControl, ServerConfig};

/// Counts deliveries instead of showing anything.
#[derive(Default)]
struct Capture {
    shown: AtomicUsize,
    badge: AtomicUsize,
    request_sounds: AtomicUsize,
    done_sounds: AtomicUsize,
}

impl NotifyControl for Capture {
    fn notify_while_visible(&self) -> bool {
        true
    }
    fn notify(&self, _title: &str, _body: &str) -> Result<(), String> {
        self.shown.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn set_badge(&self, count: u32) {
        self.badge.store(count as usize, Ordering::SeqCst);
    }
    fn sound(&self, sound: NotificationSound) -> Result<bool, String> {
        match sound {
            NotificationSound::Request => &self.request_sounds,
            NotificationSound::Done => &self.done_sounds,
        }
        .fetch_add(1, Ordering::SeqCst);
        Ok(true)
    }
}

#[test]
fn notify_routes_gate_on_notifier_and_locality() {
    // The real watcher starts with each HTTP server. Its notification store must
    // be empty here so an unrelated live agent cannot push to the user's phone.
    let config =
        std::env::temp_dir().join(format!("vibestudio-notify-smoke-{}", std::process::id()));
    std::fs::create_dir_all(&config).expect("isolated notification config");
    skill_core::paths::set_config_dir(config.clone());
    // 1) No notifier (standalone/browser server) → the whole family 404s.
    let plain = spawn(ServerConfig {
        port: 0,
        startup_maintenance: false,
        ..Default::default()
    })
    .expect("plain server");
    let p = plain.addr.port();
    let r = ureq::get(&format!("http://127.0.0.1:{p}/api/notify/status")).call();
    assert!(
        matches!(r, Err(ureq::Error::Status(404, _))),
        "no notifier must 404"
    );
    let r = ureq::post(&format!("http://127.0.0.1:{p}/api/notify/sound"))
        .set("Content-Type", "application/json")
        .send_string("{\"kind\":\"request\"}");
    assert!(
        matches!(r, Err(ureq::Error::Status(404, _))),
        "no audio control must 404"
    );

    // 2) With a notifier, this machine's own webview/browser gets the surface.
    let capture = Arc::new(Capture::default());
    let server = spawn(ServerConfig {
        port: 0,
        startup_maintenance: false,
        notifier: Some(capture.clone()),
        ..Default::default()
    })
    .expect("notifier server");
    let port = server.addr.port();
    let base = format!("http://127.0.0.1:{port}");

    let status = ureq::get(&format!("{base}/api/notify/status"))
        .call()
        .expect("status");
    assert_eq!(status.status(), 200);
    let status: serde_json::Value = serde_json::from_str(&status.into_string().unwrap()).unwrap();
    assert_eq!(status["native"], true);
    assert_eq!(status["notifyWhileVisible"], true);

    let post = ureq::post(&format!("{base}/api/notify"))
        .set("Content-Type", "application/json")
        .send_string("{\"title\":\"t\",\"body\":\"b\"}")
        .expect("notify");
    assert_eq!(post.status(), 200);
    assert_eq!(capture.shown.load(Ordering::SeqCst), 1);

    let badge = ureq::post(&format!("{base}/api/notify/badge"))
        .set("Content-Type", "application/json")
        .send_string("{\"count\":3}")
        .expect("badge");
    assert_eq!(badge.status(), 200);
    assert_eq!(capture.badge.load(Ordering::SeqCst), 3);

    for kind in ["request", "done"] {
        let sound = ureq::post(&format!("{base}/api/notify/sound"))
            .set("Content-Type", "application/json")
            .send_string(&serde_json::json!({ "kind": kind }).to_string())
            .expect("sound");
        assert_eq!(sound.status(), 200);
    }
    let invalid = ureq::post(&format!("{base}/api/notify/sound"))
        .set("Content-Type", "application/json")
        .send_string("{\"kind\":\"/tmp/custom.mp3\"}");
    assert!(
        matches!(invalid, Err(ureq::Error::Status(400, _))),
        "only built-in kinds allowed"
    );
    assert_eq!(capture.request_sounds.load(Ordering::SeqCst), 1);
    assert_eq!(capture.done_sounds.load(Ordering::SeqCst), 1);

    // 3) The same requests fronted by tailscale serve (forwarding headers) → 404,
    //    and nothing is shown on this machine.
    let fronted = ureq::get(&format!("{base}/api/notify/status"))
        .set("X-Forwarded-Host", "machine.tailnet.ts.net")
        .call();
    assert!(
        matches!(fronted, Err(ureq::Error::Status(404, _))),
        "fronted status must 404"
    );

    let fronted_post = ureq::post(&format!("{base}/api/notify"))
        .set("X-Forwarded-For", "100.64.0.7")
        .set("Content-Type", "application/json")
        .send_string("{\"title\":\"t\",\"body\":\"b\"}");
    assert!(
        matches!(fronted_post, Err(ureq::Error::Status(404, _))),
        "fronted notify must 404"
    );
    assert_eq!(
        capture.shown.load(Ordering::SeqCst),
        1,
        "no toast for a fronted request"
    );

    let fronted_sound = ureq::post(&format!("{base}/api/notify/sound"))
        .set("X-Forwarded-For", "100.64.0.7")
        .set("Content-Type", "application/json")
        .send_string("{\"kind\":\"request\"}");
    assert!(
        matches!(fronted_sound, Err(ureq::Error::Status(404, _))),
        "fronted sound must 404"
    );
    assert_eq!(
        capture.request_sounds.load(Ordering::SeqCst),
        1,
        "no local sound for a phone request"
    );

    drop((plain, server));
    let _ = std::fs::remove_dir_all(config);
}
