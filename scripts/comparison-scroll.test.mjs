import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const source = readFileSync(new URL("../client/desktop/src/comparison-scroll.js", import.meta.url), "utf8");

function fixture() {
  let now = 0;
  let nextTimer = 1;
  const tasks = [];
  function schedule(callback, delay = 0) {
    const id = nextTimer++;
    tasks.push({ id, callback, at: now + delay });
    return id;
  }
  function cancel(id) {
    const index = tasks.findIndex(task => task.id === id);
    if (index >= 0) tasks.splice(index, 1);
  }
  async function advanceAsync(milliseconds) {
    for (let step = 0; step < milliseconds; step++) {
      advance(1);
      for (let job = 0; job < 5; job++) await Promise.resolve();
    }
  }
  function advance(milliseconds) {
    const end = now + milliseconds;
    for (let count = 0; count < 1000; count++) {
      tasks.sort((a, b) => a.at - b.at || a.id - b.id);
      if (!tasks.length || tasks[0].at > end) {
        now = end;
        return;
      }
      const task = tasks.shift();
      now = task.at;
      task.callback();
    }
    assert.fail("scroll relay did not settle");
  }
  function pane({ rootRange = 800, nestedRange = 600, quantum = 1, eventDelay = 1, relay = false, requestDelay = 5, reject = false } = {}) {
    const events = {};
    const sent = [];
    class Element {
      constructor(id, range) {
        Object.assign(this, { id, scrollHeight: range + 100, clientHeight: 100,
          scrollWidth: 100, clientWidth: 100, scrollTop: 0, scrollLeft: 0,
          children: [], isConnected: true });
      }
      getAttribute(name) {
        return name === "data-comparison-scroll-key" && this.id === "nested" ? "content" : null;
      }
      scrollTo({ left = 0, top = 0 }) {
        // Native page zoom can round by several CSS pixels. The observable
        // position is available synchronously, while scroll events arrive later.
        const x = Math.floor(left / quantum) * quantum;
        const y = Math.floor(top / quantum) * quantum;
        if (this.scrollLeft === x && this.scrollTop === y) return;
        this.scrollLeft = x;
        this.scrollTop = y;
        schedule(() => events.scroll({ target: this }), eventDelay);
      }
    }
    const root = new Element("root", rootRange);
    const body = new Element("body", 0);
    const nested = new Element("nested", nestedRange);
    root.children = [body];
    body.parentElement = root;
    body.children = [nested];
    nested.parentElement = body;
    const document = {
      scrollingElement: root, documentElement: root,
      addEventListener: (name, callback) => { events[name] = callback; },
      querySelectorAll: () => { result.scans++; return [root, body, nested]; },
      querySelector: selector => selector.includes("content") ? nested : null,
      getElementById: id => [root, body, nested].find(element => element.id === id),
    };
    const window = {};
    window.top = window;
    const result = { root, nested, events, sent, window, Element, scans: 0, requests: 0, active: 0, maxActive: 0, times: [] };
    const location = {
      origin: "http://127.0.0.1:9999",
      replace(url) {
        const updates = JSON.parse(new URL(url).searchParams.get("payload"));
        sent.push(updates);
        result.times.push(now);
        result.peer?.window.__VIBESTUDIO_COMPARE_SCROLL__.receive(updates);
      },
    };
    vm.runInNewContext(`${source}(${JSON.stringify({
      origin: location.origin, channel: `${location.origin}/__scroll`, enabled: true,
      ...(relay ? { relayUrl: "http://127.0.0.1:8767/api/comparison/scroll/test" } : {}),
    })})`, {
      window, document, location, Element, CSS: { escape: value => value },
      performance: { now: () => now }, setTimeout: schedule, clearTimeout: cancel, AbortController,
      fetch: (_url, options) => new Promise((resolve, fail) => {
        result.requests++; result.active++; result.maxActive = Math.max(result.maxActive, result.active);
        const timer = schedule(() => {
          result.active--;
          if (reject) { fail(new Error("CSP blocked")); return; }
          const updates = JSON.parse(options.body);
          sent.push(updates); result.times.push(now);
          result.peer?.window.__VIBESTUDIO_COMPARE_SCROLL__.receive(updates);
          resolve({ ok: true });
        }, requestDelay);
        options.signal.addEventListener("abort", () => { cancel(timer); result.active--; fail(new Error("Timeout")); });
      }),
      requestAnimationFrame: callback => schedule(callback, 16),
      getComputedStyle: () => ({ overflow: "auto", overflowX: "auto", overflowY: "auto" }),
    });
    return result;
  }
  function pair(first = {}, second = {}) {
    const baseline = pane(first);
    const working = pane(second);
    baseline.peer = working;
    working.peer = baseline;
    return { baseline, working };
  }
  return { pair, advance, advanceAsync };
}

test("native zoom rounding does not echo or move the initiating scroll container", () => {
  const { pair, advance } = fixture();
  const { baseline, working } = pair({ nestedRange: 1980, quantum: 3 }, { nestedRange: 3179, quantum: 5 });
  baseline.nested.scrollTo({ top: 1089 });
  const original = baseline.nested.scrollTop;
  advance(2000);
  assert.equal(baseline.nested.scrollTop, original);
  assert.equal(baseline.sent.length, 1);
  assert.equal(working.sent.length, 0, "rounded programmatic position must never echo");
  assert.ok(Math.abs(working.nested.scrollTop / 3179 - original / 1980) < 0.003);
});

test("delayed scroll events remain suppressed without blocking a later programmatic scroll", () => {
  const { pair, advance } = fixture();
  const { baseline, working } = pair({ eventDelay: 400 }, { nestedRange: 1400, eventDelay: 400 });
  baseline.nested.scrollTo({ top: 300 });
  advance(2000);
  assert.equal(working.nested.scrollTop, 700);
  assert.equal(working.sent.length, 0, "events later than 250ms must not echo");
  working.nested.scrollTo({ top: 1050 });
  advance(2000);
  assert.equal(baseline.nested.scrollTop, 450);
  assert.equal(working.sent.length, 1, "a distinct programmatic position still synchronizes");
  assert.equal(baseline.sent.length, 1);
});

test("root scrolling coalesces, real input changes direction, and disabled sync stays still", () => {
  const { pair, advance } = fixture();
  const { baseline, working } = pair({}, { rootRange: 1800 });
  baseline.root.scrollTo({ top: 200 });
  baseline.root.scrollTo({ top: 400 });
  advance(200);
  assert.equal(baseline.sent.length, 1);
  assert.equal(working.root.scrollTop, 900);
  working.events.wheel();
  working.root.scrollTo({ top: 1350 });
  advance(200);
  assert.equal(baseline.root.scrollTop, 600);
  baseline.window.__VIBESTUDIO_COMPARE_SCROLL__.setEnabled(false);
  baseline.root.scrollTo({ top: 0 });
  advance(200);
  assert.equal(working.root.scrollTop, 1350);
  assert.equal(baseline.sent.length, 1);
});


test("HTTP scroll coalesces per frame with one in-flight request and no echoed updates", async () => {
  const { pair, advanceAsync } = fixture();
  const { baseline, working } = pair({ relay: true }, { relay: true, rootRange: 1800 });
  for (let frame = 0; frame < 30; frame++) {
    baseline.root.scrollTo({ top: 100 + frame * 10 });
    await advanceAsync(16);
  }
  await advanceAsync(100);
  assert.ok(baseline.sent.length >= 20, "frequent input must not retain the old 80ms throttle");
  assert.equal(baseline.maxActive, 1);
  assert.equal(working.sent.length, 0);
  assert.equal(working.root.scrollTop, Math.floor(390 / 800 * 1800));
});

test("slow HTTP coalesces latest positions without an unbounded queue", async () => {
  const { pair, advanceAsync } = fixture();
  const { baseline, working } = pair({ relay: true, requestDelay: 100 }, { relay: true });
  for (let frame = 0; frame < 20; frame++) {
    baseline.root.scrollTo({ top: frame * 20 });
    await advanceAsync(16);
  }
  await advanceAsync(250);
  assert.equal(baseline.maxActive, 1);
  assert.ok(baseline.sent.length <= 5);
  assert.equal(working.root.scrollTop, 380, "final input is retained, not an older in-flight position");
});

test("CSP rejection degrades once to rate-limited navigation and still synchronizes", async () => {
  const { pair, advanceAsync } = fixture();
  const { baseline, working } = pair({ relay: true, reject: true }, {});
  for (let frame = 0; frame < 40; frame++) {
    baseline.root.scrollTo({ top: frame * 10 });
    await advanceAsync(16);
  }
  await advanceAsync(200);
  assert.equal(baseline.requests, 1);
  assert.equal(working.root.scrollTop, 390);
  assert.equal(working.sent.length, 0);
  assert.ok(baseline.times.every((time, index, times) => !index || time - times[index - 1] >= 80));
});

test("disabled synchronization does not replay failed in-flight positions after reenable", async () => {
  const { pair, advanceAsync } = fixture();
  const { baseline, working } = pair({ relay: true, reject: true, requestDelay: 100 }, {});
  baseline.nested.scrollTo({ top: 300 });
  await advanceAsync(30);
  baseline.window.__VIBESTUDIO_COMPARE_SCROLL__.setEnabled(false);
  working.window.__VIBESTUDIO_COMPARE_SCROLL__.setEnabled(false);
  await advanceAsync(200);
  baseline.window.__VIBESTUDIO_COMPARE_SCROLL__.setEnabled(true);
  working.window.__VIBESTUDIO_COMPARE_SCROLL__.setEnabled(true);
  baseline.root.scrollTo({ top: 200 });
  await advanceAsync(200);
  assert.equal(working.root.scrollTop, 200);
  assert.equal(working.nested.scrollTop, 0);
  assert.equal(baseline.sent.flat().some(packet => !packet.root), false);
});

test("pending burst sends only eight newest containers and reuses the ordinal lookup", async () => {
  const { pair, advanceAsync } = fixture();
  const { baseline } = pair({ relay: true }, {});
  for (let index = 0; index < 12; index++) {
    const el = new baseline.Element(`extra-${index}`, 500);
    el.scrollTo({ top: 100 });
  }
  await advanceAsync(50);
  assert.deepEqual(baseline.sent[0].map(packet => packet.id), Array.from({ length: 8 }, (_, index) => `extra-${index + 4}`));
  baseline.scans = 0;
  for (let index = 0; index < 10; index++) {
    baseline.nested.scrollTo({ top: index * 10 });
    await advanceAsync(20);
  }
  assert.ok(baseline.scans <= 1, "stable container index lookup must not scan every frame");
});

test("a stalled HTTP request times out once and flushes the latest position through fallback", async () => {
  const { pair, advanceAsync } = fixture();
  const { baseline, working } = pair({ relay: true, requestDelay: 1000 }, {});
  baseline.root.scrollTo({ top: 200 });
  await advanceAsync(50);
  baseline.root.scrollTo({ top: 600 });
  await advanceAsync(450);
  assert.equal(baseline.requests, 1);
  assert.equal(baseline.active, 0);
  assert.equal(working.root.scrollTop, 600);
  assert.equal(working.sent.length, 0);
});
