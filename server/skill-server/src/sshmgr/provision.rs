//! Remote OS/arch detection + version-pinned `skill-server` provisioning — modelled
//! on how VS Code bootstraps its server: detect the platform, reuse an already
//! installed binary at a versioned path, else download it (remote `curl`/`wget`, or,
//! for a no-internet remote, a local download piped over the SSH connection).
use std::io::Read;
use std::time::Duration;

use sha2::{Digest, Sha256};

use super::conn::Remote;

/// Idempotent remote install: reuse a runnable binary, else download (curl/wget) to a
/// temp file, verify it against the required published `.sha256`, and atomically move
/// it into place. Exit codes the caller interprets: 3 = no downloader or hasher
/// (→ verified pipe fallback), 4 = missing/invalid checksum or checksum mismatch.
/// `__VERSION__`/`__URL__` are substituted (raw string ⇒ shell braces are
/// literal, unlike `format!`).
const INSTALL_SCRIPT: &str = r#"set -e
ver="__VERSION__"
dir="$HOME/.vibestudio/server/$ver"
bin="$dir/skill-server"
if [ "__SKIP_CACHE__" = 0 ] && [ -x "$bin" ]; then
  installed=$("$bin" --version 2>/dev/null) || installed=""
  case "$installed" in *" host-service=1"*) echo INSTALLED; exit 0 ;; esac
fi
mkdir -p "$dir"
url="__URL__"
tmp="$bin.tmp.$$"
sum="$tmp.sha256"
trap 'rm -f "$tmp" "$sum"' 0
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
if dl "$url.sha256" "$sum"; then :; else echo CHECKSUM_UNAVAILABLE >&2; exit 4; fi
if expected=$(awk 'NF { if (++lines != 1 || length($1) != 64 || $1 !~ /^[0-9A-Fa-f]+$/) exit 1; sum=tolower($1) } END { if (lines != 1) exit 1; print sum }' "$sum"); then :
else echo CHECKSUM_INVALID >&2; exit 4; fi
if command -v sha256sum >/dev/null 2>&1; then
  actual=$(sha256sum "$tmp") || { echo CHECKSUM_FAILED >&2; exit 4; }
elif command -v shasum >/dev/null 2>&1; then
  actual=$(shasum -a 256 "$tmp") || { echo CHECKSUM_FAILED >&2; exit 4; }
else echo NO_HASHER >&2; exit 3; fi
actual=${actual%% *}
if [ "$expected" != "$actual" ]; then
  echo CHECKSUM_MISMATCH >&2; exit 4
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

/// Remote-side prune: mark the version we successfully connected to as most-recently-used, then
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
/// release tag); `VIBESTUDIO_SERVER_VERSION` may raise that floor, never lower it. A released build's version
/// exact-matches its tag; an unstamped dev build sits at the placeholder `0.0.0` that
/// was never released, so `candidate_urls` falls back to the latest release.
pub fn server_version(app_version: &str) -> Result<String, String> {
    let configured = std::env::var("VIBESTUDIO_SERVER_VERSION")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| app_version.to_string());
    crate::server_version::minimum(app_version, &configured)
}

/// The asset URLs to try, in order: the version-pinned release first (so a released
/// build pins the remote server to the exact version it ships, never drifting to a
/// newer/incompatible API), then the latest release (so an unstamped dev build — or any
/// version that was never published — still resolves to something current). Override the
/// whole scheme with `VIBESTUDIO_SERVER_BASE_URL`. Asset names come from the release
/// policy. A custom base is exclusive: it never falls back to GitHub.
fn candidate_urls(version: &str, target: &str, base: Option<&str>) -> Vec<String> {
    #[derive(serde::Deserialize)]
    struct ReleaseAssets {
        servers: std::collections::HashMap<String, String>,
    }
    let manifest: ReleaseAssets = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"), "/../../release-assets.json"
    )))
    .expect("release-assets.json must contain the server asset mapping");
    let asset = manifest.servers.get(target)
        .expect("every supported remote platform must have a release asset");

    let releases = "https://github.com/yubinhu/vibestudio/releases";
    let locations = match base.filter(|base| !base.is_empty()) {
        Some(base) => vec![base.trim_end_matches('/').to_string()],
        None => vec![
            format!("{releases}/download/v{version}"),
            format!("{releases}/latest/download"),
        ],
    };
    locations.iter().map(|base| format!("{base}/{asset}")).collect()
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
/// release instead of failing the whole connect. Cached and downloaded executables
/// must report a supported version at least as new as the client.
pub fn ensure_installed(remote: &dyn Remote, platform: &Platform, app_version: &str) -> Result<String, String> {
    let version = server_version(app_version)?;
    let base = std::env::var("VIBESTUDIO_SERVER_BASE_URL").ok();
    let urls = candidate_urls(&version, platform.target, base.as_deref());
    ensure_installed_from_urls(remote, &version, platform.target, &urls)
}

fn ensure_installed_from_urls(remote: &dyn Remote, version: &str, target: &str, urls: &[String]) -> Result<String, String> {
    crate::server_version::require_at_least(version, version)?;
    let bin = format!("$HOME/.vibestudio/server/{version}/skill-server");

    let mut last = String::new();
    let mut skip_cache = false;
    for url in urls {
        loop {
            let script = INSTALL_SCRIPT.replace("__VERSION__", version).replace("__URL__", url)
                .replace("__SKIP_CACHE__", if skip_cache { "1" } else { "0" });
            match remote.run(&script) {
                Ok(output) => {
                    match verify_installed(remote, &bin, version) {
                        Ok(()) => return Ok(bin),
                        Err(_) if output.trim() == "INSTALLED" && !skip_cache => {
                            // A version directory may contain an earlier latest-release
                            // fallback. Redownload once without deleting that cache or
                            // stopping its worker; only verified bytes replace the file.
                            skip_cache = true;
                            continue;
                        }
                        Err(error) => return Err(error),
                    }
                }
                // Exit 3 = the remote lacks a downloader or hasher → download and verify
                // here, then pipe it over the same transport (also works through ProxyJump).
                Err(e) if e.code == Some(3) => {
                    install_via_pipe(remote, version, urls)?;
                    return Ok(bin);
                }
                // Exit 4 = the downloaded binary's checksum couldn't be verified. Never
                // try another release or transport after an integrity failure.
                Err(e) if e.code == Some(4) => {
                    return Err(format!(
                        "The downloaded skill-server could not be verified against its published checksum. Aborted. {}", e.message,
                    ));
                }
                // Download failed (e.g. a 404 for a version with no published asset) — record
                // it and fall through to the next candidate URL.
                Err(e) => { last = e.message; break; },
            }
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

fn verify_installed(remote: &dyn Remote, bin: &str, minimum: &str) -> Result<(), String> {
    let banner = remote.capture(&format!("\"{bin}\" --version"))
        .map_err(|error| format!("Could not verify the installed skill-server executable: {error}"))?;
    let actual = crate::server_version::from_banner(&banner)?;
    crate::server_version::require_at_least(&actual, minimum)
}

/// Best-effort cleanup so remotes don't accumulate a `skill-server` binary for every
/// version ever connected with (see [`KEEP_VERSIONS`]). Runs on every successful
/// provisioned connection, after the replacement worker answers with a compatible
/// version. Installation alone must not prune an older worker's restart binary
/// while that worker is still serving clients. Failures never block connecting.
pub(super) fn prune_old_versions(remote: &dyn Remote, version: &str) {
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
        let resp = agent.get(url).call().map_err(|e| format!("{url}: {e}"))?;
        let mut bytes = Vec::new();
        resp.into_reader().read_to_end(&mut bytes)
            .map_err(|e| format!("reading {url} failed: {e}"))?;
        Ok(bytes)
    })
}

fn install_via_pipe_with_fetch(
    remote: &dyn Remote,
    version: &str,
    urls: &[String],
    mut fetch: impl FnMut(&str) -> Result<Vec<u8>, String>,
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
            Err(e) => last = e,
        }
    }
    let url = used.ok_or_else(|| {
        format!("Local download of skill-server failed (tried {}). Last error: {last}", urls.join(", "))
    })?;

    let sum_bytes = fetch(&format!("{url}.sha256"))
        .map_err(|e| format!("Couldn't read the skill-server checksum. Aborted. {e}"))?;
    let sum = std::str::from_utf8(&sum_bytes)
        .map_err(|_| "The skill-server checksum is invalid. Aborted.".to_string())?;
    let mut lines = sum.lines().filter(|line| !line.trim().is_empty());
    let expected = lines.next().and_then(|line| line.split_whitespace().next())
        .filter(|sum| sum.len() == 64 && sum.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| "The skill-server checksum is invalid. Aborted.".to_string())?;
    if lines.next().is_some() {
        return Err("The skill-server checksum is invalid. Aborted.".into());
    }
    let actual = format!("{:x}", Sha256::digest(&bytes));
    if !expected.eq_ignore_ascii_case(&actual) {
        return Err("The downloaded skill-server failed its checksum check (possible corruption or tampering). Aborted.".into());
    }

    let script = PIPE_SCRIPT.replace("__VERSION__", version);
    remote.run_with_stdin(&script, &bytes)
        .map_err(|e| format!("Piping skill-server to the remote failed: {}", e.message))?;
    verify_installed(remote, &format!("$HOME/.vibestudio/server/{version}/skill-server"), version)
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
        banners: Mutex<VecDeque<Result<String, String>>>,
        captures: Mutex<Vec<String>>,
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
        fn capture(&self, cmd: &str) -> Result<String, String> {
            self.captures.lock().unwrap().push(cmd.to_string());
            self.banners.lock().unwrap().pop_front()
                .unwrap_or_else(|| Ok("skill-server 1.2.1 host-service=1".into()))
        }
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
    fn each_platform_prefers_the_pinned_release_before_latest() {
        let releases = "https://github.com/yubinhu/vibestudio/releases";
        for (target, asset) in TARGETS {
            assert_eq!(candidate_urls("1.2.1", target, None), vec![
                format!("{releases}/download/v1.2.1/{asset}"),
                format!("{releases}/latest/download/{asset}"),
            ]);
            assert_eq!(candidate_urls("1.2.1", target, Some("")), candidate_urls("1.2.1", target, None));
        }
    }

    #[test]
    fn override_uses_only_the_requested_base() {
        for (target, asset) in TARGETS {
            assert_eq!(candidate_urls("1.2.1", target, Some("https://mirror.example/assets///")), vec![
                format!("https://mirror.example/assets/{asset}"),
            ]);
        }
    }

    #[test]
    fn pinned_release_and_latest_fallback_keep_the_installed_path() {
        let target = TARGETS[0].0;
        let urls = candidate_urls("1.2.1", target, None);
        for failures in 0..urls.len() {
            let mut codes = vec![Some(1); failures];
            codes.push(None); // Installation must not prune before a verified connection.
            let remote = MockRemote::returning(&codes);
            assert_eq!(ensure_installed_from_urls(&remote, "1.2.1", target, &urls).unwrap(),
                "$HOME/.vibestudio/server/1.2.1/skill-server");
            assert_eq!(remote.requested_urls(), urls[..=failures]);
            assert_eq!(remote.commands.lock().unwrap().len(), failures + 1,
                "installing a replacement must leave the previous host's binary available");
        }
    }

    #[test]
    fn stale_or_malformed_cache_is_redownloaded_once_and_a_bad_download_is_rejected() {
        let urls = candidate_urls("1.2.8", TARGETS[0].0, None);
        for cached in ["skill-server 1.2.4 host-service=1", "unrecognized version"] {
            for replacement in ["skill-server 1.2.8 host-service=1", "skill-server 1.2.4 host-service=1", "malformed"] {
                let remote = MockRemote {
                    results: Mutex::new(VecDeque::from([Ok("INSTALLED\n".into()), Ok("DOWNLOADED\n".into())])),
                    banners: Mutex::new(VecDeque::from([Ok(cached.into()), Ok(replacement.into())])),
                    ..MockRemote::default()
                };
                let result = ensure_installed_from_urls(&remote, "1.2.8", TARGETS[0].0, &urls);
                assert_eq!(result.is_ok(), replacement.contains("1.2.8"));
                assert_eq!(remote.requested_urls(), vec![urls[0].clone(), urls[0].clone()]);
                let commands = remote.commands.lock().unwrap();
                assert!(commands[0].contains("if [ \"0\" = 0 ]"));
                assert!(commands[1].contains("if [ \"1\" = 0 ]"));
                assert_eq!(remote.captures.lock().unwrap().len(), 2);
            }
        }
    }

    #[test]
    fn compatible_newer_cache_is_reused_without_download_or_prune() {
        let remote = MockRemote {
            results: Mutex::new(VecDeque::from([Ok("INSTALLED\n".into())])),
            banners: Mutex::new(VecDeque::from([Ok("skill-server 1.2.10 host-service=1".into())])),
            ..MockRemote::default()
        };
        let urls = candidate_urls("1.2.9", TARGETS[0].0, None);
        assert!(ensure_installed_from_urls(&remote, "1.2.9", TARGETS[0].0, &urls).is_ok());
        assert_eq!(remote.commands.lock().unwrap().len(), 1);
        assert_eq!(remote.requested_urls(), urls[..1]);
    }

    #[test]
    fn remote_checksum_mismatch_never_tries_a_different_asset() {
        let target = TARGETS[0].0;
        let urls = candidate_urls("1.2.1", target, None);
        let remote = MockRemote::returning(&[Some(4)]);
        let error = ensure_installed_from_urls(&remote, "1.2.1", target, &urls).unwrap_err();
        assert!(error.contains("published checksum"));
        assert_eq!(remote.requested_urls(), urls[..1]);
        assert!(remote.piped.lock().unwrap().is_empty());
    }

    #[test]
    fn pipe_fallback_uses_the_same_order_and_matching_checksum_name() {
        let payload = b"server payload";
        let checksum = format!("{:x}  server-payload\n", Sha256::digest(payload));
        let urls = candidate_urls("1.2.1", TARGETS[0].0, None);
        for available in 0..urls.len() {
            let remote = MockRemote::default();
            let mut requested = Vec::new();
            install_via_pipe_with_fetch(&remote, "1.2.1", &urls, |url| {
                requested.push(url.to_string());
                if url == urls[available] { Ok(payload.to_vec()) }
                else if url == format!("{}.sha256", urls[available]) { Ok(checksum.as_bytes().to_vec()) }
                else { Err("404".into()) }
            }).unwrap();
            let mut expected = urls[..=available].to_vec();
            expected.push(format!("{}.sha256", urls[available]));
            assert_eq!(requested, expected);
            let piped = remote.piped.lock().unwrap();
            assert_eq!(piped.len(), 1);
            assert_eq!(piped[0].1, payload);
            assert!(piped[0].0.contains("dir=\"$HOME/.vibestudio/server/1.2.1\""));
            assert!(piped[0].0.contains("\"$dir/skill-server\""));
        }
    }

    #[test]
    fn checksum_verified_pipe_download_must_report_a_compatible_executable_version() {
        let payload = b"server payload";
        let checksum = format!("{:x}\n", Sha256::digest(payload));
        let urls = candidate_urls("1.2.8", TARGETS[0].0, None);
        let remote = MockRemote {
            banners: Mutex::new(VecDeque::from([Ok("skill-server 1.2.4 host-service=1".into())])),
            ..MockRemote::default()
        };
        let error = install_via_pipe_with_fetch(&remote, "1.2.8", &urls, |url| {
            Ok(if url.ends_with(".sha256") { checksum.as_bytes().to_vec() } else { payload.to_vec() })
        }).unwrap_err();
        assert!(error.contains("incompatible host service version 1.2.4"));
        assert_eq!(remote.piped.lock().unwrap().len(), 1);
        assert_eq!(remote.captures.lock().unwrap().len(), 1);
        assert!(remote.commands.lock().unwrap().is_empty());
    }

    #[test]
    fn rejected_pipe_checksums_abort_without_installing_or_retrying() {
        let urls = candidate_urls("1.2.1", TARGETS[0].0, None);
        let cases = [
            ("mismatch", Ok(format!("{}  server-payload\n", "0".repeat(64)).into_bytes()), "checksum check"),
            ("missing", Err("404"), "Couldn't read the skill-server checksum"),
            ("empty", Ok(Vec::new()), "checksum is invalid"),
            ("malformed", Ok(b"invalid checksum".to_vec()), "checksum is invalid"),
            ("short", Ok(b"0".repeat(63)), "checksum is invalid"),
            ("non-hex", Ok(b"z".repeat(64)), "checksum is invalid"),
            ("invalid UTF-8", Ok(vec![0xff]), "checksum is invalid"),
            ("multiple hashes", Ok(format!("{0}  first\n{0}  second\n", "0".repeat(64)).into_bytes()), "checksum is invalid"),
            ("unreadable", Err("connection closed mid-body"), "Couldn't read the skill-server checksum"),
        ];
        for (name, checksum, expected_error) in cases {
            let remote = MockRemote::default();
            let mut requested = Vec::new();
            let error = install_via_pipe_with_fetch(&remote, "1.2.1", &urls, |url| {
                requested.push(url.to_string());
                if url.ends_with(".sha256") {
                    checksum.clone().map_err(str::to_string)
                } else { Ok(b"payload".to_vec()) }
            }).unwrap_err();
            assert!(error.contains(expected_error), "{name}: {error}");
            assert!(error.contains("Aborted"), "{name}: {error}");
            assert_eq!(requested, vec![urls[0].clone(), format!("{}.sha256", urls[0])], "{name}");
            assert!(remote.piped.lock().unwrap().is_empty(), "{name} must not install");
        }
    }

    #[cfg(unix)]
    mod remote_script {
        use std::fs;
        use std::os::unix::fs::{symlink, PermissionsExt};
        use std::path::{Path, PathBuf};
        use std::process::Output;
        use std::sync::atomic::{AtomicUsize, Ordering};

        use super::*;

        static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);
        const PAYLOAD: &[u8] = b"#!/bin/sh\necho 'skill-server 1.2.1 host-service=1'\n";
        const URL: &str = "https://mirror.example/server-macos-arm64";

        struct Fixture {
            root: PathBuf,
        }

        impl Fixture {
            fn new(hasher: bool) -> Self {
                let root = std::env::temp_dir().join(format!(
                    "vibestudio-provision-{}-{}", std::process::id(),
                    NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed),
                ));
                fs::create_dir(&root).unwrap();
                let tools = root.join("tools");
                fs::create_dir(&tools).unwrap();
                for name in ["mkdir", "rm", "mv", "chmod", "awk", "ls", "tail", "touch"] {
                    let path = ["/usr/bin", "/bin"].into_iter()
                        .map(|dir| Path::new(dir).join(name)).find(|path| path.exists()).unwrap();
                    symlink(path, tools.join(name)).unwrap();
                }
                if hasher {
                    let path = ["/usr/bin/sha256sum", "/usr/bin/shasum"].into_iter()
                        .map(Path::new).find(|path| path.exists()).unwrap();
                    symlink(path, tools.join(path.file_name().unwrap())).unwrap();
                }
                fs::write(root.join("payload"), PAYLOAD).unwrap();
                let fixture = Self { root };
                fixture.checksum(Some(format!("{:X}  server-macos-arm64\n", Sha256::digest(PAYLOAD)).as_bytes()));
                // Only the downloader is faked. Run the real install shell, filesystem
                // operations, parser, and hasher against a private temporary directory.
                fixture.executable("curl", &format!(r#"#!/bin/sh
echo "$2" >> "{root}/requests"
case "$2" in
  *.sha256)
    [ -f "{root}/checksum" ] || exit 22
    /bin/cat "{root}/checksum" > "$4"
    exit "${{PROVISION_CHECKSUM_EXIT:-0}}"
    ;;
  *) /bin/cat "{root}/payload" > "$4" ;;
esac
"#, root = fixture.root.display()));
                fixture
            }

            fn executable(&self, name: &str, body: &str) {
                let path = self.root.join("tools").join(name);
                fs::write(&path, body).unwrap();
                fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
            }

            fn checksum(&self, contents: Option<&[u8]>) {
                let path = self.root.join("checksum");
                match contents {
                    Some(contents) => fs::write(path, contents).unwrap(),
                    None => fs::remove_file(path).unwrap(),
                }
            }

            fn run(&self, checksum_exit: i32) -> Output {
                // Substitute the script's remote HOME without changing this process's
                // HOME or allowing the fixture to write to the user's install path.
                let script = INSTALL_SCRIPT.replace("$HOME", self.root.to_str().unwrap())
                    .replace("__SKIP_CACHE__", "0")
                    .replace("__VERSION__", "1.2.1").replace("__URL__", URL);
                skill_core::process::hidden_command("/bin/sh").args(["-c", &script])
                    .env("PATH", self.root.join("tools"))
                    .env("PROVISION_CHECKSUM_EXIT", checksum_exit.to_string())
                    .output().unwrap()
            }

            fn bin(&self) -> PathBuf {
                self.root.join(".vibestudio/server/1.2.1/skill-server")
            }

            fn execute(&self, script: &str) -> Result<String, RunError> {
                let script = script.replace("$HOME", self.root.to_str().unwrap());
                let output = skill_core::process::hidden_command("/bin/sh").args(["-c", &script])
                    .env("PATH", self.root.join("tools")).output().unwrap();
                if output.status.success() {
                    Ok(String::from_utf8(output.stdout).unwrap())
                } else {
                    Err(RunError { code: output.status.code(), message: String::from_utf8_lossy(&output.stderr).into_owned() })
                }
            }

            fn assert_not_installed(&self) {
                assert!(!self.bin().exists());
                assert_eq!(fs::read_dir(self.bin().parent().unwrap()).unwrap().count(), 0,
                    "failed installs must remove temporary payloads and checksums");
            }
        }

        impl Drop for Fixture {
            fn drop(&mut self) {
                fs::remove_dir_all(&self.root).unwrap();
            }
        }

        impl Remote for Fixture {
            fn capture(&self, cmd: &str) -> Result<String, String> {
                self.execute(cmd).map_err(|error| error.message)
            }
            fn run(&self, cmd: &str) -> Result<String, RunError> { self.execute(cmd) }
            fn run_with_stdin(&self, _: &str, _: &[u8]) -> Result<(), RunError> { unreachable!() }
            fn same_port(&self) -> bool { false }
            fn open_session(&self, _: &str, _: u16, _: u16, _: &str) -> Result<Box<dyn SessionHandle>, LaunchError> { unreachable!() }
        }

        #[test]
        fn stale_cache_is_atomically_replaced_only_after_checksum_verification() {
            let stale = b"#!/bin/sh\necho 'skill-server 1.2.0 host-service=1'\n";
            for valid_checksum in [false, true] {
                let fixture = Fixture::new(true);
                fs::create_dir_all(fixture.bin().parent().unwrap()).unwrap();
                fs::write(fixture.bin(), stale).unwrap();
                fs::set_permissions(fixture.bin(), fs::Permissions::from_mode(0o755)).unwrap();
                if !valid_checksum { fixture.checksum(Some(b"invalid checksum")); }
                let result = ensure_installed_from_urls(&fixture, "1.2.1", TARGETS[0].0, &[URL.into()]);
                assert_eq!(result.is_ok(), valid_checksum, "{result:?}");
                assert_eq!(fs::read(fixture.bin()).unwrap(), if valid_checksum { PAYLOAD } else { stale });
                assert_eq!(fs::read_to_string(fixture.root.join("requests")).unwrap(), format!("{URL}\n{URL}.sha256\n"));
            }
        }

        #[test]
        fn verifies_installs_and_reuses_a_runnable_binary() {
            let fixture = Fixture::new(true);
            let output = fixture.run(0);
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
            assert_eq!(fs::read(fixture.bin()).unwrap(), PAYLOAD);
            assert_ne!(fs::metadata(fixture.bin()).unwrap().permissions().mode() & 0o111, 0);
            let requests = fs::read_to_string(fixture.root.join("requests")).unwrap();
            assert_eq!(requests, format!("{URL}\n{URL}.sha256\n"));

            fixture.checksum(None);
            fs::remove_file(fixture.root.join("payload")).unwrap();
            let output = fixture.run(0);
            assert!(output.status.success());
            assert_eq!(String::from_utf8(output.stdout).unwrap(), "INSTALLED\n");
            assert_eq!(fs::read_to_string(fixture.root.join("requests")).unwrap(), requests);
        }

        #[test]
        fn cleanup_keeps_three_recent_versions_and_removed_versions_can_be_reinstalled() {
            let fixture = Fixture::new(true);
            assert!(fixture.run(0).status.success());
            let installs = fixture.root.join(".vibestudio/server");
            for (index, version) in ["1.1.0", "1.1.1", "1.1.2"].iter().enumerate() {
                let directory = installs.join(version);
                fs::create_dir(&directory).unwrap();
                fs::write(directory.join("skill-server"), PAYLOAD).unwrap();
                fs::File::open(directory).unwrap().set_times(fs::FileTimes::new().set_modified(
                    std::time::UNIX_EPOCH + Duration::from_secs(100 + index as u64)
                )).unwrap();
            }
            let script = PRUNE_SCRIPT.replace("$HOME", fixture.root.to_str().unwrap())
                .replace("__VERSION__", "1.2.1")
                .replace("__KEEP_PLUS_1__", &(KEEP_VERSIONS + 1).to_string());
            let output = skill_core::process::hidden_command("/bin/sh").args(["-c", &script])
                .env("PATH", fixture.root.join("tools")).output().unwrap();
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
            assert_eq!(fs::read_dir(&installs).unwrap().count(), KEEP_VERSIONS);
            assert!(!installs.join("1.1.0").exists());
            assert!(installs.join("1.1.1/skill-server").exists());
            assert!(installs.join("1.1.2/skill-server").exists());
            assert!(fixture.bin().exists());

            // An older client can provision its removed cache again. It still
            // needs the normal executable/health checks before routing requests.
            let script = INSTALL_SCRIPT.replace("$HOME", fixture.root.to_str().unwrap())
                .replace("__SKIP_CACHE__", "0")
                .replace("__VERSION__", "1.1.0").replace("__URL__", URL);
            let output = skill_core::process::hidden_command("/bin/sh").args(["-c", &script])
                .env("PATH", fixture.root.join("tools")).output().unwrap();
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
            assert_eq!(fs::read(installs.join("1.1.0/skill-server")).unwrap(), PAYLOAD);
        }

        #[test]
        fn missing_malformed_mismatched_and_unreadable_checksums_never_install() {
            let bad_checksums = [
                None,
                Some(Vec::new()),
                Some(b"invalid checksum".to_vec()),
                Some(b"0".repeat(63)),
                Some(b"z".repeat(64)),
                Some(format!("{0}  first\n{0}  second\n", "0".repeat(64)).into_bytes()),
                Some(format!("{}  server-macos-arm64\n", "0".repeat(64)).into_bytes()),
            ];
            for checksum in bad_checksums {
                let fixture = Fixture::new(true);
                fixture.checksum(checksum.as_deref());
                let output = fixture.run(0);
                assert_eq!(output.status.code(), Some(4), "{}", String::from_utf8_lossy(&output.stderr));
                fixture.assert_not_installed();
            }
            // A downloader can emit a complete-looking hash before reporting a read
            // failure. Its exit status must be checked before parsing those bytes.
            let fixture = Fixture::new(true);
            assert_eq!(fixture.run(18).status.code(), Some(4));
            fixture.assert_not_installed();
        }

        #[test]
        fn missing_remote_tools_request_verified_pipe_fallback() {
            let fixture = Fixture::new(false);
            let output = fixture.run(0);
            assert_eq!(output.status.code(), Some(3));
            assert!(String::from_utf8_lossy(&output.stderr).contains("NO_HASHER"));
            fixture.assert_not_installed();

            fs::remove_file(fixture.root.join("tools/curl")).unwrap();
            let output = fixture.run(0);
            assert_eq!(output.status.code(), Some(3));
            assert!(String::from_utf8_lossy(&output.stderr).contains("NO_DOWNLOADER"));
            fixture.assert_not_installed();
        }

        #[test]
        fn failed_remote_hasher_aborts_without_installing() {
            let fixture = Fixture::new(false);
            fixture.executable("sha256sum", "#!/bin/sh\necho incomplete\nexit 1\n");
            let output = fixture.run(0);
            assert_eq!(output.status.code(), Some(4));
            assert!(String::from_utf8_lossy(&output.stderr).contains("CHECKSUM_FAILED"));
            fixture.assert_not_installed();
        }
    }
}
