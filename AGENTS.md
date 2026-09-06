# VibeStudio — agent guide

VibeStudio is a **Tauri 2 desktop app** for viewing, editing, versioning, and
running [Agent Skills](https://agentskills.io/home). The repo is laid out by the
**client / server** boundary it's built around:

- **`server/` — the backend** (the unit you build, ship, and run over SSH). Pure,
  transport-agnostic Rust: all real work (filesystem, git, skill discovery,
  secrets, terminals, the on-device LLM) lives in `server/skill-core`;
  `server/skill-term` handles tmux-backed terminals; `server/skill-server` is the
  HTTP face — it exposes them over `/api/*` (+ SSE) and serves the built UI. It
  runs **in-process inside the desktop** (loopback) or **standalone on a remote host**.
- **`client/` — what connects to a server.** `client/web` is the React 19 + TS SPA
  (Vite; CodeMirror, react-router v7, xterm) that talks to the backend through
  `client/web/lib/api.ts`. `client/desktop` is the thin Tauri shell: it spawns a
  loopback `skill-server` and points its webview at that origin.

## The one rule that matters most

**Every capability is reached over HTTP/JSON (+ SSE for streaming).** `skill-server`
is the whole API — there is no `invoke` transport. That single contract is what lets
the client and server run on different machines (the VS Code-remote model). Adding a
feature = logic in `server/skill-core` → an `/api/<name>` route in
`server/skill-server` → one function in `client/web/lib/api.ts`.

**Read [design.md](design.md) before adding a feature** — it is the authoritative
architecture doc (the HTTP-only rationale, the feature recipe, the dev-workflow
table, and the on-device commit-message reference example).

## Commands

- `npm run dev` — native desktop (`tauri dev`); the shell spawns its own loopback
  `skill-server`, so no separate backend is needed.
- `npm run dev:vite` — the SPA only, in a browser (`:1420`); pair with
  `cargo run -p skill-server` (`:8765`), which the Vite `/api` proxy targets.
- `npm run build` — `tsc --noEmit && vite build` (the SPA lives in `client/web`,
  built to `./dist` at the repo root).
- `npm run lint` — ESLint.
- `npm test` — release manifest and frontend workspace state tests.
- **Mobile UX in a browser (no Mac/simulator):** `cargo run -p skill-server
  --features russh-transport -- --mobile-dev` + `npm run dev:vite`, then a phone
  viewport in the browser device toolbar. Mobile mode is server-detected (the
  server answering `/api/remote/profiles`), not device-detected, so the full
  phone experience runs on Linux with hot reload. See [plans/mobile-ux.md](plans/mobile-ux.md).

Heed deprecation notices and follow the existing patterns in the relevant crate/module.
