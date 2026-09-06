#!/usr/bin/env bash
# Vendor the prebuilt llama.cpp `llama-server` engine (MIT-licensed) for the local
# platform into client/desktop/binaries/<target-triple>/ for optional on-device use.
# Releases use coding-agent CLIs by default and do not bundle this engine.
#
#   scripts/fetch-engine.sh                 # latest llama.cpp release, host platform
#   LLAMA_BUILD=b9484 scripts/fetch-engine.sh   # pin a specific release tag
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEST_ROOT="$REPO_ROOT/client/desktop/binaries"

# Host → (llama.cpp release asset substring, Rust target triple).
# Windows hosts report MINGW*/MSYS*/CYGWIN* under the bash that CI (and Git Bash) use.
case "$(uname -s)/$(uname -m)" in
  Linux/x86_64)                              ASSET_GREP="ubuntu-x64";  TRIPLE="x86_64-unknown-linux-gnu" ;;
  Linux/aarch64)                             ASSET_GREP="ubuntu-arm64"; TRIPLE="aarch64-unknown-linux-gnu" ;;
  Darwin/arm64)                              ASSET_GREP="macos-arm64";  TRIPLE="aarch64-apple-darwin" ;;
  Darwin/x86_64)                             ASSET_GREP="macos-x64";    TRIPLE="x86_64-apple-darwin" ;;
  MINGW*/x86_64|MSYS*/x86_64|CYGWIN*/x86_64) ASSET_GREP="win-cpu-x64";  TRIPLE="x86_64-pc-windows-msvc" ;;
esac
[ -z "${TRIPLE:-}" ] && { echo "Unsupported host $(uname -s)/$(uname -m) — vendor the matching llama.cpp release by hand." >&2; exit 1; }

# Resolve a Python interpreter — `python3` on Linux/macOS, often only `python` on
# Windows runners (setup-python doesn't always create a `python3` shim there).
PY="$(command -v python3 || command -v python || true)"
[ -z "$PY" ] && { echo "Python 3 not found (need python3 or python on PATH)." >&2; exit 1; }

# Fetch a URL to stdout. Authenticate when GITHUB_TOKEN is set: unauthenticated
# api.github.com is 60 req/hr per IP, which CI's shared egress IPs can exhaust.
fetch() {
  if [ -n "${GITHUB_TOKEN:-}" ]; then
    curl -fsSL --retry 3 --retry-all-errors --max-time 30 -H "Authorization: Bearer $GITHUB_TOKEN" "$1"
  else
    curl -fsSL --retry 3 --retry-all-errors --max-time 30 "$1"
  fi
}

API="https://api.github.com/repos/ggml-org/llama.cpp/releases"
REL="$API/latest"; [ -n "${LLAMA_BUILD:-}" ] && REL="$API/tags/$LLAMA_BUILD"

# Native Windows python.exe prints CRLF; strip \r so $TAG/$URL carry no trailing
# carriage return (curl rejects an embedded CR as "Malformed input to a URL function",
# and a CR-tainted "NONE\r" would also slip past the no-asset guard below).
read -r TAG URL < <(fetch "$REL" | "$PY" -c "
import sys, json
d = json.load(sys.stdin)
g = '$ASSET_GREP'
bad = ('vulkan', 'cuda', 'sycl', 'hip', 'musa')  # CPU build for a guaranteed baseline
a = [x for x in d['assets'] if g in x['name'].lower() and not any(k in x['name'].lower() for k in bad)]
print(d.get('tag_name', '?'), a[0]['browser_download_url'] if a else 'NONE')
" | tr -d '\r')
[ "$URL" = "NONE" ] && { echo "No CPU asset matching '$ASSET_GREP' in llama.cpp $TAG" >&2; exit 1; }

echo "llama.cpp $TAG → $TRIPLE  ($(basename "$URL"))"
tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/x"
# Linux/macOS releases ship .tar.gz; Windows ships .zip — handle both.
case "$URL" in
  *.tar.gz|*.tgz)
    fetch "$URL" > "$tmp/engine.tgz"
    tar -xzf "$tmp/engine.tgz" -C "$tmp/x" ;;
  *)
    fetch "$URL" > "$tmp/engine.zip"
    # The .zip path is Windows-only, where $PY is the native python.exe from
    # setup-python — it can't resolve Git Bash's MSYS /tmp paths. Translate to
    # Windows form (cygpath) and pass via argv so backslashes aren't string-escaped.
    zip="$tmp/engine.zip"; out="$tmp/x"
    if command -v cygpath >/dev/null 2>&1; then
      zip="$(cygpath -w "$zip")"; out="$(cygpath -w "$out")"
    fi
    "$PY" -c "import sys, zipfile; zipfile.ZipFile(sys.argv[1]).extractall(sys.argv[2])" "$zip" "$out" ;;
esac

bin="$(find "$tmp/x" -type f \( -name 'llama-server' -o -name 'llama-server.exe' \) | head -1)"
[ -z "$bin" ] && { echo "llama-server not found in the archive" >&2; exit 1; }

dest="$DEST_ROOT/$TRIPLE"
rm -rf "$dest"; mkdir -p "$dest"
cp -a "$(dirname "$bin")/." "$dest/"   # binary + its shared libs (rpath=\$ORIGIN / same-dir DLLs)
chmod +x "$dest/$(basename "$bin")" 2>/dev/null || true
printf '%s\n' "$TAG" > "$dest/.llama-build"
echo "vendored → client/desktop/binaries/$TRIPLE/ ($(du -sh "$dest" | cut -f1))"
