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

[`.github/workflows/testflight.yml`](.github/workflows/testflight.yml) builds and
releases an **internal-only TestFlight build when a stable GitHub release is
published**. Routine releases use GitHub-hosted macOS 26 runners with Xcode 26.6;
this Mac does not need to be online. The existing **Alpha** tester group receives
the build. No public App Store submission or external testing is performed.

The workflow verifies that the release is public, its tag is on `master`, and
repository CI succeeded for the exact source commit. It builds that immutable
commit in a separate checkout from the release tooling. The existing simulator
CI remains the native launch, authentication, and privacy gate.

To release an already published tag or start a fresh build after a terminal
Apple rejection:

```bash
gh workflow run testflight.yml --ref master -f tag=v1.2.11
gh run list -w testflight.yml --limit 5
```

A manual run must target `master`. A tag push still creates only the desktop/server
draft; TestFlight starts after publication. If a future workflow publishes using
`GITHUB_TOKEN`, it must explicitly dispatch `testflight.yml`: that token's release
events do not trigger another workflow. Do not convert TestFlight to
`workflow_call` without replacing its workflow-specific build-number allocator.

### Hosted signing configuration

Configure these repository Actions secrets once, using `gh secret set` with
values supplied on stdin. Never commit, print, or attach credential files.

| Secret | Value |
| --- | --- |
| `IOS_CERTIFICATE` | Base64 Apple Distribution `.p12`, including its private key |
| `IOS_CERTIFICATE_PASSWORD` | Password for that `.p12` |
| `IOS_MOBILE_PROVISION` | Base64 App Store Connect provisioning profile for `one.vibestudio.app`, matching the certificate |
| `APPLE_API_KEY` | App Store Connect API key ID |
| `APPLE_API_ISSUER` | API issuer ID |
| `APPLE_API_PRIVATE_KEY` | `.p8` private-key contents |

The existing macOS Developer ID/notarization secrets are separate. The workflow
pins app ID `6789766775`, bundle `one.vibestudio.app`, developer team `5J5PGFKG9H`,
and the existing internal Alpha group. Its API key needs access to the app and
permission to upload builds and manage beta metadata/groups.

The app is archived unsigned with the checked-in native project; never regenerate
it with `tauri ios init`, which would lose custom native startup and app-lock code.
The signing helper imports the distribution identity into a temporary runner
keychain, then exports using the matching profile and certificate. It explicitly
sets `testFlightInternalTestingOnly=true` and
`manageAppVersionAndBuildNumber=false`. It does not use the older signing identity
pinned in the native project. The keychain and credential files are removed on
completion, including failed runs. No personal/login keychain password is needed.

Certificate and profile expiry are checked before signing; expiration fails the
run and expiry within 30 days produces a warning. Renew the matching pair and
update the two certificate secrets and profile secret together. Replace the API
secrets when rotating its key. Keep the selected Xcode version compatible with
[Apple's upload requirements](https://developer.apple.com/help/app-store-connect/manage-builds/upload-builds).

### Build identity, retries, and verification

Marketing versions match the release tag. The workflow stamps Cargo and a
temporary Tauri config, overriding the committed `tauri.ios.conf.json` version.
Apple build numbers are `1000 + github.run_number`; rerunning a run preserves its
number. Builds are serialized with a queue, and allocation refuses an existing
or superseded number. Before reaching 9999, migrate the allocator explicitly.

The `testflight-allocation` artifact records the tag, source SHA, versions, run
identity, and beta notes. The `testflight-ipa` artifact records signed bytes and
their SHA-256 **before upload**. Both are retained for 90 days. Use **Re-run failed
jobs** for transient failures: a retry restores the original allocation and IPA,
or resumes distribution if Apple already has that build. It never silently
rebuilds an IPA that may already have been uploaded. Expired or inconsistent
retry records fail closed; reconcile the Apple build before a fresh dispatch.

A lost upload acknowledgement still proceeds to processing checks for that exact
build. The job succeeds only after Apple processing is `VALID`, audience is
`INTERNAL_ONLY`, Alpha membership and existing testers are verified, beta notes
are saved, and the internal state is `IN_BETA_TESTING`. Processing is polled for
up to 20 minutes; a timeout can be resumed with the same run. Evidence and the
verified receipt are attached to the Actions run. Upload success alone is not
delivery success.

For emergency local releases, use the same unsigned archive followed by a manual
App Store Connect export with an explicitly configured distribution identity and
profile. Preserve the internal-only export flag and select a build number above
existing Apple builds. Resume hosted releases with a fresh dispatch afterward;
never reuse an accepted version/build combination. See the
[implementation notes](docs/testflight-ci-plan.md),
[Tauri signing guide](https://v2.tauri.app/distribute/sign/ios/), and
[Apple internal-testing guide](https://developer.apple.com/help/app-store-connect/test-a-beta-version/add-internal-testers).

## Screenshot harness (headless, never touches the live app)

The desktop's own server runs on `:8765` and **must not be killed** (it may host
the agent session driving the release). Verify against a throwaway server with
its own config and tmux socket directory. `--no-startup-maintenance` is required:
Tailscale Serve belongs to the machine, so an isolated config directory alone
does not prevent a test server from changing the live phone-access target.

```bash
# Keep tmux's socket path short enough for macOS as well as Linux.
visual_fixture=$(mktemp -d /tmp/vs-visual.XXXXXX)
mkdir -p "$visual_fixture/config" "$visual_fixture/tmux"
cargo build -p skill-server   # workspace target is ./target, NOT ./server/target
# Fresh server on a spare port; never inherit the agent's tmux connection.
env -u VIBESTUDIO_SERVER_TOKEN -u TMUX -u TMUX_PANE \
  XDG_CONFIG_HOME="$visual_fixture/config" TMUX_TMPDIR="$visual_fixture/tmux" \
  ./target/debug/skill-server --port 8799 --no-startup-maintenance \
  > "$visual_fixture/server.log" 2>&1 &
visual_server_pid=$!
# Vite pointed at it (its /api proxy target is overridable):
VITE_API_TARGET=http://127.0.0.1:8799 node node_modules/vite/bin/vite.js --port 1421 --strictPort \
  > "$visual_fixture/vite.log" 2>&1 &
visual_vite_pid=$!
```

Then drive `http://localhost:1421` with `playwright-core` if installed, or any
headless Chromium/CDP harness against cached Chromium
(`~/.cache/ms-playwright/chromium-*/chrome-linux64/chrome`). Studio needs a real
skill root from `GET /api/skills/discover`, reached via `/#/skills/<encoded-root>`.
Confirm both processes started successfully from their logs before browsing.
After the checks, stop only the processes and private tmux server created above:

```bash
kill "$visual_vite_pid" "$visual_server_pid"
wait "$visual_vite_pid" "$visual_server_pid" 2>/dev/null || true
env -u TMUX -u TMUX_PANE TMUX_TMPDIR="$visual_fixture/tmux" \
  tmux kill-server 2>/dev/null || true
rm -rf "$visual_fixture"
```

Never run a bare `tmux kill-server`, stop the live host service, or kill processes
by name or port during these checks.

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
