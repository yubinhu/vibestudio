# TestFlight release implementation

Implemented September 20, 2026. Operational instructions and credential renewal
are in [RELEASING.md](../RELEASING.md#ios--testflight).

## Release boundary

[The TestFlight workflow](../.github/workflows/testflight.yml) starts when a stable
GitHub release is published, or through a manual `workflow_dispatch` of an existing
published tag on `master`. It runs on GitHub-hosted macOS 26 with Xcode 26.6. The
maintainer's computer is not required after the one-time secret migration.

The desktop/server release pipeline still produces a draft. Publishing the draft
starts TestFlight; creating the tag alone does not deliver an iOS build. If a
future publisher uses `GITHUB_TOKEN`, explicitly dispatch TestFlight after it
publishes: that token's release event will not start another workflow.
[GitHub documents this trigger restriction](https://docs.github.com/en/actions/concepts/security/github_token).

The app and release tools use separate immutable checkouts. The app source is the
published tag's resolved SHA, which must be on `master` and have passing exact-SHA
CI. A manual workflow run uses the current workflow's scripts even for an older
app tag. The native Xcode project is never regenerated.

## Signing and audience

The proven local process was an unsigned Tauri archive followed by a manual
App Store Connect export. The hosted workflow follows the same process. This
avoids the obsolete distribution identity pinned in the native project.

GitHub secrets hold the Apple Distribution PKCS12 identity/password, matching
profile, and App Store Connect API key/issuer/private key. Signing secrets are
available only during signing preparation; API credentials only during Apple
requests and upload. Private key material is never included in artifacts. The
runner creates a temporary keychain, restores its original search list, and
removes the new keychain and profile during cleanup. The login keychain password
and access controls are not changed.

[The signing helper](../scripts/testflight-signing.py) validates certificate and
profile expiration and produces explicit export options. They preserve
`testFlightInternalTestingOnly=true` and
`manageAppVersionAndBuildNumber=false`. The IPA is independently checked for
bundle/team/version/build, device architecture, provisioning, entitlements,
certificate, and strict code signature before upload.

The audience remains the existing internal **Alpha** group for
`one.vibestudio.app`. External beta testing and production App Store submission
are outside this workflow. The initial local release was **1.2.11 (10)**; the
hosted allocator begins at **1001**.

## Durable retries

[GitHub preflight](../scripts/testflight-release.mjs) allocates
`1000 + github.run_number`, recording tag, SHA, version, build number, and run
identity before building. Reruns preserve the allocation. The workflow remains
standalone: reusable workflows inherit their caller's `github` context and would
break a workflow-specific run-number allocator.

The global concurrency group uses `cancel-in-progress: false` and `queue: max`.
This preserves up to 100 pending releases instead of replacing an older pending
run. Queue order is when runs enter the queue; the Apple maximum-number check
rejects a superseded allocation rather than uploading out of order.
[GitHub concurrency reference](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax#concurrency).

The allocation artifact and signed IPA/checksum artifact are retained for 90 days.
Reruns validate the allocation and restore the exact signed bytes, or resume Apple
processing when the build already exists. Missing, expired, conflicting, or
changed retry evidence fails closed. A new dispatch gets a new build number; do
that only after reconciling an uncertain earlier upload or terminal rejection.
Use failed-job or specific-job reruns. A full workflow rerun deletes the previous
artifacts and intentionally fails the missing-evidence guard.

[Apple reconciliation](../scripts/testflight-apple.mjs) checks the app and group,
queries all builds matching the allocated number, and refuses wrong-version,
expired, rejected, or wrongly addressed builds. Read-only transient errors retry
with bounded backoff; API writes are reconciled on rerun rather than blindly
repeated. An upload acknowledgement is followed by up to 20 minutes of processing
checks. A successful receipt requires `VALID`, `INTERNAL_ONLY`,
`IN_BETA_TESTING`, Alpha membership, existing testers, and saved beta notes.

## Verification

Focused tests cover allocation and exact-SHA CI gates, artifact tampering, API
pagination/retries, build identity and audience failures, processing timeouts,
rerun idempotence, distribution proofs, signing metadata, and cleanup ownership.
The IPA verifier has also been run against the already released local IPA.

Hosted delivery and retry were verified on September 20, 2026:

- [Setup CI](https://github.com/yubinhu/vibestudio/actions/runs/35497560232)
  passed frontend tests/lint/build, both server platforms, desktop, and iOS simulator.
- [Hosted release](https://github.com/yubinhu/vibestudio/actions/runs/35498192852/attempts/1)
  built published tag `v1.2.11` as **1.2.11 (1002)** and verified internal Alpha
  delivery, beta notes, and its existing tester.
- [Specific-job retry](https://github.com/yubinhu/vibestudio/actions/runs/35498192852/attempts/2)
  restored the allocation and signed IPA, verified the same Apple build, and
  skipped both archive and upload. Both receipts identify Apple build
  `f9556e9e-2b08-403a-9686-454d6599515c` and the same package checksum.

The first setup attempt exposed an incompatible PKCS#12 wrapper; repackaging the
same identity with explicit 3DES/SHA-1 wrapping fixed the hosted import. A full
workflow rerun also confirmed GitHub's artifact deletion behavior and the
missing-evidence guard. Neither failed setup attempt uploaded a build.

Primary references: [GitHub hosted signing](https://docs.github.com/en/actions/how-tos/deploy/deploy-to-third-party-platforms/sign-xcode-applications),
[Tauri iOS signing](https://v2.tauri.app/distribute/sign/ios/),
[Apple uploads](https://developer.apple.com/help/app-store-connect/manage-builds/upload-builds),
and [Apple internal testing](https://developer.apple.com/help/app-store-connect/test-a-beta-version/add-internal-testers).
