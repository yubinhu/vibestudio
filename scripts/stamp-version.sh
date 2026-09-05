#!/usr/bin/env bash
# Stamp a version into the server workspace and the desktop package. The Tauri app
# and skill-server both read CARGO_PKG_VERSION, so release tags must keep these
# placeholders in sync.
# Called by CI on a tag push (.github/workflows/release.yml); the committed value is
# a `0.0.0` dev placeholder.
#
#   scripts/stamp-version.sh 0.1.6
set -euo pipefail

version="${1:?usage: stamp-version.sh <version>}"
if [[ ! "$version" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]; then
  echo "Expected a stable semantic version, for example 1.1.4" >&2
  exit 1
fi
root="$(cd "$(dirname "$0")/.." && pwd)"

for manifest in "$root/Cargo.toml" "$root/client/desktop/Cargo.toml"; do
  # Each manifest has exactly one top-level `version = "..."` line.
  # `-i.bak` + rm is portable across GNU (Linux) and BSD (macOS) sed.
  sed -i.bak -E "s/^version = \"[^\"]*\"/version = \"${version}\"/" "$manifest"
  rm -f "$manifest.bak"
  rel="${manifest#$root/}"
  echo "Stamped $rel -> $(grep -m1 '^version' "$manifest")"
done
