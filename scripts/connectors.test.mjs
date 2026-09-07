// Exercise connector discovery with controlled HTTP responses: no agent or MCP
// process is started. These tests cover cache scope, request races and the
// distinction between passive configuration and dated runtime observations.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

const tick = () => new Promise((resolve) => setImmediate(resolve));
function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
function inventory(connectors = [], sources = []) {
  return { connectors, sources, agents: [{ id: "future-agent", label: "Future Agent", clients: ["CLI"] }], scannedAt: 123 };
}
function connector(id, sourceId, state = "configured") {
  return { id, name: id, kind: "app", availability: [{ agentId: "future-agent", state, sourceId, sourceLabel: sourceId, scope: "account" }] };
}
const config = { id: "settings", label: "Settings", state: "scanned", discovery: "configuration" };
const runtime = { id: "runtime", label: "Agent runtime", state: "scanned", discovery: "runtime" };

function storeUnderTest(api) {
  let subscribeOnRead = false;
  let clock = 100_000;
  const exports = {};
  const source = readFileSync(new URL("../client/web/lib/connectors.ts", import.meta.url), "utf8");
  const { outputText } = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } });
  vm.runInNewContext(outputText, {
    exports, Date: { now: () => clock }, Promise, Set, Map, Error,
    require: (id) => {
      if (id === "./api") return api;
      if (id === "react") return {
        useCallback: (callback) => callback,
        useSyncExternalStore: (subscribe, snapshot) => {
          if (subscribeOnRead) subscribe(() => {});
          return snapshot();
        },
      };
      throw new Error(`Unexpected import ${id}`);
    },
  }, { filename: "connectors.ts" });
  return {
    ...exports,
    advance: (ms) => { clock += ms; },
    mount: (project) => {
      subscribeOnRead = true;
      const snapshot = exports.useConnectors(project);
      subscribeOnRead = false;
      return snapshot;
    },
  };
}

test("an unavailable discovery endpoint remains unknown rather than an empty success", async () => {
  const store = storeUnderTest({ connectorsDiscover: async () => { throw Object.assign(new Error("missing"), { status: 404 }); } });
  await store.refreshConnectors();
  const state = store.useConnectors();
  assert.equal(state.loading, false);
  assert.equal(state.inventory, null);
  assert.match(state.error, /Update the server/);
});

test("runtime cloud results survive revisits and passive refresh without preserving removed settings", async () => {
  let scans = 0;
  let checks = 0;
  const store = storeUnderTest({
    connectorsCheck: async () => { checks++; return inventory([connector("old-config", "settings"), connector("cloud", "runtime", "connected")], [config, runtime]); },
    connectorsDiscover: async () => { scans++; return inventory([connector("new-config", "settings")], [config]); },
  });
  await store.checkConnectors();
  const checkedAt = store.useConnectors().checkedAt;
  store.advance(60_000);
  store.mount();
  await tick();
  assert.equal(scans, 1, "a stale revisit should discover newly configured apps even after a runtime check");
  assert.equal(checks, 1, "navigation must never start another runtime check");
  assert.equal(store.useConnectors().inventory.connectors.find((c) => c.id === "cloud").availability[0].state, "connected");
  await store.refreshConnectors();
  const state = store.useConnectors();
  assert.deepEqual(Array.from(state.inventory.connectors, (c) => c.id).sort(), ["cloud", "new-config"]);
  assert.equal(state.inventory.connectors.find((c) => c.id === "cloud").availability[0].state, "connected");
  assert.equal(state.checkedAt, checkedAt, "a passive refresh cannot advance runtime verification time");
});

test("a fresh page discovers a configured Gmail plugin without a runtime check or persisted browser state", async () => {
  const source = { id: "codex-gmail-plugin", label: "Plugin Gmail", agentId: "codex", state: "scanned", discovery: "configuration" };
  const gmail = { id: "app-gmail", name: "Gmail", kind: "app", availability: [{ agentId: "codex", state: "configured", sourceId: source.id, sourceLabel: source.label, scope: "plugin" }] };
  const response = { ...inventory([gmail], [source]), agents: [{ id: "codex", label: "Codex", clients: ["CLI", "Desktop", "VS Code"] }] };
  let scans = 0;
  let checks = 0;
  const api = {
    connectorsDiscover: async () => { scans++; return response; },
    connectorsCheck: async () => { checks++; throw new Error("Unexpected runtime check"); },
  };
  // A new module instance models a reload: each starts with an empty cache.
  for (let visit = 0; visit < 2; visit++) {
    const store = storeUnderTest(api);
    assert.equal(store.mount().inventory, null);
    await tick();
    const state = store.useConnectors();
    assert.equal(state.loading, false);
    assert.equal(state.inventory.connectors[0].name, "Gmail");
    assert.equal(state.inventory.connectors[0].availability[0].agentId, "codex");
    assert.equal(state.checkedAt, null);
  }
  assert.equal(scans, 2);
  assert.equal(checks, 0);
});

test("an explicit runtime check queued immediately after a passive read is actually run", async () => {
  const passive = deferred();
  const checked = deferred();
  const calls = [];
  const store = storeUnderTest({
    connectorsDiscover: () => { calls.push("discover"); return passive.promise; },
    connectorsCheck: () => { calls.push("check"); return checked.promise; },
  });
  const first = store.refreshConnectors();
  const second = store.checkConnectors();
  assert.equal(first, second);
  await tick();
  assert.deepEqual(calls, ["discover"]);
  passive.resolve(inventory());
  await tick();
  assert.deepEqual(calls, ["discover", "check"]);
  assert.equal(store.useConnectors().checking, true);
  checked.resolve(inventory([connector("cloud", "runtime", "connected")], [runtime]));
  await second;
  assert.equal(store.useConnectors().inventory.connectors[0].id, "cloud");
  assert.equal(store.useConnectors().refreshing, false);
});

test("late responses remain in their original project scope", async () => {
  const a = deferred();
  const b = deferred();
  const store = storeUnderTest({ connectorsDiscover: (project) => project === "/a" ? a.promise : b.promise });
  const first = store.refreshConnectors("/a");
  const second = store.refreshConnectors("/b");
  b.resolve(inventory([connector("b", "settings")], [config]));
  await second;
  a.resolve(inventory([connector("a", "settings")], [config]));
  await first;
  assert.equal(store.useConnectors("/a").inventory.connectors[0].id, "a");
  assert.equal(store.useConnectors("/b").inventory.connectors[0].id, "b");
  assert.equal(store.useConnectors().inventory, null);
});

test("a managed access change invalidates old runtime evidence, including an inflight check", async () => {
  const checked = deferred();
  const store = storeUnderTest({ connectorsCheck: () => checked.promise, connectorsDiscover: async () => inventory([], [config]) });
  const pending = store.checkConnectors();
  await tick();
  store.invalidateConnectors();
  const refreshed = store.refreshConnectors();
  checked.resolve(inventory([connector("deleted", "runtime", "connected")], [runtime]));
  await Promise.all([pending, refreshed]);
  assert.equal(store.useConnectors().inventory.connectors.length, 0);
  assert.equal(store.useConnectors().checkedAt, null);
});

test("a failed check retains the last successful inventory and its original timestamp", async () => {
  let fail = false;
  const store = storeUnderTest({ connectorsCheck: async () => {
    if (fail) throw new Error("Agent unavailable");
    return inventory([connector("cloud", "runtime", "connected")], [runtime]);
  } });
  await store.checkConnectors();
  const checkedAt = store.useConnectors().checkedAt;
  store.advance(60_000);
  fail = true;
  await store.checkConnectors();
  assert.equal(store.useConnectors().inventory.connectors[0].id, "cloud");
  assert.equal(store.useConnectors().checkedAt, checkedAt);
  assert.match(store.useConnectors().error, /Agent unavailable/);
});
