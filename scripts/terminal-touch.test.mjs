import assert from "node:assert/strict";
import test from "node:test";
import { loadWebModule } from "./test-helpers.mjs";

class Target {
  listeners = new Map();
  addEventListener(type, callback, options) {
    const capture = typeof options === "boolean" ? options : !!options?.capture;
    const listeners = this.listeners.get(type) ?? [];
    listeners.push({ callback, capture });
    this.listeners.set(type, listeners);
  }
  removeEventListener(type, callback, options) {
    const capture = typeof options === "boolean" ? options : !!options?.capture;
    this.listeners.set(type, (this.listeners.get(type) ?? []).filter((listener) =>
      listener.callback !== callback || listener.capture !== capture));
  }
  emit(type, fields = {}) {
    const event = {
      type, defaultPrevented: false, stopped: false, ...fields,
      preventDefault() { this.defaultPrevented = true; },
      stopImmediatePropagation() { this.stopped = true; },
    };
    const listeners = [...(this.listeners.get(type) ?? [])].sort((a, b) => Number(b.capture) - Number(a.capture));
    for (const { callback } of listeners) {
      callback(event);
      if (event.stopped) break;
    }
    return event;
  }
  get listenerCount() {
    return [...this.listeners.values()].reduce((count, listeners) => count + listeners.length, 0);
  }
}

const point = (clientX = 100, clientY = 100) => ({ clientX, clientY });

function fixture({ coarse = true } = {}) {
  const host = new Target(), window = new Target(), document = new Target();
  const timers = new Map(), frames = new Map(), calls = [];
  let now = 0, nextId = 0, active = true;
  document.hidden = false;
  window.matchMedia = () => ({ matches: coarse });
  const { attachTerminalTouch } = loadWebModule("lib/terminalTouch.ts", {
    window, document, performance: { now: () => now },
    setTimeout(callback, ms) { const id = ++nextId; timers.set(id, { callback, at: now + ms }); return id; },
    clearTimeout(id) { timers.delete(id); },
    requestAnimationFrame(callback) { const id = ++nextId; frames.set(id, callback); return id; },
    cancelAnimationFrame(id) { frames.delete(id); },
  });
  const record = (name) => (...args) => calls.push([name, ...structuredClone(args)]);
  const controller = attachTerminalTouch(host, {
    active: () => active,
    ...Object.fromEntries(["tap", "scroll", "startSelection", "moveSelection", "endSelection", "showMenu", "dismissSelection"]
      .map((name) => [name, record(name)])),
  });
  const touch = (type, touches, changedTouches = touches) => host.emit(type, { touches, changedTouches });
  return {
    host, window, document, controller, calls, frames, timers,
    setActive(value) { active = value; },
    named: (name) => calls.filter(([type]) => type === name),
    start: (p = point()) => touch("touchstart", [p]),
    move: (p) => touch("touchmove", [p]),
    end: (p = point()) => touch("touchend", [], [p]),
    cancel: () => touch("touchcancel", [], [point()]),
    secondFinger: () => touch("touchstart", [point(), point(200, 200)]),
    advance(ms) {
      const end = now + ms;
      while (true) {
        const next = [...timers.entries()].filter(([, timer]) => timer.at <= end).sort((a, b) => a[1].at - b[1].at)[0];
        if (!next) break;
        now = next[1].at;
        timers.delete(next[0]);
        next[1].callback();
      }
      now = end;
    },
    frame(ms = 16) {
      now += ms;
      const pending = [...frames.values()];
      frames.clear();
      for (const callback of pending) callback(now);
    },
  };
}

test("a short tap forwards one terminal click and suppresses the browser compatibility click", () => {
  const h = fixture();
  h.start(); h.advance(100);
  const release = h.end(point(102, 102));
  h.advance(1_000);
  assert.deepEqual(h.calls, [["tap", point(102, 102)]]);
  assert.equal(release.defaultPrevented, true);
  assert.equal(h.timers.size, 0);
});

test("holding selects at the finger; dragging extends it, and release offers a menu without a terminal click", () => {
  const h = fixture();
  h.start(); h.advance(600);
  assert.deepEqual(h.calls, [["startSelection", point()]]);
  assert.equal(h.move(point(150, 125)).defaultPrevented, true);
  const release = h.end(point(160, 130));
  assert.deepEqual(h.calls, [
    ["startSelection", point()], ["moveSelection", point(150, 125)],
    ["endSelection", point(160, 130)], ["showMenu", point(160, 130)],
  ]);
  assert.equal(release.defaultPrevented, true);
  assert.equal(h.frames.size, 0);
});

test("small finger jitter still permits a hold at the latest position", () => {
  const h = fixture();
  h.start(); h.advance(100); h.move(point(103, 102)); h.advance(500);
  h.end(point(103, 102));
  assert.deepEqual(h.named("startSelection"), [["startSelection", point(103, 102)]]);
  assert.equal(h.named("tap").length, 0);
});

test("horizontal movement cancels the hold and never scrolls or clicks", () => {
  const h = fixture();
  h.start(); h.advance(20);
  assert.equal(h.move(point(140, 102)).defaultPrevented, false);
  h.advance(600); h.move(point(140, 180)); h.end(point(140, 180));
  assert.deepEqual(h.calls, []);
  assert.equal(h.frames.size, 0);
});

test("vertical movement cancels the hold and turns accumulated travel into terminal wheel ticks", () => {
  const h = fixture();
  h.start(); h.advance(100);
  assert.equal(h.move(point(100, 120)).defaultPrevented, true);
  h.advance(100); h.move(point(100, 130));
  h.advance(100); h.move(point(100, 170));
  h.advance(100); h.end(point(100, 170));
  assert.deepEqual(h.named("scroll"), [
    ["scroll", -1, point(100, 130)], ["scroll", -1, point(100, 170)],
  ]);
  assert.equal(h.named("startSelection").length, 0);
  assert.equal(h.named("tap").length, 0);
  assert.equal(h.frames.size, 0, "pausing before release prevents momentum");
});

test("the first tap after selection dismisses it without activating a link or terminal menu item", () => {
  const h = fixture();
  h.start(); h.advance(600); h.end();
  h.start(point(200, 200)); h.advance(100);
  assert.equal(h.end(point(200, 200)).defaultPrevented, true);
  assert.equal(h.named("dismissSelection").length, 1);
  assert.equal(h.named("tap").length, 0);
  h.start(point(200, 200)); h.advance(100); h.end(point(200, 200));
  assert.deepEqual(h.named("tap"), [["tap", point(200, 200)]]);
});

const interruptions = {
  "second finger": (h) => h.secondFinger(),
  touchcancel: (h) => h.cancel(),
  reset: (h) => h.controller.reset(),
  blur: (h) => h.window.emit("blur"),
  hidden: (h) => { h.document.hidden = true; h.document.emit("visibilitychange"); },
  dispose: (h) => h.controller.dispose(),
};

for (const [name, interrupt] of Object.entries(interruptions)) {
  test(`${name} cancels a pending hold without selecting, showing a menu, or clicking`, () => {
    const h = fixture();
    h.start(); h.advance(100); interrupt(h); h.advance(600); h.end();
    assert.equal(h.named("startSelection").length, 0);
    assert.equal(h.named("endSelection").length, 0);
    assert.equal(h.named("showMenu").length, 0);
    assert.equal(h.named("tap").length, 0);
    assert.equal(h.timers.size, 0);
    assert.equal(h.frames.size, 0);
  });

  test(`${name} ends a selection exactly once without opening the menu or clicking`, () => {
    const h = fixture();
    h.start(); h.advance(600); h.move(point(180, 130)); interrupt(h); h.end();
    h.controller.reset();
    assert.equal(h.named("startSelection").length, 1);
    assert.deepEqual(h.named("endSelection"), [["endSelection", point(180, 130)]]);
    assert.equal(h.named("showMenu").length, 0);
    assert.equal(h.named("tap").length, 0);
    assert.equal(h.timers.size, 0);
    assert.equal(h.frames.size, 0);
  });
}

test("disposal removes host, window, and document listeners", () => {
  const h = fixture();
  h.controller.dispose();
  assert.equal(h.host.listenerCount + h.window.listenerCount + h.document.listenerCount, 0);
  h.start(); h.advance(600); h.end();
  assert.equal(h.named("startSelection").length, 0);
  assert.equal(h.named("tap").length, 0);
});

test("a touch context menu is intercepted before xterm while a desktop right click keeps its normal behavior", () => {
  for (const coarse of [true, false]) {
    const h = fixture({ coarse });
    let nativeMenus = 0;
    h.host.addEventListener("contextmenu", () => nativeMenus++);
    const idleMenu = h.host.emit("contextmenu");
    assert.equal(idleMenu.defaultPrevented, coarse);
    assert.equal(nativeMenus, coarse ? 0 : 1);
    h.start();
    const touchMenu = h.host.emit("contextmenu");
    assert.equal(touchMenu.defaultPrevented, true);
    assert.equal(touchMenu.stopped, true);
    h.advance(600); h.end();
    assert.equal(h.host.emit("contextmenu").defaultPrevented, true);
    h.controller.reset();
    assert.equal(h.host.emit("contextmenu").defaultPrevented, coarse);
  }
});

function flick(h) {
  h.start(); h.advance(10); h.move(point(100, 200));
}

test("a fast vertical lift coasts, and a new touch stops that momentum", () => {
  const h = fixture();
  flick(h); h.end(point(100, 200));
  const initialScrolls = h.named("scroll").length;
  assert.equal(h.frames.size, 1);
  h.frame();
  assert.ok(h.named("scroll").length > initialScrolls);
  h.start();
  assert.equal(h.frames.size, 0);
  const stoppedScrolls = h.named("scroll").length;
  h.frame();
  assert.equal(h.named("scroll").length, stoppedScrolls);
  h.cancel();
});

test("cancellation and loss of activity never turn a flick into momentum", () => {
  for (const finish of [(h) => h.cancel(), (h) => { h.setActive(false); h.end(); }]) {
    const h = fixture();
    flick(h); finish(h);
    assert.equal(h.frames.size, 0);
    assert.equal(h.named("tap").length, 0);
  }
  const h = fixture();
  flick(h); h.end(point(100, 200)); h.setActive(false);
  const count = h.named("scroll").length;
  h.frame();
  assert.equal(h.frames.size, 0);
  assert.equal(h.named("scroll").length, count);
});

test("an inactive terminal cannot start a tap or a hold", () => {
  const h = fixture();
  h.setActive(false); h.start(); h.advance(600); h.end();
  assert.equal(h.named("tap").length, 0);
  assert.equal(h.named("startSelection").length, 0);
});
