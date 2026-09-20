import assert from "node:assert/strict";
import test from "node:test";
import { loadWebModule } from "./test-helpers.mjs";

const tick = () => new Promise((resolve) => setImmediate(resolve));
function load(name, globals = {}, dependencies = {}) {
  return loadWebModule(`lib/${name}.ts`, {
    Date, ...globals,
    require: (id) => {
      if (id in dependencies) return dependencies[id];
      if (id === "react") return { useSyncExternalStore: (_subscribe, get) => get() };
      throw new Error(`Unexpected import ${id}`);
    },
  });
}
const attention = load("sessionAttention");
const state = (counter, status, kind = null, boot = "boot") => ({
  sequence: `${boot}:${counter}`, state: status, kind, changedAt: counter * 1000,
});
const session = (id, a, created = "1") => ({
  id, label: id, agent: "claude", cwd: "/work", created, activity: "0", bellAt: "0", attention: a,
});

function harness(initial, { notifyWhileVisible = false, pushGranted = false, notifyStatus } = {}) {
  const workspace = load("workspaceConnection", { window: { dispatchEvent() {} }, Event: class {} });
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
    terminalEvents: (event, down, open, gap) => { events = { event, down, open, gap }; return { close() {} }; },
    notifyStatus: notifyStatus ?? (async () => ({ native: true, notifyWhileVisible })),
    notifyBadge: async () => {},
    notifyNative: async (title, body) => { notices.push({ title, body }); },
  };
  const store = load("sessions", {
    localStorage: { getItem: (k) => storage.get(k) ?? null, setItem: (k, v) => storage.set(k, v) },
    window: { addEventListener() {} },
    document: { get hidden() { return hidden; }, hasFocus: () => focused, addEventListener() {} },
    Notification: { permission: pushGranted ? "granted" : "default" },
    setTimeout: (fn, delay) => { const id = ++timerId; timers.set(id, { fn, delay }); return id; },
    clearTimeout: (id) => timers.delete(id),
  }, {
    "@/lib/api": api,
    "@/lib/log": { log: { warn() {} } },
    "@/lib/push": { canPush: () => pushGranted },
    "@/lib/routes": { sessionsPath: (id) => `/sessions?id=${id}` },
    "./sessionAttention": attention,
    "./workspaceConnection": workspace,
    "./attentionSound": {
      playAttentionSound: async (kind, current) => { if (current()) sounds.push(kind); },
      unlockAttentionSound() {},
    },
  });
  return {
    store, sounds, notices,
    outage: () => workspace.setWorkspaceAvailable(false),
    restore: () => workspace.setWorkspaceAvailable(true),
    gap: () => events.gap(),
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

test("mobile shows a single banner on Home and for another session, but not the watched session", async () => {
  const h = harness([session("a", state(1, "working"))], { notifyWhileVisible: true });
  h.connect(); await h.settle();
  const done = session("a", state(2, "idle", "done"));
  h.setList([done]); h.event("attention", done); h.event("attention", done);
  await h.settle();
  assert.deepEqual(h.sounds, ["done"]);
  assert.equal(h.notices.length, 1);
  h.store.setWatched("another-session");
  const blocked = session("a", state(3, "blocked", "request"));
  h.setList([blocked]); h.event("attention", blocked);
  await h.settle();
  assert.equal(h.notices.length, 2);
  h.store.setWatched("a");
  const working = session("a", state(4, "working"));
  h.setList([working]); h.event("attention", working);
  const request = session("a", state(5, "blocked", "request"));
  h.setList([request]); h.event("attention", request);
  await h.settle();
  assert.deepEqual(h.sounds, ["done", "request", "request"]);
  assert.equal(h.notices.length, 2);
});

test("opening a mobile session before its alert settles cancels its done sound and banner", async () => {
  const h = harness([session("a", state(1, "working"))], { notifyWhileVisible: true });
  h.connect(); await h.settle();
  const done = session("a", state(2, "idle", "done"));
  h.setList([done]); h.event("attention", done);
  h.store.setWatched("a");
  await h.settle();
  assert.deepEqual(h.sounds, []);
  assert.deepEqual(h.notices, []);
});

test("mobile foreground notification capability recovers after a failed startup probe", async () => {
  let probes = 0;
  const h = harness([session("a", state(1, "working"))], {
    notifyStatus: async () => {
      if (++probes === 1) throw new Error("local listener is restarting");
      return { native: true, notifyWhileVisible: true };
    },
  });
  await h.settle(); // failed initial probe
  h.connect(); await h.settle();
  const done = session("a", state(2, "idle", "done"));
  h.setList([done]); h.event("attention", done);
  await h.settle();
  assert.equal(probes, 2);
  assert.equal(h.notices.length, 1);
});

test("desktop keeps foreground banners quiet; native background delivery wins over Web Push APIs", async () => {
  const h = harness([session("a", state(1, "working"))], { pushGranted: true });
  h.connect(); await h.settle();
  const done = session("a", state(2, "idle", "done"));
  h.setList([done]); h.event("attention", done);
  await h.settle();
  assert.deepEqual(h.notices, []);
  h.hide(true);
  const request = session("a", state(3, "blocked", "request"));
  h.setList([request]); h.event("attention", request);
  await h.settle();
  assert.equal(h.notices.length, 1);
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

test("refresh keeps status labels until the next snapshot arrives and through a failed fetch", async () => {
  const h = harness([session("a", state(1, "working")), session("b", state(2, "idle"))]);
  h.connect(); await h.settle();
  const labels = () => [...h.store.useSessions().sessions].map(attention.attentionLabel);
  const initial = h.store.useSessions().sessions;
  let resolve;
  h.deferList(new Promise((r) => { resolve = r; }));
  const refresh = h.store.refresh();
  await h.settle();
  assert.equal(h.store.useSessions().sessions, initial);
  assert.deepEqual(labels(), ["Working", "Idle"]);

  resolve([session("a", state(3, "idle", "done")), session("b", state(4, "working"))]);
  await refresh; await h.settle();
  assert.deepEqual(labels(), ["Finished", "Working"]);

  let reject;
  h.deferList(new Promise((_resolve, r) => { reject = r; }));
  const failedRefresh = h.store.refresh();
  reject(new Error("temporary connection failure"));
  await failedRefresh; await h.settle();
  assert.deepEqual(labels(), ["Finished", "Working"]);
});

test("an explicit unknown transition clears the old agent label", async () => {
  const h = harness([session("a", state(1, "working"))]);
  h.connect(); await h.settle();
  const released = session("a", state(2, "unknown"));
  h.setList([released]); h.event("attention", released);
  await h.settle();
  assert.equal(attention.attentionLabel(h.store.useSessions().sessions[0]), null);
  assert.deepEqual(h.sounds, []);
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

test("sessions without detector attention retain bell notifications and one done sound", async () => {
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


test("remote outage keeps sessions and watched selection, then rebaselines silently", async () => {
  const a = session("a", state(1, "working"));
  const b = session("b", state(2, "working"));
  const h = harness([a, b]);
  h.connect(); await h.settle();
  h.store.setWatched("a");
  h.focus(false);
  h.event("attention", { ...b, attention: state(3, "blocked", "request") });
  h.outage();
  const preserved = h.store.useSessions().sessions;
  h.setList([{ ...a, attention: state(4, "idle", "done") }, { ...b, attention: state(5, "idle", "done") }]);
  await h.store.refresh(); await h.settle();
  assert.equal(h.store.useSessions().sessions, preserved);
  assert.deepEqual(h.sounds, []);
  h.restore(); h.connect(); await h.settle();
  assert.deepEqual([...h.store.useSessions().sessions].map((s) => s.id), ["a", "b"]);
  assert.equal(h.store.isUnread(h.store.useSessions().sessions[0], {}, "a"), false);
  assert.deepEqual(h.sounds, []);
  assert.deepEqual(h.notices, []);
});
