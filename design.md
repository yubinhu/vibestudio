# VibeStudio — Architecture & Design Principles

> Read before adding a feature. **One rule: every capability is reached over HTTP. There is no second transport.**

## One transport: HTTP only

Two parts that can run on **different machines** (the VS Code-remote model):

- **Backend (Rust, `server/`).** All real work — fs, git, skill discovery, secrets,
  terminals, on-device LLM — lives in `server/skill-core` (transport-agnostic, **no Tauri
  deps**); `skill-term` handles tmux terminals; `skill-server` exposes everything over
  `/api/*` (+ **SSE** for streaming) and serves the built UI.
- **Frontend (React/TS, `client/web/`).** Reaches the backend only through
  `client/web/lib/api.ts` — `http()` + `EventSource`, **never Tauri `invoke`, no `isTauri`
  branch**. `client/desktop/` is the thin Tauri shell.

**Browser** loads the SPA from skill-server and calls `/api/*` same-origin. **Desktop** brings
up one local loopback `skill-server` and points the webview at `http://127.0.0.1:<port>`; for
the remote case that local server is a **switchboard** reverse-proxying `/api/*` to an
on-demand remote server (the tunnel's local end is also loopback). So **"local" is just
"remote where the host is localhost"** — identical code both ways, `/api/*` always same-origin
(the desktop CSP `default-src 'self'` covers it). Any *outbound* network beyond the server must
originate in **Rust**, never the webview. (We dropped the `invoke` fast path because dual
transports silently diverge — a feature wired only through `invoke` broke browser/remote.)

## Adding a feature

1. **Logic** → a fn in `skill-core` (Tauri-free).
2. **HTTP route** → a match arm in `skill-server`'s `handle()` at `/api/<name>`. **This is the
   API** — if you can't reach it from skill-server, it isn't done.
3. **Frontend** → one fn in `client/web/lib/api.ts` calling `http(...)`.

- **Streaming** = SSE (`request.into_writer()` + chunked `data:`, see `stream_terminal`),
  consumed via `EventSource`. Rides a plain socket and an SSH tunnel alike — no duplex channel.
- **No native-only capabilities.** A native OS dialog only sees the *client* machine, breaking
  the remote model. Browse the (possibly remote) fs via `/api/fs/list-dir` + the in-app
  `FolderPicker`; import via `/api/import/zip`; export via `/api/download/skill`.

*Skill packaging (`.skill`):* export emits a **`.skill`** — a deflate zip with one top-level
`name/` folder, the shareable install unit (import accepts `.skill` and `.zip` alike; a `.skill`
*is* a zip). Opinionated exclusions in `skill::build_zip`: **`.git`** (so history-resident `.env`
can't leak — same threat the `remotesync` `env_in_history` guard blocks on the publish channel),
`.venv`, build junk, and any on-disk `.env` (the opt-in "bundle secrets" path writes an
authoritative one). **The rule for `IGNORED_DIRS`: leave out only per-machine build/runtime
state and secrets — never authored content.** So `node_modules`/`.next`/`__pycache__` go, but
`evals/`, `references/`, `assets/`, `scripts/` all ship — they're part of the skill. Packaging is **gated on `skill::validate_skill_md`** — a parseable, allow-listed
frontmatter head (kebab-case `name` ≤64, angle-bracket-free `description` ≤1024) — so an emitted
`.skill` always installs cleanly; the gate's reason surfaces in the UI (`exportSkill` fetches the
bytes as a blob precisely so a rejection isn't saved as the "file").

*Reference (keyless commit messages):* `commitmsg.rs` (diff prep, cache) → `commit_agent.rs`
shells out to a logged-in coding-agent CLI (Claude Code → Codex → Gemini, keyless via
subscription OAuth; opencode last, BYO-key); `engine.rs` (llama.cpp) is opt-in offline
(`VIBESTUDIO_COMMIT_AGENT=llama`). Routes `POST /api/commit-message/generate`,
`GET /api/commit-message/model-status` → `api.generateCommitMessage()` / `api.commitModelStatus()`.

## UI layout (frontend IA)

Hash router (`createHashRouter`, Tauri webview) with one persistent shell + lazy pages.
**The shell never unmounts; each page mounts its own `NavBar`** (the shell does not).

- **Shell** (`app/AppShell.tsx`) globally mounts only: the `<Outlet>` (hidden via
  `display:none` on `/sessions`, not unmounted), an always-mounted `SessionsHost` (live ptys
  survive nav), and `UpdateBanner`. No `StrictMode` (would double-attach pty/xterm). Only guard:
  `useDiscardBlocker`, fires *only* after an autosave failure — no auth gate.
- **Routes:** `/` Home · `/connectors` · `/mining` · `/sessions` (element `null`; UI is
  `SessionsHost`) · `/skills/:root` (children: index = SKILL.md form, `file/*` = file pane,
  `commit/:sha` = worktree diff only) · `/markdown/:path` (standalone editor) · `*` → `/`.
- **Studio** = full-height column: `TopBar` (**no Save button — autosave is wordless**; a
  *version* is a git commit) → `PreviewBanner` (past-version only) → **Sidebar | center Outlet |
  optional `AgentPanel`** (resizable Terminals). Sidebar = one `SplitStack`: `FileTree` +
  `SourceControl` accordion (**New Changes** = working-tree + Save-version, also ⌘/Ctrl+S ·
  **Versions** = history, click checks a version into the worktree · **Remote/GitHub**,
  collapsed). Center = `SkillDocument`/`FilePane`/diff. Layout prefs are **global**
  (`studioLayout.ts`), not per-skill. (The "Versions panel" below = `SourceControl.tsx`.)
- **Design system** ([visual guide](design/visual-system.md), `globals.css`): Tailwind v4,
  CSS variables + `@theme inline`, class-based dark (`.dark`, set pre-paint).
  **Lime + ink identity:** `--action` / `--action-fg` pair for filled primary controls;
  `--accent` for readable links, selected states and focus rings; semantic colors
  for status. The full-color mark comes from `design/vibestudio-logo.svg` through
  `VibeStudioMark`. Primitives: one `Modal`, `btn{Primary,Ghost,Danger}` (one filled
  primary per row), `Badge` via `color-mix`, `useConfirm` (`window.confirm` is a no-op
  in the `wry` webview). The app name is "VibeStudio" (one word).

## Workspace defaults and history

Paths and launch settings belong to the **active server**, reached over ordinary
proxied `/api/preferences*` and `/api/recents/*` routes. `preferences.json` remembers
picker directories per workflow and session defaults per agent, including the last
agent; `recents.json` keeps the last 30 successful skill/standalone Markdown opens.
These JSON stores use file locks and atomic replacement across server processes.
Theme and panel layout remain client-side. Home shows the four most recent items
as compact shortcuts; the header's Open action can browse any skill or Markdown file.
See [the persistence audit](docs/persistence.md).

## Skill versioning: tracked by default

Each personal skill is its **own git repo** (versioned/diffed/rolled-back/synced
independently). **Auto-tracked:** `GET /api/skills/discover` → `discover_and_autotrack` →
`gitops::auto_track_personal`, which off-thread `git init`s + lands a baseline **"Initial
version"** commit (an unborn HEAD reads all-dirty and can't sync; with no git identity we stop
at the empty repo and prompt on first manual save).

- **Eligible = personal, not a `generated-skills/` proposal, not inside a parent repo** (never
  nest `.git` in someone's project); already-`.git` roots are skipped. `ensure_exclude` seeds a
  local `.git/info/exclude` (never a committed `.gitignore`) before `git add -A`.
- **Opt-out is sticky:** `git-untrack` deletes the skill's `.git` and denylists its path
  (`~/.config/vibestudio/untracked.json`) so discovery won't re-create it; `git-track` clears
  it + re-baselines. Untrack refuses when a parent repo owns history.
- Routes: `git-track`/`git-untrack`, `git-commit` (Save-version) + `git-log`/`git-status`/
  `git-info`; surfaced in `SourceControl.tsx`.

## External edits: conflict-safe writes

Skill files have **other writers** — coding agents in the app's own terminals, `git
pull`/checkout, formatters, vim. The editor must never **silently clobber** them, and should
**show the latest** when it safely can. The principle: *never overwrite a disk version newer
than the one you loaded, and never silently discard unsaved edits.* We adopt **VS Code's
on-disk reconciliation** — not Notion's op-sync (Notion owns its datastore and every writer
speaks its protocol; ours are dumb whole-file emitters, so a CRDT/op layer has no anchor) —
with the **per-skill git** as the eventual merge + recovery engine. Wordless autosave is an
*asset* here: each successful write refreshes the baseline, so the merge base stays fresh and
conflicts stay rare.

- **Optimistic-concurrency tag.** `read-file` returns an `etag` (sha256 prefix of the bytes);
  the editor echoes it on `write-file` as `expectedEtag`. `write_file_impl` is a
  **compare-and-swap**: if disk no longer matches the tag it returns `WriteOutcome::Stale`
  (carrying the current disk bytes) **instead of overwriting**. A `None` tag = legacy
  unconditional overwrite (callers not yet tracking a baseline, e.g. `saveSkillMd`).
- **Clean buffer → silent reload.** A `useExternalFileSync` poll (`/api/fs/stat` — mtime+size
  only, gated full re-read on change) plus a window-focus re-read detect external writes while
  the file is open; with a clean buffer the latest is swapped in (the text editors in place via
  `useAutosave().markClean` so the cursor survives and it isn't written back; the SKILL.md form
  via `reload(true)`). Focus alone was insufficient — the common case is an agent in Studio's
  own terminal, which never blurs the window. **No fs-watcher** — a poll rides the HTTP-only
  transport identically local or over the SSH tunnel; the CAS, not the poll, is the no-clobber
  guarantee.
- **Dirty buffer → conflict.** An external change while the buffer has unsaved edits (caught by
  the poll, or by a stale autosave's CAS) surfaces a **non-blocking inline banner** (Use disk /
  Keep mine), never a modal (autosave is wordless and fires constantly, so it can't pop a dialog
  per write) and never `window.confirm`. The user's edits are kept either way.
- **Implemented:** CAS + etag end-to-end; the `useExternalFileSync` stat-poll; reconcile in
  `FilePane`, `SkillDocument` (the SKILL.md form — CAS + clean reload + banner), and
  `MarkdownRoute`. **Deferred (slice 2):** `git merge-file` 3-way auto-merge of *disjoint* edits
  (so most external changes reconcile invisibly — banner only on a true overlap) and a `git
  stash create` snapshot before any force-overwrite (so "Keep mine" is always recoverable).

## Dev workflows

| Goal | Backend | Frontend | Open |
|------|---------|----------|------|
| Browser, local backend | `cargo run -p skill-server` (`:8765`) | `npm run dev:vite` (`:1420`) | **`localhost:1420`** — Vite proxies `/api` → 8765 |
| Browser/desktop, **remote** backend | skill-server on the remote host | `VITE_API_TARGET=http://<remote>:8765 npm run dev:vite` | `localhost:1420` |
| Native desktop | the shell spawns a loopback `skill-server` | `npm run tauri dev` | the native window |
| Production / remote | `npm run build` then run skill-server | (served by skill-server) | skill-server's port (UI + API, one origin) |
| **Browser-only / phone** (tailnet) | skill-server on `127.0.0.1:8765` + `tailscale serve --bg 8765` | (served by skill-server) | `https://<machine>.<tailnet>.ts.net` |

The Vite `/api` proxy (`vite.config.ts`, target via `VITE_API_TARGET`) defaults to `:8765`;
`tauri dev` spawns its own loopback server there. Desktop runs the server in-process via
`skill_server::spawn(ServerConfig)` (`client/desktop/src/lib.rs`); `ServerConfig` carries the
bearer `token` + `examples_base`.

## Tray-governed lifecycle + "Open on your phone"

The desktop app is **tray-resident** (`client/desktop/src/lib.rs`): closing the window hides it
— the in-process server, terminals, and phone access stay up — and the tray's **Quit** is the
one explicit full teardown: it kills every studio terminal on the machine
(`skill_term::list_sessions` → `kill_session`), the live SSH session, and the engine, then
exits. Icon present = reachable; icon gone = nothing of ours running. Every OTHER exit (update
restart, plain Cmd+Q, crash) still leaves tmux agents running for the next launch to pick up —
update restarts must never kill a working agent. There is **no separate daemon process**.

**"Open on your phone" (`/api/phone/*`, `server/skill-server/src/phone.rs`) — the HUB is the
server.** Remote dialog → *Open on your phone* → QR (the tray's item deep-links the same modal
via `#/?phone=1`). `PhoneControl.enable()` fronts **the answering server** with the
`tailscale serve` of **the machine it runs on** and returns that machine's `https://<magicdns>`
URL + QR SVG. The phone routes are **ordinary routes — proxied like everything else**: switched
onto a remote, the *remote's* PhoneControl answers, so the QR points at the remote itself and
the phone reaches the stable machine directly — the client is an accessor, never a relay; it
can sleep/shut down without costing the phone anything. (The desktop client itself always
accesses remotes over SSH — reliability — never the tailnet.) Every loopback server carries a
PhoneControl, provisioned remotes included; remote launches are **tokenless** (a browser can't
send a bearer; loopback + tailnet is the trust boundary, same as local — the token is still
recorded/injected for reattach compat with older servers). Guided failures describe the hub's
machine: `operator` (one-time `tailscale set --operator`), `consent` (tailnet HTTPS approval
link), `tailscale` missing/stopped. The desktop binds `PHONE_PORT` (8765) by preference — the
serve mapping persists in tailscaled, so a stable port lets it find the app on the next launch
— with an ephemeral fallback when taken; `enable()` re-runs `serve` against the current port,
so a version bump's new remote port can't leave a stale mapping (`status()` reports
not-serving rather than a dead QR). `embed-ui` builds compile `dist/` into the binary
(`include_dir`; `build.rs` re-runs on dist changes — without it a rebuild silently ships a
stale SPA) so the standalone/headless binary serves the UI with no dist on disk. skill-term
sets tmux `exit-empty off` (server-scoped) at session creation: backends come and go (dev +
app share the tmux server), and the server must survive zero-session gaps.

**Browser-only constraints.** The SPA and API are root-absolute (Vite base `/`, `API_BASE=""`),
so the server must sit at the origin root — no sub-path mounts. Tokens don't work from a plain
browser (the SPA never sends `Authorization`, and the attach SSE is an `EventSource`, which
can't): browser mode = `token: None`, with reachability (loopback bind + tailnet) as the auth
boundary. CORS is loopback-only and POSTs from foreign origins are refused at the choke point
(`origin_allowed`), so a random website in a tailnet browser can't drive the API; anyone who can
*open* the URL, though, has full control — including the Remote-SSH switchboard, which stays
live on a loopback standalone server. Auto-resume of the last SSH remote is loopback-origin-only
(`remote.ts maybeResume`), so a phone hitting the shared URL never silently flips the server's
backing data. Multi-viewer polish (a second browser observing a remote connect/disconnect) is
deliberately not handled yet.

**Roadmap — account-backed access (not built).** The Tailscale prerequisite is three services in
a trench coat — trust (reachability = auth), browser-valid certs, and NAT traversal — and a
VibeStudio account could take over all three: cookie login (the keystone; it also unlocks LAN
mode and phone→remote direct), Let's Encrypt via DNS-01 on a domain we control
(`<user>.tunnel.…` — the `ts.net` trick: our DNS publishes the ACME TXT record, the private key
never leaves the user's machine), and an outbound relay the app dials so NAT'd machines are
reachable with zero router config (SNI passthrough only — the relay stays a blind pipe).
Tailscale then demotes from prerequisite to the self-host/BYO-network path. Pairs with the
account-backed secrets direction; a natural first paid tier (relay bandwidth is a real cost).
Do **not** rebuild the mesh itself (WireGuard / hole punching / DERP) — relay-only is the 95%
shortcut; browser-P2P (WebRTC) only if relay bandwidth ever forces it.

## Terminals: persistent by design

Agent terminals are tmux sessions (`ass-*`); the backend is only a **bridge** (`tmux attach`
in a PTY).

1. **A terminal outlives everything but an explicit kill** — closing a tab, closing the app
   window, dropping SSH, or restarting/upgrading a backend never stops the agent inside. The
   tray's **Quit** is an explicit kill: it ends every studio terminal (see the tray-governed
   lifecycle section) — that's the desktop's "off switch", not an incidental exit.
2. **The `ass-*` namespace is machine-wide, unfiltered:** every backend lists/attaches/kills all
   studio sessions, so any client picks up any agent. The pid in `ass-<pid>-<secs>-<seq>` only
   prevents name collisions; `@ass_owner_pid` is provenance, not a lifecycle key.
3. **Only auto-reaping = a high-bar GC** (`sweep_stale`, at startup): collected only when
   unattached **and** every pane is back at a plain shell **and** idle ≥1 week.

Multiple backends per machine are supported (shared namespace); the inference-engine reaper
kills only *orphaned* engines (reparented to init) — never a sibling's live child **on Unix**
(the Windows fallback kills by image name and can hit a sibling, accepted for that rare case).

## Agent registry (`skill-core/src/agents.rs`)

Agent-agnostic: nothing outside the registry matches a family name. One `AgentDef` per CLI:

- **skills_dirs / reads_shared** — where it discovers skills (own folders + the shared
  `~/.agents/skills`).
- **launch** — the *interactive TUI* with the prompt pre-submitted (claude/codex/cursor:
  positional; gemini: `-i`; opencode: `--prompt`). An app-driven run is an ordinary session
  (same approvals/lifetime; the previewed prompt is the *whole* prompt); the caller brings the
  user to its terminal. (Headless modes dropped — claude `-p` ends the run at turn end.)
- **resume** — reopen the run dir's latest conversation (claude/opencode: `--continue`; codex:
  `resume --last`; gemini: `--resume` — all cwd-scoped, so each run gets a stable dir).

Features consume capabilities, not names (mining = `launch` + navigate; "continue" = `resume`;
`canMine` = `can_launch`); the UI degrades when a capability is `None` (no TUI launch → not
offered for mining; no cwd-scoped resume, e.g. Cursor → can't revive). **New agent = one
entry**; leave a capability `None` if undocumented.

## Connection manager (VS Code "Remote - SSH")

A **local proxy switchboard**; the webview never changes origin.

- `/api/remote/{list,connect,disconnect,status,last}` is **always local** (`SshRemoteControl`,
  `server/skill-server/src/sshmgr/`); shells out to `ssh`, or `wsl.exe` for `wsl:<distro>`
  targets. A `Transport` enum abstracts the two (a WSL distro is just Linux).
- **Mobile is remote-ONLY (iPhone, feature `russh-transport`).** A phone holds no
  skills/agents/engine, so there is **no local workspace on mobile** — the in-process loopback
  server exists purely as a switchboard that proxies to a remote. iOS can't spawn `ssh`, so the
  phone speaks SSH in-process (`sshmgr/russh_tx.rs`, russh on `ring`) behind the same seam
  (`conn.rs`'s `Remote` trait — one connect orchestration drives both; the desktop keeps the
  shell-out because it inherits `~/.ssh/config`/agent/ProxyJump). Credentials are saved
  profiles: host/port/user as JSON, the private key in the **iOS Keychain** — the `SecureStore`
  client-capability trait (impl `client/desktop/src/securestore.rs`, same pattern as
  `NotifyControl`), managed over the pinned-local `/api/remote/profiles*` routes and an
  on-device `/api/ssh/keygen` (also pinned local — a key is born where its keystore lives, and
  no route ever returns it). Host keys are TOFU-pinned to `~/.config/vibestudio/russh_known_hosts`
  (no `~/.ssh` on iOS; fails closed). `WindowEvent::Resumed` → `resume_check()` reconnects after
  the app foregrounds (on iOS tao/wry deliver applicationWillEnterForeground as a *window*
  event; `RunEvent::Resumed` is never emitted — it needs `ControlFlow::Poll`) — needed because
  iOS kills the backgrounded tunnel (it ACTIVELY probes the forwarded port's `/api/health`
  rather than trusting the keepalive-lagged liveness flag, which reads stale right after a
  resume). The desktop/mobile build split lives in `client/desktop/Cargo.toml`'s target tables
  (iOS-only; not features — `tauri ios build` can't disable default features) and a
  `cfg(target_os = "ios")` `setup_mobile`; `scripts/ios-sim.sh` builds + runs it in the
  Simulator, `gen/apple` holds the Xcode project (ATS loopback exception in its Info.plist).
  **Connect-first UI:** since a disconnected phone has nothing to show, the SPA gates on a
  `mobile` flag (the remote store probes whether `/api/remote/profiles` answers) and renders a
  dedicated full-screen **connect screen** (`pages/MobileConnect.tsx`, Termius-style — big
  saved-connection cards + an on-device key-gen add flow, shared with the top-chrome
  `RemoteMenu` via `components/connections.tsx`) instead of the workspace whenever
  `mobile && status !== connected`. Connecting reveals the normal remote-backed workspace;
  disconnecting returns to the connect screen. Desktop is untouched (`mobile` is false). Still
  to prove: on-device/TestFlight validation (background→resume, idle SSE over a real network).
- While connected, **every other `/api/*` (incl. the `/api/terminal/attach` and `/api/events`
  SSE streams and `/api/phone/*` — the phone hub is the remote) is reverse-proxied** to the
  remote (`proxy.rs`) with the recorded token injected upstream (current servers launch
  tokenless — see the phone section — but the header keeps reattach compatible with older,
  token-enforcing servers).
  Pinned local: `/api/update/*`, `/api/logs/client`, and `/api/notify*` (a toast/dock badge
  belongs to the machine whose screen you're looking at — and only to its own webview: a
  tailscale-served phone request gets the 404 and uses the Web Notification API instead).
  `/api/push/*` (Web Push: key, subscribe, attention) is deliberately NOT pinned — with a
  hub connected it proxies, so subscriptions live next to the bell watcher that fires them.
  Non-`/api` GETs serve the local UI.
- **Connect flow:** list targets (`~/.ssh/config` + WSL distros) → detect arch (`uname`) →
  ensure a version-pinned static-musl `skill-server` (checksum-verified) → launch loopback-bound
  (tokenless; the token is generated + recorded for legacy reattach) → one transport child is
  both tunnel and lifeline. ssh uses `ssh -L`; WSL shares Windows loopback (no `-L`). On
  "connected" the SPA reloads; tmux terminals survive reconnects.
- **Resume/recents:** the last host is remembered on the connecting machine (`/api/remote/last`,
  `sshmgr/lastconn.rs`) and auto-reconnected; `disconnect(forget=true)` clears it. Recents
  (`/api/recents/list`) are a *normal proxied* route, so they follow the active server.
- **Same code everywhere;** two gates keep it from brokering where it shouldn't: a provisioned
  remote (`--lifeline-stdin`) and a non-loopback bind both leave `ServerConfig::remote = None`.
  Provisioning pulls `skill-server-<target>` from the GitHub release matching the app version
  (override via `VIBESTUDIO_SERVER_BASE_URL` / `_VERSION`).

## Roadmap

- **Kill Rust↔TS wire drift:** generate `api.ts` DTOs from serde structs (`ts-rs`) + a CI check.
- **Skill-usage feedback loop (mining):** the miner already extracts `skills_used` + distills
  user feedback; recurrent runs can report "skill triggered N times / never since accepted" and
  feed shortfalls back as improvements (undertriggering is the compounding risk).
