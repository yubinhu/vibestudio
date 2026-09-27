# Paseo and VibeStudio — competitive research, 2026-09-20

This is dated research and a set of proposals, not an implementation plan that has been approved or a description of features VibeStudio already ships.

**VibeStudio follow-up, 2026-09-27:** saved Claude Code and Codex conversation search and exact-ID resume are now implemented. The history assessment and sequence below reflect that change; they still distinguish provider-owned saved conversations from the proposed richer task records. The Paseo findings remain the September 20 source review, not a new review of its current release.

**Assessment:** Paseo has a more complete experience for supervising coding agents across devices. Its strongest advantages are structured conversations and approvals, convenient phone pairing, task workspaces with integrated review, and programmatic orchestration. VibeStudio already has much of the remote execution foundation and offers a deeper skill-authoring workflow. The highest-value response is to connect that foundation to a better run-and-review experience while retaining skill discovery, versioning and mining as the product's specialization.

**Scope and confidence**

- Paseo was cloned into `/home/harvey/repos/paseo`. The inspected revision is [`d636abd7a4ce302e7ccb9eb6074f637c6dd4d83b`](https://github.com/getpaseo/paseo/commit/d636abd7a4ce302e7ccb9eb6074f637c6dd4d83b), dated September 18, with package version `0.9.0-beta.2`.
- VibeStudio was inspected at `a51f9f87618e7746d236ef2298cffb5dffbaeccc` **plus the existing local changes**, including the browser E2E additions, terminal touch work and updated documentation. Those changes were left intact.
- Research included both source trees, architecture and operational documentation, current Paseo public documentation/changelog, and browser inspection of its homepage and web-app welcome screen. The public web app displayed `v0.8.0` during inspection.
- This was not a paired-phone trial, a real-provider task benchmark, a security audit, or a test-suite run. “Implemented” below means a concrete implementation was located, not that every platform and provider was exercised.
- Paseo's homepage presents authored interactive mockups. They demonstrate positioning and workflow design; source inspection supplies the evidence for actual capabilities.

**Ranked findings**

The ranking estimates relevance to VibeStudio's current promise of running agents from desktop and phone. It is a product judgment, not measured conversion or retention data.

| Priority | What Paseo does better | Our current position | Suggested response |
|---|---|---|---|
| 1 | Readable conversations, typed questions and approval controls | Ordinary sessions expose an agent TUI through xterm | Add a structured run mode for a small provider set |
| 2 | Optional phone pairing without SSH/VPN setup | Native SSH key installation; browser QR depends on Tailscale | Reduce setup steps; evaluate optional outbound connectivity |
| 3 | Task workspace → diff → agent revision → PR | Rich skill diffs/versioning, but generic runs are separate terminals | Connect each run to changed files and review feedback |
| 4 | Task grouping and richer run history | Saved Claude/Codex conversations support search and exact-ID resume; mining has separate history | Add task records linking conversations to summaries, skill versions and changed files |
| 5 | Native remote push and useful mobile input | PWA Web Push and native local notifications; no native APNs yet | Complete native background attention and add a context composer |
| 6 | CLI, SDK, MCP, schedules and heartbeats | Internal HTTP API; no equivalent supported orchestration surface found | Expose a small agent-facing interface and scheduled skill runs |
| 7 | Cached conversation state and safe retry receipts | Strong tmux survival/recovery; mutation retries deliberately limited | Add operation identities and cached run summaries |
| 8 | Model/profile controls and extensible provider support | Multi-agent launcher with ordinary-session flags entered manually | Model capabilities and reusable run profiles |
| 9 | Public proof, documentation and distribution | Current landing page states the value with little demonstration | Show real desktop-to-phone and skill-improvement journeys |
| 10 | Specific native-interaction and performance evidence | Real functional browser/native coverage already exists | Add connected-device journeys and measured hot-path budgets |

**1. Their interface understands agent work**

Paseo represents messages, tool calls, task lists, permissions, questions and lifecycle events as data. Its permission renderer displays the plan or question and sends an identified response. Tool details have a dedicated mobile sheet. This lets a phone user read the result and act on a request without manipulating a terminal screen. The relevant implementations are the [semantic event model](https://github.com/getpaseo/paseo/blob/d636abd7a4ce302e7ccb9eb6074f637c6dd4d83b/packages/protocol/src/agent-types.ts#L371), [permission and question rendering](https://github.com/getpaseo/paseo/blob/d636abd7a4ce302e7ccb9eb6074f637c6dd4d83b/packages/app/src/agent-stream/view.tsx#L1397), and [mobile tool sheet](https://github.com/getpaseo/paseo/blob/d636abd7a4ce302e7ccb9eb6074f637c6dd4d83b/packages/app/src/components/tool-call-sheet.tsx#L73).

Our ordinary session view renders [TerminalPane](../client/web/components/SessionsWorkspace.tsx), backed by xterm. The [agent registry](../server/skill-core/src/agents.rs) intentionally launches the original interactive CLI. This preserves the provider's own behavior and avoids maintaining a replacement for every CLI feature, but the phone user still operates terminal controls. Our [attention watcher](../server/skill-server/src/events.rs) infers state from terminal/process observations; its blocked state cannot itself supply a typed approval response.

The practical gap is visible in a simple scenario: an agent needs a decision while the user is away from the desk. Paseo can show the relevant text and response controls. VibeStudio can notify the user and reconnect to the terminal, where they must find and answer the prompt.

**Proposal:** support an explicit structured session mode for one or two important providers. Start with the latest result, tool status, questions/approvals, errors and a mobile composer. Keep the current terminal mode available. This requires provider integration and event contracts; reliable approvals cannot be obtained through visual styling or terminal-text parsing alone. Expanded plan cards and within-chat Find are specifically 0.9 beta refinements; the basic structured interaction predates them.

**2. They own more of the phone onboarding journey**

Paseo's optional relay flow is enable pairing, scan a QR code or paste a link, then connect. Both endpoints connect outward, so the user does not first install a VPN or configure inbound access. The relay is opt-in. See the [current connectivity guide](https://paseo.sh/docs/connectivity) and [pairing-offer implementation](https://github.com/getpaseo/paseo/blob/d636abd7a4ce302e7ccb9eb6074f637c6dd4d83b/packages/server/src/server/pairing-offer.ts#L25).

VibeStudio already has a QR flow, so “add QR pairing” misdiagnoses the gap. Our browser phone flow still requires Tailscale on both devices. Native iOS connection requires a host/user and SSH key setup, including copying public-key installation commands onto the computer. These are concrete steps in [PhoneModal](../client/web/components/PhoneModal.tsx) and [the SSH connection form](../client/web/components/connections.tsx). Password bootstrap remains [proposed](mobile-ux.md).

**Proposal:** measure the path from installation to the first working phone session. Improve the existing setup guidance and key bootstrap, and evaluate optional outbound connectivity with authenticated device enrollment. SSH and Tailscale remain useful alternatives. This can preserve our HTTP/JSON + SSE contract; it does not require a WebSocket rewrite.

There is a countervailing advantage: our SSH connection provisions a checksum-verified, version-appropriate remote server and starts/reuses it. Paseo's SSH guide explicitly requires an existing running daemon. We should preserve that capability.

Pairing convenience is not evidence of superior security. Paseo's [security document](https://github.com/getpaseo/paseo/blob/d636abd7a4ce302e7ccb9eb6074f637c6dd4d83b/SECURITY.md#L39) acknowledges that replay protection within a live encrypted session is not implemented. This review did not audit its separate production relay service. A relay also introduces hosting, availability and device-management responsibilities.

**3. They connect execution to review and shipping**

Paseo gives a task a workspace containing agents, terminals, files, diffs and browser tabs. A workspace can use an existing directory or a managed git worktree. The [worktree service](https://github.com/getpaseo/paseo/blob/d636abd7a4ce302e7ccb9eb6074f637c6dd4d83b/packages/server/src/server/paseo-worktree-service.ts#L64) creates the checkout, records its branch/base and handles failure cleanup. Repo-defined setup, teardown and services make a new worktree usable; services receive distinct ports. See [worktree documentation](https://paseo.sh/docs/worktrees).

Their git surface implements commit, push/pull, PR creation, merge and auto-merge. More useful than the number of commands is the connection between review and agent input: users can attach PR comments or a review thread to the next message. See the [git actions](https://github.com/getpaseo/paseo/blob/d636abd7a4ce302e7ccb9eb6074f637c6dd4d83b/packages/app/src/git/actions-store.ts#L105) and [review-to-composer integration](https://github.com/getpaseo/paseo/blob/d636abd7a4ce302e7ccb9eb6074f637c6dd4d83b/packages/app/src/git/pull-request-panel/pane.tsx#L295).

Our [SourceControl](../client/web/pages/studio/SourceControl.tsx) already provides real diffs, rendered Markdown review, version history, rollback and remote synchronization. These are skill-centered. Generic sessions do not have an attached repository-wide changes/PR workspace. A skill nested in a parent repo gets folder-scoped changes while parent history remains outside our version UI. [Terminal file previews](../client/web/components/TerminalFilePreview.tsx) help inspect output but are read-only.

Studio already [embeds terminals beside skill editing](../client/web/pages/studio/AgentPanel.tsx), and generated skill proposals can reopen their mining conversation. The missing layer is a durable run-to-changes relationship and structured review feedback, especially for generic project sessions.

**Proposal:** first connect an agent run to its changed skill files. Let users select a changed passage, attach a comment, ask the agent to revise it, compare the next result, and save a version. Add project worktree isolation when parallel tasks justify it, followed by an optional PR flow for project skills. This builds on our existing review components. A worktree separates checkouts; it does not sandbox processes, credentials or network access. Automatic PR-tab opening and mobile Jump to file are 0.9 beta additions.

**4. They preserve work as retrievable tasks**

Paseo has a [history screen](https://github.com/getpaseo/paseo/blob/d636abd7a4ce302e7ccb9eb6074f637c6dd4d83b/packages/app/src/screens/sessions-screen.tsx#L73) with host selection, pagination and search, plus session import. Its [search implementation](https://github.com/getpaseo/paseo/blob/d636abd7a4ce302e7ccb9eb6074f637c6dd4d83b/packages/server/src/server/agent-history-search.ts#L9) matches workspace name, session title, branch and project with typo tolerance. It does not search every transcript. Within-chat Find is a separate beta feature.

At the original review, ordinary sessions centered on live tmux inventory and generic resume had no dialog. The September 27 follow-up adds **History** on Home and in Sessions: it searches saved Claude Code and Codex conversations on the active host and resumes the selected conversation ID in an ordinary terminal. Provider stores retain the conversations after terminal closure; VibeStudio does not create a second transcript store. An already-running matching conversation is reused. The [past-session contract](../docs/persistence.md#past-agent-sessions) owns supported providers, search, retention and unavailable states. Mining retains its separate run history and continuation in [MiningRoute](../client/web/pages/mining/MiningRoute.tsx).

Both products already prioritize attention. VibeStudio has Needs input, Working and Finished presentation, unread state, sounds and notifications in [sessionAttention](../client/web/lib/sessionAttention.ts). Saved-conversation retrieval is now implemented; the remaining opportunity is a task record that connects a conversation to skill versions, outcomes, changed files and semantic failure/review states.

**Remaining proposal:** persist a run record containing title, project/skill, skill version, provider, start/end state, final summary, changed files and resume identity. Build on saved-conversation retrieval while keeping transcript ownership with the provider. Add task grouping and archive behavior for those richer records, and connect Home's attention and recent-work views to them with skill discovery readily accessible. This recommendation assumes the current desktop/phone agent-running promise is strategic; a primarily skill-authoring product would give the editor more prominence.

**5. They support more of the away-from-desk loop**

Paseo implements native remote push: the app obtains an Expo token, registers it with the daemon, and the daemon sends title/body/data through Expo independently of the phone's live connection. See [registration](https://github.com/getpaseo/paseo/blob/d636abd7a4ce302e7ccb9eb6074f637c6dd4d83b/packages/app/src/push-notifications/internal/subscriptions.ts#L23) and [dispatch](https://github.com/getpaseo/paseo/blob/d636abd7a4ce302e7ccb9eb6074f637c6dd4d83b/packages/server/src/server/push/push-service.ts#L24). This is implemented support for suspended-phone notification delivery, not a measured guarantee from this research. Presence policy can suppress push; the web implementation is a no-op, and F-Droid does not enable the Expo path. Notification text goes through Expo, so relay encryption claims do not cover that content.

VibeStudio already has browser/PWA Web Push and native local notifications. The precise missing piece is [native APNs delivery for a fully suspended or terminated app](mobile-ux.md). If native iOS is central to the product, that is a meaningful gap.

Paseo also implements voice and a rich attachment composer. Its [context types](https://github.com/getpaseo/paseo/blob/d636abd7a4ce302e7ccb9eb6074f637c6dd4d83b/packages/app/src/attachments/types.ts#L96) cover file selections, images, issues/PRs and review bundles; its [voice runtime](https://github.com/getpaseo/paseo/blob/d636abd7a4ce302e7ccb9eb6074f637c6dd4d83b/packages/app/src/voice/voice-runtime.ts#L712) starts actual capture and orchestration. “Local” speech runs on the daemon host, which may be remote from the phone. Default local STT is English-only unless the alternate multilingual model is selected. See [voice documentation](https://paseo.sh/docs/voice).

Our terminal already supports image paste through upload-and-path insertion. The useful addition is typed context and mobile text composition, rather than merely an image button. Prioritize context attachment and native attention delivery, then dictation; a full conversational voice system can follow observed demand.

**6. Other agents and programs can operate their product**

Paseo provides an end-user CLI, a [supported TypeScript SDK](https://paseo.sh/docs/sdk), and agent tools. A program can create an agent, wait for the result, send a follow-up and exit while the daemon continues owning the session. Its [MCP adapter](https://github.com/getpaseo/paseo/blob/d636abd7a4ce302e7ccb9eb6074f637c6dd4d83b/packages/server/src/server/agent/mcp-server.ts#L31) uses the shared tool catalog. This makes automation visible in the same app as manually initiated work.

[Schedules](https://paseo.sh/docs/schedules) create fresh runs on a cadence; heartbeats revisit an existing conversation. Their [schedule service](https://github.com/getpaseo/paseo/blob/d636abd7a4ce302e7ccb9eb6074f637c6dd4d83b/packages/server/src/server/schedule/service.ts#L279) contains persistence/recovery and execution logic. The host still needs to be running and awake.

VibeStudio's HTTP API is an excellent prerequisite, but its frontend bridge and server lifecycle flags do not constitute an equivalent documented orchestration interface. Connector discovery inventories integrations used by other agents; that is distinct from letting those agents operate VibeStudio.

**Proposal:** expose a small CLI and MCP adapter that call the existing HTTP API. Start with skill discovery/read/validation, run launch/status/result and version/diff operations. Then add “run this skill on a schedule” with explicit host, agent settings and inspectable results. That would strengthen the purpose of reusable skills.

Paseo's plugin system and Hub broaden its reach further. Plugins can add providers, UI and server functionality; Hub dispatches external events. These are later opportunities for us. Plugins are trusted executable code and their interfaces are evolving; npm plugin installation is specifically a 0.9 beta feature. The actual Hub event-consumer service is separate from the audited monorepo. Its operational quality was not established here. See [plugins](https://paseo.sh/docs/plugins) and [Hub](https://paseo.sh/docs/hub).

**7. They distinguish a surviving process from recoverable application state**

Our detached service, persistent tmux sessions, attachment identities and reconnect handling are substantial strengths. The [SSE event design](../server/skill-server/src/events.rs) deliberately treats events as hints and refetches authoritative snapshots. An event-replay buffer is not automatically required for that contract.

Paseo adds durable readable application state. It caches transcript/directory replicas in [IndexedDB on web](https://github.com/getpaseo/paseo/blob/d636abd7a4ce302e7ccb9eb6074f637c6dd4d83b/packages/app/src/runtime/replica-cache/row-store.web.ts#L67) and [SQLite on native](https://github.com/getpaseo/paseo/blob/d636abd7a4ce302e7ccb9eb6074f637c6dd4d83b/packages/app/src/runtime/replica-cache/row-store.native.ts#L41). Its [timeline contract](https://github.com/getpaseo/paseo/blob/d636abd7a4ce302e7ccb9eb6074f637c6dd4d83b/docs/timeline-sync.md#L77) separates cached readability, connection status and history freshness. Provider history supplies durable transcript authority; this is not a claim that every transient event is permanently logged.

An especially transferable implementation is [durable request receipts](https://github.com/getpaseo/paseo/blob/d636abd7a4ce302e7ccb9eb6074f637c6dd4d83b/packages/server/src/server/message-receipts/index.ts#L25). A repeated completed request returns its existing success; conflicting reuse of an ID fails; a send interrupted at an ambiguous point returns an unknown outcome instead of blindly repeating it. Our [HTTP helper](../client/web/lib/api.ts) correctly avoids automatically retrying session/git mutations because duplication is possible.

**Proposal:** give session creation and future scheduled runs persistent operation IDs and result receipts. Add host-scoped cached summaries with explicit freshness. Expand transcript caching only with structured sessions. None of this requires replacing SSE, tmux or Rust.

Their shared [protocol package and compatibility policy](https://github.com/getpaseo/paseo/blob/d636abd7a4ce302e7ccb9eb6074f637c6dd4d83b/docs/protocol-compatibility.md) also provide a useful example: additive wire contracts and centralized feature detection. Our documented generated-Rust-to-TypeScript DTO work would address wire drift within the existing architecture. Different upgrade policies have tradeoffs; theirs is not proof that our minimum-server-version policy is wrong.

**8. Their provider abstraction reaches the user**

Paseo's [model selector](https://github.com/getpaseo/paseo/blob/d636abd7a4ce302e7ccb9eb6074f637c6dd4d83b/packages/app/src/components/combined-model-selector.tsx#L26) supports provider/model selection and reusable profiles. Its [provider registry](https://github.com/getpaseo/paseo/blob/d636abd7a4ce302e7ccb9eb6074f637c6dd4d83b/packages/server/src/server/agent/provider-registry.ts#L37) combines dedicated integrations and generic ACP support. Broad compatibility is useful, but a catalog entry does not establish equal support for every capability.

Our registry already supports several agent families. The ordinary [NewSessionDialog](../client/web/components/NewSessionDialog.tsx) puts model flags in an extra-arguments field; the mining workflow has richer controls. The gap is therefore consistency and discoverability, not an absence of multiple agents.

**Proposal:** saved run profiles should expose provider, model, permission behavior and supported resume/context capabilities. Keep advanced arguments available. Treat structured controls, terminal-only operation and resume support as explicit capabilities; adding a long logo list would do less for usability.

**9. Their public presentation supplies much more evidence**

The [Paseo homepage](https://paseo.sh/) shows desktop work, phone review and the path from building to shipping. It also offers platform-specific downloads, provider compatibility, automation examples, documentation, changelog and community entry points. Browser inspection confirmed the rendered experience, while [the landing-page source](https://github.com/getpaseo/paseo/blob/d636abd7a4ce302e7ccb9eb6074f637c6dd4d83b/packages/website/src/components/landing-page.tsx#L89) shows that these demonstrations are authored mockups.

Our current [landing-page source](../docs/index.html) has a clear promise and desktop/iOS links, but only a short explanation and three benefits. It does not show the workflow or explain the skill-improvement cycle visually. This comparison concerns the checked-out page; production deployment of our pending page changes was not verified.

**Proposal:** demonstrate two real journeys: start a task at the desk and review it on the phone; then turn a successful run into a reviewed, versioned reusable skill. Add a short setup walkthrough, a capability table and a user-facing changelog. Publish screenshots or recordings of actual behavior and state the desktop/phone prerequisites. This is a relatively small investment compared with structured provider integration.

Paseo has native Android distribution as well as iOS, desktop and web. That broadens its audience. Expanding our platform commitments should follow demand and support capacity. Neither star counts nor selected testimonials establish retention, user satisfaction or active usage; this review makes no such claims.

**10. Their most useful engineering lesson is measurement**

Paseo has terminal output/keystroke benchmarks that distinguish daemon transport from browser render latency under load. Its [stress harness](https://github.com/getpaseo/paseo/blob/d636abd7a4ce302e7ccb9eb6074f637c6dd4d83b/packages/app/e2e/browser/terminal-keystroke-stress.spec.ts#L41) exercises concurrent agent updates and large diff payloads. Its [native keyboard harness](https://github.com/getpaseo/paseo/blob/d636abd7a4ce302e7ccb9eb6074f637c6dd4d83b/packages/app/e2e/mobile/terminal-keyboard/android.mjs#L68) checks actual IME behavior. These are specific practices worth borrowing; the performance suites are opt-in, not proven continuously enforced CI gates.

Our current [browser E2E fixtures](../e2e/fixtures.ts) isolate real backend/tmux processes, and [CI](../.github/workflows/ci.yml) includes browser coverage, backend checks and native iOS checks. The [clipboard suite](../e2e/terminal-clipboard.spec.ts) explicitly distinguishes synthetic events from OS selection menus. The native harness currently covers app lock/startup rather than a connected SSH session.

**Proposal:** add measured keypress-to-render and reconnect-to-usable baselines, plus native connected-session journeys for selection, copy/paste, keyboard persistence and background/resume. Preserve videos/traces and before/after results for difficult regressions. Follow our [isolated-backend procedure](../docs/development.md#isolated-backend). There is no evidence here that either project is universally faster, less buggy or more secure.

**Advantages VibeStudio should preserve**

| Capability | Why it matters |
|---|---|
| Skill discovery and authoring | Agent-specific discovery, frontmatter validation and skill assets give us a focused workflow beyond conversation management |
| Independent skill versions and rendered Markdown review | Users can review behavior changes and retain a reusable artifact |
| Mining with proposal acceptance/discard | Past work can improve future agent behavior; a useful basis for differentiation |
| Remote service provisioning | Users can connect to a suitable SSH host without separately deploying our backend |
| Detached host plus tmux lifecycle | Agents survive client closure; viewers have independent attachments and sensible geometry ownership |
| Conflict-aware editing | Etags and reconciliation protect against overwriting agent edits |
| Connector inventory and shared secret handling | Setup visibility spans the agent tools actually installed on the host |
| One HTTP/JSON + SSE boundary | New clients and automation can reuse the same backend behavior |

These are visible in [the architecture](../design.md), [SkillDocument](../client/web/pages/studio/SkillDocument.tsx), [MiningRoute](../client/web/pages/mining/MiningRoute.tsx), [terminal attachments](../server/skill-term/src/attachments.rs), and [connector documentation](../docs/connectors.md). Paseo also supports agent skills and configuration; our distinction is depth of skill creation, review, versioning and learning from prior runs, not exclusive support for the skill format.

**Proposed sequence and success criteria**

Effort labels below are relative engineering size, not delivery estimates. No implementation is authorized by this research document.

| Sequence | Deliverable | Relative effort | Evidence that it helped |
|---|---|---|---|
| 1 | Public workflow demo; clearer setup; consistent run profiles | Small to medium | New users can explain the value and launch a configured session without discovering CLI flags |
| 2 | Richer task records, grouping/archive and durable operation receipts, building on saved-session search/resume | Medium | A reopened task retains its outcome and changed-file context; acknowledged operations cannot duplicate across retries/restarts |
| 3 | One structured provider mode with mobile composer and approvals | Large | Phone users can read a result and answer a question without terminal key navigation |
| 4 | Run-linked changed files and review comments; native remote push | Medium to large | A notification leads to the right run, useful review and a verified revision |
| 5 | Easier device enrollment; optional worktrees for parallel project tasks | Medium to large | First phone connection requires fewer external steps; parallel tasks do not edit the same checkout accidentally |
| 6 | Small CLI/MCP surface and scheduled skill runs | Medium to large | Agents/scripts can run and inspect the same work users see in the app |

Native push and onboarding can proceed independently of the structured-view work. Full voice conversation, a broad plugin system, hosted team orchestration and extensive IDE features should follow demonstrated demand.

Measure first-phone-session completion, setup abandonment by step, time to understand and answer an approval, time from completion to reviewed version, run-resume success after network loss, and repeat use of accepted skill versions. These measurements can use local instrumentation and consenting usability participants; adding telemetry is a separate product decision.

The product opportunity is to make **run → review → improve skill → reuse** a coherent cycle. Paseo provides strong examples for execution and supervision. VibeStudio's existing skill workflow gives us a specific reason to build the rest.
