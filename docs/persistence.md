# Workspace defaults and history

This document owns the persistence contract for workspace defaults and recent
files. Paths, agent launch defaults, and opened-document history belong to the
active server. Theme and layout belong to the viewing client. The desktop keeps
the same web origin when switching SSH hosts, so browser storage alone cannot
safely scope filesystem defaults. The [architecture](../design.md) explains how
workspace requests reach the selected server.

## State ownership

| State | Owner and storage | Behavior |
| --- | --- | --- |
| Folder picker locations | Active server, `preferences.json` | Confirmed selections are remembered independently for Open, Import, and Session. Cancelling does not save a location. |
| Session launch defaults | Active server, `preferences.json` | Settings are stored per agent, together with the last selected agent. A successful launch saves the resolved working directory. |
| Recently opened work | Active server, `recents.json` | Successful skill and standalone Markdown opens are recorded newest first, deduplicated by path and capped at 30 entries. Reopening moves an item to the front. Older entries without timestamps remain readable. |
| Theme, studio accordion/widths, session rail width | Viewing client, browser `localStorage` | Presentation settings are origin-scoped; another browser or loopback port has separate settings. |
| Session order and viewed/unread marks | Viewing client, browser `localStorage`, indexed by tmux session ID | Ordering and read status do not synchronize between clients. |
| Last SSH host | Connecting machine | The connection manager owns this setting; see [connection lifecycle](../design.md#connection-manager-vs-code-remote---ssh). |

The server contract is implemented in [preferences.rs](../server/skill-core/src/preferences.rs)
and [recents.rs](../server/skill-core/src/recents.rs), reached through the ordinary
proxied preferences and recents functions in [api.ts](../client/web/lib/api.ts).
Client presentation state lives in [theme.ts](../client/web/lib/theme.ts),
[studioLayout.ts](../client/web/lib/studioLayout.ts), and
[sessions.ts](../client/web/lib/sessions.ts).

## Past agent sessions

**History** on Home and in Sessions lists saved Claude Code and Codex
conversations on the active host, including conversations started outside
VibeStudio. Search matches titles, agent names and working folders; results are
ordered by their saved activity and loaded in pages. Opening History does not
start an agent or make a model request.

The agents' native stores own persistence and retention. VibeStudio reads their
conversation metadata without copying transcripts into a second store. Closing
a VibeStudio terminal or restarting the host therefore leaves a recorded
conversation available in History. Conversations deleted by the agent, unsaved
sessions, Codex archived threads and subagent conversations are excluded. History
is not a backup of terminal scrollback. Provider-specific discovery, limits,
configuration-directory overrides and metadata parsing live in
[session_history.rs](../server/skill-core/src/session_history.rs).

Resume reopens the **selected conversation ID** in an ordinary agent terminal,
using its recorded working folder. It never falls back to the most recent
conversation in that folder. A VibeStudio terminal still running the same agent
conversation is reused, including after a repeated resume request. After the
agent exits, Resume starts a new terminal and keeps the old terminal's scrollback.
Missing executables or folders have an explanatory unavailable state; partial
or unreadable history is reported without hiding results from another provider.
Only agents with a verified history/exact-resume capability are included;
other agents and shell terminals retain the existing live-session workflow.

The ordinary proxied routes are `GET /api/terminal/history` and
`POST /api/terminal/history/resume`. The registry's separate cwd-latest resume
capability continues serving mining's isolated run directories. Host-specific
history and exact launch behavior are implemented by
[skill-term history](../server/skill-term/src/history.rs); the reusable
[History dialog](../client/web/components/SessionHistoryDialog.tsx) returns to
the existing terminal after opening a conversation. Native saved titles remain
authoritative; the terminal-specific VibeStudio naming override described below
is not a persistent conversation rename.

## Picker and launch defaults

The folder picker tries the current form's explicit directory, then the last
confirmed selection for that workflow, then Home. Each candidate must be readable;
a missing or unreadable directory falls through to the next candidate. Missing
preferences do not prevent browsing. See [picker.ts](../client/web/lib/picker.ts)
and [FolderPicker.tsx](../client/web/components/FolderPicker.tsx).

The current skill's directory takes precedence over remembered launch defaults.
The launch dialog restores the last agent when it is available, otherwise chooses
an available agent. Failed preference writes do not prevent a selected file or an
already-created session from opening. See
[NewSessionDialog.tsx](../client/web/components/NewSessionDialog.tsx).

## Recent in the UI

Home's **Recent** section shows the latest four entries as compact shortcuts.
Each displays its name, with the full path in a tooltip; the shortcuts wrap on
narrow screens. Opening one routes to the skill or standalone Markdown editor.
Removing an entry removes only its history record, not the underlying file or
skill. The implementation is [RecentStrip.tsx](../client/web/components/RecentStrip.tsx).

## Storage and failure handling

[paths.rs](../server/skill-core/src/paths.rs) resolves the server's configuration
directory: an explicit override wins, followed by `$XDG_CONFIG_HOME/vibestudio`,
then `~/.config/vibestudio`. Missing stores start empty. Preferences and recents
use private files on Unix. Corrupt or unreadable stores report errors and are
preserved rather than overwritten.

[state_store.rs](../server/skill-core/src/state_store.rs) serializes writes with
file locks across server processes and replaces JSON through a temporary file and
rename. The [client history store](../client/web/lib/recents.ts) serializes
requests, prevents late reads from erasing pending changes, and exposes failed
reads and writes in Recent.

Legacy browser history and session defaults migrate only when a loopback
switchboard explicitly reports Local. Existing server entries and launch settings
win. The old browser copy remains if migration cannot finish; an SSH server never
inherits local paths. Migration lives in the client history store and
[terminalPrefs.ts](../client/web/lib/terminalPrefs.ts).

## Session titles

The sessions rail, compact picker and Home cards display a user override, the
agent's saved title, or the launch label, in that order. Right-click or hold the
title to choose **Rename**; a focused session also supports Shift+F10/Menu and F2.
The compact picker includes **Rename current session…** without a separate toolbar
button. `POST /api/terminal/rename` accepts a
terminal `id` and a single-line `title` of up to 200 Unicode characters. An explicit
`null` removes the VibeStudio override. The host verifies that the terminal exists
and validates the name before making a change.

For an exactly identified Codex thread, VibeStudio first uses Codex's supported
`thread/name/set` API. A successful native rename removes any older VibeStudio
override; Codex then owns the name. This does not resume the thread or run a model
turn. If the native operation is unavailable or fails, the host saves the name in
`session-titles.json` in its configuration directory. Other agents use this same
fallback. Names are keyed by the stable terminal ID, survive host restarts, and
appear to other clients on their next inventory refresh. They do not transfer to
a newly created terminal. Closing a terminal removes its override when possible.

The fallback uses the locked, atomic JSON store. Write errors remain visible in
the rename dialog and preserve existing data. An unreadable names file is logged
and omitted from inventory enrichment so live terminals remain available. The
dialog's **Use automatic title** action removes a local override and reveals the
current agent title or launch label; native Codex names can be edited again.

## Validation

[Frontend workspace-state tests](../scripts/workspace-state.test.mjs) cover picker
fallback, history ordering, failure handling, and migration guards.
[HTTP workspace-state tests](../server/skill-server/tests/workspace_state.rs)
exercise server persistence and proxied access. Run them using the shared
[validation guidance](development.md#validation); use an
[isolated backend](development.md#isolated-backend) for manual checks.
