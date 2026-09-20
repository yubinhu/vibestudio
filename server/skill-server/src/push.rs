//! Web Push sender — real notifications on phones with no vendor infrastructure.
//! The bell watcher (events.rs) calls [`notify_bells`] on every turn-finish edge;
//! each stored subscription gets an encrypted push POSTed straight to its
//! endpoint (Apple's is `web.push.apple.com`, which accepts HTTP/1.1 — so `ureq`
//! delivers without tokio). Payloads use the Declarative Web Push shape
//! (iOS 18.4+ renders them with no service-worker code); `sw.js` renders the
//! same JSON as a classic push on engines that don't.
//!
//! Crypto (all on `ring`, already in the lockfile via rustls):
//! - VAPID (RFC 8292): ES256 JWT over {aud, exp, sub}, signed with a P-256 key
//!   generated once and persisted 0600 in the config dir.
//! - Payload encryption (RFC 8291, aes128gcm): per-message ephemeral ECDH against
//!   the subscription's `p256dh`, HKDF-SHA256 keyed by its `auth` secret.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ring::rand::SecureRandom;
use ring::signature::KeyPair;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// A client that reported focus within this window suppresses pushes: someone is
/// looking at a live UI (desktop or phone), where the SSE dot/toast path already
/// announces the bell. Clients re-ping every 30s while focused.
const ATTENTION_FRESH: Duration = Duration::from_secs(60);

/// Pushes are pointless once the user has moved on — don't let Apple queue a
/// turn-finish for hours.
const TTL_SECS: u32 = 3600;

/// VAPID `sub` contact (spec-required; a bare localhost URL gets BadJwtToken).
fn contact() -> String {
    std::env::var("VIBESTUDIO_PUSH_CONTACT").unwrap_or_else(|_| "mailto:push@agentskills.io".into())
}

// ───────────────────────────── persisted state ─────────────────────────────

#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub(crate) struct Subscription {
    pub endpoint: String,
    /// Subscription public key (65-byte P-256 point), base64url.
    pub p256dh: String,
    /// 16-byte auth secret, base64url.
    pub auth: String,
}

#[derive(Serialize, Deserialize, Default)]
struct Store {
    /// PKCS#8 of the VAPID signing key, base64 (standard). Generated on first use.
    #[serde(default)]
    vapid_pkcs8: String,
    #[serde(default)]
    subs: Vec<Subscription>,
}

fn store_path() -> Result<std::path::PathBuf, String> {
    Ok(skill_core::paths::config_dir()?.join("push.json"))
}

fn store_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

fn load() -> Store {
    let Ok(path) = store_path() else {
        return Store::default();
    };
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => Store::default(),
    }
}

fn save(store: &Store) -> Result<(), String> {
    let dir = skill_core::paths::ensure_config_dir()?;
    let path = dir.join("push.json");
    let json = serde_json::to_vec_pretty(store).map_err(|e| e.to_string())?;
    // Atomic tmp+rename (the config store's convention): notify_bells reads
    // load() lock-free, so a plain truncate-then-write could be observed as a
    // partial/empty file — serde would collapse it to Store::default() and the
    // bell batch would be silently dropped. The pid keeps two processes' tmp
    // files from colliding.
    let tmp = dir.join(format!("push.json.tmp.{}", std::process::id()));
    std::fs::write(&tmp, &json).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(())
}

/// The VAPID keypair, generating + persisting it on first use.
fn vapid_key() -> Result<ring::signature::EcdsaKeyPair, String> {
    let rng = ring::rand::SystemRandom::new();
    let alg = &ring::signature::ECDSA_P256_SHA256_FIXED_SIGNING;
    let _guard = store_lock();
    let mut store = load();
    if store.vapid_pkcs8.is_empty() {
        let doc = ring::signature::EcdsaKeyPair::generate_pkcs8(alg, &rng)
            .map_err(|_| "VAPID key generation failed".to_string())?;
        store.vapid_pkcs8 = base64::engine::general_purpose::STANDARD.encode(doc.as_ref());
        save(&store)?;
    }
    let pkcs8 = base64::engine::general_purpose::STANDARD
        .decode(&store.vapid_pkcs8)
        .map_err(|e| format!("corrupt VAPID key: {e}"))?;
    ring::signature::EcdsaKeyPair::from_pkcs8(alg, &pkcs8, &rng)
        .map_err(|_| "corrupt VAPID key".to_string())
}

/// The raw 65-byte public key, base64url — what `PushManager.subscribe` takes as
/// `applicationServerKey`.
pub(crate) fn public_key() -> Result<String, String> {
    Ok(URL_SAFE_NO_PAD.encode(vapid_key()?.public_key().as_ref()))
}

pub(crate) fn add_subscription(sub: Subscription) -> Result<usize, String> {
    if !sub.endpoint.starts_with("https://") {
        return Err("subscription endpoint must be https".into());
    }
    // Reject malformed keys at the door, not at send time (65-byte P-256 point,
    // 16-byte auth secret — RFC 8291).
    let p256dh = URL_SAFE_NO_PAD
        .decode(&sub.p256dh)
        .map_err(|e| format!("bad p256dh: {e}"))?;
    let auth = URL_SAFE_NO_PAD
        .decode(&sub.auth)
        .map_err(|e| format!("bad auth: {e}"))?;
    if p256dh.len() != 65 || auth.len() != 16 {
        return Err("malformed subscription keys".into());
    }
    let _guard = store_lock();
    let mut store = load();
    store.subs.retain(|s| s.endpoint != sub.endpoint);
    store.subs.push(sub);
    save(&store)?;
    Ok(store.subs.len())
}

pub(crate) fn remove_subscription(endpoint: &str) -> Result<usize, String> {
    let _guard = store_lock();
    let mut store = load();
    store.subs.retain(|s| s.endpoint != endpoint);
    save(&store)?;
    Ok(store.subs.len())
}

// ───────────────────────────── attention (suppression) ─────────────────────────────

fn attention() -> MutexGuard<'static, HashMap<String, Instant>> {
    static ATT: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    ATT.get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

/// A UI reported a focus edge. `client` is a per-tab id; any client focused
/// within [`ATTENTION_FRESH`] suppresses pushes.
pub(crate) fn set_attention(client: &str, focused: bool) {
    let mut att = attention();
    if focused {
        att.insert(client.to_string(), Instant::now());
    } else {
        att.remove(client);
    }
    // `elapsed()`, never `Instant::now() - FRESH`: the latter underflow-panics
    // when monotonic uptime is below FRESH (a machine/WSL-VM booted <60s ago),
    // which would kill this pooled worker thread for the process lifetime.
    att.retain(|_, at| at.elapsed() < ATTENTION_FRESH);
}

fn someone_watching() -> bool {
    attention()
        .values()
        .any(|at| at.elapsed() < ATTENTION_FRESH)
}

// ───────────────────────────── sending ─────────────────────────────

/// One bell edge, as the watcher sees it.
pub(crate) struct Bell {
    pub id: String,
    pub label: String,
    /// The agent's last output line (a captured preview), or `None` when the pane
    /// held nothing substantive — then the body falls back to the fixed phrase.
    pub last: Option<String>,
    /// Typed detector edge. None preserves the legacy terminal-bell fallback.
    pub kind: Option<skill_core::agent_detection::AttentionKind>,
}

/// Declarative Web Push payload (Safari 18.4+ renders it OS-side; sw.js renders
/// the same JSON as a classic push elsewhere).
fn payload(bell: &Bell) -> Vec<u8> {
    use skill_core::agent_detection::AttentionKind;
    let (title, fallback) = match bell.kind {
        Some(AttentionKind::Request) => (
            format!("{} needs attention", bell.label),
            "Your turn — the agent needs input.",
        ),
        Some(AttentionKind::Done) => (
            format!("{} finished", bell.label),
            "Your turn — the agent finished.",
        ),
        None => (bell.label.clone(), "Your turn — the agent finished."),
    };
    let body = bell.last.as_deref().unwrap_or(fallback);
    json!({
        "web_push": 8030,
        "notification": {
            "title": title,
            "body": body,
            "navigate": format!("/#/terminals?id={}", urlencoding::encode(&bell.id)),
            // Not part of the declarative schema (iOS ignores it); sw.js uses it
            // so a classic push replaces the page's same-session web banner
            // instead of stacking a duplicate.
            "tag": bell.id,
        }
    })
    .to_string()
    .into_bytes()
}

/// Fired by the bell watcher. Skips entirely while any UI is focused (the SSE
/// dot/toast path owns that case); otherwise pushes every bell to every
/// subscription on a detached thread (ureq blocks; the watcher must not).
pub(crate) fn notify_bells(bells: Vec<Bell>) {
    if bells.is_empty() || someone_watching() {
        return;
    }
    let subs = { load().subs };
    if subs.is_empty() {
        return;
    }
    std::thread::spawn(move || {
        for bell in &bells {
            for sub in &subs {
                match send_one(sub, &payload(bell), post_push) {
                    Ok(status) if status == 404 || status == 410 => {
                        log::info!("push: pruning dead subscription ({status})");
                        let _ = remove_subscription(&sub.endpoint);
                    }
                    Ok(status) if status >= 400 => {
                        log::warn!("push: endpoint returned {status}");
                    }
                    Ok(_) => {}
                    Err(e) => log::warn!("push: send failed: {e}"),
                }
            }
        }
    });
}

/// The HTTP transport: (endpoint, headers, body) → response status.
type Transport = fn(&str, &[(&str, String)], Vec<u8>) -> Result<u16, String>;

/// Encrypt + send one message to one subscription. The transport is injected so
/// tests (and a future h2/curl fallback, should Apple tighten ALPN on the
/// HTTP/1.1-tolerant web.push.apple.com) swap without touching the crypto.
fn send_one(sub: &Subscription, plaintext: &[u8], transport: Transport) -> Result<u16, String> {
    let body = encrypt(sub, plaintext)?;
    let jwt = vapid_jwt(&sub.endpoint)?;
    let k = public_key()?;
    let headers = [
        ("Authorization", format!("vapid t={jwt}, k={k}")),
        ("TTL", TTL_SECS.to_string()),
        ("Urgency", "high".to_string()),
        ("Content-Encoding", "aes128gcm".to_string()),
    ];
    transport(&sub.endpoint, &headers, body)
}

/// The real transport: a blocking HTTP/1.1 POST. Isolated on purpose — see
/// `send_one`.
fn post_push(endpoint: &str, headers: &[(&str, String)], body: Vec<u8>) -> Result<u16, String> {
    let mut req = ureq::post(endpoint).timeout(std::time::Duration::from_secs(20));
    for (k, v) in headers {
        req = req.set(k, v);
    }
    match req.send_bytes(&body) {
        Ok(resp) => Ok(resp.status()),
        Err(ureq::Error::Status(code, _)) => Ok(code),
        Err(e) => Err(e.to_string()),
    }
}

/// ES256 VAPID JWT for the endpoint's origin (RFC 8292).
fn vapid_jwt(endpoint: &str) -> Result<String, String> {
    let aud = origin_of(endpoint)?;
    let exp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs()
        + 12 * 3600;
    let header = URL_SAFE_NO_PAD.encode(br#"{"typ":"JWT","alg":"ES256"}"#);
    let claims = URL_SAFE_NO_PAD.encode(
        json!({ "aud": aud, "exp": exp, "sub": contact() })
            .to_string()
            .as_bytes(),
    );
    let signing_input = format!("{header}.{claims}");
    let rng = ring::rand::SystemRandom::new();
    let sig = vapid_key()?
        .sign(&rng, signing_input.as_bytes())
        .map_err(|_| "VAPID signing failed".to_string())?;
    Ok(format!(
        "{signing_input}.{}",
        URL_SAFE_NO_PAD.encode(sig.as_ref())
    ))
}

/// `https://host[:port]` of a push endpoint URL — the JWT `aud` claim.
fn origin_of(endpoint: &str) -> Result<String, String> {
    let rest = endpoint
        .strip_prefix("https://")
        .ok_or_else(|| "push endpoint must be https".to_string())?;
    let host = rest.split('/').next().unwrap_or(rest);
    if host.is_empty() {
        return Err("push endpoint has no host".into());
    }
    Ok(format!("https://{host}"))
}

// ───────────────────────────── RFC 8291 encryption ─────────────────────────────

struct HkdfLen(usize);
impl ring::hkdf::KeyType for HkdfLen {
    fn len(&self) -> usize {
        self.0
    }
}

fn hkdf(salt: &[u8], ikm: &[u8], info: &[u8], out: &mut [u8]) -> Result<(), String> {
    ring::hkdf::Salt::new(ring::hkdf::HKDF_SHA256, salt)
        .extract(ikm)
        .expand(&[info], HkdfLen(out.len()))
        .and_then(|okm| okm.fill(out))
        .map_err(|_| "HKDF failed".to_string())
}

/// RFC 8291 aes128gcm: single record, sender ("as") ephemeral key in the header.
fn encrypt(sub: &Subscription, plaintext: &[u8]) -> Result<Vec<u8>, String> {
    let ua_pub = URL_SAFE_NO_PAD
        .decode(&sub.p256dh)
        .map_err(|e| format!("bad p256dh: {e}"))?;
    let auth = URL_SAFE_NO_PAD
        .decode(&sub.auth)
        .map_err(|e| format!("bad auth: {e}"))?;
    if ua_pub.len() != 65 || auth.len() != 16 {
        return Err("malformed subscription keys".into());
    }
    let rng = ring::rand::SystemRandom::new();

    let eph = ring::agreement::EphemeralPrivateKey::generate(&ring::agreement::ECDH_P256, &rng)
        .map_err(|_| "ECDH keygen failed".to_string())?;
    let as_pub = eph
        .compute_public_key()
        .map_err(|_| "ECDH pubkey failed".to_string())?;
    let as_pub = as_pub.as_ref().to_vec();
    let peer = ring::agreement::UnparsedPublicKey::new(&ring::agreement::ECDH_P256, ua_pub.clone());
    let shared = ring::agreement::agree_ephemeral(eph, &peer, |secret| secret.to_vec())
        .map_err(|_| "ECDH agreement failed".to_string())?;

    // ikm = HKDF(salt=auth, ikm=shared, info="WebPush: info\0" || ua_pub || as_pub)
    let mut info = Vec::with_capacity(14 + 65 + 65);
    info.extend_from_slice(b"WebPush: info\0");
    info.extend_from_slice(&ua_pub);
    info.extend_from_slice(&as_pub);
    let mut ikm = [0u8; 32];
    hkdf(&auth, &shared, &info, &mut ikm)?;

    let mut salt = [0u8; 16];
    rng.fill(&mut salt)
        .map_err(|_| "salt generation failed".to_string())?;
    let mut cek = [0u8; 16];
    hkdf(&salt, &ikm, b"Content-Encoding: aes128gcm\0", &mut cek)?;
    let mut nonce = [0u8; 12];
    hkdf(&salt, &ikm, b"Content-Encoding: nonce\0", &mut nonce)?;

    // Single record: plaintext || 0x02 (final-record delimiter), sealed in place.
    let mut record = plaintext.to_vec();
    record.push(0x02);
    let key = ring::aead::LessSafeKey::new(
        ring::aead::UnboundKey::new(&ring::aead::AES_128_GCM, &cek)
            .map_err(|_| "AEAD key failed".to_string())?,
    );
    key.seal_in_place_append_tag(
        ring::aead::Nonce::assume_unique_for_key(nonce),
        ring::aead::Aad::empty(),
        &mut record,
    )
    .map_err(|_| "AEAD seal failed".to_string())?;

    // aes128gcm header: salt(16) || rs(4, BE) || idlen(1) || keyid(as_pub, 65)
    let rs: u32 = 4096;
    let mut out = Vec::with_capacity(16 + 4 + 1 + 65 + record.len());
    out.extend_from_slice(&salt);
    out.extend_from_slice(&rs.to_be_bytes());
    out.push(65);
    out.extend_from_slice(&as_pub);
    out.extend_from_slice(&record);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attention_payload_distinguishes_request_from_completion() {
        use skill_core::agent_detection::AttentionKind;
        let notice = |kind| Bell {
            id: "ass-1".into(),
            label: "Codex".into(),
            last: None,
            kind,
        };
        let request: serde_json::Value =
            serde_json::from_slice(&payload(&notice(Some(AttentionKind::Request)))).unwrap();
        let done: serde_json::Value =
            serde_json::from_slice(&payload(&notice(Some(AttentionKind::Done)))).unwrap();
        assert_eq!(request["notification"]["title"], "Codex needs attention");
        assert_eq!(
            request["notification"]["body"],
            "Your turn — the agent needs input."
        );
        assert_eq!(done["notification"]["title"], "Codex finished");
        assert_eq!(
            done["notification"]["body"],
            "Your turn — the agent finished."
        );
    }

    /// Receiver ("ua") side of RFC 8291, so the roundtrip proves our sender against
    /// an independent implementation of the spec's key schedule.
    fn decrypt(
        ua_priv: ring::agreement::EphemeralPrivateKey,
        ua_pub: &[u8],
        auth: &[u8],
        msg: &[u8],
    ) -> Vec<u8> {
        let salt = &msg[..16];
        let idlen = msg[20] as usize;
        assert_eq!(idlen, 65, "keyid must be the sender public key");
        let as_pub = &msg[21..21 + 65];
        let ciphertext = &msg[21 + 65..];

        let peer =
            ring::agreement::UnparsedPublicKey::new(&ring::agreement::ECDH_P256, as_pub.to_vec());
        let shared =
            ring::agreement::agree_ephemeral(ua_priv, &peer, |s| s.to_vec()).expect("ua ECDH");

        let mut info = Vec::new();
        info.extend_from_slice(b"WebPush: info\0");
        info.extend_from_slice(ua_pub);
        info.extend_from_slice(as_pub);
        let mut ikm = [0u8; 32];
        hkdf(auth, &shared, &info, &mut ikm).unwrap();
        let mut cek = [0u8; 16];
        hkdf(salt, &ikm, b"Content-Encoding: aes128gcm\0", &mut cek).unwrap();
        let mut nonce = [0u8; 12];
        hkdf(salt, &ikm, b"Content-Encoding: nonce\0", &mut nonce).unwrap();

        let key = ring::aead::LessSafeKey::new(
            ring::aead::UnboundKey::new(&ring::aead::AES_128_GCM, &cek).unwrap(),
        );
        let mut buf = ciphertext.to_vec();
        let plain = key
            .open_in_place(
                ring::aead::Nonce::assume_unique_for_key(nonce),
                ring::aead::Aad::empty(),
                &mut buf,
            )
            .expect("AEAD open");
        assert_eq!(plain.last(), Some(&0x02), "final-record delimiter");
        plain[..plain.len() - 1].to_vec()
    }

    #[test]
    fn rfc8291_roundtrip() {
        let rng = ring::rand::SystemRandom::new();
        let ua_priv =
            ring::agreement::EphemeralPrivateKey::generate(&ring::agreement::ECDH_P256, &rng)
                .unwrap();
        let ua_pub = ua_priv.compute_public_key().unwrap().as_ref().to_vec();
        let mut auth = [0u8; 16];
        rng.fill(&mut auth).unwrap();

        let sub = Subscription {
            endpoint: "https://web.push.apple.com/x".into(),
            p256dh: URL_SAFE_NO_PAD.encode(&ua_pub),
            auth: URL_SAFE_NO_PAD.encode(auth),
        };
        let msg = encrypt(&sub, b"{\"web_push\":8030}").unwrap();
        assert_eq!(&msg[16..20], &4096u32.to_be_bytes(), "record size");
        let plain = decrypt(ua_priv, &ua_pub, &auth, &msg);
        assert_eq!(plain, b"{\"web_push\":8030}");
    }

    #[test]
    fn vapid_jwt_verifies_and_has_correct_claims() {
        let _env = TempConfig::new();
        let before = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs();
        let jwt = vapid_jwt("https://web.push.apple.com/QLpq/abc").unwrap();
        let after = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs();
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);

        let header: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[0]).unwrap()).unwrap();
        assert_eq!(header["alg"], "ES256");
        let claims: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
        assert_eq!(claims["aud"], "https://web.push.apple.com");
        assert!(claims["sub"].as_str().unwrap().starts_with("mailto:"));
        let expires = claims["exp"].as_u64().unwrap();
        assert!(expires > after, "JWT must still be valid when returned");
        assert!(expires <= before + 24 * 3600, "VAPID validity must not exceed 24 hours");

        // Verify the signature against the advertised public key.
        let pubkey = URL_SAFE_NO_PAD.decode(public_key().unwrap()).unwrap();
        let msg = format!("{}.{}", parts[0], parts[1]);
        let sig = URL_SAFE_NO_PAD.decode(parts[2]).unwrap();
        ring::signature::UnparsedPublicKey::new(&ring::signature::ECDSA_P256_SHA256_FIXED, &pubkey)
            .verify(msg.as_bytes(), &sig)
            .expect("JWT signature must verify");
    }

    #[test]
    fn store_roundtrip_and_prune() {
        let _env = TempConfig::new();
        let sub = |ep: &str| Subscription {
            endpoint: ep.into(),
            p256dh: URL_SAFE_NO_PAD.encode([4u8; 65]),
            auth: URL_SAFE_NO_PAD.encode([7u8; 16]),
        };
        assert_eq!(add_subscription(sub("https://a/1")).unwrap(), 1);
        assert_eq!(add_subscription(sub("https://b/2")).unwrap(), 2);
        // Re-subscribing the same endpoint replaces, not duplicates.
        assert_eq!(add_subscription(sub("https://a/1")).unwrap(), 2);
        assert_eq!(remove_subscription("https://a/1").unwrap(), 1);
        assert!(add_subscription(sub("http://insecure/x")).is_err());
    }

    #[test]
    fn attention_suppresses_and_expires() {
        set_attention("tab-1", true);
        assert!(someone_watching());
        set_attention("tab-1", false);
        assert!(!someone_watching());
    }

    #[test]
    fn attention_never_underflow_panics_early_after_boot() {
        // On a machine booted <60s ago, monotonic uptime < ATTENTION_FRESH, so
        // any `Instant::now() - ATTENTION_FRESH` would panic. `elapsed()` can't —
        // this just proves the calls are reachable without one.
        set_attention("boot", true);
        let _ = someone_watching();
        set_attention("boot", false);
    }

    /// Point the config dir at a fresh tempdir for the test's duration. Tests
    /// touching the store run serially (env var is process-global).
    struct TempConfig {
        dir: std::path::PathBuf,
        prev: Option<String>,
        _guard: MutexGuard<'static, ()>,
    }
    impl TempConfig {
        fn new() -> Self {
            static SERIAL: OnceLock<Mutex<()>> = OnceLock::new();
            let guard = SERIAL
                .get_or_init(Mutex::default)
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let dir = std::env::temp_dir().join(format!("ss-push-test-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let prev = std::env::var("XDG_CONFIG_HOME").ok();
            std::env::set_var("XDG_CONFIG_HOME", &dir);
            TempConfig {
                dir,
                prev,
                _guard: guard,
            }
        }
    }
    impl Drop for TempConfig {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}
