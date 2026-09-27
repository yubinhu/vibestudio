# Development and validation

Run commands from the repository root unless a section says otherwise. See the
[source-build prerequisites](../README.md#build-from-source) for toolchain setup.
[package.json](../package.json) defines the npm scripts; the
[architecture guide](../design.md) explains the client/server boundary.

## Development modes

| Goal | Backend | Frontend / entry point |
| --- | --- | --- |
| Native desktop | Started by the shell | `npm run dev` opens the app window |
| Browser using an existing host | Keep that host running | `VITE_API_TARGET=http://127.0.0.1:<host-port> npm run dev:vite`; open `http://localhost:1420` |
| Browser with a separate development backend | Use the [isolated backend](#isolated-backend) below | The recipe starts Vite against that backend |
| Mobile UI in a browser | Use the [mobile variant](#mobile-ui) of the isolated backend | Open Vite with a phone viewport |
| Built UI and API on one origin | `npm run build`, then run `skill-server --dist dist` with the intended host configuration | Open the server's URL |

Native dev creates a client switchboard and ensures the detached host service;
it does not need a second backend process. The browser proxy can instead target
an existing local or SSH-forwarded host URL. These modes use real workspace data;
use fixture files for destructive UI checks.

### Ports

[vite.config.ts](../vite.config.ts) owns proxy defaults and
[the desktop shell](../client/desktop/src/lib.rs) chooses native server ports.

| Mode | Vite origin | API proxy / switchboard | Preferred host port |
| --- | --- | --- | --- |
| Browser | `1420` | `8765` unless `VITE_API_TARGET` is set | Standalone server defaults to `8765` |
| Native dev | `1420` | `8767` (`--mode native`) | `8766` for a new host |
| Packaged desktop | No Vite | Ephemeral loopback port | `8765` for a new host |

An existing verified host record takes precedence over the preferred port; native
dev, desktop and SSH access can share that service. If overriding the native dev
switchboard with `VIBESTUDIO_PORT`, set `VITE_API_TARGET` to the matching URL.
Native iOS uses its own ephemeral loopback origin. See
[host lifecycle](../design.md#durable-host-lifecycle--open-on-your-phone) for
discovery, persistence and browser phone access.

### WSLg: no desktop window after a successful build

A running Linux webview does not establish that Windows can display its window.
If `npm run dev` compiles but nothing appears, inspect WSLg's current logs before
changing app rendering settings:

```bash
rg -n 'terminated with signal|not starting it again' /mnt/wslg/stderr.log
rg -n 'rdp_peer|weston-notify.sock' /mnt/wslg/weston.log
```

A compositor crash followed by repeated `rdp_peer is not initalized` messages
(the spelling comes from Weston), a missing `weston-notify.sock`, and the Windows
display client reaching its restart limit indicate a broken WSLg desktop
connection. Native acceptance tests can still pass inside Linux in this state;
they do not prove that a window is visible on the Windows desktop.

Save work and arrange to stop active WSL agents and services before recovery.
From **Windows PowerShell**, run `wsl --shutdown`, reopen the distribution, and
retry `npm run dev`. This stops all running WSL distributions, including tmux
sessions and the host service; agents must not run it during ordinary validation.
See Microsoft's [WSL GUI app guide](https://learn.microsoft.com/en-us/windows/wsl/tutorials/gui-apps)
for restart and update instructions.

If WSLg is connected but WebKit still has graphics problems, follow
[Tauri's Linux graphics guide](https://v2.tauri.app/develop/debug/linux-graphics/).
For an isolated X11/software-rendering diagnostic, use
`GDK_BACKEND=x11 WEBKIT_DISABLE_COMPOSITING_MODE=1 npm run dev`.
These overrides can reduce rendering performance and cannot repair a disconnected
WSLg desktop bridge; they are not app defaults.

### Home UI lab

The lab uses the same backend proxy settings as browser mode. Point
`VITE_API_TARGET` at your chosen backend when running `npm run dev:home-lab`;
do not infer its port from a running native dev window. The comparison workflow,
entry URL and build output belong in the
[Home UI lab guide](../client/web/app/home-lab/README.md).

### Native live UI comparison checks

The [comparison architecture](../design.md#live-ui-comparison) and bundled
[agent skill](../skills/ui-compare/SKILL.md) describe the feature and its configuration.
Build the SPA, then run the isolated
[acceptance runner](../client/desktop/tests/comparison_smoke.py):
`python3 client/desktop/tests/comparison_smoke.py --xvfb` on Linux, or omit `--xvfb`
on a native desktop. It creates a temporary Git/Vite project and checks the real
comparison adapter and HTTP manager without starting the host service, changing
phone access or installing skills.

For custom fixtures, use the underlying
[native harness](../client/desktop/examples/comparison_smoke.rs). Supply a configuration
pointing at a temporary Git repository and set private `XDG_CONFIG_HOME` and
`TMUX_TMPDIR`. Run
`cargo run --manifest-path client/desktop/Cargo.toml --example comparison_smoke -- /tmp/fixture/config.json`.
On Linux, `xvfb-run --auto-servernum` with `GDK_BACKEND=x11` selects the virtual
screen; headless machines without GPU support can set `WEBKIT_DISABLE_COMPOSITING_MODE=1`.
The harness prints its HTTP URL and session ID, then confirms that all three native
children are visible. Its optional `VIBESTUDIO_COMPARISON_SMOKE_SCRIPT` injects
fixture-only JavaScript for reporting actual CSS dimensions and scroll positions.

Acceptance checks should edit a saved component and verify working-pane HMR with the
baseline unchanged, assert both actual CSS viewports after preset/rotation/custom-size
changes, and scroll both documents and nested containers in both directions (including
different content heights and disabling sync). Close through HTTP and the native
window control, verify the viewer disappears while its checkout and servers remain,
and reopen the same artifact. Switch between associated diffs through the toolbar;
opening an existing viewer must focus it without duplicating children. Finally stop
through HTTP and verify only the owned checkout and servers were removed. Normal
browser tests cannot establish native child allocation, zoom or window lifecycle.

[Session artifact browser tests](../e2e/session-comparisons-ui.spec.ts) cover the
session UI, metadata, explicit association and creation with isolated fixtures.
Run `python3 scripts/comparison_cli_test.py` for the helper's exact session/host
association checks. Persistence and pinned reopen behavior are covered by the
comparison core and HTTP tests.

The native runner also measures continuous root/nested scroll latency, checks for
reflected updates, and exercises the navigation fallback under a strict preview CSP.
The device chooser's **Sync from Chrome** action explicitly refreshes its local
catalog. It has no automatic refresh schedule or agent task. `npm run devices:update`
regenerates the bundled offline snapshot with the
[data-only generator](../scripts/update-comparison-devices.mjs).
The core's ignored `comparison_devices::tests::live_upstream_is_accepted` check
verifies live-source parsing when run explicitly with network access; ordinary tests
use fixture data and do not require Chrome or internet access.

The [pinned Tauri version](../client/desktop/Cargo.lock) exposes unstable multiwebviews.
Its Linux GtkLayout adapter positions children within the allocated content area.
Child size requests do not contribute to the window minimum, preventing a resize
feedback loop from GTK decorations or display scaling. Comparison needs
macOS 11+ for native page zoom; the rest of VibeStudio retains its existing platform
minimum. Windows uses WebView2. Large custom viewports increase the minimum window size
to preserve CSS dimensions within the native zoom range. Native pixel rounding can
shift arbitrary custom sizes by a few CSS pixels when scaled; both panes use the same
geometry. Linux runtime validation does not substitute for native macOS/Windows checks.

## Isolated backend

For browser interaction tests and release screenshots, use a separate foreground
server with a private config directory and tmux socket directory. Pass
`--no-startup-maintenance`: Tailscale Serve configuration is machine-wide, so a
private config directory alone cannot protect the live phone-access mapping.

This Bash recipe works on Linux/macOS with Rust, Node dependencies, tmux, `curl`
and `rg` installed. Run it in a dedicated terminal. Choose unused backend/Vite ports;
startup must fail rather than displace an existing service. It builds the server,
waits for its ready message and Vite, and retains both until you press Enter.

```bash
(
  set -e
  # For the mobile variant, change these two arrays as described below.
  server_build_flags=()
  server_run_flags=()
  cargo build -p skill-server "${server_build_flags[@]}"

  # Keep the socket path short enough for macOS as well as Linux.
  visual_fixture=$(mktemp -d /tmp/vs-visual.XXXXXX)
  mkdir -p "$visual_fixture/config" "$visual_fixture/tmux"
  visual_server_pid=
  visual_vite_pid=
  cleanup_visual() {
    for visual_pid in "$visual_vite_pid" "$visual_server_pid"; do
      if [ -n "$visual_pid" ]; then
        kill "$visual_pid" 2>/dev/null || true
        wait "$visual_pid" 2>/dev/null || true
      fi
    done
    # Explicit fixture socket: never fall back to the user's tmux server.
    env -u TMUX -u TMUX_PANE tmux \
      -S "$visual_fixture/tmux/tmux-$(id -u)/default" kill-server 2>/dev/null || true
    # Logs and config remain available for inspection.
  }
  trap cleanup_visual EXIT
  trap 'exit 130' INT
  trap 'exit 143' TERM

  env -u VIBESTUDIO_SERVER_TOKEN -u TMUX -u TMUX_PANE \
    XDG_CONFIG_HOME="$visual_fixture/config" TMUX_TMPDIR="$visual_fixture/tmux" \
    ./target/debug/skill-server --port 8799 --no-startup-maintenance \
    "${server_run_flags[@]}" > "$visual_fixture/server.log" 2>&1 &
  visual_server_pid=$!
  VITE_API_TARGET=http://127.0.0.1:8799 \
    node node_modules/vite/bin/vite.js --host 127.0.0.1 --port 1421 --strictPort \
    > "$visual_fixture/vite.log" 2>&1 &
  visual_vite_pid=$!

  for visual_attempt in {1..100}; do
    kill -0 "$visual_server_pid" "$visual_vite_pid" 2>/dev/null || {
      cat "$visual_fixture/server.log" "$visual_fixture/vite.log"
      exit 1
    }
    if rg -q '^SKILL_SERVER_READY port=8799$' "$visual_fixture/server.log" \
      && rg -q 'Local:.*:1421/' "$visual_fixture/vite.log" \
      && curl --max-time 1 -fsS http://127.0.0.1:1421/ >/dev/null; then
      break
    fi
    sleep 0.1
  done
  rg -q '^SKILL_SERVER_READY port=8799$' "$visual_fixture/server.log"
  rg -q 'Local:.*:1421/' "$visual_fixture/vite.log"
  kill -0 "$visual_server_pid" "$visual_vite_pid"
  curl --max-time 1 -fsS http://127.0.0.1:1421/ >/dev/null
  printf 'Open http://127.0.0.1:1421; logs: %s\n' "$visual_fixture"
  read -r -p 'Press Enter after browser checks to stop this fixture. '
)
```

Drive that URL with your browser automation tool or a manual browser. The app uses
hash routes: Studio is `/#/skills/<encoded-root>`, with a skill root returned by
`GET /api/skills/discover`. Config and terminal isolation do not sandbox filesystem
operations: select temporary skills/repos for edits. Connecting to a real remote
also uses that remote's workspace and service.

Cleanup targets only the captured processes and explicit private tmux socket.
Never use a bare `tmux kill-server`, stop the live host, or kill by process name or
port for these checks. Remove the printed fixture directory after reviewing its
logs; mobile fixtures can contain private test keys.

## Mobile UI

In the [isolated backend recipe](#isolated-backend), set:

```bash
server_build_flags=(--features russh-transport)
server_run_flags=(--mobile-dev)
```

`--mobile-dev` supplies a file-backed credential store; `russh-transport` uses the
same in-process SSH implementation as iOS. The SPA detects mobile capability from
`/api/remote/profiles`, independently of viewport or user agent. Set a phone
viewport to exercise connection screens, layout, file picking and terminal input
with Vite hot reload. See the
[connection architecture](../design.md#connection-manager-vs-code-remote---ssh)
for credential and reconnect behavior.

Browser checks do not exercise native iOS suspension, Keychain, biometrics,
notification presentation or WKWebView keyboard behavior. Use the
[native simulator harness](../client/desktop/gen/apple/Tests/NativeUI/README.md)
for supported native checks, and a physical device for the limitations it lists.
Future mobile work is tracked in the [mobile plan](../plans/mobile-ux.md).

## Validation

### Browser E2E

The focused [browser suite](../e2e/) exercises discovery/opening, autosave across
navigation and reload, session status, and terminal clipboard events through the
real backend and tmux.
It runs in desktop Chromium and phone-sized WebKit. A scripted agent supplies
repeatable terminal states without model accounts or API keys.

Use Node 22, Rust, git and tmux on Linux/macOS. Install the test browsers once:

```bash
npm ci
npx playwright install --with-deps chromium webkit
npm run test:e2e
```

The command builds the UI and backend. Each test starts a foreground backend on
an ephemeral loopback port with private home/config/cache directories and a
private tmux socket, disables startup maintenance, and cleans up its own processes.
Failure screenshots, traces and backend logs land in `test-results/`; CI uploads
them as an artifact. To debug one case after building, use
`npx playwright test --project=desktop -g "test name"`.
Phone-sized WebKit coverage does not replace the native iOS simulator checks.

### Other checks

Run checks appropriate to the change. The common frontend loop is:

```bash
npm test
npm run lint
npm run build
```

For backend changes, run the affected crate's tests and the workspace gates:

```bash
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
```

Tests that create servers or sessions must own and clean up their temporary
state; use the isolation principles above for manual integration checks. Terminal
tests need tmux, and SSH command tests need the shells under test (CI installs
fish and zsh). Check skipped tests rather than counting them as platform coverage.

[ci.yml](../.github/workflows/ci.yml) is the exhaustive definition of platform,
mobile transport, desktop and native simulator gates. Use its commands for the
affected platform; browser success alone does not validate a native shell.
For docs-only edits, check links, anchors, command syntax and agreement with source.
Release-specific checks and publication belong in [RELEASING.md](../RELEASING.md).
