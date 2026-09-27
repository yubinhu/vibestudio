# Terminal links and the tmux boundary

Historical research checked against upstream documentation and source on 2026-09-19.
The selected approach is implemented; [terminal links](../docs/terminal-links.md)
owns current behavior, compatibility and platform limitations. External project
comparisons below describe the research date, not a continuously updated inventory.

## Decision and future options

The selected approach keeps tmux responsible for durable sessions and puts link
interaction in the existing xterm frontend. It avoids owning a multiplexer fork
or replacing the terminal renderer solely for this feature.

If future requirements justify owning more terminal behavior, prototype tmux
control mode behind `skill-term` first. A separate terminal daemon using
`portable-pty` and `libghostty-vt` is a credible larger direction; extracting a
supported library from tmux would require substantial upstream refactoring and
long-term ownership.

## Baseline at the time of research

The [terminal architecture](../design.md#terminals-persistent-by-design) already
separated durable tmux sessions from the HTTP/SSE bridge and frontend renderer.
Before link support, `TerminalPane.tsx` loaded only xterm's fit addon, and the tmux
attachment did not explicitly advertise hyperlink capability. The missing pieces
were at those integration boundaries.

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

## Historical implementation verification

The implementation was checked with frontend build/lint/tests, Rust tests and
Chromium against the real HTTP/SSE backend and tmux 3.4. Coverage included web and
named OSC 8 links, quoted filenames, line/column previews, wrapped Unicode paths,
missing files, ordinary clicks, selection, synthetic touch taps, focus restoration,
changed directories and reconnection. Modifier clicks did not forward terminal
mouse input. Native Windows/macOS/iOS webviews were not exercised in that pass.
Current validation expectations and platform gaps belong in the
[terminal link contract](../docs/terminal-links.md#implementation-and-verification).

Follow-up testing found two cross-cutting issues: a healthy but older host could
lack the resolver route, and test-server startup could change the machine-wide
Tailscale mapping. Their maintained contracts now live in
[host compatibility](../design.md#host-compatibility) and the
[isolated-backend procedure](../docs/development.md#isolated-backend).
