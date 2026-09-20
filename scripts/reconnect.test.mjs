import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

const tick = () => new Promise((resolve) => setImmediate(resolve));
const deferred = () => {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
};
function load(name, globals = {}, dependencies = {}) {
  const source = readFileSync(new URL(`../client/web/lib/${name}.ts`, import.meta.url), "utf8")
    .replaceAll("import.meta.env.VITE_API_BASE", "undefined");
  const { outputText } = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  });
  const exports = {};
  vm.runInNewContext(outputText, {
    exports, Date, TextEncoder, Uint8Array, ...globals,
    require: (id) => {
      if (id in dependencies) return dependencies[id];
      if (id === "react") return { useSyncExternalStore: (_subscribe, get) => get() };
      throw new Error(`Unexpected import ${id}`);
    },
  }, { filename: `${name}.ts` });
  return exports;
}
function connection() {
  return load("workspaceConnection", { window: { dispatchEvent() {} }, Event: class { constructor(type) { this.type = type; } } });
}
function terminal() {
  const workspace = connection();
  const streams = [], requests = [], states = [], output = [], timers = new Map();
  let nextTimer = 0, nextInput, nextPaste, reset = () => {};
  class Source {
    listeners = new Map();
    closed = false;
    constructor(url) { this.url = url; streams.push(this); }
    addEventListener(name, fn) { this.listeners.set(name, fn); }
    close() { this.closed = true; }
    ready(token) { this.listeners.get("ready")?.({ data: JSON.stringify({ attachmentId: token }) }); }
    data(data) { this.onmessage?.({ data }); }
    fail() { this.onerror?.(); }
  }
  const module = load("terminalAttachment", {
    EventSource: Source,
    setTimeout: (fn, ms) => { const id = ++nextTimer; timers.set(id, { fn, ms }); return id; },
    clearTimeout: (id) => timers.delete(id),
  }, { "./workspaceConnection": workspace });
  const handle = module.createTerminalAttachment("session", {
    cols: 80, rows: 24,
    onReady: () => { output.push("reset"); return reset(); },
    onState: (state) => states.push(state),
    onData: (bytes) => output.push(new TextDecoder().decode(bytes)),
  }, {
    url: (cols, rows) => `/attach?cols=${cols}&rows=${rows}`,
    send: (path, args) => {
      requests.push({ path, ...args });
      if (path === "terminal/input" && nextInput) { const result = nextInput; nextInput = null; return result; }
      if (path === "terminal/paste-image" && nextPaste) { const result = nextPaste; nextPaste = null; return result; }
      return Promise.resolve({ path: "/tmp/image.png" });
    },
    encode: (bytes) => Buffer.from(bytes).toString("base64"),
    decode: (data) => new TextEncoder().encode(data),
  });
  return {
    workspace, streams, requests, states, output, handle,
    input: () => requests.filter((r) => r.path === "terminal/input"),
    nextInput: (promise) => { nextInput = promise; },
    nextPaste: (promise) => { nextPaste = promise; },
    reset: (fn) => { reset = fn; },
    async ready(token = "viewer-a") { streams.at(-1).onopen?.(); streams.at(-1).ready(token); await tick(); },
    retry() {
      for (const [id, timer] of [...timers]) if (timer.ms === 1200) { timers.delete(id); timer.fn(); }
    },
    timeout() {
      for (const [id, timer] of [...timers]) if (timer.ms === 5000) { timers.delete(id); timer.fn(); }
    },
  };
}

test("HTTP open is not attachment readiness; reset drains before output or input", async () => {
  const h = terminal();
  const render = deferred();
  h.reset(() => render.promise);
  h.streams[0].onopen();
  h.handle.write("before-ready", true);
  h.streams[0].ready("viewer-a");
  h.streams[0].data("fresh screen");
  h.handle.write("before-render", true);
  assert.equal(h.handle.inputToken(), null);
  assert.equal(h.input().length, 0);
  await tick();
  assert.deepEqual(h.output, ["reset"]);
  render.resolve(); await tick();
  assert.deepEqual(h.output, ["reset", "fresh screen"]);
  h.handle.write("hello", true); await tick();
  assert.equal(h.input()[0].attachmentId, "viewer-a");
  assert.equal(h.input()[0].claimGeometry, true);
});

test("a lost stream drops queued input and old failures cannot invalidate a new viewer", async () => {
  const h = terminal(); await h.ready();
  const old = deferred(); h.nextInput(old.promise);
  h.handle.write("sent", true);
  h.handle.write("queued", true);
  h.streams[0].fail();
  h.handle.write("during-gap", true);
  h.retry(); await h.ready("viewer-b");
  h.handle.write("new", true);
  old.reject(new Error("old connection failed")); await tick();
  assert.equal(h.handle.inputToken(), "viewer-b");
  assert.deepEqual(h.input().map((r) => Buffer.from(r.data, "base64").toString()), ["sent", "new"]);
  assert.equal(h.requests.some((r) => r.path === "terminal/detach" && r.attachmentId === "viewer-a"), true);
});

test("failed input pauses the viewer and never retries the mutation", async () => {
  const h = terminal(); await h.ready();
  const failed = deferred(); h.nextInput(failed.promise);
  h.handle.write("sent", true); h.handle.write("pending", true);
  failed.reject(new Error("transport lost")); await tick();
  assert.equal(h.handle.inputToken(), null);
  assert.equal(h.streams[0].closed, true);
  h.retry(); await h.ready("viewer-b");
  assert.equal(h.input().length, 1);
});

test("resizes use current dimensions on reconnect and passive replies do not claim geometry", async () => {
  const h = terminal(); await h.ready();
  h.handle.write("query response"); await tick();
  assert.equal(h.input()[0].claimGeometry, false);
  h.streams[0].fail(); h.handle.resize(120, 40); h.retry();
  assert.match(h.streams[1].url, /cols=120&rows=40/);
  await h.ready("viewer-b");
  const sizes = h.requests.filter((r) => r.path === "terminal/resize");
  assert.equal(sizes.at(-1).attachmentId, "viewer-b");
  assert.equal(sizes.at(-1).cols, 120);
  h.streams[0].data("obsolete");
  assert.equal(h.output.includes("obsolete"), false);
});

test("image encoding and upload responses cannot paste across a reconnect", async () => {
  const h = terminal(); await h.ready();
  const bytes = deferred();
  const paste = h.handle.pasteImage({ arrayBuffer: () => bytes.promise }, "image/png");
  h.streams[0].fail(); h.retry(); await h.ready("viewer-b");
  bytes.resolve(new Uint8Array([1]).buffer);
  await assert.rejects(paste, /connection is unavailable/);
  assert.equal(h.requests.some((r) => r.path === "terminal/paste-image"), false);
  const result = deferred(); h.nextPaste(result.promise);
  const uploading = h.handle.pasteImage(new Blob(["image"]), "image/png"); await tick();
  h.streams[1].fail(); result.resolve({ path: "/tmp/old.png" });
  await assert.rejects(uploading, /connection is unavailable/);
});

test("workspace outage freezes attachment; restore reattaches without replaying input", async () => {
  const h = terminal(); await h.ready();
  h.streams[0].data("preserved");
  h.workspace.setWorkspaceAvailable(false);
  assert.equal(h.streams[0].closed, true);
  assert.equal(h.handle.inputToken(), null);
  h.handle.write("offline", true); h.retry();
  assert.equal(h.streams.length, 1);
  assert.deepEqual(h.output, ["reset", "preserved"]);
  h.workspace.setWorkspaceAvailable(true);
  assert.equal(h.streams.length, 2);
  await h.ready("viewer-b");
  assert.equal(h.input().length, 0);
});

test("detach releases only its viewer and cancels further retry", async () => {
  const h = terminal(); await h.ready(); h.handle.detach();
  h.streams[0].fail(); h.retry(); h.handle.write("closed", true);
  assert.equal(h.streams.length, 1);
  assert.equal(h.input().length, 0);
  assert.equal(h.requests.at(-1).path, "terminal/detach");
  assert.equal(h.requests.at(-1).attachmentId, "viewer-a");
});

test("an old server without ready stays read-only instead of sending tokenless input", () => {
  const h = terminal(); h.streams[0].onopen(); h.timeout();
  h.streams[0].data("legacy output"); h.handle.write("unsafe", true);
  assert.equal(h.states.at(-1), "incompatible");
  assert.equal(h.input().length, 0);
});

async function remote(initial) {
  const workspace = connection();
  let status = initial, reloads = 0, retries = 0, disconnects = 0, updating = false;
  const api = {
    remoteStatus: async () => { if (status instanceof Error) throw status; return status; },
    sshProfiles: async () => null,
    remoteRetry: async () => { retries++; status = { state: "reconnecting", host: initial.host }; },
    remoteConnect: async (host) => { status = { state: "connected", host }; },
    remoteDisconnect: async () => { disconnects++; status = { state: "idle" }; },
  };
  const store = load("remote", {
    window: { location: { hostname: "remote.example", reload() { reloads++; } } },
    setInterval: () => 1, clearInterval() {}, setTimeout,
  }, { "./api": api, "./editorState": { flushEditor: async () => {} }, "./workspaceConnection": workspace,
    "./updates": { isUpdateInProgress: () => updating } });
  await tick();
  return { store, workspace, set: (next) => { status = next; }, updating: (active) => { updating = active; },
    reloads: () => reloads, retries: () => retries, disconnects: () => disconnects };
}

test("updater tunnel teardown cannot trigger a reload, while ordinary recovery still updates", async () => {
  const h = await remote({ state: "connected", host: "workstation" });
  h.updating(true);
  h.set({ state: "reconnecting", host: "workstation" }); await h.store.refresh();
  assert.equal(h.store.useRemote().status.state, "reconnecting", "downloads must not freeze remote status updates");
  h.set({ state: "idle" }); await h.store.refresh();
  assert.equal(h.reloads(), 0, "native updater shutdown must not reload the webview");
  assert.equal(h.workspace.workspaceConnection().available, false);
  h.updating(false); await h.store.refresh();
  assert.equal(h.reloads(), 1, "a failed/finished updater restores normal workspace rebind behavior");
});

test("same host reconnect and attention error retain the mounted workspace", async () => {
  const h = await remote({ state: "connected", host: "workstation" });
  assert.equal(h.reloads(), 0);
  for (const state of ["reconnecting", "error", "reconnecting", "connected"]) {
    h.set({ state, host: "workstation" }); await h.store.refresh();
    assert.equal(h.store.useRemote().workspaceHost, "workstation");
    assert.equal(h.store.useRemote().interrupted, state !== "connected");
    assert.equal(h.workspace.workspaceConnection().available, state === "connected");
  }
  assert.equal(h.reloads(), 0);
});

test("status transport failures preserve the remote instead of switching to local", async () => {
  const h = await remote({ state: "connected", host: "workstation" });
  h.set(new Error("network lost")); await h.store.refresh();
  assert.equal(h.store.useRemote().status.state, "reconnecting");
  assert.equal(h.store.useRemote().workspaceHost, "workstation");
  assert.equal(h.reloads(), 0);
  await h.store.useRemote().retry();
  assert.equal(h.retries(), 1);
  assert.equal(h.reloads(), 0);
});

test("new host and explicit disconnect rebind all host-owned caches", async () => {
  const h = await remote({ state: "connected", host: "a" });
  h.set({ state: "connected", host: "b" }); await h.store.refresh();
  assert.equal(h.reloads(), 1);
  const local = await remote({ state: "idle" });
  await local.store.connect("a");
  assert.equal(local.reloads(), 1);
  const detached = await remote({ state: "connected", host: "a" });
  await detached.store.disconnect();
  assert.equal(detached.disconnects(), 1);
  assert.equal(detached.reloads(), 1);
});

test("an initial mobile connection error does not invent a workspace to preserve", async () => {
  const h = await remote({ state: "error", host: "a" });
  assert.equal(h.store.useRemote().workspaceHost, null);
  assert.equal(h.store.useRemote().interrupted, false);
  assert.equal(h.reloads(), 0);
});

function httpClient(fetch, workspace) {
  return load("api", { fetch, setTimeout, URLSearchParams }, {
    "@/lib/skill": {}, "@/lib/agents": {}, "@/lib/log": { log: { debug() {} } },
    "./terminalAttachment": {}, "./workspaceConnection": workspace,
  });
}

test("ordinary HTTP operations are blocked during outage but local Retry remains reachable", async () => {
  const workspace = connection(), calls = [];
  const api = httpClient(async (url) => { calls.push(url); return { ok: true, json: async () => ({ ok: true }) }; }, workspace);
  workspace.setWorkspaceAvailable(false);
  await assert.rejects(api.terminalList(), /connection is unavailable/);
  await assert.rejects(api.readFile("/remote", "file.md"), /connection is unavailable/);
  await api.remoteRetry();
  assert.deepEqual(calls, ["/api/remote/retry"]);
});

test("an HTTP response from before a reconnect cannot overwrite the restored workspace", async () => {
  const workspace = connection(), response = deferred();
  const api = httpClient(() => response.promise, workspace);
  const stale = api.terminalList();
  workspace.setWorkspaceAvailable(false); workspace.setWorkspaceAvailable(true);
  response.resolve({ ok: true, json: async () => [{ id: "old" }] });
  await assert.rejects(stale, /connection is unavailable/);
});
