import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

const tick = () => new Promise((resolve) => setImmediate(resolve));
function load(name, globals = {}, dependencies = {}) {
  const source = readFileSync(new URL(`../client/web/lib/${name}.ts`, import.meta.url), "utf8");
  const { outputText } = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  });
  const exports = {};
  vm.runInNewContext(outputText, {
    exports, Date, ...globals,
    require: (id) => {
      if (id in dependencies) return dependencies[id];
      if (id === "react") return { useSyncExternalStore: (_subscribe, get) => get() };
      throw new Error(`Unexpected import ${id}`);
    },
  }, { filename: `${name}.ts` });
  return exports;
}
const attention = load("sessionAttention");
const state = (counter, status, kind = null, boot = "boot") => ({
  sequence: `${boot}:${counter}`, state: status, kind, changedAt: counter * 1000,
});
const session = (id, a, created = "1") => ({
  id, label: id, agent: "claude", cwd: "/work", created, activity: "0", bellAt: "0", attention: a,
});

function harness(initial) {
  let list = initial;
  let focused = true;
  let hidden = false;
  let events;
  let nextList;
  const sounds = [], notices = [], timers = new Map(), storage = new Map();
  let timerId = 0;
  const api = {
    terminalList: () => {
      const next = nextList;
      nextList = undefined;
      return next ?? Promise.resolve(list);
    },
    terminalEvents: (event, down, open) => { events = { event, down, open }; return { close() {} }; },
    notifyStatus: async () => ({ native: true }),
    notifyBadge: async () => {},
    notifyNative: async (title, body) => { notices.push({ title, body }); },
  };
  const store = load("sessions", {
    localStorage: { getItem: (k) => storage.get(k) ?? null, setItem: (k, v) => storage.set(k, v) },
    window: { addEventListener() {} },
    document: { get hidden() { return hidden; }, hasFocus: () => focused, addEventListener() {} },
    setTimeout: (fn, delay) => { const id = ++timerId; timers.set(id, { fn, delay }); return id; },
    clearTimeout: (id) => timers.delete(id),
  }, {
    "@/lib/api": api,
    "@/lib/log": { log: { warn() {} } },
    "@/lib/push": { canPush: () => false },
    "@/lib/routes": { sessionsPath: (id) => `/sessions?id=${id}` },
    "./sessionAttention": attention,
    "./attentionSound": {
      playAttentionSound: async (kind, current) => { if (current()) sounds.push(kind); },
      unlockAttentionSound() {},
    },
  });
  return {
    store, sounds, notices,
    setList: (next) => { list = next; },
    deferList: (promise) => { nextList = promise; },
    focus: (value) => { focused = value; },
    hide: (value) => { hidden = value; },
    connect: () => events.open(),
    event(kind, s) {
      events.event(kind, { id: s.id, label: s.label, agent: s.agent, cwd: s.cwd, at: s.bellAt, attention: s.attention });
    },
    async settle() {
      await tick();
      for (const [id, timer] of [...timers]) {
        if (timer.delay === 120) { timers.delete(id); timer.fn(); }
      }
      await tick();
    },
  };
}

test("blocked sessions lead the rail, preserving manual order within priority", () => {
  const list = [session("a", state(1, "working")), session("b", state(2, "blocked", "request")), session("c", state(3, "blocked", "request"))];
  assert.deepEqual([...attention.orderSessions(list, ["a", "c", "b"])].map((s) => s.id), ["c", "b", "a"]);
  assert.equal(attention.attentionLabel(list[1]), "Needs input");
  assert.equal(attention.newerAttention(state(10, "working"), state(9, "blocked")), true);
  assert.equal(attention.newerAttention(state(9, "blocked"), state(10, "working")), false);
});

test("initial blocked state is visible and prioritized without announcing", async () => {
  const h = harness([session("a", state(1, "working")), session("b", state(2, "blocked", "request"))]);
  h.focus(false);
  h.connect();
  await h.settle();
  assert.equal(h.store.useSessions().sessions[0].id, "b");
  assert.equal(h.store.useSessions().unreadCount, 0);
  assert.deepEqual(h.sounds, []);
  assert.deepEqual(h.notices, []);
});

test("a locally created agent's first input request announces before its first detected list", async () => {
  const h = harness([]);
  h.connect(); await h.settle();
  h.store.noteCreated(session("new", undefined));
  const blocked = session("new", state(1, "blocked", "request"));
  h.setList([blocked]); h.event("attention", blocked);
  await h.settle();
  assert.deepEqual(h.sounds, ["request"]);
});

test("request dings even while watched; SSE, list, and bell cannot duplicate it", async () => {
  const h = harness([session("a", state(1, "working"))]);
  h.connect();
  await h.settle();
  h.store.setWatched("a");
  const blocked = session("a", state(2, "blocked", "request"));
  h.setList([blocked]);
  h.event("attention", blocked);
  h.event("attention", blocked);
  h.event("bell", { ...blocked, attention: undefined, bellAt: String(Date.now() / 1000 + 1) });
  await h.settle();
  assert.deepEqual(h.sounds, ["request"]);
  assert.deepEqual(h.notices, []);
  assert.equal(h.store.useSessions().unreadCount, 0);
});

test("done is silent when watched, audible in another session, and requests become unread", async () => {
  const h = harness([session("a", state(1, "working"))]);
  h.connect();
  await h.settle();
  h.store.setWatched("a");
  let next = session("a", state(2, "idle", "done"));
  h.setList([next]); h.event("attention", next);
  await h.settle();
  assert.deepEqual(h.sounds, []);
  h.store.releaseWatched("a");
  next = session("a", state(3, "working"));
  h.setList([next]); h.event("attention", next);
  await h.settle();
  next = session("a", state(4, "blocked", "request"));
  h.setList([next]); h.event("attention", next);
  await h.settle();
  assert.equal(h.store.useSessions().unreadCount, 1);
  h.store.markSeen("a");
  assert.equal(h.store.useSessions().unreadCount, 0);
  next = session("a", state(5, "idle", "done"));
  h.setList([next]); h.event("attention", next);
  await h.settle();
  assert.deepEqual(h.sounds, ["request", "done"]);
});

test("reconnect and server restart establish silent baselines", async () => {
  const h = harness([session("a", state(1, "working"))]);
  h.focus(false); h.connect(); await h.settle();
  const blocked = session("a", state(2, "blocked", "request"));
  h.setList([blocked]); h.connect(); await h.settle();
  h.event("attention", blocked); await h.settle();
  const restarted = session("a", state(1, "blocked", "request", "newboot"));
  h.setList([restarted]); await h.store.refresh(); await h.settle();
  assert.deepEqual(h.sounds, []);
  assert.deepEqual(h.notices, []);
});

test("a superseded request cancels its sound and toast", async () => {
  const h = harness([session("a", state(1, "working"))]);
  h.focus(false); h.connect(); await h.settle();
  const blocked = session("a", state(2, "blocked", "request"));
  h.setList([blocked]); h.event("attention", blocked);
  const working = session("a", state(3, "working"));
  h.setList([working]); h.event("attention", working);
  await h.settle();
  assert.deepEqual(h.sounds, []);
  assert.deepEqual(h.notices, []);
  assert.equal(h.store.useSessions().unreadCount, 0);
});

test("closing a session cancels its pending attention alert", async () => {
  const h = harness([session("a", state(1, "working"))]);
  h.focus(false); h.connect(); await h.settle();
  const blocked = session("a", state(2, "blocked", "request"));
  h.setList([blocked]); h.event("attention", blocked);
  h.setList([]); h.event("closed", blocked);
  await h.settle();
  assert.deepEqual(h.sounds, []);
  assert.deepEqual(h.notices, []);
});

test("a late list response cannot overwrite a newer attention event", async () => {
  const initial = session("a", state(8, "working"));
  const h = harness([initial]);
  h.connect(); await h.settle();
  let resolve;
  h.deferList(new Promise((r) => { resolve = r; }));
  const refresh = h.store.refresh();
  const blocked = session("a", state(9, "blocked", "request"));
  h.setList([blocked]); h.event("attention", blocked);
  resolve([initial]); await refresh; await h.settle();
  assert.equal(h.store.useSessions().sessions[0].attention.sequence, "boot:9");
  assert.deepEqual(h.sounds, ["request"]);
});

test("a pre-detection snapshot without attention cannot erase a live SSE state", async () => {
  const initial = session("a", undefined);
  const h = harness([initial]);
  h.connect(); await h.settle();
  let resolve;
  h.deferList(new Promise((r) => { resolve = r; }));
  const refresh = h.store.refresh();
  const blocked = session("a", state(1, "blocked", "request"));
  h.setList([blocked]); h.event("attention", blocked);
  resolve([initial]); await refresh; await h.settle();
  assert.equal(h.store.useSessions().sessions[0].attention.sequence, "boot:1");
  assert.deepEqual(h.sounds, []);
});

test("older servers retain bell notifications and one done sound", async () => {
  const initial = session("a", undefined);
  const h = harness([initial]);
  h.focus(false); h.connect(); await h.settle();
  const next = { ...initial, bellAt: String(Math.floor(Date.now() / 1000) + 1) };
  h.setList([next]); h.event("bell", next); h.event("bell", next);
  await h.settle();
  assert.deepEqual(h.sounds, ["done"]);
  assert.equal(h.notices.length, 1);
});

test("native sounds do not require a browser gesture and never also play browser audio", async () => {
  const calls = [];
  const sound = load("attentionSound", {
    localStorage: { getItem: () => null }, window: { addEventListener() {} },
    fetch: () => { throw new Error("Must not fetch fallback audio"); },
  }, { "./api": { notifySound: async (kind) => calls.push(kind) } });
  await sound.playAttentionSound("request", () => true);
  assert.deepEqual(calls, ["request"]);
});

test("browser fallback needs a gesture and cancels a stale sound during audio decoding", async () => {
  let plays = 0, resolveDecode;
  class AudioContext {
    state = "suspended";
    resume() { this.state = "running"; return Promise.resolve(); }
    decodeAudioData() { return new Promise((r) => { resolveDecode = r; }); }
    createBufferSource() { return { connect() {}, start() { plays++; } }; }
  }
  const sound = load("attentionSound", {
    AudioContext, localStorage: { getItem: () => null }, window: { addEventListener() {} },
    fetch: async () => ({ ok: true, arrayBuffer: async () => new ArrayBuffer(0) }),
  }, { "./api": { notifySound: async () => { throw Object.assign(new Error("Unavailable"), { status: 404 }); } } });
  await sound.playAttentionSound("request", () => true);
  assert.equal(resolveDecode, undefined); // no gesture: drop it rather than queue it
  sound.unlockAttentionSound();
  let current = true;
  const pending = sound.playAttentionSound("request", () => current);
  await tick();
  current = false;
  resolveDecode({}); await pending;
  assert.equal(plays, 0);
  await sound.playAttentionSound("request", () => true);
  assert.equal(plays, 1);
});

test("muting suppresses native playback and persists the preference", async () => {
  const values = new Map(), calls = [];
  const sound = load("attentionSound", {
    localStorage: { getItem: (k) => values.get(k), setItem: (k, v) => values.set(k, v) },
    window: { addEventListener() {} },
  }, { "./api": { notifySound: async (kind) => calls.push(kind) } });
  assert.equal(sound.useAttentionSound(), true);
  sound.setAttentionSoundEnabled(false);
  await sound.playAttentionSound("request", () => true);
  assert.equal(sound.useAttentionSound(), false);
  assert.equal(values.get("vibestudio-attention-sound"), "off");
  assert.deepEqual(calls, []);
});
