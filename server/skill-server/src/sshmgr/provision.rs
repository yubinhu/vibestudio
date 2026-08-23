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
/// exact-matches its tag; an unstamped dev build sits at the placeholder `0.1.0` that
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
/// whole scheme with `VIBESTUDIO_SERVER_BASE_URL` (no trailing slash; the asset name
/// `skill-server-<target>` is appended) — when set, only that single URL is tried.
fn candidate_urls(version: &str, target: &str) -> Vec<String> {
    let asset = format!("skill-server-{target}");
    if let Some(base) = std::env::var("VIBESTUDIO_SERVER_BASE_URL").ok().filter(|v| !v.is_empty()) {
        return vec![format!("{}/{asset}", base.trim_end_matches('/'))];
    }
    let releases = "https://github.com/yubinhu/vibestudio/releases";
    vec![
        format!("{releases}/download/v{version}/{asset}"),
        format!("{releases}/latest/download/{asset}"),
    ]
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
/// (e.g. an unstamped dev build at `0.1.0`) transparently falls back to the latest
/// release instead of failing the whole connect.
pub fn ensure_installed(remote: &dyn Remote, platform: &Platform, app_version: &str) -> Result<String, String> {
    let version = server_version(app_version);
    let bin = format!("$HOME/.vibestudio/server/{version}/skill-server");
    let urls = candidate_urls(&version, platform.target);

    let mut last = String::new();
    for url in &urls {
        let script = INSTALL_SCRIPT.replace("__VERSION__", &version).replace("__URL__", url);
        match remote.run(&script) {
            Ok(_) => {
                prune_old_versions(remote, &version);
                return Ok(bin);
            }
            // Exit 3 = the remote has neither curl nor wget → download here and pipe it
            // over the same transport (works for no-internet remotes / through ProxyJump).
            Err(e) if e.code == Some(3) => {
                install_via_pipe(remote, &version, &urls)?;
                prune_old_versions(remote, &version);
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
        platform.target,
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

    // Download here, trying each candidate until one resolves (the pinned URL may 404).
    let mut bytes: Vec<u8> = Vec::new();
    let mut used: Option<&str> = None;
    let mut last = String::new();
    for url in urls {
        match agent.get(url).call() {
            Ok(resp) => {
                bytes.clear();
                match resp.into_reader().read_to_end(&mut bytes) {
                    Ok(_) => {
                        used = Some(url);
                        break;
                    }
                    Err(e) => last = format!("reading {url} failed: {e}"),
                }
            }
            Err(e) => last = format!("{url}: {e}"),
        }
    }
    let url = used.ok_or_else(|| {
        format!("Local download of skill-server failed (tried {}). Last error: {last}", urls.join(", "))
    })?;

    // Best-effort integrity check against the published `.sha256` (skip only if that
    // asset is unavailable, e.g. an older release).
    if let Ok(sum_resp) = agent.get(&format!("{url}.sha256")).call() {
        let mut sum = String::new();
        let _ = sum_resp.into_reader().read_to_string(&mut sum);
        if let Some(expected) = sum.split_whitespace().next().filter(|s| !s.is_empty()) {
            let actual: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
            if !expected.eq_ignore_ascii_case(&actual) {
                return Err("The downloaded skill-server failed its checksum check (possible corruption or tampering).".into());
            }
        }
    }

    let script = PIPE_SCRIPT.replace("__VERSION__", version);
    remote.run_with_stdin(&script, &bytes)
        .map_err(|e| format!("Piping skill-server to the remote failed: {}", e.message))
}
