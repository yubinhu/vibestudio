# Releasing VibeStudio

How desktop releases and iOS TestFlight builds are verified and shipped. Read
this end-to-end before your first release; after that use **The process** below.

## Signing configuration

Configure the following [repository Actions secrets](https://github.com/yubinhu/vibestudio/settings/secrets/actions):

| Secret | Value |
| --- | --- |
| `TAURI_SIGNING_PRIVATE_KEY` | Updater signing private key matching `plugins.updater.pubkey` in `client/desktop/tauri.conf.json` |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | Private-key password, if encrypted; otherwise leave unset |
| `APPLE_CERTIFICATE` | Base64-encoded Developer ID certificate and private key export (`.p12`) |
| `APPLE_CERTIFICATE_PASSWORD` | Password for the `.p12` export |
| `APPLE_SIGNING_IDENTITY` | Developer ID identity associated with the certificate |
| `APPLE_ID` | Apple account used for notarization |
| `APPLE_PASSWORD` | App-specific password for that Apple account |
| `APPLE_TEAM_ID` | Apple Developer team associated with the certificate |

Use the settings page or `gh secret set --repo yubinhu/vibestudio NAME`, supplying
values through standard input. Keep private keys and certificate contents out of
git and logs. Keep the updater signing key and app identifier stable so installed
apps can verify updates. Actions provides `GITHUB_TOKEN` automatically.

Windows installers use updater signatures but are not Authenticode-signed.

## How the pipeline works

A release is driven by **pushing a `vX.Y.Z` git tag**, or manually dispatching
`build` with an existing tag as its required `tag` input:

1. The tag push triggers [`.github/workflows/release.yml`](.github/workflows/release.yml) (workflow name: `build`).
2. A preflight validates the tag and checks the updater signing secret/public key
   before starting the build matrix. Every build checks out that tag and
   **stamps the version** (`scripts/stamp-version.sh` rewrites the
   `Cargo.toml` placeholders — the committed value is a `0.0.0` dev placeholder, so
   **there is no version-bump commit**, the tag *is* the version).
3. It builds, per OS:
   - **macOS** — one universal (arm64+x86_64) `.dmg`, Developer ID-signed + Apple-notarized + stapled.
   - **Windows** — NSIS `_x64-setup.exe` (currently **unsigned**).
   - **Linux** — `.deb` only (AppImage is disabled; its bundler is flaky on CI runners).
   - **`skill-server`** standalone binaries for 4 targets (musl x86_64/arm64, macOS x86_64/arm64) — used by Remote-SSH provisioning.
4. `tauri-action` creates a **DRAFT** GitHub Release named `VibeStudio vX.Y.Z`
   and uploads each platform's bundles and updater `.sig` files. After all desktop
   and server builds succeed, one `finalize` job validates the complete asset set,
   renames the installers and Mac update archive using the release commit's
   [asset naming policy](release-assets.json), and generates
   one complete `latest.json` with all eight platform entries. Signatures stay in
   the release so retries can reconstruct the manifest. The macOS `.app.tar.gz`
   is retained for auto-update. Windows and Linux updates reuse their installers.
5. The build calls `provision-smoke` with the explicit tag. Each shipped server
   is checksum-verified, checked for the correct version, and booted on its native
   architecture. The build is green only after these checks pass.
6. **You publish the draft.** Publishing flips it to "Latest", which the updater
   reads. [`release-tidy.yml`](.github/workflows/release-tidy.yml) reuses the same
   finalizer for validation/repair, and also supports manual dispatch. Download
   links are ready before publishing; no post-publication rename is required.

Until you publish, **nothing reaches users** — a draft is invisible to the updater.

## Asset names

The release commit's `release-assets.json` is the source of truth for filenames.
The packaging workflow, finalizer, smoke checks, and server provisioner share it.

| Purpose | Filename |
| --- | --- |
| Human download: Mac installer | `VibeStudio-macos.dmg` (Apple silicon + Intel) |
| Human download: Windows installer | `VibeStudio-windows.exe` (x64) |
| Human download: Linux installer | `VibeStudio-linux.deb` (x64 Debian/Ubuntu) |
| Automatic Mac update | `autoupdate-macos-universal.app.tar.gz` |
| Update manifest | `latest.json` |
| Apple silicon / Intel Mac servers | `server-macos-arm64` / `server-macos-x64` |
| ARM64 / x64 Linux servers | `server-linux-arm64-musl` / `server-linux-x64-musl` |

Signatures append `.sig` to the complete payload filename; server checksums append
`.sha256`. Generate each checksum **after** naming its binary so the checksum file
also contains the correct basename. Renaming desktop assets does not change their
bytes or signatures. Keep the updater manifest's schema, platform keys, and signing
key stable; only its payload URLs change.

Release tooling requires the naming policy and updater signature sidecars. The
finalizer reads the policy and signing key from the immutable release commit and
refuses to rename published assets. It accepts Tauri's raw bundle filenames and
the policy's finalized names so interrupted draft finalization can be retried.

Server provisioning uses the policy's filenames at the app's version tag, then
the latest release. Custom download mirrors must provide the same filenames and
matching `.sha256` files. Every new server download must pass checksum verification.

Keep human installer links prominent in release notes; server, auto-update, and
verification files are supporting assets.

## The process

> Per release, the human may say where to **pause** (e.g. "stop after CI, I'll
> publish") or to run the whole thing. **Default: pause for confirmation before
> publishing** (step 8) — everything up to and including the draft is reversible;
> publishing is the outward-facing step.

0. **Pick the version.** Next semver after the last tag. Reusing the number of an
   *unpublished* draft is fine — no user ever received it (see "Overwrite a draft").
   Never rebuild or replace the binaries of a published version.
1. **Local test.** From the repo root:
   ```bash
   npm run build          # tsc --noEmit && vite build
   npm run lint           # eslint
   npm test               # release manifest and workspace state tests
   cargo test --workspace
   ```
   Also review the full diff since the last **published** release
   (`git diff vPREV..HEAD`) for potential bugs before proceeding.
2. **Visual check — the 3 key pages.** Render and *look* (tsc won't catch layout
   bugs). Screenshot **Home**, **Studio**, **Sessions** and confirm no console
   errors. See "Screenshot harness" below. **Gotcha: the SPA is a hash router** —
   `goto("…/skills/<root>")` lands on Home; you must use `…/#/skills/<root>`.
3. **Confirm the tag will be on-branch.** The tagged commit **must be an ancestor
   of `master` and pushed** (`git rev-list --left-right --count origin/master...HEAD`
   → `0  0`). This ensures releases contain reviewed code. Release jobs explicitly
   request `contents: write`; repository Actions policy must allow that permission.
4. **Tag and push.**
   ```bash
   git tag -a vX.Y.Z -m "VibeStudio vX.Y.Z" <commit>   # usually HEAD
   git push origin vX.Y.Z
   ```
   To retry an existing tag using the maintained workflow on `master`:
   ```bash
   gh workflow run release.yml --ref master -f tag=vX.Y.Z
   ```
   This checks out and stamps the supplied tag, including the standalone servers.
   Dispatching a branch without a version tag is rejected.
5. **Watch CI to completion.**
   ```bash
   RUN=$(gh run list --limit 10 --json databaseId,headBranch,name \
     -q '.[] | select(.headBranch=="vX.Y.Z" and .name=="build") | .databaseId' | head -1)
   gh run watch "$RUN" --exit-status --interval 30
   ```
   **macOS notarization is usually the long pole** (~5–20 min; Apple's notary service
   occasionally hangs on a transient — re-run that leg if it stalls far past 20 min).
   After desktop bundles finish, the `skill-server` matrix uploads standalone binaries,
   then finalization and the four native smoke checks must pass.
6. **Fix any errors.** If a leg fails: fix on `master`, push, then **delete and
   re-create the tag at the new HEAD** and re-push (`gh release delete vX.Y.Z --yes
   --cleanup-tag` if a draft was made; then re-tag). Re-running a leg is fine for
   transient infra/notary failures.
7. **Write the release message onto the draft.** Succinct, in the house style: a
   one-line **bold headline**, then a few bullets each led by a **bold** phrase.
   Cover everything since the last *published* release (not the last tag —
   skipped/overwritten drafts mean users may be jumping several commits).
   `gh release list` shows which is "Latest" vs "Draft". Put it on the draft
   right away — drafts are invisible, and it's proofreadable in the UI:
   ```bash
   gh release edit vX.Y.Z --notes-file notes.md
   ```
8. **Publish** (after confirmation, per the pause note):
   ```bash
   gh release edit vX.Y.Z --draft=false --latest
   ```
9. **Verify the published release.** Confirm the final asset set and public links:
   ```bash
   gh run watch "$(gh run list -w release-tidy --limit 1 --json databaseId -q '.[0].databaseId')" --exit-status
   gh release view vX.Y.Z --json isDraft,assets -q '.isDraft, [.assets[].name]'
   ```
   Expect: the 3 renamed installers + `autoupdate-macos-universal.app.tar.gz` +
   `latest.json` + the 4 `server-*` binaries (+ `.sha256`) + updater `.sig` files.
   Verify the
   [public feed](https://github.com/yubinhu/vibestudio/releases/latest/download/latest.json)
   and the three installer links from that release without authentication.

## iOS / TestFlight

The desktop tag workflow does not upload iOS builds. The `iOS simulator` job in
`ci.yml` builds the actual arm64 simulator app without distribution credentials,
installs and launches it, verifies the connection screen is visible, and saves
screenshots and startup diagnostics.
This checks native compilation, linking and startup; device signing, remote SSH
sessions and TestFlight delivery still need separate verification.

For TestFlight, use a Mac with a supported Xcode/iOS SDK and access to the existing
App Store Connect app for `one.vibestudio.app`. Supply an Apple Distribution
certificate/private key and matching App Store Connect provisioning profile, plus
credentials allowed to upload builds. The desktop Developer ID/notarization
secrets are separate. For manual signing, Tauri accepts a base64-encoded `.p12`
in `IOS_CERTIFICATE`, its password in `IOS_CERTIFICATE_PASSWORD`, and a
base64-encoded profile in `IOS_MOBILE_PROVISION`. Alternatively, configure Xcode
automatic signing with a suitable account or the complete `APPLE_API_KEY`
(key ID), `APPLE_API_ISSUER` and `APPLE_API_KEY_PATH` trio. The checked-in release
Xcode configuration uses manual signing: for automatic signing, change that
configuration in the build checkout first. See [Tauri's signing guide](https://v2.tauri.app/distribute/sign/ios/).

Build from a clean checkout of the desktop release tag. Keep the Cargo manifests'
committed `0.0.0` placeholders. `tauri.ios.conf.json` currently overrides the app
version, so Cargo stamping alone does not version an iOS release. Merge a
temporary config with the release version and a build number unused in App Store
Connect. For example, after setting `IOS_RELEASE_VERSION` and `IOS_BUILD_NUMBER`:

```bash
npm ci
npm run build # iOS resolves bundled resources before beforeBuildCommand
rustup target add aarch64-apple-ios
export IOS_RELEASE_VERSION IOS_BUILD_NUMBER
bash scripts/stamp-version.sh "$IOS_RELEASE_VERSION"
ios_release_dir=$(mktemp -d -t vibestudio-ios)
ios_release_config="$ios_release_dir/release.json"
python3 - "$ios_release_config" <<'PY'
import json, os, sys
with open(sys.argv[1], "w") as destination:
    json.dump({"version": os.environ["IOS_RELEASE_VERSION"],
               "bundle": {"iOS": {"bundleVersion": os.environ["IOS_BUILD_NUMBER"]}}},
              destination)
PY
npm run tauri -- ios build --target aarch64 --ci \
  --export-method app-store-connect --config "$ios_release_config"
rm "$ios_release_config"
rmdir "$ios_release_dir"
```

The explicit export method overrides the checked-in `debugging` setting. Inspect
the resulting `.ipa` version, bundle ID and provisioning before uploading. With
API-key authentication, place the private key in an `altool` search directory as
`AuthKey_<KEY_ID>.p8` (for example `~/.appstoreconnect/private_keys/`), then run:

```bash
xcrun altool --upload-app --type ios \
  --file client/desktop/gen/apple/build/arm64/VibeStudio.ipa \
  --apiKey "$APPLE_API_KEY" --apiIssuer "$APPLE_API_ISSUER"
```

Wait for Apple processing, check the build's TestFlight status, and add it to the
intended tester group. External testing can require Beta App Review. Upload
success alone does not confirm testers can install it. See the
[Tauri build/upload guide](https://v2.tauri.app/distribute/app-store/) and
[Apple's upload requirements](https://developer.apple.com/help/app-store-connect/manage-builds/upload-builds).

## Screenshot harness (headless, never touches the live app)

The desktop's own server runs on `:8765` and **must not be killed** (it may host
the agent session driving the release). Verify against a throwaway server instead:

```bash
# fresh server on a spare port, no auth token:
cargo build -p skill-server   # workspace target is ./target, NOT ./server/target
env -u VIBESTUDIO_SERVER_TOKEN ./target/debug/skill-server --port 8799 &
# vite pointed at it (its /api proxy target is overridable):
VITE_API_TARGET=http://127.0.0.1:8799 npx vite --port 1421 --strictPort &
```

Then drive `http://localhost:1421` with `playwright-core` if installed, or any
headless Chromium/CDP harness against cached Chromium
(`~/.cache/ms-playwright/chromium-*/chrome-linux64/chrome`). Studio needs a real
skill root from `GET /api/skills/discover`, reached via `/#/skills/<encoded-root>`.

## Key facts & gotchas

- **No version-bump commit** — the tag is the source of truth; `stamp-version.sh`
  injects it in CI. Manifests stay `0.0.0`.
- **On-branch tags only** (step 3) — release reviewed code from the default branch.
- **Drafts are invisible to the updater** — only the published "Latest" release feeds auto-update.
- **Overwrite an unpublished draft version:** `gh release delete vX.Y.Z --yes
  --cleanup-tag`, then re-tag at the new commit and re-push. Safe because no user got the draft.
- **Hash router** — screenshots/deep links need `/#/…`.
- **Updater signing guard** — CI fails fast if the signing secret is absent or
  `tauri.conf.json` still carries a placeholder pubkey. Finalization rejects
  signatures from a key that does not match the configured public key. Configure
  the secrets listed in "Signing configuration" before building.
- **macOS** signing/notarization secrets and the **updater signing key** live in
  repo Actions secrets (see `release.yml` env). Windows Authenticode is currently
  off.
