//! End-to-end proof that the WHOLE switchboard works over russh — not just the transport
//! primitives (those are covered in `sshmgr::russh_tx`), but the real connect orchestration:
//! detect (uname over russh) → provision (reuse the pre-placed binary) → launch the server on
//! the remote → russh `-L` forward → the remote server answers HTTP through the tunnel.
//!
//! Explicitly ignored: needs a disposable local SSH account and a matching `skill-server`.
//! Prepare the account's versioned install using the version printed by the built binary:
//!   cargo build -p skill-server
//!   target/debug/skill-server --version
//!   # Place that binary at ~/.vibestudio/server/<printed-version>/skill-server in the SSH account.
//!   RUSSH_E2E=1 RUSSH_IT_HOST=127.0.0.1 RUSSH_IT_PORT=2222 RUSSH_IT_USER=$USER \
//!     RUSSH_IT_KEY=<key> cargo test -p skill-server --features russh-transport --test russh_e2e -- --ignored --nocapture
//! The owning harness must stop the detached host afterward; disconnect only closes the tunnel.
#![cfg(feature = "russh-transport")]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use skill_server::{RemoteControl, SecureStore, SshProfile, SshRemoteControl};

/// Must match the version dir the harness places the binary under.
const VERSION: &str = env!("CARGO_PKG_VERSION");

fn env() -> (String, String, String, String) {
    assert_eq!(std::env::var("RUSSH_E2E").ok().as_deref(), Some("1"),
        "set RUSSH_E2E=1 to run against the disposable SSH fixture");
    let required = |name| {
        std::env::var(name).ok().filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| panic!("{name} is required for the real-SSH test"))
    };
    (
        required("RUSSH_IT_HOST"),
        required("RUSSH_IT_PORT"),
        required("RUSSH_IT_USER"),
        required("RUSSH_IT_KEY"),
    )
}

#[test]
#[ignore = "requires RUSSH_E2E=1, RUSSH_IT_* and a disposable SSH host with the matching server binary"]
fn full_switchboard_over_russh() {
    let (host, port, user, key) = env();

    // Force the russh transport (creds_for reads these); this is exactly what the mobile
    // switchboard does, minus the stored-profile lookup.
    std::env::set_var("VIBESTUDIO_RUSSH", "1");
    std::env::set_var("VIBESTUDIO_RUSSH_KEY", &key);

    let ctrl = SshRemoteControl::new(VERSION.to_string());
    let target = format!("{user}@{host}:{port}");
    ctrl.connect(&target).expect("connect kickoff");

    // Poll the async connect to a terminal state.
    let deadline = Instant::now() + Duration::from_secs(60);
    let base = loop {
        let st = ctrl.status();
        match st.state.as_str() {
            "connected" => break ctrl.active_target().expect("target when connected").base_url,
            "error" => panic!("connect failed: {:?}", st.message),
            _ if Instant::now() >= deadline => panic!("timed out in state {:?}: {:?}", st.state, st.message),
            _ => std::thread::sleep(Duration::from_millis(300)),
        }
    };

    // base_url is the forwarded local port → hit the REMOTE server's /api/health through the
    // russh tunnel. The returned pid is the launched remote server's, proving we reached it
    // (not something local).
    let body = http_get(&base, "/api/health").expect("GET /api/health through the tunnel");
    assert!(body.contains("\"version\""), "health via tunnel missing version: {body}");
    assert!(body.contains("\"pid\""), "health via tunnel missing pid: {body}");

    ctrl.disconnect(true).expect("disconnect");
}

/// The MOBILE credential path, end to end: a saved profile in a [`SecureStore`] whose key is
/// in-memory OpenSSH text (on device it comes from the iOS Keychain; here from the test key
/// file) resolves a connect id and drives the whole switchboard — no `VIBESTUDIO_RUSSH` env,
/// no key path, no `ssh` binary.
///
/// The connect id is a bare ALIAS (`stored-alias`), not `user@host:port`, on purpose: the two
/// e2e tests share a process, and the sibling above sets `VIBESTUDIO_RUSSH=1` +
/// `VIBESTUDIO_RUSSH_KEY`. An alias can't be parsed as `user@host[:port]`, so `creds_for`'s env
/// fallback yields `None` for it — the connection can ONLY succeed via the store. If the store
/// thread-through ever regresses, this test fails instead of silently passing on leaked env creds.
#[test]
#[ignore = "requires RUSSH_E2E=1, RUSSH_IT_* and a disposable SSH host with the matching server binary"]
fn full_switchboard_from_a_stored_profile() {
    let (host, port, user, key) = env();

    struct OneProfile {
        profile: SshProfile,
        key_text: String,
    }
    impl SecureStore for OneProfile {
        fn list_profiles(&self) -> Result<Vec<SshProfile>, String> {
            Ok(vec![self.profile.clone()])
        }
        fn get_profile(&self, id: &str) -> Result<Option<SshProfile>, String> {
            Ok((self.profile.id == id).then(|| self.profile.clone()))
        }
        fn put_profile(&self, _p: &SshProfile, _k: &str) -> Result<(), String> {
            unimplemented!("read-only test store")
        }
        fn delete_profile(&self, _id: &str) -> Result<(), String> {
            unimplemented!("read-only test store")
        }
        fn get_private_key(&self, id: &str) -> Result<Option<String>, String> {
            Ok((self.profile.id == id).then(|| self.key_text.clone()))
        }
    }

    let id = "stored-alias".to_string(); // NOT user@host:port — the env fallback can't satisfy it
    let store = OneProfile {
        profile: SshProfile {
            id: id.clone(),
            host: host.clone(),
            port: port.parse().expect("RUSSH_IT_PORT"),
            user: user.clone(),
        },
        key_text: std::fs::read_to_string(&key).expect("read the test key file"),
    };

    let ctrl = SshRemoteControl::with_secure_store(VERSION.to_string(), Some(Arc::new(store)));
    ctrl.connect(&id).expect("connect kickoff");

    let deadline = Instant::now() + Duration::from_secs(60);
    let base = loop {
        let st = ctrl.status();
        match st.state.as_str() {
            "connected" => break ctrl.active_target().expect("target when connected").base_url,
            "error" => panic!("connect failed: {:?}", st.message),
            _ if Instant::now() >= deadline => panic!("timed out in state {:?}: {:?}", st.state, st.message),
            _ => std::thread::sleep(Duration::from_millis(300)),
        }
    };

    let body = http_get(&base, "/api/health").expect("GET /api/health through the tunnel");
    assert!(body.contains("\"pid\""), "health via tunnel missing pid: {body}");

    ctrl.disconnect(true).expect("disconnect");
}

/// Minimal HTTP/1.1 GET so the test needs no HTTP dependency. `base` is `http://127.0.0.1:PORT`.
fn http_get(base: &str, path: &str) -> Result<String, String> {
    let addr = base.strip_prefix("http://").ok_or("base is not http://")?;
    let mut stream = TcpStream::connect(addr).map_err(|e| format!("connect {addr}: {e}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
    write!(stream, "GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n")
        .map_err(|e| e.to_string())?;
    let mut resp = String::new();
    stream.read_to_string(&mut resp).map_err(|e| e.to_string())?;
    let status = resp.lines().next().unwrap_or("");
    if !status.contains(" 200 ") {
        return Err(format!("non-200 through tunnel: {status:?}"));
    }
    Ok(resp)
}
