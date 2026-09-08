//! The connection-manager API (`/api/remote/*`), handled LOCALLY by the desktop's
//! in-process server — never proxied. `ctx.remote` is the desktop's SSH controller;
//! it's `None` on the standalone (remote) binary, where these routes 404. The token
//! is deliberately never surfaced here (it lives only in the proxy and the ssh
//! command line).
use serde_json::{json, Value};
use tiny_http::Method;

use crate::{Reply, ServerCtx, SshProfile};

pub fn handle(method: &Method, path: &str, body: &str, ctx: &ServerCtx) -> Reply {
    // Saved-connection profiles (the mobile switchboard's credential store) sit
    // under /api/remote/ so the dispatch loop pins them local, but they hang off
    // the SecureStore, not the connection manager — a server without one
    // (desktop, standalone) 404s and the SPA never shows the credential UI.
    if path.starts_with("/api/remote/profiles") {
        let Some(store) = ctx.secure_store.as_ref() else {
            return err(404, "Saved connections are not available on this server.");
        };
        return profiles(method, path, body, store.as_ref());
    }
    let Some(remote) = ctx.remote.as_ref() else {
        return err(404, "Remote control is not available on this server.");
    };
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);

    match (method, path) {
        (Method::Get, "/api/remote/list") => match remote.list_hosts() {
            Ok(hosts) => ok(&hosts),
            Err(e) => err(400, &e),
        },
        (Method::Get, "/api/remote/status") => ok(&remote.status()),
        (Method::Post, "/api/remote/retry") => match remote.retry() {
            Ok(()) => ok(&json!({ "ok": true })),
            Err(e) => err(400, &e),
        },
        // The host to auto-reconnect to on launch (VS Code-style). Always handled
        // locally — it's THIS machine's connection memory, not the remote's.
        (Method::Get, "/api/remote/last") => ok(&json!({ "host": remote.last_host() })),
        (Method::Post, "/api/remote/connect") => {
            let host = v.get("host").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
            if host.is_empty() {
                return err(400, "A host is required.");
            }
            // Reject anything that isn't a plain ssh destination. Critically, a value
            // starting with `-` (or carrying odd characters) could be parsed by the
            // `ssh` client as an OPTION (e.g. `-oProxyCommand=…`) rather than a host,
            // which is local command execution. The desktop also passes `--` before
            // the host as defense-in-depth, but the API rejects it outright.
            if !valid_host(&host) {
                return err(400, "Invalid host. Use an SSH alias or user@host[:port].");
            }
            match remote.connect(&host) {
                Ok(()) => ok(&json!({ "ok": true })),
                Err(e) => err(400, &e),
            }
        }
        (Method::Post, "/api/remote/disconnect") => {
            // An explicit disconnect means "I want Local" — `disconnect(true)` also
            // forgets the remembered resume host (atomically with invalidating any
            // in-flight connect) so the next launch starts Local. App-exit teardown
            // calls `disconnect(false)`, so quitting while connected still resumes.
            match remote.disconnect(true) {
                Ok(()) => ok(&json!({ "ok": true })),
                Err(e) => err(400, &e),
            }
        }
        _ => err(404, "Not found"),
    }
}

/// A plain ssh destination: an alias or `user@host[:port]`. No leading `-` (option
/// injection) and only characters that appear in real hostnames/users/aliases.
fn valid_host(h: &str) -> bool {
    !h.starts_with('-')
        && h.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '@' | ':'))
}

/// The saved-connection routes: list / save / delete profiles against the
/// device's [`crate::SecureStore`]. The private key travels in exactly once
/// (save) and is never read back out over HTTP.
fn profiles(method: &Method, path: &str, body: &str, store: &dyn crate::SecureStore) -> Reply {
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").trim().to_string();

    match (method, path) {
        (Method::Get, "/api/remote/profiles") => match store.list_profiles() {
            Ok(list) => ok(&list),
            Err(e) => err(400, &e),
        },
        (Method::Post, "/api/remote/profiles/save") => {
            let (host, user) = (s("host"), s("user"));
            let port = v.get("port").and_then(|x| x.as_u64()).unwrap_or(22);
            let key = v.get("privateKey").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
            // host/user become the `user@host:port` connection id, which must
            // survive the connect route's `valid_host` check — so hold each part
            // to the same alphabet (minus the separators themselves).
            let part_ok = |p: &str| {
                !p.is_empty()
                    && !p.starts_with('-')
                    && p.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
            };
            if !part_ok(&host) {
                return err(400, "Invalid host name.");
            }
            if !part_ok(&user) {
                return err(400, "Invalid user name.");
            }
            let port = match u16::try_from(port) {
                Ok(p) if p != 0 => p,
                _ => return err(400, "Invalid port."),
            };
            if let Err(e) = validate_private_key(&key) {
                return err(400, &e);
            }
            let profile = SshProfile { id: format!("{user}@{host}:{port}"), host, port, user };
            match store.put_profile(&profile, &key) {
                Ok(()) => ok(&json!({ "ok": true, "id": profile.id })),
                Err(e) => err(400, &e),
            }
        }
        (Method::Post, "/api/remote/profiles/delete") => {
            let id = s("id");
            if id.is_empty() {
                return err(400, "A profile id is required.");
            }
            match store.delete_profile(&id) {
                Ok(()) => ok(&json!({ "ok": true })),
                Err(e) => err(400, &e),
            }
        }
        _ => err(404, "Not found"),
    }
}

/// The key that will be handed to the russh transport at connect time — reject
/// anything it couldn't use NOW, so the failure surfaces at save (with the key
/// in hand) rather than as a cryptic connect error later. Passphrase-less only:
/// the store is the protection (OS keystore), and a passphrase would need its
/// own storage anyway.
fn validate_private_key(key: &str) -> Result<(), String> {
    if key.is_empty() {
        return Err("A private key is required — generate or paste one.".into());
    }
    #[cfg(feature = "russh-transport")]
    {
        russh::keys::decode_secret_key(key, None)
            .map(|_| ())
            .map_err(|e| format!("That private key can't be used: {e} (encrypted keys aren't supported — generate one in-app)."))
    }
    #[cfg(not(feature = "russh-transport"))]
    {
        // Without the transport there's nothing to decode with (and no mobile
        // build lacks it); hold the line at the obvious shape check.
        if key.starts_with("-----BEGIN OPENSSH PRIVATE KEY-----") {
            Ok(())
        } else {
            Err("That doesn't look like an OpenSSH private key.".into())
        }
    }
}

fn ok<T: serde::Serialize>(v: &T) -> Reply {
    Reply {
        status: 200,
        body: serde_json::to_vec(v).unwrap_or_default(),
        content_type: "application/json".into(),
        extra: vec![],
    }
}

fn err(status: u16, msg: &str) -> Reply {
    Reply {
        status,
        body: serde_json::to_vec(&json!({ "error": msg })).unwrap_or_default(),
        content_type: "application/json".into(),
        extra: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SecureStore as _; // trait methods on the Arc<MemStore> assertions
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    /// In-memory SecureStore with real put/delete, standing in for the Keychain one.
    #[derive(Default)]
    struct MemStore {
        profiles: Mutex<Vec<SshProfile>>,
        keys: Mutex<HashMap<String, String>>,
    }

    impl crate::SecureStore for MemStore {
        fn list_profiles(&self) -> Result<Vec<SshProfile>, String> {
            Ok(self.profiles.lock().unwrap().clone())
        }
        fn get_profile(&self, id: &str) -> Result<Option<SshProfile>, String> {
            Ok(self.profiles.lock().unwrap().iter().find(|p| p.id == id).cloned())
        }
        fn put_profile(&self, profile: &SshProfile, key: &str) -> Result<(), String> {
            let mut ps = self.profiles.lock().unwrap();
            ps.retain(|p| p.id != profile.id);
            ps.push(profile.clone());
            self.keys.lock().unwrap().insert(profile.id.clone(), key.to_string());
            Ok(())
        }
        fn delete_profile(&self, id: &str) -> Result<(), String> {
            self.profiles.lock().unwrap().retain(|p| p.id != id);
            self.keys.lock().unwrap().remove(id);
            Ok(())
        }
        fn get_private_key(&self, id: &str) -> Result<Option<String>, String> {
            Ok(self.keys.lock().unwrap().get(id).cloned())
        }
    }

    fn ctx(store: Option<Arc<dyn crate::SecureStore>>) -> crate::ServerCtx {
        crate::ServerCtx {
            dist: "dist".into(),
            bundled_skills: None,
            examples_base: None,
            token: None,
            remote: None,
            local_backend: None,
            #[cfg(feature = "local-backend")]
            host_service_identity: None,
            phone: None,
            notifier: None,
            editor: None,
            secure_store: store,
            port: 0,
        }
    }

    fn call(method: Method, path: &str, body: &str, ctx: &crate::ServerCtx) -> (u16, Value) {
        let r = handle(&method, path, body, ctx);
        (r.status, serde_json::from_slice(&r.body).unwrap_or(Value::Null))
    }

    /// A private key the save route's validation accepts. With the transport
    /// compiled in it must be a REAL decodable key (validation decodes it).
    fn test_key() -> String {
        #[cfg(feature = "russh-transport")]
        {
            crate::sshmgr::keygen::generate_ed25519("test@remote-api").expect("keygen").private_openssh
        }
        #[cfg(not(feature = "russh-transport"))]
        {
            "-----BEGIN OPENSSH PRIVATE KEY-----\nnot-checked-without-the-transport\n-----END OPENSSH PRIVATE KEY-----\n".into()
        }
    }

    // No SecureStore (desktop, standalone) ⇒ the profile routes 404, which is the
    // SPA's cue to hide the credential UI entirely.
    #[test]
    fn profiles_404_without_a_store() {
        let (status, v) = call(Method::Get, "/api/remote/profiles", "", &ctx(None));
        assert_eq!(status, 404, "got {v}");
    }

    // The full save → list → delete cycle: the id is minted as user@host:port (the
    // exact string the connect route accepts), the key lands in the store, and the
    // key is never echoed back on any read path.
    #[test]
    fn save_list_delete_roundtrip() {
        let store = Arc::new(MemStore::default());
        let ctx = ctx(Some(store.clone()));
        let body = json!({ "host": "pi.local", "user": "harvey", "port": 2022, "privateKey": test_key() }).to_string();
        let (status, v) = call(Method::Post, "/api/remote/profiles/save", &body, &ctx);
        assert_eq!(status, 200, "save failed: {v}");
        assert_eq!(v["id"], "harvey@pi.local:2022");
        assert!(valid_host(v["id"].as_str().unwrap()), "the minted id must pass the connect route's host check");

        let (status, v) = call(Method::Get, "/api/remote/profiles", "", &ctx);
        assert_eq!(status, 200);
        assert_eq!(v[0]["id"], "harvey@pi.local:2022");
        assert!(!v.to_string().to_lowercase().contains("key"), "no key material on the list route: {v}");
        assert!(store.get_private_key("harvey@pi.local:2022").unwrap().is_some(), "key stored");

        let (status, _) = call(Method::Post, "/api/remote/profiles/delete", &json!({ "id": "harvey@pi.local:2022" }).to_string(), &ctx);
        assert_eq!(status, 200);
        assert!(store.list_profiles().unwrap().is_empty());
        assert!(store.get_private_key("harvey@pi.local:2022").unwrap().is_none(), "key removed with the profile");
    }

    // The saved host/user become a connect id, so each part is held to the same
    // alphabet valid_host enforces — an option-injection host (`-oProxyCommand=…`)
    // or a user smuggling separators must be rejected at save.
    #[test]
    fn save_rejects_hostile_parts() {
        let ctx = ctx(Some(Arc::new(MemStore::default())));
        for (host, user, port) in [
            ("-oProxyCommand=evil", "harvey", 22u64),
            ("pi.local", "a@b", 22),
            ("pi local", "harvey", 22),
            ("pi.local", "harvey", 0),
            ("pi.local", "harvey", 70000),
            ("", "harvey", 22),
        ] {
            let body = json!({ "host": host, "user": user, "port": port, "privateKey": test_key() }).to_string();
            let (status, v) = call(Method::Post, "/api/remote/profiles/save", &body, &ctx);
            assert_eq!(status, 400, "({host}, {user}, {port}) must be rejected, got {v}");
        }
    }

    // Garbage key material fails at save time (with the key in hand), not as a
    // cryptic connect error later.
    #[test]
    fn save_rejects_a_bad_key() {
        let ctx = ctx(Some(Arc::new(MemStore::default())));
        let body = json!({ "host": "pi.local", "user": "harvey", "privateKey": "not a key" }).to_string();
        let (status, _) = call(Method::Post, "/api/remote/profiles/save", &body, &ctx);
        assert_eq!(status, 400);
    }
}
