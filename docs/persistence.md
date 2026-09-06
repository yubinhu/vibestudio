# Workspace defaults and history

## Audit

The persistence boundary is **the owner of the state**. Paths, agent launch
defaults, and opened documents belong to the active server. Theme and layout
belong to the viewing client. The desktop keeps the same web origin when switching
SSH hosts, so browser storage alone cannot safely scope filesystem defaults.

| State | Before | Result of this change |
| --- | --- | --- |
| Folder browser location | Every mount called `listDir("")`, starting at Home. Session Browse ignored the working-directory field. | `preferences.json` remembers confirmed selections independently for Open, Import, and Session. An explicit form directory takes precedence. Missing/unreadable directories fall back to the remembered directory, then Home. Cancelling does not save a location. |
| Session launch defaults | Origin-wide `localStorage`, per-agent cwd/flags; the selected agent always reset to the first detected agent. Local paths could follow the client onto an SSH server. | Per-agent settings and last agent are stored on the active server. The current skill's folder still overrides the remembered cwd. A successful launch saves the resolved cwd. |
| Recently opened work | Already stored in server `recents.json`; successful skill and standalone Markdown opens recorded it. Only eight items were kept. The home page embedded the library with its Recent strip disabled, hiding history entirely. | Home shows the latest four entries as compact Recent shortcuts. The server keeps up to 30 records; reopening bumps an item without duplicates. Older entries remain readable without timestamps. |
| History writes | Unlocked whole-file rewrites; client writes could race, and a cold-start read could replace a newer optimistic entry. | File locks serialize writes across server processes; temporary files and rename prevent torn JSON. Client requests are serialized, late reads cannot erase pending changes, and failures are visible in Recent. |
| Theme, studio accordion/widths, session rail width | Browser `localStorage`. | Retained as client presentation preferences. They are still origin-scoped, so a different loopback port/browser has separate visual settings. |
| Session order and viewed/unread marks | Browser `localStorage`, indexed by tmux session ID. | Retained as viewer state. This does not synchronize read status or ordering between clients. |
| Last SSH host | Server file on the connecting machine; pinned-local `/api/remote/last`. | Already the correct owner; retained. |
| Mining configuration | Completed/current run records are persisted; a fresh launch dialog resets its window, agent, model, effort, and prompt. | Retained. Remembering a reusable mining preset is a separate improvement; edited prompts are operation-specific. |
| New/import installation target | Starts at the shared `universal` home (or first available home). | Retained. This audit does not make destination choice sticky. Skill names, descriptions, source URLs, uploads, and credentials entered into forms remain operation-specific. |

Server state lives under `$XDG_CONFIG_HOME/vibestudio`, otherwise
`~/.config/vibestudio` (or the desktop's explicit configuration override).
`preferences.json` and `recents.json` are private files on Unix. Missing stores
start empty. Corrupt/unreadable stores report errors and are preserved rather
than overwritten. Failed preference writes do not prevent a file or already
created session from opening.

Legacy browser history/session defaults migrate only when a loopback switchboard
explicitly reports Local. Existing server launch settings win. The old copy is
retained if migration cannot finish; an SSH server never inherits local paths.

## Recent in the existing UI

Home's **Recent** section shows the latest four entries as compact links, with
each item's name and path. The links wrap into fewer columns on narrow screens.
Opening an entry routes to the skill or standalone Markdown editor. The page
header's **Open** action accepts a path or opens the remembered file picker through **Browse…**.
Removing a recent entry only removes its history record; it does not delete the
underlying file or skill.

## Validation

```sh
npm run build
npm run lint
node --test scripts/finalize-release.test.mjs scripts/workspace-state.test.mjs
cargo test --workspace
```

The frontend checks cover directory precedence/fallback, unavailable preferences,
late history reads, ordered mutations, failed writes, and remote migration guards.
The isolated HTTP test covers persisted defaults/history across server instances,
proxied reads/writes, invalid directories, and corrupt-file preservation. It needs
permission to bind loopback ports. Headless Chromium screenshots of Home, Studio,
Sessions, the remembered picker, and Home at 390px were inspected.
Recent skill/Markdown reopening and confirmed-folder restoration passed
against an isolated server and temporary files. Discovery used a fixture to avoid
auto-tracking the user's skills. There were no JavaScript exceptions or failed
requests; the standalone fixture returned its expected 404s for unavailable
remote-profile, desktop-notification, and editor capabilities. All QA processes
were stopped afterward.
