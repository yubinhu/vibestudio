//! Remote OS/arch detection + version-pinned `skill-server` provisioning — modelled
//! on how VS Code bootstraps its server: detect the platform, reuse an already
//! installed binary at a versioned path, else download it (remote `curl`/`wget`, or,
//! for a no-internet remote, a local download piped over the SSH connection).
use std::io::Read;
use std::time::Duration;

use sha2::{Digest, Sha256};

use super::conn::Remote;

/// Idempotent remote install: reuse a runnable binary, else download (curl/wget) to a
/// temp file, verify it against the published `.sha256` (best-effort — skipped only if
/// the checksum asset or a hasher is absent), and atomically move it into place. Exit
/// codes the caller interprets: 3 = no downloader (→ pipe fallback), 4 = checksum
/// mismatch. `__VERSION__`/`__URL__` are substituted (raw string ⇒ shell braces are
/// literal, unlike `format!`).
const INSTALL_SCRIPT: &str = r#"set -e
ver="__VERSION__"
dir="$HOME/.vibestudio/server/$ver"
bin="$dir/skill-server"
if [ -x "$bin" ] && "$bin" --version >/dev/null 2>&1; then echo INSTALLED; exit 0; fi
mkdir -p "$dir"
url="__URL__"
tmp="$bin.tmp.$$"
dl() {
  if command -v curl >/dev/null 2>&1; then curl -fsSL "$1" -o "$2"; return $?; fi
  if command -v wget >/dev/null 2>&1; then wget -qO "$2" "$1"; return $?; fi
  return 3
}
if dl "$url" "$tmp"; then :; else
  rc=$?
  if [ "$rc" = 3 ]; then echo NO_DOWNLOADER >&2; exit 3; fi
  echo DOWNLOAD_FAILED >&2; exit 1
fi
expected=$(dl "$url.sha256" - 2>/dev/null | awk '{print $1}')
if [ -n "$expected" ]; then
  if command -v sha256sum >/dev/null 2>&1; then actual=$(sha256sum "$tmp" | awk '{print $1}')
  elif command -v shasum >/dev/null 2>&1; then actual=$(shasum -a 256 "$tmp" | awk '{print $1}')
  else actual=""; fi
  if [ -n "$actual" ] && [ "$expected" != "$actual" ]; then rm -f "$tmp"; echo CHECKSUM_MISMATCH >&2; exit 4; fi
fi
chmod +x "$tmp"
mv -f "$tmp" "$bin"
echo DOWNLOADED
"#;

/// The no-downloader fallback's remote side: receive the binary on stdin, install it.
const PIPE_SCRIPT: &str = r#"set -e
dir="$HOME/.vibestudio/server/__VERSION__"
mkdir -p "$dir"
tmp="$dir/skill-server.tmp.$$"
cat > "$tmp"
chmod +x "$tmp"
mv -f "$tmp" "$dir/skill-server"
"#;

/// How many version-pinned `skill-server` binaries to keep on a remote. Each connect
/// provisions one under `~/.vibestudio/server/<version>/`; without pruning, iterating
/// on the app (a new version per release) would pile them up forever. We retain the most
/// recently used few and delete the rest.
const KEEP_VERSIONS: usize = 3;

/// Remote-side prune: mark the version we just provisioned as most-recently-used, then
/// delete all but the newest `KEEP_VERSIONS` version directories under
/// `~/.vibestudio/server`. mtime-ordered with a touch-on-use, so it's effectively LRU
/// and the version we're about to launch is always kept. Deleting a binary another client
/// still has running is safe on Unix (the live process keeps its open inode); that client
/// just re-downloads on its next connect. `__VERSION__`/`__KEEP_PLUS_1__` are substituted
/// (raw string ⇒ literal shell braces, like the install scripts above).
const PRUNE_SCRIPT: &str = r#"set -e
root="$HOME/.vibestudio/server"
cur="__VERSION__"
[ -d "$root" ] || exit 0
[ -e "$root/$cur" ] && touch "$root/$cur" 2>/dev/null || true
ls -1dt "$root"/*/ 2>/dev/null | tail -n +__KEEP_PLUS_1__ | while IFS= read -r d; do
  rm -rf "$d"
done
exit 0
"#;

/// The release version whose `skill-server` asset we prefer. Defaults to the running
/// app's version (`app_version`, from `tauri.conf.json`, which CI stamps from the
/// release tag); override with `VIBESTUDIO_SERVER_VERSION`. A released build's version
/// exact-matches its tag; an unstamped dev build sits at the placeholder `0.0.0` that
/// was never released, so `candidate_urls` falls back to the latest release.
pub fn server_version(app_version: &str) -> String {
    std::env::var("VIBESTUDIO_SERVER_VERSION")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| app_version.to_string())
}

/// The asset URLs to try, in order: the version-pinned release first (so a released
/// build pins the remote server to the exact version it ships, never drifting to a
/// newer/incompatible API), then the latest release (so an unstamped dev build — or any
/// version that was never published — still resolves to something current). Override the
/// whole scheme with `VIBESTUDIO_SERVER_BASE_URL`. At each location try the current
/// asset name, then its legacy `skill-server-<target>` name before moving on. A custom
/// base is exclusive: neither of its candidates falls back to GitHub.
fn candidate_urls(version: &str, target: &str, base: Option<&str>) -> Vec<String> {
    #[derive(serde::Deserialize)]
    struct ReleaseAssets {
        servers: std::collections::HashMap<String, String>,
    }
    let manifest: ReleaseAssets = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"), "/../../release-assets.json"
    )))
    .expect("release-assets.json must contain the server asset mapping");
    let mut assets = Vec::new();
    if let Some(asset) = manifest.servers.get(target) {
        assets.push(asset.clone());
    }
    assets.push(format!("skill-server-{target}"));

    let releases = "https://github.com/yubinhu/vibestudio/releases";
    let locations = match base.filter(|base| !base.is_empty()) {
        Some(base) => vec![base.trim_end_matches('/').to_string()],
        None => vec![
            format!("{releases}/download/v{version}"),
            format!("{releases}/latest/download"),
        ],
    };
    locations.iter().flat_map(|base| assets.iter().map(move |asset| format!("{base}/{asset}"))).collect()
}

/// A detected remote platform — the rust target triple naming its release asset.
pub struct Platform {
    pub target: &'static str,
}

/// Detect the remote OS/arch via `uname -sm`. Linux/macOS only; a Windows remote (no
/// `uname`) yields a clear not-yet-supported error. A WSL distro reports Linux, so it
/// flows through the normal Linux path.
pub fn detect(remote: &dyn Remote) -> Result<Platform, String> {
    let out = remote.capture("uname -sm")
        .map_err(|e| format!("Couldn't reach the remote (note: native Windows remotes aren't supported yet — but a WSL distro is). {e}"))?;
    let u = out.trim();
    let target = if u.starts_with("Linux") && u.contains("x86_64") {
        "x86_64-unknown-linux-musl"
    } else if u.starts_with("Linux") && (u.contains("aarch64") || u.contains("arm64")) {
        "aarch64-unknown-linux-musl"
    } else if u.starts_with("Darwin") && u.contains("arm64") {
        "aarch64-apple-darwin"
    } else if u.starts_with("Darwin") && u.contains("x86_64") {
        "x86_64-apple-darwin"
    } else if u.is_empty() {
        return Err("Couldn't detect the remote platform (Windows remotes aren't supported yet).".into());
    } else {
        return Err(format!("Unsupported remote platform: {u}"));
    };
    Ok(Platform { target })
}

/// Ensure a runnable `skill-server` is installed on the remote; returns its path (with
/// a literal `$HOME` for the remote shell to expand). Idempotent. Tries each candidate
/// asset URL in turn — version-pinned first, then latest — so a 404 on the pinned URL
/// (e.g. an unstamped dev build at `0.0.0`) transparently falls back to the latest
/// release instead of failing the whole connect.
pub fn ensure_installed(remote: &dyn Remote, platform: &Platform, app_version: &str) -> Result<String, String> {
    let version = server_version(app_version);
    let base = std::env::var("VIBESTUDIO_SERVER_BASE_URL").ok();
    let urls = candidate_urls(&version, platform.target, base.as_deref());
    ensure_installed_from_urls(remote, &version, platform.target, &urls)
}

fn ensure_installed_from_urls(remote: &dyn Remote, version: &str, target: &str, urls: &[String]) -> Result<String, String> {
    let bin = format!("$HOME/.vibestudio/server/{version}/skill-server");

    let mut last = String::new();
    for url in urls {
        let script = INSTALL_SCRIPT.replace("__VERSION__", version).replace("__URL__", url);
        match remote.run(&script) {
            Ok(_) => {
                prune_old_versions(remote, version);
                return Ok(bin);
            }
            // Exit 3 = the remote has neither curl nor wget → download here and pipe it
            // over the same transport (works for no-internet remotes / through ProxyJump).
            Err(e) if e.code == Some(3) => {
                install_via_pipe(remote, version, urls)?;
                prune_old_versions(remote, version);
                return Ok(bin);
            }
            // Exit 4 = the downloaded binary didn't match the published checksum.
            Err(e) if e.code == Some(4) => {
                return Err(
                    "The downloaded skill-server failed its checksum check (possible corruption or tampering). Aborted.".into(),
                );
            }
            // Download failed (e.g. a 404 for a version with no published asset) — record
            // it and fall through to the next candidate URL.
            Err(e) => last = e.message,
        }
    }
    Err(format!(
        "Couldn't download skill-server for {} (app version {version}). Tried: {}. The matching \
         release may not be published yet — set VIBESTUDIO_SERVER_VERSION or \
         VIBESTUDIO_SERVER_BASE_URL to override. Last error: {last}",
        target,
        urls.join(", ")
    ))
}

/// Best-effort cleanup so remotes don't accumulate a `skill-server` binary for every
/// version ever connected with (see [`KEEP_VERSIONS`]). Runs on every successful
/// connect, after the current version is in place; failures are logged and ignored —
/// keeping the remote tidy must never block connecting.
fn prune_old_versions(remote: &dyn Remote, version: &str) {
    let script = PRUNE_SCRIPT
        .replace("__VERSION__", version)
        .replace("__KEEP_PLUS_1__", &(KEEP_VERSIONS + 1).to_string());
    if let Err(e) = remote.run(&script) {
        log::debug!("pruning old skill-server versions failed (ignored): {}", e.message);
    }
}

/// No-downloader fallback: fetch the asset on THIS machine (verifying its checksum
/// here, since the remote can't reach the network), then stream it to the remote over
/// ssh (`cat > tmp && chmod +x && mv`). Tries each candidate URL in turn, mirroring
/// `ensure_installed`'s pinned-then-latest order.
fn install_via_pipe(remote: &dyn Remote, version: &str, urls: &[String]) -> Result<(), String> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(120))
        .build();
    install_via_pipe_with_fetch(remote, version, urls, |url| {
        let resp = agent.get(url).call().map_err(|e| FetchError::Unavailable(format!("{url}: {e}")))?;
        let mut bytes = Vec::new();
        resp.into_reader().read_to_end(&mut bytes)
            .map_err(|e| FetchError::ReadFailed(format!("reading {url} failed: {e}")))?;
        Ok(bytes)
    })
}

enum FetchError {
    Unavailable(String),
    ReadFailed(String),
}

fn install_via_pipe_with_fetch(
    remote: &dyn Remote,
    version: &str,
    urls: &[String],
    mut fetch: impl FnMut(&str) -> Result<Vec<u8>, FetchError>,
) -> Result<(), String> {
    // Download here, trying each candidate until one resolves (the pinned URL may 404).
    let mut bytes: Vec<u8> = Vec::new();
    let mut used: Option<&str> = None;
    let mut last = String::new();
    for url in urls {
        match fetch(url) {
            Ok(download) => {
                bytes = download;
                used = Some(url);
                break;
            }
            Err(FetchError::Unavailable(e) | FetchError::ReadFailed(e)) => last = e,
        }
    }
    let url = used.ok_or_else(|| {
        format!("Local download of skill-server failed (tried {}). Last error: {last}", urls.join(", "))
    })?;

    // Best-effort integrity check against the published `.sha256` (skip only if that
    // asset is unavailable, e.g. an older release).
    match fetch(&format!("{url}.sha256")) {
        Ok(sum_bytes) => {
            let sum = String::from_utf8_lossy(&sum_bytes);
            if let Some(expected) = sum.split_whitespace().next().filter(|s| !s.is_empty()) {
                let actual: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
                if !expected.eq_ignore_ascii_case(&actual) {
                    return Err("The downloaded skill-server failed its checksum check (possible corruption or tampering).".into());
                }
            }
        }
        Err(FetchError::Unavailable(_)) => {}
        Err(FetchError::ReadFailed(e)) => return Err(format!("Couldn't read the skill-server checksum. Aborted. {e}")),
    }

    let script = PIPE_SCRIPT.replace("__VERSION__", version);
    remote.run_with_stdin(&script, &bytes)
        .map_err(|e| format!("Piping skill-server to the remote failed: {}", e.message))
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use super::*;
    use crate::sshmgr::conn::{LaunchError, SessionHandle};
    use crate::sshmgr::ssh::RunError;

    const TARGETS: [(&str, &str); 4] = [
        ("aarch64-apple-darwin", "server-macos-arm64"),
        ("x86_64-apple-darwin", "server-macos-x64"),
        ("aarch64-unknown-linux-musl", "server-linux-arm64-musl"),
        ("x86_64-unknown-linux-musl", "server-linux-x64-musl"),
    ];

    #[derive(Default)]
    struct MockRemote {
        results: Mutex<VecDeque<Result<String, RunError>>>,
        commands: Mutex<Vec<String>>,
        piped: Mutex<Vec<(String, Vec<u8>)>>,
    }

    impl MockRemote {
        fn returning(codes: &[Option<i32>]) -> Self {
            Self {
                results: Mutex::new(codes.iter().map(|code| match code {
                    Some(code) => Err(RunError { code: Some(*code), message: "download failed".into() }),
                    None => Ok(String::new()),
                }).collect()),
                ..Self::default()
            }
        }

        fn requested_urls(&self) -> Vec<String> {
            self.commands.lock().unwrap().iter().filter_map(|script| {
                script.lines().find_map(|line| line.strip_prefix("url=\"")?.strip_suffix('"').map(str::to_string))
            }).collect()
        }
    }

    impl Remote for MockRemote {
        fn capture(&self, _: &str) -> Result<String, String> { unreachable!() }
        fn run(&self, cmd: &str) -> Result<String, RunError> {
            self.commands.lock().unwrap().push(cmd.to_string());
            self.results.lock().unwrap().pop_front().expect("unexpected remote command")
        }
        fn run_with_stdin(&self, cmd: &str, stdin: &[u8]) -> Result<(), RunError> {
            self.piped.lock().unwrap().push((cmd.to_string(), stdin.to_vec()));
            Ok(())
        }
        fn same_port(&self) -> bool { false }
        fn open_session(&self, _: &str, _: u16, _: u16, _: &str) -> Result<Box<dyn SessionHandle>, LaunchError> {
            unreachable!()
        }
    }

    #[test]
    fn each_platform_prefers_both_pinned_names_before_latest() {
        let releases = "https://github.com/yubinhu/vibestudio/releases";
        for (target, asset) in TARGETS {
            assert_eq!(candidate_urls("1.2.0", target, None), vec![
                format!("{releases}/download/v1.2.0/{asset}"),
                format!("{releases}/download/v1.2.0/skill-server-{target}"),
                format!("{releases}/latest/download/{asset}"),
                format!("{releases}/latest/download/skill-server-{target}"),
            ]);
            assert_eq!(candidate_urls("1.2.0", target, Some("")), candidate_urls("1.2.0", target, None));
        }
    }

    #[test]
    fn override_tries_both_names_only_at_the_requested_base() {
        for (target, asset) in TARGETS {
            assert_eq!(candidate_urls("1.2.0", target, Some("https://mirror.example/assets///")), vec![
                format!("https://mirror.example/assets/{asset}"),
                format!("https://mirror.example/assets/skill-server-{target}"),
            ]);
        }
    }

    #[test]
    fn older_pinned_releases_and_latest_fallbacks_keep_the_installed_path() {
        let target = TARGETS[0].0;
        let urls = candidate_urls("1.2.0", target, None);
        for failures in 0..urls.len() {
            let mut codes = vec![Some(1); failures];
            codes.extend([None, None]); // Successful install, then best-effort pruning.
            let remote = MockRemote::returning(&codes);
            assert_eq!(ensure_installed_from_urls(&remote, "1.2.0", target, &urls).unwrap(),
                "$HOME/.vibestudio/server/1.2.0/skill-server");
            assert_eq!(remote.requested_urls(), urls[..=failures]);
        }
    }

    #[test]
    fn remote_checksum_mismatch_never_tries_a_different_asset() {
        let target = TARGETS[0].0;
        let urls = candidate_urls("1.2.0", target, None);
        let remote = MockRemote::returning(&[Some(1), Some(4)]);
        let error = ensure_installed_from_urls(&remote, "1.2.0", target, &urls).unwrap_err();
        assert!(error.contains("checksum check"));
        assert_eq!(remote.requested_urls(), urls[..2]);
        assert!(remote.piped.lock().unwrap().is_empty());
    }

    #[test]
    fn pipe_fallback_uses_the_same_order_and_matching_checksum_name() {
        let payload = b"server payload";
        let checksum = format!("{:x}  server-payload\n", Sha256::digest(payload));
        let urls = candidate_urls("1.2.0", TARGETS[0].0, None);
        for available in 0..urls.len() {
            let remote = MockRemote::default();
            let mut requested = Vec::new();
            install_via_pipe_with_fetch(&remote, "1.2.0", &urls, |url| {
                requested.push(url.to_string());
                if url == urls[available] { Ok(payload.to_vec()) }
                else if url == format!("{}.sha256", urls[available]) { Ok(checksum.as_bytes().to_vec()) }
                else { Err(FetchError::Unavailable("404".into())) }
            }).unwrap();
            let mut expected = urls[..=available].to_vec();
            expected.push(format!("{}.sha256", urls[available]));
            assert_eq!(requested, expected);
            let piped = remote.piped.lock().unwrap();
            assert_eq!(piped.len(), 1);
            assert_eq!(piped[0].1, payload);
            assert!(piped[0].0.contains("dir=\"$HOME/.vibestudio/server/1.2.0\""));
            assert!(piped[0].0.contains("\"$dir/skill-server\""));
        }
    }

    #[test]
    fn pipe_checksum_mismatch_aborts_without_installing_or_retrying() {
        let urls = candidate_urls("1.2.0", TARGETS[0].0, None);
        let remote = MockRemote::default();
        let mut requested = Vec::new();
        let error = install_via_pipe_with_fetch(&remote, "1.2.0", &urls, |url| {
            requested.push(url.to_string());
            Ok(if url.ends_with(".sha256") { b"invalid checksum".to_vec() } else { b"payload".to_vec() })
        }).unwrap_err();
        assert!(error.contains("checksum check"));
        assert_eq!(requested, vec![urls[0].clone(), format!("{}.sha256", urls[0])]);
        assert!(remote.piped.lock().unwrap().is_empty());
    }

    #[test]
    fn older_assets_without_checksums_still_support_pipe_installation() {
        let urls = candidate_urls("1.2.0", TARGETS[0].0, Some("https://mirror.example"));
        let remote = MockRemote::default();
        install_via_pipe_with_fetch(&remote, "1.2.0", &urls, |url| {
            if url == urls[1] { Ok(b"legacy server".to_vec()) } else { Err(FetchError::Unavailable("404".into())) }
        }).unwrap();
        assert_eq!(remote.piped.lock().unwrap()[0].1, b"legacy server");
    }

    #[test]
    fn unreadable_checksum_aborts_without_installing_or_retrying() {
        let urls = candidate_urls("1.2.0", TARGETS[0].0, None);
        let remote = MockRemote::default();
        let mut requested = Vec::new();
        let error = install_via_pipe_with_fetch(&remote, "1.2.0", &urls, |url| {
            requested.push(url.to_string());
            if url.ends_with(".sha256") { Err(FetchError::ReadFailed("connection closed mid-body".into())) }
            else { Ok(b"payload".to_vec()) }
        }).unwrap_err();
        assert!(error.contains("Couldn't read the skill-server checksum"));
        assert_eq!(requested, vec![urls[0].clone(), format!("{}.sha256", urls[0])]);
        assert!(remote.piped.lock().unwrap().is_empty());
    }
}
