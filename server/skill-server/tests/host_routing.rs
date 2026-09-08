//! A dropped host must never redirect a pending workspace read/write to Local.
use skill_server::{
    spawn, LocalBackendControl, RemoteControl, RemoteHost, RemoteStatus, RemoteTarget, ServerConfig,
};
use std::sync::{Arc, Mutex};

struct Remote(Mutex<(RemoteStatus, Option<RemoteTarget>)>);
impl RemoteControl for Remote {
    fn list_hosts(&self) -> Result<Vec<RemoteHost>, String> {
        Ok(vec![])
    }
    fn connect(&self, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn disconnect(&self, _: bool) -> Result<(), String> {
        Ok(())
    }
    fn status(&self) -> RemoteStatus {
        self.0.lock().unwrap().0.clone()
    }
    fn active_target(&self) -> Option<RemoteTarget> {
        self.0.lock().unwrap().1.clone()
    }
}
struct Local(Mutex<Result<RemoteTarget, String>>);
impl LocalBackendControl for Local {
    fn target(&self) -> Result<RemoteTarget, String> {
        self.0.lock().unwrap().clone()
    }
}
#[derive(Default)]
struct Editor(Mutex<Vec<Option<String>>>);
impl skill_server::EditorControl for Editor {
    fn detect(&self) -> Option<String> {
        Some("Fixture editor".into())
    }
    fn open(&self, _: &str, host: Option<&str>) -> Result<(), String> {
        self.0.lock().unwrap().push(host.map(str::to_owned));
        Ok(())
    }
}
fn open_editor(base: &str) -> u16 {
    match ureq::post(&format!("{base}/api/editor/open"))
        .set("Content-Type", "application/json")
        .send_string(r#"{"path":"/workspace/repo"}"#)
    {
        Ok(response) | Err(ureq::Error::Status(_, response)) => response.status(),
        Err(error) => panic!("{error}"),
    }
}
fn get_status(url: &str) -> u16 {
    match ureq::get(url).call() {
        Ok(response) | Err(ureq::Error::Status(_, response)) => response.status(),
        Err(error) => panic!("{error}"),
    }
}

#[test]
fn unavailable_remote_and_local_workers_fail_closed_but_client_health_stays_available() {
    let dir = std::env::temp_dir().join(format!("vibestudio-host-routing-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    skill_core::paths::set_config_dir(dir.clone());
    let worker = spawn(ServerConfig {
        startup_maintenance: false,
        token: Some("worker-token".into()),
        ..Default::default()
    })
    .unwrap();
    let target = RemoteTarget {
        base_url: worker.url(),
        token: "worker-token".into(),
    };
    let remote = Arc::new(Remote(Mutex::new((
        RemoteStatus {
            state: "connected".into(),
            host: Some("workbox".into()),
            message: None,
        },
        Some(target.clone()),
    ))));
    let local = Arc::new(Local(Mutex::new(Ok(target.clone()))));
    let editor = Arc::new(Editor::default());
    let client = spawn(ServerConfig {
        startup_maintenance: false,
        remote: Some(remote.clone()),
        local_backend: Some(local.clone()),
        editor: Some(editor.clone()),
        ..Default::default()
    })
    .unwrap();
    let base = client.url();
    assert_eq!(get_status(&format!("{base}/api/preferences")), 200);
    assert_eq!(open_editor(&base), 200);
    assert_eq!(*editor.0.lock().unwrap(), vec![Some("workbox".into())]);

    for state in ["reconnecting", "error", "detecting"] {
        *remote.0.lock().unwrap() = (
            RemoteStatus {
                state: state.into(),
                host: Some("workbox".into()),
                message: Some("Connection lost".into()),
            },
            None,
        );
        assert_eq!(
            get_status(&format!("{base}/api/preferences")),
            503,
            "{state} must not read Local"
        );
        let result = ureq::post(&format!("{base}/api/preferences/picker"))
            .set("Content-Type", "application/json")
            .send_string(r#"{"context":"session","path":"/wrong-machine"}"#);
        assert!(
            matches!(result, Err(ureq::Error::Status(503, _))),
            "{state} must not write Local"
        );
        assert_eq!(get_status(&format!("{base}/api/health")), 200);
        assert_eq!(get_status(&format!("{base}/api/remote/status")), 200);
        assert_eq!(
            open_editor(&base),
            503,
            "{state} must not open the remote folder locally"
        );
        assert_eq!(editor.0.lock().unwrap().len(), 1);
    }
    // Recovery uses the selected remote again; only explicit idle selects Local.
    *remote.0.lock().unwrap() = (
        RemoteStatus {
            state: "connected".into(),
            host: Some("workbox".into()),
            message: None,
        },
        Some(target),
    );
    assert_eq!(get_status(&format!("{base}/api/preferences")), 200);
    *remote.0.lock().unwrap() = (
        RemoteStatus {
            state: "idle".into(),
            host: None,
            message: None,
        },
        None,
    );
    assert_eq!(get_status(&format!("{base}/api/preferences")), 200);
    *local.0.lock().unwrap() = Err("Local host service is recovering".into());
    assert_eq!(get_status(&format!("{base}/api/preferences")), 503);
    assert_eq!(get_status(&format!("{base}/api/health")), 200);
    std::fs::remove_dir_all(dir).unwrap();
}
