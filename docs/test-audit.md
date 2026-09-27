# Automated test audit — historical findings and follow-up

The original audit reviewed commit `1f97f5f` and the working tree on 2026-09-19.
It used source/contract review and the test runs recorded below, rather than
line-coverage measurement or mutation testing. This document was reconciled with
the subsequent cleanup and browser coverage on 2026-09-27. It is a historical
record and remaining-work list, not a current test inventory or a fresh test run.

The audit's conclusion still applies: the suite needs clear ownership, reliable
fixtures, and coverage of important contracts, rather than a numeric ceiling.
Most cases protect useful behavior. Removing obsolete cases must preserve the
distinct failure boundaries they still cover.

## Completed follow-up

Commit [`0a4375b`](https://github.com/yubinhu/vibestudio/commit/0a4375bc1874725eab5f6c2e2174997fa154bde8)
(`test: consolidate coverage and repair stale fixtures`) implemented the following
audit recommendations. Do not repeat these as outstanding fixes.

| Original finding | Completed change |
| --- | --- |
| Terminal lifecycle, registry and pasted-image tests touched normal user state; agent discovery probed installed tools | Stateful terminal cases now run in owned subprocesses with private home/config/cache/tmux roots and controlled discovery inputs. Required tmux availability fails explicitly. |
| Secrets roundtrip mutated process-global `XDG_CONFIG_HOME` | The test now runs in a separate process with its own configuration directory. |
| Default discovery scanned real HOME | `live_discovery_smoke` is explicitly ignored as a manual diagnostic. |
| SSH fixtures used an invalid `e2e-test` version and silently passed without prerequisites | Switchboard fixtures use the compiled version; the four real-SSH cases are explicitly ignored and validate their required environment when selected. |
| GPU, symlink, executable-name and JWT assertions could miss their named failure | GPU override runs in a child process; copy tests create real symlinks; executable variants have exact platform assertions; JWT validity is checked relative to issuance time. |
| Stabilization, basic attachment and progressive-filter cases duplicated coverage or tested unused APIs | Stabilization tests were removed with relevant state distinctions retained under Tracker; the weak attachment roundtrip and redundant filter case were removed. |
| Orphaned/no-op helpers and redundant adapter assertions | The unused Swift launch checker, manifest test wrapper and `discoverSkills()` test fragment were removed. The production `discoverSkills()` wrapper remains. |
| Repeated frontend compilation harnesses | Several suites now share `scripts/test-helpers.mjs`; this does not replace their React hook approximations. |
| Claims in test names exceeded their assertions | AppShell/search names and the detector-free attention fallback description were corrected. |
| Three pipe-checksum cases repeated setup | A named-case table retains the rejection matrix. Remote-shell checksum coverage remains separate. |

The original inventory counted 517 named cases; the cleanup commit recorded 503.
Neither number describes the current tree, which has subsequent tests. The commit
also records its own validation; those results do not validate later changes.
It deliberately left production behavior and CI configuration unchanged, including
the unused stabilization helpers and missing desktop Rust test execution.

The later [browser suite](../e2e/) adds real React/browser-to-backend checks for
skill search and recents, autosave across navigation/reload, attention state,
terminal clipboard events, and saved-session history. History coverage includes
exact-ID resume, concurrent/repeated requests, unavailable folders, executable
availability, stale responses and pagination. Each case owns its backend, files,
credential-free agent fixture and tmux socket. This closes selected gaps in the
original audit, without proving native iOS gestures or all editor reconciliation
and reconnect lifecycles. [Development and validation](development.md#validation)
owns the current commands and coverage limits.

## Remaining work

These findings were not completed by `0a4375b` or the browser scenarios above.
Recheck the relevant implementation when taking one on; this is not a promise
that every historical line location or test count remains unchanged.

### Fixture reliability and platform execution

- Isolate inherited Git signing, hooks and global configuration in older fixtures.
  The override test in [commit_agent.rs](../server/skill-core/src/commit_agent.rs)
  still changes process-global environment; prefer injected inputs or a child
  process over a growing environment mutex.
- Bound fake-server `recv()`/`accept()` and thread joins in
  [workspace_state.rs](../server/skill-server/tests/workspace_state.rs),
  [sshmgr/session.rs](../server/skill-server/src/sshmgr/session.rs) and
  [sshmgr/wsl.rs](../server/skill-server/src/sshmgr/wsl.rs). A missing request must
  fail within a deadline and clean up the fixture.
- Finish the prerequisite audit outside the migrated terminal/SSH cases. Missing
  Git/tmux, unsupported OSC 8 versions and unavailable IPv6 must not be mistaken
  for executed coverage. Use explicit manual/platform lanes or controlled
  fixtures, and report skips separately.
- Add an owned disposable SSH harness with explicit detached-host cleanup and a
  lane that actually executes the repaired external tests. Marking them ignored
  and correcting SemVer does not prove provisioning and tunnel behavior.
- Execute applicable desktop Rust tests in CI. The root Cargo workspace excludes
  `client/desktop`, and its CI job currently runs `cargo check`. Linux tests,
  macOS Keychain/native decoding, and Windows media handling need their relevant
  environments. Native iOS builds and the authentication harness cover different
  boundaries. Connected-device SSH, keyboard and background/resume behavior also
  remain separate from browser and simulator coverage.

### Assertions that need a stronger failure boundary

| Owner | Remaining improvement |
| --- | --- |
| [proxy_smoke.rs](../server/skill-server/tests/proxy_smoke.rs) | Give the upstream a unique response and record method, URL/query, bearer and exact request body. A local fallback must not satisfy the proxy assertion. Check pinned-local identity separately. |
| [terminal_links.rs](../server/skill-server/tests/terminal_links.rs) | Keep the real filesystem/tmux resolution proof, but distinguish the upstream in the proxy phase. |
| [push.rs](../server/skill-server/src/push.rs) | Replace shared-state, uptime-dependent suppression/expiry cases with controlled time and isolated state. The JWT assertion was fixed; these attention tests were not. |
| [release-assets.test.mjs](../scripts/release-assets.test.mjs) | Replace shell-source-string assertions with controlled execution against a stub `gh`; retain the existing packaging/checksum execution. |
| [remote-menu.test.mjs](../scripts/remote-menu.test.mjs), [updater-lifecycle.test.mjs](../scripts/updater-lifecycle.test.mjs) | Use real React mounting, cleanup and timers for lifecycle claims. Shared module loading alone does not establish those semantics. |
| [app-shell.test.mjs](../scripts/app-shell.test.mjs) | Add a lifecycle proof that the same terminal survives navigation/reconnection. Static SSR proves rendering and input gating. |

### Product contracts with material gaps

1. **Editor data preservation and concurrency.** The new browser tests prove
   autosave across navigation and reload. Still cover clean external reload,
   dirty conflicts, resolution choices and delayed old-file responses in the real
   editors. Standalone Markdown and skill-editor reconciliation differ; make the
   supported behavior explicit in the cases. The sequential stale-write test in
   [skill.rs](../server/skill-core/src/skill.rs) does not establish atomic
   compare-and-swap: checking disk and writing are separate operations. Add a
   deterministic competing-writer case, then align the implementation and
   documented guarantee. Include HTTP etag/status behavior.
2. **Packaging and extraction.** Inspect archive contents: exclude nested `.env`,
   `.git`, runtime folders and symlinks while retaining authored `evals`,
   `references`, `assets` and `scripts`. Use crafted local archives to exercise
   traversal and expanded-size rejection; path-containment unit tests alone do
   not establish ZIP behavior.
3. **HTTP routing, gateway and trust.** Cover distinguishable upstream identity,
   method/body/header/status preservation, binary and SSE responses, unavailable
   hosts, bearer/origin decisions and pinned-local routes. Gateway coverage needs
   token replacement, 401 refresh/retry, reauthentication and streamed/bodiless
   response behavior beyond ID parsing.
4. **OAuth, tracking and preferences.** Use fake services/stores for callback
   persistence, rotating refresh tokens and single-flight refresh. Exercise
   tracking opt-out through discovery and explicit re-track, eligibility and
   missing-identity baseline behavior. Test preference setters through their
   public workflows instead of relying only on generic storage roundtrips.
5. **Phone controls and native updates.** Tailscale parsing and push crypto do
   not establish enable/disable/idempotency or push-route behavior. Frontend
   updater mocks do not establish native installation rollback and exit behavior.
   Keep release-artifact smoke checks: they validate shipped assets, a different
   boundary from source-level host-service tests.

Lower-priority gaps include progressive discovery retaining a live gallery query,
positive CUDA-library fixtures, provider-specific session-title fixtures, and an
independent known-answer Web Push crypto vector. Keep attention classification,
process lifecycle, wire delivery and user announcements under their separate
owners rather than merging distinct obligations into one broad test.

### Small cleanup and ownership decisions

The test-only consolidation left unused private stabilization helpers and the
unused `discoverSkills()` production wrapper. Review their callers before removal.
The unused public `identify_agent_in_job` API and its process-group cases need a
separate scope decision. Preserve the imported matcher/process compatibility
corpus and the helpers Tracker uses; broad deletion would remove useful coverage.
Likewise, offline engine/GPU and migration cases still protect supported opt-in
and compatibility behavior.

Move launch-recipe matrices to the agent registry where appropriate, retaining a
mining delegation proof. Share remaining fixture boilerplate only where semantics
match. For future cases, require a current contract, one primary owner, a plausible
regression the assertion would catch, a deliberate platform lane and owned
fixtures. Retire a case when its contract disappears or another owner covers the
same failure boundary with equivalent assertions.

## Original validation — 2026-09-19 only

These are the original audit's results, before `0a4375b` and the browser suite.
They are retained as historical evidence, not instructions or current pass counts.

| Run | Recorded result |
| --- | --- |
| Node 22.22.2 `npm test` | 115 passed; two explicit skips for absent zsh/fish. |
| Default Linux workspace/all-target Rust run, excluding real-HOME discovery and global-XDG secrets | 373 passed; two filtered. |
| Secrets roundtrip in its own test process | One passed. |
| Mobile-feature server library tests | Runner reported 61 passed; two silently returned without real SSH, so 59 actually exercised. |
| Desktop Rust library tests on Linux | Eight passed. |
| Portable C++ app-lock executable | Passed. |
| Root Cargo doctests | Zero tests. |
| Independent private-IP experiment, excluded from app totals | Four passed. |

The default Rust run isolated config/tmux but inherited Git configuration. The
paste-cache side effect was identified after that run, so the audit cannot
establish whether normal expiry cleanup removed any pre-existing expired cached
images. Subsequent commands isolated cache too; the cleanup commit later made
the affected test own its cache. The real-HOME diagnostic was not executed.

macOS/Windows-specific tests, the native iOS simulator, real SSH opt-ins and
shipped-artifact smoke were not executed in this audit. Warm test runs took
seconds; there was no observed runtime basis for an arbitrary 300-test ceiling.
