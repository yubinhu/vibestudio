# VibeStudio — agent guide

VibeStudio is a **Tauri 2 desktop app** for viewing, editing, versioning, and
running [Agent Skills](https://agentskills.io/home). The repo is laid out by the
**client / server** boundary it's built around:

- **`server/` — the backend** (the unit you build, ship, and run over SSH). Pure,
  transport-agnostic Rust: all real work (filesystem, git, skill discovery,
  secrets, terminals, the on-device LLM) lives in `server/skill-core`;
  `server/skill-term` handles tmux-backed terminals; `server/skill-server` is the
  HTTP face — it exposes them over `/api/*` (+ SSE) and serves the built UI. It
  runs as a **detached per-user host service**, locally or on a remote host;
  standalone foreground mode remains available for development.
- **`client/` — what connects to a server.** `client/web` is the React 19 + TS SPA
  (Vite; CodeMirror, react-router v7, xterm) that talks to the backend through
  `client/web/lib/api.ts`. `client/desktop` is the thin Tauri shell: it spawns a
  loopback switchboard and points its webview at that origin. Workspace requests
  proxy to the local host service or an SSH-connected remote service.

## The one rule that matters most

**Every capability is reached over HTTP/JSON (+ SSE for streaming).** `skill-server`
is the whole API — there is no `invoke` transport. That single contract is what lets
the client and server run on different machines (the VS Code-remote model). Adding a
feature = logic in `server/skill-core` → an `/api/<name>` route in
`server/skill-server` → one function in `client/web/lib/api.ts`.

**Read [design.md](design.md) before adding a feature** — it is the authoritative
architecture doc (the HTTP-only rationale, feature recipe, runtime contracts,
and on-device commit-message reference example).

## Development

Use the [development guide](docs/development.md) for commands, ports, mobile
browser development and validation. Integration checks must follow its
[isolated-backend procedure](docs/development.md#isolated-backend); leave the live
host, phone-access mapping and tmux agents running.

Heed deprecation notices and follow the existing patterns in the relevant crate/module.

## Documentation maintenance

Each detailed fact has one canonical home. Update that home and link to it
elsewhere; short summaries may repeat the main idea. Executable definitions own
exact values (commands, tokens, asset names); prose explains their meaning and
links to source. Keep plans explicit about proposals and dated research, separate
from implemented behavior. Ask before editing README files.

| Topic | Canonical home |
| --- | --- |
| Architecture, host compatibility, updates and connection lifecycle | [design.md](design.md) |
| Development modes, ports and validation | [docs/development.md](docs/development.md) |
| Release process and signing | [RELEASING.md](RELEASING.md) |
| Workspace preferences and history | [docs/persistence.md](docs/persistence.md) |
| Connector discovery and status | [docs/connectors.md](docs/connectors.md) |
| Terminal link interaction and resolution | [docs/terminal-links.md](docs/terminal-links.md) |
| Visual rules and asset workflow | [design/visual-system.md](design/visual-system.md) |
| User setup/help and data handling | [support](docs/support.html), [privacy](docs/privacy.html) |

Workflow-specific guides (such as the Home UI lab and native simulator harness)
remain beside their tools; the development and release guides link to them.
