# Terminal links and the tmux boundary

Research checked against upstream documentation and source on 2026-09-19.

## Recommendation

Implement HTTP and file links in VibeStudio's existing xterm frontend, and advertise
OSC 8 support on its tmux attachment. Keep tmux responsible for durable sessions.
This feature does not require a fork, a replacement multiplexer, or a native
terminal renderer.

If future requirements justify owning more terminal behavior, prototype tmux
control mode behind `skill-term` first. A separate terminal daemon using
`portable-pty` and `libghostty-vt` is a credible larger direction; extracting a
supported library from tmux would require substantial upstream refactoring and
long-term ownership.

## What VibeStudio currently owns

[`design.md`](../design.md) specifies that a terminal must survive closing the
desktop, losing SSH, and restarting the backend. `server/skill-term` already
provides a Rust library boundary around that behavior: tmux owns the agent PTY,
each viewer runs `tmux attach-session` in another PTY, and HTTP/SSE connects the
viewer to xterm. The frontend renders the terminal and handles its interactions.

Before this change, `TerminalPane.tsx` loaded only xterm's fit addon. It did not
register a provider for ordinary URLs or filesystem paths, or a custom OSC 8
handler. Its attachment used `TERM=xterm-256color` without explicitly advertising
hyperlink capability. These are missing integration pieces at the rendering and
attachment boundaries.

## Ordinary text and OSC 8 are separate paths

- **Ordinary output**, such as `https://example.com`, `src/main.rs:42:7`, or
  `/home/user/project/README.md`, needs text detection and a click handler. xterm
  exposes [`registerLinkProvider`](https://xtermjs.org/docs/api/terminal/classes/terminal/#registerlinkprovider)
  and an [official web-links addon](https://github.com/xtermjs/xterm.js/tree/master/addons/addon-web-links).
  This can work with old tmux versions because the visible text survives.
- **OSC 8 output** has a hidden destination and visible label, like a browser
  anchor. xterm provides a separate
  [`ILinkHandler`](https://xtermjs.org/docs/api/terminal/interfaces/ilinkhandler/).
  Non-HTTP destinations require opting in; the application must then restrict
  activation to the supported protocols instead of blindly opening arbitrary
  schemes.
- tmux added OSC 8 support in **3.4** and hyperlink display in copy mode in
  **3.5**. Its documented `-T hyperlinks` client option can advertise support for
  the specific VibeStudio attachment without editing the user's global tmux
  configuration. Older tmux cannot recover a hidden OSC 8 destination after it
  has discarded it. See the
  [upstream changelog](https://github.com/tmux/tmux/blob/master/CHANGES),
  [client option documentation](https://github.com/tmux/tmux/blob/master/tmux.1),
  and [feature implementation](https://github.com/tmux/tmux/blob/master/tty-features.c).

## What other projects do

| Project | Relevant approach | Implication for VibeStudio |
| --- | --- | --- |
| VS Code / xterm | Detects URLs and verified file paths in the terminal; Ctrl/Cmd-click opens them; supports line and column suffixes. | This is the closest interaction model, and VibeStudio already uses xterm. |
| tmux / iTerm2 | Control mode exposes commands, events, and raw pane output through a text protocol while tmux keeps the sessions alive. | More frontend control without rewriting durable process management. |
| Herdr | Own detached server and PTYs, with embedded libghostty-vt and its own terminal UI. | Demonstrates building a multiplexer around a terminal library, rather than turning tmux into one. |
| Ghostty / libghostty | Embeddable terminal engine; libghostty-vt maintains terminal state and parses sequences. | A possible future terminal core, but does not provide VibeStudio's complete durable service and HTTP contract. |
| WezTerm | Integrated terminal and multiplexer, with configurable text-to-link rules and independent multiplexing domains. | Useful separation of link policy, terminal state, and session ownership. |
| cmux | Native macOS UI built on libghostty; optional tmux ownership for live persistence. | Replacing the renderer does not automatically replace the need for a durable multiplexer. |

**VS Code:** Its documented order is URI/URL links, existing file links, folders,
then lower-confidence word searches. File links can carry line and column
positions; existence checks can be disabled for slow remote environments.
Shell integration adds command working-directory awareness. VibeStudio should
resolve files on the active server through HTTP, so SSH paths never accidentally
open a similarly named client-local file. Sources:
[terminal links](https://code.visualstudio.com/docs/terminal/basics#_links),
[shell integration](https://code.visualstudio.com/docs/terminal/shell-integration).

**tmux as a library:** The upstream build declares a `tmux` executable, without
a supported public embeddable library target. The well-known Python `libtmux` is
a command wrapper around installed tmux; it does not embed the C server. A fork
could create a library, but would need to define initialization, shutdown,
event-loop ownership, globals, error handling, and API compatibility. That is an
engineering inference from the executable architecture, not an upstream promise
that embedding is impossible. Sources:
[build definition](https://github.com/tmux/tmux/blob/master/Makefile.am),
[libtmux project](https://github.com/tmux-python/libtmux).

**Control mode:** `tmux -C` / `-CC` is upstream's integration interface, designed
for iTerm2. It reports raw application bytes in escaped `%output` messages,
alongside session/window/pane notifications and command responses. tmux's own
copy-mode UI is not included. Migrating VibeStudio would require reconstructing
initial screen/scrollback, input encoding, resizing, flow control, reattachment,
and viewer ownership. It is a good prototype candidate, not a small change to
the current PTY attach command. Source:
[tmux control mode](https://github.com/tmux/tmux/wiki/Control-Mode).

**Herdr:** The likely project meant by “Herder” is
[`herdrdev/herdr`](https://github.com/herdrdev/herdr), already credited in
VibeStudio's agent-detection NOTICE. Its current
[`Cargo.toml`](https://github.com/herdrdev/herdr/blob/master/Cargo.toml) uses
`portable-pty`, Ratatui, and Tokio; its
[`build.rs`](https://github.com/herdrdev/herdr/blob/master/build.rs) compiles and
statically links vendored `libghostty-vt`. Its
[Rust terminal wrapper](https://github.com/herdrdev/herdr/blob/master/src/ghostty/mod.rs)
retains OSC 8 metadata and resolves bounded plain-text link regions. It maintains
its own server lifecycle and runs inside an existing outer terminal. The README
distinguishes live detach from restoring saved state after its server restarts.

**Ghostty:** Its embeddable VT library is available to C and Zig, including
WebAssembly targets. Upstream still describes its API signatures as changing
and does not yet tag a standalone library version. Adopting it would need a
pinned revision, bindings, builds, and a renderer integration for each client
environment. Source:
[Ghostty library documentation](https://github.com/ghostty-org/ghostty#cross-platform-libghostty-for-embeddable-terminals).

**WezTerm:** Its
[`hyperlink_rules`](https://wezterm.org/config/lua/config/hyperlink_rules.html)
turn terminal text into destinations independently of the child process. Its
[multiplexing domains](https://wezterm.org/multiplexing.html) separate local UI
from an independently running local or remote mux server. Using its full mux
would introduce its protocol and remote version requirements alongside
VibeStudio's existing service.

**cmux:** Its [README](https://github.com/manaflow-ai/cmux) describes embedding
libghostty in Swift/AppKit. It explicitly distinguishes restoring window layout
and scrollback from preserving arbitrary live processes, and offers a local
tmux owner for the latter. Its platform and UI architecture differ from the
shared browser/desktop/phone client in this repository.

## Implementation and validation criteria

Link recognition belongs in the frontend. File existence and current directory
belong on the active server, exposed through the existing HTTP API. Link
activation should preserve ordinary terminal input, mouse handling, copy/drag,
and touch scrolling. A modifier click on a link must not also send a mouse
action to the running agent.

Cover plain HTTP/HTTPS, absolute and relative paths, `~/` paths, `file://` URIs,
line/column suffixes, quoted spaces, punctuation, Unicode, wrapped output, and
links scrolled into tmux history. Exercise named OSC 8 labels through a real
tmux attachment, including reconnect. Reject unsupported URL schemes, report
missing files clearly, and verify that file resolution follows a changed pane
directory. Keep fixture sessions on a private tmux socket.

File links require the **active workspace host** to run server v1.2.8 or newer.
The original desktop update left a healthy persistent WSL/SSH worker on v1.2.4:
reuse checked only its protocol. Host selection now also requires the server to be
at least the client's release version, with verified upgrades on explicit connection
and a Retry error during background recovery. Test this mixed version case as well
as matching fresh installations. A missing resolver route on older clients
must explain the host upgrade requirement, never guess relative paths using the
session's saved launch directory. An explicit host upgrade should verify the new
binary first, stop only the identity-verified HTTP service, then restart and check
that the same tmux pane process survives and the reported link resolves. Verify
the workspace host's health directly: the desktop switchboard's health endpoint
reports its own version.

Run frontend build/lint/tests and Rust checks for the changed crates, followed
by browser interaction against the actual HTTP/SSE backend. A native platform
build is still needed to verify that platform's webview and external-browser
launch behavior; a Linux browser test alone cannot establish that.

## Implemented behavior and verification

- Hover to underline links; Ctrl-click on Windows/Linux, Cmd-click on macOS,
  or tap on a touch device to open them. Ordinary clicks and Select mode retain
  their terminal behavior.
- HTTP/HTTPS links use the existing external-browser handling. File links open
  an in-app read-only source/image preview, with line/column navigation and the
  existing optional VS Code button. Plain text and OSC 8 links share this policy.
- Paths are validated when clicked, without a filesystem request on every
  hover. Relative paths use the pane's current directory at click time, not a
  recorded historical command directory. Missing files report an inline error;
  directories are not file previews. Quoted paths support spaces.
- tmux 3.4+ attachments advertise hyperlinks only for that client. Earlier
  versions still support links detected from visible text. tmux 3.4 does not
  preserve named hyperlink destinations in copy mode; that requires 3.5+.

Verified with `npm run build`, `npm run lint`, `npm test` (106 passed, 2 skipped),
and `cargo test --workspace` (363 passed). New coverage includes parser/cell
mapping tests, live tmux directory and active-pane changes, exact session
matching, OSC 8 reattachment, and the HTTP resolver through a remote proxy.
Rust tests used private `TMUX_TMPDIR` and `XDG_CONFIG_HOME` directories.

Chromium testing against the real HTTP/SSE backend and tmux 3.4 covered web and
named OSC 8 links, quoted filenames, requested lines/columns, wrapped Unicode
paths, missing files, ordinary clicks, Select mode, synthetic touch taps,
focus restoration, changed directories, and a fresh browser attachment.
Modifier clicks did not forward mouse input to the terminal; ordinary clicks
still did. Native Windows/macOS/iOS webviews were not exercised. The iOS shell
currently lacks the desktop shell's external new-window handler, so external
web-link launching there remains a platform follow-up.

Testing exposed an existing isolation bug: `--no-startup-maintenance` still
repointed machine-wide Tailscale Serve mappings. Both server entry points now
honor the flag for phone resync too. The user's approved production mapping
was restored to port 8765 after verification.
