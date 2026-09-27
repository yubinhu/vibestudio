# Session configuration

The desktop exposes JSON at `/api/comparison/`: `POST start`, `POST update`,
`POST stop`, `POST open`, `POST close`, `GET status?id=…`, `GET list`, and
`GET capabilities`, `GET devices`, and `POST devices/refresh`. Start, open and stop return HTTP 202; poll status until ready, stopped or failed. Server state `ready`
means both preview URLs respond; `windowOpen: true` additionally confirms native
presentation. The command helper waits for both with `--wait`.

```json
{
  "artifact": {
    "title": "Account layout",
    "description": "Review navigation spacing and narrow layouts",
    "owner": {
      "hostId": "local",
      "terminalId": "ass-exact-terminal-id",
      "provider": "codex",
      "conversationId": "exact-native-conversation-id"
    }
  },
  "repository": "/home/me/project",
  "workingDirectory": "/tmp/agent-checkout",
  "baselineRef": "main",
  "baselineWorktree": "/tmp/my-review-baseline",
  "baseline": {
    "directory": "frontend",
    "command": "npm run dev -- --host 127.0.0.1 --port {port} --strictPort"
  },
  "working": {
    "directory": "frontend",
    "command": "npm run dev -- --host 127.0.0.1 --port {port} --strictPort"
  },
  "route": "/",
  "viewport": { "width": 390, "height": 844, "preset": "custom", "orientation": "portrait" },
  "syncScroll": true
}
```

- `artifact` identifies the review in the session UI. `owner.hostId` is `local`
  or the exact SSH/WSL workspace identifier. Supply `terminalId`, or both
  `provider` and `conversationId`; include all known fields so a resumed native
  conversation retains its artifacts. `title` is required; `description` explains
  what to review. The helper supplies these fields when omitted; direct HTTP
  callers should supply them. Legacy unassigned diffs can be attached explicitly
  in the session UI. `update.artifact` replaces the complete metadata object.
- `repository` is the Git repository used to resolve the baseline. The working
  directory defaults to this repository, but can be anywhere else.
- `baselineRef` defaults to the repository's mainline. It resolves once to a
  commit SHA and checks out detached; later branch changes do not move it.
- `baselineWorktree` is optional. Choose a new, nonexistent path. Otherwise a
  temporary path is allocated. The session removes only its own checkout.
- `baseline` and `working` each accept `command`, `directory` (relative to that
  pane's root), `port`, `env`, `readyTimeoutSeconds`, or `url` for an existing
  server. Ports default to distinct unused loopback ports. Commands run through
  the host shell, with `{port}` expanded and `PORT` supplied. Explicit ports
  must be unused; the session never displaces listeners.
- The default command supports detected Vite frontend scripts. For other
  frameworks, supply commands with their correct host/port flags. Install
  baseline dependencies in its command when required; never assume a new
  worktree already has generated files or ignored configuration. With an inferred
  command, the baseline may borrow dependencies while keeping its Vite cache
  separate; explicit commands own their dependency setup.
- Root `env` is shared by both commands; each pane's `env` overrides it. Use
  explicit backend environment settings for native frontend adapters. URLs
  must be reachable from the desktop. Existing URLs are never stopped.
- `route` is a same-origin path, optionally including query/hash, applied to both.
  The default `/` preserves the path and hash of each explicitly supplied URL.
- Viewport `width` and `height` are authoritative CSS pixels; `custom` is a
  responsive viewport without a device label. For a named device, read
  `GET /api/comparison/devices` and use its device ID and dimensions. VibeStudio
  serves a cached Chrome DevTools catalog; reading it does not start a network
  request or scheduled task. Explicitly request `POST /api/comparison/devices/refresh`
  to fetch the latest list, then read the catalog until `refreshing` is false.
  The last good catalog remains available when offline. The
  [catalog source and cache](../../../server/skill-core/src/comparison_devices.rs)
  and [viewport selection rules](../../../client/web/lib/comparisonViewports.ts)
  are the canonical implementation references.
- Supply both dimensions along with `preset`; setting a device ID alone does not
  change the size. The UI rotation control swaps width and height while retaining
  the device ID. When updating through the API, swap the dimensions yourself and
  set the matching `orientation`.
- Named presets provide responsive browser viewports. They do not emulate device
  pixel ratio (DPR), user agent, touch input, native browser chrome or hardware.
  Laptop and desktop presets are generic display sizes, not exact computer models.

An update file contains only the fields being changed:

```json
{
  "route": "/#/settings",
  "viewport": { "width": 844, "height": 390, "preset": "custom", "orientation": "landscape" },
  "syncScroll": true
}
```

Resource configuration is immutable while a preview runs. Create a new artifact
for different source directories or a new baseline. Raw HTTP update/open/close/stop
bodies include `id`; the helper supplies it. `open` optionally accepts a full
`config` to restore runtime settings of a stopped artifact, preserving its owner,
source directories and pinned baseline. `close` preserves the checkout and servers;
`stop` removes owned resources while keeping the artifact record.

Status includes metadata, creation/update timestamps, pinned SHA, resolved URLs,
lifecycle state, errors and log paths. `windowRequested` records user intent;
`windowOpen` acknowledges native presentation. `presentationRevision` lets opening
an existing viewer focus it without creating another one. Stopping or failure
removes owned logs; an in-memory failure retains a short output tail for diagnosis.

Artifacts survive desktop restarts without automatically restarting any commands.
Environment maps and volatile logs/errors are not saved; `restoreRequired` means
runtime environment must be supplied again. Commands and URLs are saved in the
private artifact store, so put credentials in environment settings.
