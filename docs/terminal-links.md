# Terminal links

VibeStudio keeps stock tmux for durable agent sessions and implements link
interaction in xterm. The [terminal architecture](../design.md#terminals-persistent-by-design)
owns session and attachment lifetime; the dated
[research note](../plans/terminal-links-research.md) records why this boundary was
chosen and compares alternatives.

## Interaction

Hover to underline a detected link. Ctrl-click on Windows/Linux, Cmd-click on
macOS, or tap on a touch device to open it. Ordinary clicks, text selection and
Select mode retain their terminal behavior; activating a link must not also send
a mouse action to the running agent.

HTTP/HTTPS links use external-browser handling. File links open a read-only source
or image preview inside VibeStudio. A line/column suffix navigates the source
preview. The optional **Open in VS Code** action opens the resolved file on the
selected host; WSL targets use `wsl+<distro>`, and SSH targets use
`ssh-remote+<host>`. The editor action currently passes the path without the
preview's line/column position.

Visible-text detection supports absolute and relative paths, `~/` paths,
`file://` URIs, line/column suffixes, quoted spaces, Unicode and wrapped output.
OSC 8 hyperlinks have a separate detection path but share the activation policy.
Unsupported URL schemes are rejected. Missing files report an error; directories
are not file previews. Binary and oversized files report preview limitations.

## Resolution and compatibility

`POST /api/terminal/resolve-link` resolves a file on the **active workspace host**.
The server reads the tmux pane's current working directory and validates and
canonicalizes the target. Relative paths use that directory at click time, not
the session's launch folder or a historical command directory. Validation happens
when clicked, without a filesystem request on every hover.

The resolver was introduced in server v1.2.8. Active host selection follows the
[host compatibility policy](../design.md#host-compatibility); a missing route must
report an upgrade requirement rather than guess a remote file's location. The
desktop switchboard's health reports its own version, so verify the workspace
host directly when diagnosing an old resolver.

For tmux 3.4+, VibeStudio advertises OSC 8 support on its own attachment with
`-T hyperlinks`, without editing global tmux configuration. Earlier tmux versions
still support links recognized from visible text. Named OSC 8 destinations in
tmux copy mode require tmux 3.5+.

The iOS shell does not yet install the desktop shell's external new-window
handler. Native iOS external web-link launching remains a platform follow-up;
successful Chromium checks do not establish that behavior.

## Implementation and verification

| Responsibility | Source |
| --- | --- |
| Detection, punctuation and terminal cell mapping | [terminalLinks.ts](../client/web/lib/terminalLinks.ts) |
| xterm providers, modifier/touch input and OSC 8 | [TerminalPane.tsx](../client/web/components/TerminalPane.tsx) |
| Current pane lookup | [skill-term terminal_links.rs](../server/skill-term/src/terminal_links.rs) |
| Filesystem validation | [skill-core terminal_links.rs](../server/skill-core/src/terminal_links.rs) |
| Source/image preview | [TerminalFilePreview.tsx](../client/web/components/TerminalFilePreview.tsx) |
| Native VS Code routing | [editor.rs](../client/desktop/src/editor.rs) |

For changes, cover parser/cell mapping and live pane-directory changes, then use
an [isolated backend](development.md#isolated-backend) for real HTTP/SSE interaction.
Exercise web, file and named OSC 8 links, Unicode/wrapping, missing files, normal
clicks/selection, touch scrolling, focus restoration and reconnection. Verify
modifier activation does not forward terminal mouse input. Native webview and
external-browser behavior need validation on the affected platform.
