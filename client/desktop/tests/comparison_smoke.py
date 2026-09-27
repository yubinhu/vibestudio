#!/usr/bin/env python3
"""Exercise live UI comparison in real native child webviews with isolated data.

Prerequisites: npm dependencies, a built dist/, Rust/Tauri build prerequisites,
and a desktop display (or --xvfb on Linux). No host service or phone mapping is
started. Failed runs retain their fixture directory and logs for diagnosis.
"""
import argparse
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid


REPOSITORY = Path(__file__).resolve().parents[3]
READY = re.compile(r"COMPARISON_SMOKE_READY url=(\S+) id=(\S+)")

HTML = """<!doctype html>
<meta name="viewport" content="width=device-width, initial-scale=1">
<style>
body { margin: 0; background: #eef2ff; font: 20px system-ui; }
h1 { padding: 20px; }
#nested { height: 240px; overflow: auto; background: #dbeafe; }
#nested > div { height: 2200px; padding: 10px; }
.tall { height: 4200px; background: linear-gradient(#eef2ff, #93c5fd); }
</style>
<h1 id="content"></h1>
<div id="nested" data-comparison-scroll-key="nested"><div>Nested scroll</div></div>
<div class="tall"></div>
<script type="module" src="/app.js"></script>
"""

# Evaluated by the native Rust example in each real preview webview. It reports
# browser viewport/scroll state; no DOM emulation or external browser is used.
PROBE = """(() => {
  if (window.__comparisonSmokeInstalled) return;
  window.__comparisonSmokeInstalled = true;
  let handled = '', busy = false, sequence = 0, motion = null, completed = null;
  let frames = [], incoming = 0, cspBlocked = 0;
  const scrollTo = Element.prototype.scrollTo;
  const now = () => performance.timeOrigin + performance.now();
  Element.prototype.scrollTo = function (...args) {
    if (motion && now() >= motion.start && now() <= motion.start + motion.duration) incoming++;
    return scrollTo.apply(this, args);
  };
  document.addEventListener('securitypolicyviolation', event => {
    if (event.effectiveDirective === 'connect-src' && event.blockedURI.startsWith('http') &&
        !event.blockedURI.startsWith(location.origin)) cspBlocked++;
  });
  function tick() {
    const time = now();
    if (motion && time >= motion.start && completed?.id !== motion.id) {
      const target = motion.target === 'nested' ? document.querySelector('#nested') : document.scrollingElement;
      const range = target.scrollHeight - target.clientHeight;
      if (time <= motion.start + motion.duration) {
        if (location.port === motion.port) scrollTo.call(target, {
          top: (.1 + .6 * (time - motion.start) / motion.duration) * range, left: 0, behavior: 'instant',
        });
        frames.push({time, fraction: target.scrollTop / Math.max(1, range)});
      } else {
        const lags = frames.filter(frame => frame.time > motion.start + 300 && frame.time < motion.start + motion.duration - 100)
          .map(frame => frame.time - (motion.start + (frame.fraction - .1) / .6 * motion.duration)).sort((a, b) => a - b);
        completed = { id: motion.id, samples: frames.length, incoming,
          distinct: new Set(frames.map(frame => Math.round(frame.fraction * 100000))).size,
          medianLag: lags[Math.floor(lags.length / 2)], p95Lag: lags[Math.floor(lags.length * .95)] };
      }
    }
    requestAnimationFrame(tick);
  }
  requestAnimationFrame(tick);
  setInterval(async () => {
    if (busy) return;
    busy = true;
    try {
      const root = document.scrollingElement;
      const nested = document.querySelector('#nested');
      const content = document.querySelector('#content');
      if (!root || !nested || !content) return;
      const command = await fetch('/__control', {cache: 'no-store'}).then(r => r.json());
      if (command.continuous && motion?.id !== command.id) {
        motion = command; completed = null; frames = []; incoming = 0;
      }
      if (command.id && command.id !== handled && command.port === location.port) {
        handled = command.id;
        const target = command.target === 'nested' ? nested : root;
        if (!command.continuous) scrollTo.call(target, {left: 0, top: command.fraction * (target.scrollHeight - target.clientHeight)});
      }
      await fetch('/__report', {
        method: 'POST',
        body: JSON.stringify({
          port: location.port, time: Date.now(), sequence: ++sequence, handled,
          width: innerWidth, height: innerHeight, text: content.textContent,
          loads: window.hmrLoads,
          rootY: root.scrollTop, rootRange: root.scrollHeight - root.clientHeight,
          nestedY: nested.scrollTop, nestedRange: nested.scrollHeight - nested.clientHeight,
          motion: completed, cspBlocked, strictCsp: Boolean(document.querySelector('meta[http-equiv="Content-Security-Policy"]')),
        })
      });
    } catch (_) {
      // A request interrupted by HMR, navigation or shutdown is retried next tick.
    } finally { busy = false; }
  }, 200);
})();
"""


def component(text, root_height, nested_height):
    return (
        f"document.querySelector('#content').textContent = {json.dumps(text)};\n"
        f"document.querySelector('.tall').style.height = '{root_height}px';\n"
        f"document.querySelector('#nested > div').style.height = '{nested_height}px';\n"
        "window.hmrLoads = (window.hmrLoads || 0) + 1;\n"
        "if (import.meta.hot) import.meta.hot.accept();\n"
    )


def request(url, body=None, timeout=3):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(url, data=data, headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=timeout) as response:
        return json.load(response)


def atomic_json(path, value):
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps(value), encoding="utf-8")
    temporary.replace(path)


def git(repo, *args):
    result = subprocess.run(
        ["git", "-C", str(repo), *args], check=True, capture_output=True,
        text=True, timeout=20,
    )
    return result.stdout.strip()


def port_closed(url):
    parsed = urllib.parse.urlparse(url)
    try:
        with socket.create_connection((parsed.hostname, parsed.port), timeout=0.2):
            return False
    except OSError:
        return True


class Smoke:
    def __init__(self, args):
        self.args = args
        self.root = Path(tempfile.mkdtemp(prefix="vs-comparison-smoke-"))
        self.repo = self.root / "repo"
        self.log_path = self.root / "native.log"
        self.process = None
        self.log = None
        self.base = None
        self.session_id = None
        self.other_ids = []
        self.session = None
        self.ports = None
        self.passed = []
        self.token = uuid.uuid4().hex
        self.working_edit = component("Working uncommitted component", 6400, 3400)

    def setup(self):
        self.repo.mkdir()
        for directory in ("config", "tmux"):
            (self.root / directory).mkdir()
        atomic_json(self.root / "control.json", {})
        (self.root / "smoke.js").write_text(PROBE, encoding="utf-8")
        (self.repo / "package.json").write_text('{"type":"module"}', encoding="utf-8")
        (self.repo / "index.html").write_text(HTML, encoding="utf-8")
        (self.repo / "app.js").write_text(component("Baseline component", 4200, 2200), encoding="utf-8")
        # Absolute fixture paths survive the detached baseline checkout. Each
        # pane appends reports to its own file, avoiding cross-process writes.
        config = """import fs from 'node:fs';
import path from 'node:path';
const fixture = FIXTURE, token = TOKEN;
const side = process.env.VIBESTUDIO_COMPARISON_SIDE;
export default {plugins:[{name:'comparison-smoke-reports',configureServer(server) {
  const record = path.join(fixture, side + '-server.json');
  server.httpServer.once('listening', () => {
    fs.writeFileSync(record, JSON.stringify({pid:process.pid, port:server.httpServer.address().port, token}));
  });
  server.middlewares.use('/__control', (req,res) => {
    res.setHeader('Content-Type','application/json');
    res.setHeader('Cache-Control','no-store');
    res.end(fs.readFileSync(path.join(fixture,'control.json')));
  });
  server.middlewares.use('/__report', (req,res) => {
    let body=''; req.on('data',chunk=>body+=chunk);
    req.on('end',()=>{
      fs.appendFileSync(path.join(fixture, side + '-reports.jsonl'),body+'\\n');
      res.end('ok');
    });
  });
  // Failure cleanup can close only these exact fixture servers if the native
  // process has already died. Never signal a process discovered by port/name.
  server.middlewares.use('/__shutdown', (req,res) => {
    if (new URL(req.url, 'http://fixture').searchParams.get('token') !== token) {res.statusCode=403;res.end('{}');return;}
    res.setHeader('Content-Type','application/json'); res.end('{"ok":true}');
    setTimeout(()=>server.close().then(()=>process.exit(0)),50);
  });
}}]};
""".replace("FIXTURE", json.dumps(str(self.root))).replace("TOKEN", json.dumps(self.token))
        (self.repo / "vite.config.mjs").write_text(config, encoding="utf-8")
        git(self.repo, "init", "-b", "main")
        git(self.repo, "add", ".")
        git(self.repo, "-c", "user.name=Comparison fixture", "-c", "user.email=fixture@example.invalid", "commit", "-m", "Pinned baseline")
        self.sha = git(self.repo, "rev-parse", "HEAD")
        executable = [shutil.which("node"), str(REPOSITORY / "node_modules/vite/bin/vite.js")]
        command = subprocess.list2cmdline(executable) if os.name == "nt" else shlex.join(executable)
        command += " --host 127.0.0.1 --port {port} --strictPort"
        atomic_json(self.root / "session.json", {
            "repository": str(self.repo), "baselineRef": "main",
            "artifact": {"title": "Comparison fixture", "description": "Native lifecycle acceptance", "owner": {
                "hostId": "comparison-fixture-host", "terminalId": "comparison-fixture-terminal",
                "provider": "codex", "conversationId": "comparison-fixture-conversation",
            }},
            "baselineWorktree": str(self.root / "baseline"),
            "baseline": {"command": command, "readyTimeoutSeconds": 30},
            "working": {"command": command, "readyTimeoutSeconds": 30},
            "viewport": {"width": 390, "height": 844, "preset": "phone", "orientation": "portrait"},
        })

    def launch(self):
        env = dict(os.environ)
        for name in ("VIBESTUDIO_SERVER_TOKEN", "TMUX", "TMUX_PANE", "VIBESTUDIO_COMPARISON_URL"):
            env.pop(name, None)
        env.update({
            "XDG_CONFIG_HOME": str(self.root / "config"),
            "TMUX_TMPDIR": str(self.root / "tmux"),
            "VIBESTUDIO_COMPARISON_SMOKE_SCRIPT": str(self.root / "smoke.js"),
            "VIBESTUDIO_COMPARISON_SMOKE_WINDOW_STATE": str(self.root / "windows.json"),
            "VIBESTUDIO_COMPARISON_SMOKE_WINDOW_COMMAND": str(self.root / "window-command.json"),
        })
        command = ["cargo", "run", "--locked", "--manifest-path", str(REPOSITORY / "client/desktop/Cargo.toml"), "--example", "comparison_smoke", "--", str(self.root / "session.json")]
        if self.args.xvfb:
            env.update({"GDK_BACKEND": "x11", "WEBKIT_DISABLE_COMPOSITING_MODE": "1"})
            command = ["xvfb-run", "--auto-servernum", "--server-args=-screen 0 1800x1200x24", *command]
        self.log = self.log_path.open("w", encoding="utf-8")
        self.process = subprocess.Popen(
            command, cwd=REPOSITORY, env=env, stdout=self.log, stderr=subprocess.STDOUT,
            start_new_session=os.name != "nt",
        )

    def status(self):
        return request(self.base + "/api/comparison/status?" + urllib.parse.urlencode({"id": self.session_id}))

    def update(self, changes):
        return request(self.base + "/api/comparison/update", {"id": self.session_id, **changes})

    def alive(self):
        if self.process.poll() is not None:
            raise AssertionError(f"Native example exited with {self.process.returncode}; see {self.log_path}")

    def await_value(self, read, check, label, timeout=None):
        deadline = time.monotonic() + (timeout or self.args.timeout)
        last = None
        while time.monotonic() < deadline:
            self.alive()
            last = read()
            if check(last):
                self.passed.append(label)
                print(f"PASS {label}", flush=True)
                return last
            time.sleep(0.1)
        raise AssertionError(f"Timed out: {label}\nLast observation: {json.dumps(last, indent=2)}")

    def latest(self):
        rows = {}
        for side in ("baseline", "working"):
            path = self.root / f"{side}-reports.jsonl"
            if not path.exists():
                continue
            # Ignore a trailing partial write and keep the most recent complete
            # record from each pane. Fixture reports are deliberately small.
            for line in reversed(path.read_text(encoding="utf-8").splitlines()):
                try:
                    rows[side] = json.loads(line)
                    break
                except json.JSONDecodeError:
                    continue
        return rows

    def await_reports(self, check, label):
        def complete(rows):
            return all(side in rows for side in ("baseline", "working")) and check(rows)
        return self.await_value(self.latest, complete, label)

    def viewport(self, width, height, preset, orientation, tolerance=1, initial=False):
        if not initial:
            self.update({"viewport": {"width": width, "height": height, "preset": preset, "orientation": orientation}})
        def matching(rows):
            panes = list(rows.values())
            return all(abs(p["width"] - width) <= tolerance and abs(p["height"] - height) <= tolerance for p in panes) and abs(panes[0]["width"] - panes[1]["width"]) <= 1 and abs(panes[0]["height"] - panes[1]["height"]) <= 1
        return self.await_reports(matching, f"matching native CSS viewports: {preset} {width}×{height}")

    def control(self, side, target, fraction):
        identifier = uuid.uuid4().hex
        atomic_json(self.root / "control.json", {"id": identifier, "port": self.ports[side], "target": target, "fraction": fraction})
        return identifier

    @staticmethod
    def fraction(row, target):
        return row[target + "Y"] / max(row[target + "Range"], 1)

    def scroll(self, side, target, fraction):
        identifier = self.control(side, target, fraction)
        def synchronized(rows):
            source = self.fraction(rows[side], target)
            return rows[side]["handled"] == identifier and abs(source - fraction) < 0.003 and abs(self.fraction(rows["baseline"], target) - self.fraction(rows["working"], target)) < 0.003
        return self.await_reports(synchronized, f"proportional {target} scroll from {side}")

    def run(self):
        def listener():
            match = READY.search(self.log_path.read_text(encoding="utf-8", errors="replace"))
            return match.groups() if match else None
        self.base, self.session_id = self.await_value(listener, bool, "isolated native listener", self.args.startup_timeout)
        def ready(session):
            if session["state"] == "failed":
                raise AssertionError(session.get("error") or "Comparison startup failed")
            return session["state"] == "ready" and session.get("windowOpen") is True
        self.session = self.await_value(self.status, ready, "native window creation acknowledged")
        assert self.session["baselineSha"] == self.sha, self.session
        self.ports = {side: str(urllib.parse.urlparse(self.session[side + "Url"]).port) for side in ("baseline", "working")}
        self.viewport(390, 844, "phone", "portrait", initial=True)

        # Change content and scroll ranges through real Vite HMR. Neither pane is
        # reloaded by the runner, so the load count proves an accepted hot update.
        (self.repo / "app.js").write_text(self.working_edit, encoding="utf-8")
        self.await_reports(lambda rows: rows["baseline"]["text"] == "Baseline component" and rows["baseline"]["loads"] == 1 and rows["working"]["text"] == "Working uncommitted component" and rows["working"]["loads"] > 1 and rows["working"]["rootRange"] > rows["baseline"]["rootRange"] + 1800 and rows["working"]["nestedRange"] > rows["baseline"]["nestedRange"] + 900, "uncommitted HMR update with pinned baseline and unequal scroll ranges")
        for target in ("root", "nested"):
            self.scroll("baseline", target, 0.55)
            self.scroll("working", target, 0.18)

        self.continuous_scroll("baseline", "root")
        self.continuous_scroll("working", "nested")
        self.strict_csp_scroll()
        self.update({"syncScroll": False})
        # The HTTP update is asynchronous relative to the native main-thread
        # reconciliation tick. Let that tick run before producing the gesture.
        time.sleep(0.6)
        before = self.latest()["baseline"]["nestedY"]
        identifier = self.control("working", "nested", 0.8)
        rows = self.await_reports(lambda rows: rows["working"]["handled"] == identifier and abs(self.fraction(rows["working"], "nested") - 0.8) < 0.003, "scroll input applied while synchronization is disabled")
        deadline = time.monotonic() + 1.2
        while time.monotonic() < deadline:
            self.alive()
            rows = self.latest()
            assert abs(rows["baseline"]["nestedY"] - before) <= 2, rows
            time.sleep(0.1)
        self.passed.append("disabled synchronization leaves peer unchanged")
        print("PASS disabled synchronization leaves peer unchanged", flush=True)
        self.update({"syncScroll": True})
        self.viewport(844, 390, "phone", "landscape")
        self.viewport(375, 667, "iphone-se", "portrait")
        self.viewport(430, 932, "iphone-14-pro-max", "portrait")
        self.viewport(412, 915, "pixel-7", "portrait")
        self.viewport(768, 1024, "tablet", "portrait")
        self.viewport(1440, 900, "desktop", "landscape")
        self.viewport(527, 811, "custom", "portrait", tolerance=3)
        self.viewport(1280, 900, "custom", "landscape", tolerance=3)
        self.stable_window("1280×900 viewport")
        self.stable_window("shrunk native window", 960, 650)
        self.stable_window("expanded native window", 1360, 920)
        self.viewport(3840, 3840, "custom", "portrait", tolerance=4)
        self.stable_window("large custom viewport minimum")
        self.viewport(390, 844, "phone", "portrait")
        self.stable_window("window shrinks again after large viewport", 1000, 800)
        self.session = self.status()
        assert self.session["baselineSha"] == self.sha
        self.window_lifecycle()
        self.stop_and_verify()
        assert (self.repo / "app.js").read_text(encoding="utf-8") == self.working_edit
        assert git(self.repo, "diff", "--name-only") == "app.js"
        self.passed.append("working edit preserved after owned-resource cleanup")
        print("PASS working edit preserved after owned-resource cleanup", flush=True)

    def continuous_scroll(self, side, target, fast=True):
        identifier = uuid.uuid4().hex
        atomic_json(self.root / "control.json", {
            "id": identifier, "continuous": True, "port": self.ports[side], "target": target,
            "start": time.time() * 1000 + 700, "duration": 1800,
        })
        rows = self.await_reports(lambda rows: all(row.get("motion") and row["motion"]["id"] == identifier for row in rows.values()), f"continuous {target} scroll from {side}")
        peer = "working" if side == "baseline" else "baseline"
        source_stats, peer_stats = rows[side]["motion"], rows[peer]["motion"]
        assert source_stats["incoming"] == 0, f"Continuous scrolling echoed back: {source_stats}"
        assert peer_stats["samples"] >= 15, peer_stats
        if fast:
            assert peer_stats["incoming"] >= 35 and peer_stats["distinct"] >= 30, peer_stats
            assert peer_stats["p95Lag"] < 65, peer_stats
            label = f"frame-coalesced {target} relay without echo"
        else:
            assert 8 <= peer_stats["incoming"] <= 30, peer_stats
            label = "strict CSP navigation fallback stays bounded without echo"
        self.passed.append(label)
        print(f"PASS {label}: {json.dumps(peer_stats)}", flush=True)
        return rows

    def strict_csp_scroll(self):
        html = self.repo / "index.html"
        html.write_text(HTML.replace("<!doctype html>", '<!doctype html>\n<meta http-equiv="Content-Security-Policy" content="connect-src \'self\';">', 1), encoding="utf-8")
        self.update({"route": "/?strict-csp"})
        self.await_reports(lambda rows: rows["working"].get("strictCsp") is True, "strict connect-src self preview loaded")
        rows = self.continuous_scroll("working", "nested", fast=False)
        assert rows["working"]["cspBlocked"] == 1, "A blocked HTTP relay must degrade once, not retry each frame"
        html.write_text(HTML, encoding="utf-8")
        self.update({"route": "/"})
        self.await_reports(lambda rows: all(row.get("strictCsp") is False for row in rows.values()), "regular previews restored after CSP fallback")

    def native_windows(self):
        try:
            state = json.loads((self.root / "windows.json").read_text(encoding="utf-8"))
            return {window["id"]: window for window in state["windows"]}
        except (OSError, ValueError, KeyError):
            # The small telemetry record may be in the middle of a write.
            return {}

    def stable_window(self, label, width=None, height=None):
        if width is not None:
            atomic_json(self.root / "window-command.json", {
                "id": uuid.uuid4().hex, "action": "resize", "comparisonId": self.session_id,
                "width": width, "height": height,
            })
            # GTK reports client decoration extents in the configured size;
            # allow those fixed extents but require a requested shrink to work.
            self.await_value(self.native_windows, lambda windows: self.session_id in windows and windows[self.session_id]["width"] is not None and abs(windows[self.session_id]["width"] - width) < 120 and abs(windows[self.session_id]["height"] - height) < 120, f"requested size applied: {label}")
        time.sleep(0.5)
        samples = []
        deadline = time.monotonic() + 3
        while time.monotonic() < deadline:
            self.alive()
            window = self.native_windows().get(self.session_id)
            if window and window["visible"]:
                samples.append((window["width"], window["height"]))
            time.sleep(0.15)
        assert len(samples) >= 10, samples
        for dimension in (0, 1):
            values = [size[dimension] for size in samples]
            assert max(values) - min(values) <= 2, f"Window geometry keeps changing ({label}): {samples}"
        self.passed.append(f"native window geometry stabilizes: {label}")
        print(f"PASS native window geometry stabilizes: {label} ({samples[-1]})", flush=True)

    def window_lifecycle(self):
        original = self.status()
        request(self.base + "/api/comparison/close", {"id": self.session_id})
        self.await_value(self.native_windows, lambda windows: self.session_id in windows and not windows[self.session_id]["present"], "Close destroys the native viewer")
        closed = self.status()
        assert closed["state"] == "ready" and not closed["windowOpen"] and not closed["windowRequested"], closed
        assert Path(original["baselineWorktree"]).exists()
        assert all(not port_closed(original[side + "Url"]) for side in ("baseline", "working"))
        self.passed.append("Close preserves pinned checkout and running preview servers")
        print("PASS Close preserves pinned checkout and running preview servers", flush=True)

        opened_at = time.time() * 1000
        request(self.base + "/api/comparison/open", {"id": self.session_id})
        self.await_value(self.status, lambda session: session["windowOpen"], "reopen acknowledges a new native viewer")
        reopened = self.status()
        for key in ("baselineSha", "baselineWorktree", "baselineUrl", "workingUrl"):
            assert reopened[key] == original[key], (key, reopened, original)
        self.await_reports(lambda rows: all(row["time"] > opened_at for row in rows.values()) and rows["baseline"]["text"] == "Baseline component" and rows["working"]["text"] == "Working uncommitted component", "reopened viewer retains baseline and saved working edits")

        atomic_json(self.root / "window-command.json", {"id": uuid.uuid4().hex, "action": "close", "comparisonId": self.session_id})
        self.await_value(self.native_windows, lambda windows: self.session_id in windows and not windows[self.session_id]["present"], "OS window close removes only the viewer")
        assert self.status()["state"] == "ready"
        assert all(not port_closed(original[side + "Url"]) for side in ("baseline", "working"))
        request(self.base + "/api/comparison/open", {"id": self.session_id})
        self.await_value(self.status, lambda session: session["windowOpen"], "reopen after OS close")

        # A second artifact shares the same owner but borrows fixture servers.
        # This verifies selection/focus without starting redundant dev processes.
        second = request(self.base + "/api/comparison/start", {
            "repository": str(self.repo), "artifact": {
                "title": "Second fixture diff", "owner": original["config"]["artifact"]["owner"],
            },
            "baseline": {"url": original["baselineUrl"]},
            "working": {"url": original["workingUrl"]},
            "viewport": original["config"]["viewport"],
        })
        second_id = second["id"]
        self.other_ids.append(second_id)
        self.await_value(self.native_windows, lambda windows: windows.get(second_id, {}).get("focused") and windows[second_id]["children"] == 3, "a second associated diff opens with its own native children")
        artifacts = request(self.base + "/api/comparison/list")
        assert {item["id"] for item in artifacts if item["config"].get("artifact", {}).get("owner") == original["config"]["artifact"]["owner"]} == {self.session_id, second_id}
        revision = self.status()["presentationRevision"]
        request(self.base + "/api/comparison/open", {"id": self.session_id})
        self.await_value(self.native_windows, lambda windows: windows.get(self.session_id, {}).get("focused") and not windows.get(second_id, {}).get("focused") and windows[self.session_id]["children"] == 3, "Open focuses an existing diff without duplicate children")
        assert self.status()["presentationRevision"] > revision
        # Close/Open can arrive before the native reconciliation tick. The
        # retained native viewer must receive a fresh presentation receipt.
        request(self.base + "/api/comparison/close", {"id": self.session_id})
        request(self.base + "/api/comparison/open", {"id": self.session_id})
        self.await_value(self.status, lambda session: session["windowOpen"], "immediate Close/Open restores presentation acknowledgement")
        atomic_json(self.root / "window-command.json", {"id": uuid.uuid4().hex, "action": "switch", "comparisonId": self.session_id, "targetId": second_id})
        self.await_value(self.native_windows, lambda windows: windows.get(second_id, {}).get("focused") and self.session_id in windows and not windows[self.session_id]["present"], "toolbar selector opens the chosen diff and closes the previous viewer")
        assert self.status()["state"] == "ready"
        request(self.base + "/api/comparison/stop", {"id": second_id})

    def stop_and_verify(self):
        request(self.base + "/api/comparison/stop", {"id": self.session_id})
        baseline = Path(self.session["baselineWorktree"])
        urls = [self.session[side + "Url"] for side in ("baseline", "working")]
        deadline = time.monotonic() + self.args.timeout
        last = None
        while time.monotonic() < deadline:
            try:
                last = self.status()
                if last["state"] == "failed":
                    raise AssertionError(last.get("error") or "Comparison cleanup failed")
            except (OSError, urllib.error.URLError):
                # The native example may exit when its final window is removed.
                # Resource disappearance below must still prove cleanup finished.
                pass
            if not baseline.exists() and all(port_closed(url) for url in urls):
                self.passed.append("Stop removes the baseline worktree and closes both owned ports")
                print("PASS Stop removes the baseline worktree and closes both owned ports", flush=True)
                return
            time.sleep(0.1)
        raise AssertionError(f"Stop did not clean up owned resources: {last}")

    def cleanup(self):
        if self.base:
            for identifier in [self.session_id, *self.other_ids]:
                if identifier:
                    try:
                        request(self.base + "/api/comparison/stop", {"id": identifier})
                    except (OSError, ValueError):
                        pass
        # These token-checked endpoints exist only in our two Vite fixtures.
        # They also cover failures before the native example prints readiness.
        for side in ("baseline", "working"):
            try:
                record = json.loads((self.root / f"{side}-server.json").read_text())
                request(f"http://127.0.0.1:{record['port']}/__shutdown?token={record['token']}", {})
            except (OSError, ValueError, KeyError):
                pass
        if self.process:
            try:
                self.process.wait(timeout=6)
            except subprocess.TimeoutExpired:
                if os.name == "nt":
                    subprocess.run(["taskkill", "/PID", str(self.process.pid), "/T", "/F"], capture_output=True, timeout=10, check=False)
                else:
                    try:
                        os.killpg(self.process.pid, signal.SIGTERM)
                    except ProcessLookupError:
                        pass
                try:
                    self.process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    if os.name == "nt":
                        self.process.kill()
                    else:
                        try:
                            os.killpg(self.process.pid, signal.SIGKILL)
                        except ProcessLookupError:
                            pass
                    self.process.wait(timeout=5)
        if self.log:
            self.log.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--xvfb", action="store_true", help="Run with an isolated Xvfb display on Linux")
    parser.add_argument("--keep", action="store_true", help="Keep fixture files even when every check passes")
    parser.add_argument("--timeout", type=float, default=20, help="Seconds per assertion (default: 20)")
    parser.add_argument("--startup-timeout", type=float, default=600, help="Seconds for cargo build and native startup (default: 600)")
    args = parser.parse_args()
    if args.timeout <= 0 or args.startup_timeout <= 0:
        parser.error("Timeouts must be positive")
    for executable in ("git", "node", "cargo"):
        if not shutil.which(executable):
            parser.error(f"Required executable is missing: {executable}")
    if args.xvfb and (sys.platform != "linux" or not shutil.which("xvfb-run")):
        parser.error("--xvfb requires Linux and xvfb-run")
    if not (REPOSITORY / "node_modules/vite/bin/vite.js").is_file():
        parser.error("Install repository npm dependencies before running this check")
    if not (REPOSITORY / "dist/index.html").is_file():
        parser.error("Run npm run build before running this check")
    smoke = Smoke(args)
    success = False
    print(f"Native comparison fixture: {smoke.root}", flush=True)
    try:
        smoke.setup()
        smoke.launch()
        smoke.run()
        success = True
        print(json.dumps({"result": "passed", "checks": smoke.passed}, indent=2), flush=True)
        return 0
    except (Exception, KeyboardInterrupt) as error:
        print(f"FAIL {error}\nFixture and logs retained at {smoke.root}", file=sys.stderr, flush=True)
        return 1
    finally:
        smoke.cleanup()
        if success and not args.keep:
            shutil.rmtree(smoke.root)


if __name__ == "__main__":
    sys.exit(main())
