# Release migration from AltrinaAI to yubinhu

This is the one-time migration guide. Use [RELEASING.md](../RELEASING.md) for
routine releases and the build/publish checklist.

## What needs to move

`yubinhu/vibestudio` is a separate repository from `AltrinaAI/vibestudio`.
Changing git remotes and source URLs did not transfer GitHub Releases, release
assets, tags, Actions secrets, or the updater endpoint that is already compiled
into installed apps. The old repository still serves its own releases; its URL
does not redirect to the new repository.

The migration audit found `v1.1.3` as the latest published old release. Its
installers and standalone servers are available at
[the old release](https://github.com/AltrinaAI/vibestudio/releases/tag/v1.1.3).
The source already points all new builds at `yubinhu/vibestudio`:

| Consumer | Configuration |
| --- | --- |
| Desktop installer | `client/desktop/tauri.conf.json`, `plugins.updater.endpoints` |
| Update notification and manual download | `server/skill-core/src/update.rs` |
| Remote-SSH server download | `server/skill-server/src/sshmgr/provision.rs` |
| Public installer links | `README.md` |

Keep `identifier: one.vibestudio.app` and the committed updater public key
unchanged. Both match `v1.1.3`; changing the repository owner does not require a
new app identity or signing key.

## Restore signing secrets before building

Add the following repository Actions secrets to
[yubinhu/vibestudio](https://github.com/yubinhu/vibestudio/settings/secrets/actions)
from the original local backups or credential store. GitHub lists existing
secret names but cannot return their values.

| Secret | Value needed |
| --- | --- |
| `TAURI_SIGNING_PRIVATE_KEY` | The **original** updater signing private key corresponding to the public key in `tauri.conf.json` |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | The original key's password, if encrypted; leave unset for an unencrypted key |
| `APPLE_CERTIFICATE` | Base64-encoded Developer ID certificate export, including its private key (`.p12`) |
| `APPLE_CERTIFICATE_PASSWORD` | Password used when exporting the `.p12` |
| `APPLE_SIGNING_IDENTITY` | The Developer ID signing identity associated with the certificate |
| `APPLE_ID` | Apple account used for notarization |
| `APPLE_PASSWORD` | App-specific password for that Apple account |
| `APPLE_TEAM_ID` | The Apple Developer team that owns the signing certificate |

The old repository has the updater key and all six Apple secrets above; it has
no updater password secret. The documented original key location is
`~/.tauri/vibestudio.key`. If the local key cannot be recovered, do not generate
a replacement and assume existing apps will accept it: those apps trust the
original public key. Recover that key or plan a separately communicated manual
installation path.

Use the settings page or `gh secret set --repo yubinhu/vibestudio NAME`, supplying
values through standard input. Keep key and certificate contents out of shell
arguments, logs, git, and issue/PR text. `GITHUB_TOKEN` is provided automatically
by Actions; it is not a secret to copy. Windows Authenticode signing is currently
disabled, so the old repository's Azure signing credentials are not required
for this pipeline. The updater signing key is still required on Windows.

## Restore downloads and version-pinned servers

Copy already-published historical releases if they must remain available under
the new repository. Copy the release's original tag/commit, notes, and **all**
assets into a draft in the new repository, then verify it before publishing.
Use the original files; do not rebuild a previously published version and
replace its installers with different binaries.

For `v1.1.3`, the complete asset set contains 13 files:

- `VibeStudio-macOS.dmg`, `VibeStudio-Windows-x64-setup.exe`, and
  `VibeStudio-Linux-x86_64.deb`.
- `VibeStudio_universal.app.tar.gz` and `latest.json`.
- Four `skill-server-<target>` binaries, each with its `.sha256` file:
  `aarch64-apple-darwin`, `x86_64-apple-darwin`,
  `aarch64-unknown-linux-musl`, and `x86_64-unknown-linux-musl`.

Update each `platforms.*.url` in the copied `latest.json` to the matching asset
in `yubinhu/vibestudio`, preserving every signature, the version, and the
publication date. These signatures cover installer bytes, so moving identical
assets to a new URL does not require the private signing key. Verify downloaded
asset hashes match the originals and each manifest URL resolves before making
the copied release Latest. After copying multiple releases, explicitly keep the
newest intended stable release as Latest.

This restores README downloads, the new feed, and Remote-SSH's pinned URLs.
Copied historical installers still contain the old updater endpoint; restoring
downloads alone does not move existing users to the new feed. Keep the old
repository and its release assets available during the migration.

## Build the release that moves installed apps

1. Push the corrected workflows to the new repository's default branch. Confirm
   they appear under Actions and restore the required signing secrets.
2. Create a **new** release tag after `v1.1.3` (for example, `v1.1.4`) on the
   tested source containing the new updater URL. Push that tag, or dispatch the
   build workflow with that explicit tag. Manual builds check out and stamp the
   supplied tag, rather than building a `0.0.0` branch snapshot.
3. Wait for the signing preflight, every desktop/server build, and draft
   finalization to succeed. Finalization gives assets their stable names and
   updates and checks `latest.json` while the release is still a draft.
4. Verify the draft, write the release notes, and publish using the normal
   release checklist. For any needed asset repair, `release-tidy` also supports
   a manual run for an explicit tag and is safe to rerun on an already tidied
   release.
5. Verify the new public feed and downloads below before changing anything in
   the old repository.

## Bridge the old updater endpoint

After the first **newly built** release is published and verified, the old
endpoint must advertise that newer release to installed apps:

`https://github.com/AltrinaAI/vibestudio/releases/latest/download/latest.json`

Back up the manifest currently served there, then deliberately replace that
asset with the verified new release's `latest.json`. Keep its new repository
URLs and original signatures. The bridge manifest's version must be greater
than `1.1.3`; mirroring a `1.1.3` manifest cannot update an app already on
`1.1.3`. Do not change the old historical installers or server binaries.

The old feed then offers the new signed binary. After installing and restarting,
that binary checks `yubinhu/vibestudio` directly. Verify this using an installed
old version, including signature verification, restart, and a subsequent check
against the new endpoint. Also verify manual installer downloads on each OS.

The new repository's workflows do **not** write to the old repository. Perform
the bridge as a separate migration operation after checking the published
release; no cross-repository token is needed for ordinary future builds. Keep
the old feed available for users who return later, and avoid publishing another
old-repository release that would replace its Latest endpoint.

## Verify public links

These must work without authentication:

- [New updater feed](https://github.com/yubinhu/vibestudio/releases/latest/download/latest.json).
- [macOS installer](https://github.com/yubinhu/vibestudio/releases/latest/download/VibeStudio-macOS.dmg),
  [Windows installer](https://github.com/yubinhu/vibestudio/releases/latest/download/VibeStudio-Windows-x64-setup.exe),
  and [Linux installer](https://github.com/yubinhu/vibestudio/releases/latest/download/VibeStudio-Linux-x86_64.deb).
- Every `platforms.*.url` in `latest.json`, with a nonempty signature matching
  the downloaded bytes and the committed updater public key.
- Each `releases/download/vX.Y.Z/skill-server-<target>` URL and its `.sha256`
  for the version being shipped.
- The old updater feed, advertising the first verified newer release after
  the bridge is installed.

## GitHub Actions cost

This repository is public and the build uses standard GitHub-hosted runners.
Their Actions minutes are free, including the standard macOS and Windows
runners; a paid personal plan or paid minutes are not needed for this setup.
Larger runners and usage beyond applicable storage allowances can incur charges,
and private repositories have different minute allowances. See
[GitHub's current Actions billing documentation](https://docs.github.com/en/billing/concepts/product-billing/github-actions).
