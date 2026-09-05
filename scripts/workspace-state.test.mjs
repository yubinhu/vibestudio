// Run with: node --test scripts/workspace-state.test.mjs
// Exercise the actual TS stores with controlled HTTP promises. No browser,
// framework dependency, filesystem mutation, or live server is required.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

function moduleUnderTest(name, api, storage = new Map()) {
  const source = readFileSync(new URL(`../client/web/lib/${name}.ts`, import.meta.url), "utf8");
  const { outputText } = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  });
  const exports = {};
  const context = vm.createContext({
    exports, Date, Promise, Set, Error,
    location: { hostname: "localhost" },
    localStorage: { getItem: (key) => storage.get(key) ?? null, removeItem: (key) => storage.delete(key) },
    require: (id) => {
      if (id === "./api") return api;
      if (id === "react") return { useEffect: () => {}, useSyncExternalStore: (_subscribe, get) => get() };
      throw new Error(`Unexpected import ${id}`);
    },
  });
  vm.runInContext(outputText, context, { filename: `${name}.ts` });
  return exports;
}
const tick = () => new Promise((resolve) => setImmediate(resolve));
function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

test("picker restores the workflow's folder and lets current form context win", async () => {
  const calls = [];
  const picker = moduleUnderTest("picker", {
    preferencesGet: async () => ({ pickerLocations: { open: "/docs", session: "/projects" } }),
    listDir: async (path, files) => { calls.push([path, files]); return { path }; },
  });
  assert.equal((await picker.loadPickerStart("open", undefined, true)).listing.path, "/docs");
  assert.equal((await picker.loadPickerStart("session", "/current-skill", false)).listing.path, "/current-skill");
  assert.deepEqual(calls, [["/docs", true], ["/current-skill", false]]);
});

test("picker skips missing current and saved directories and recovers at Home", async () => {
  const calls = [];
  const picker = moduleUnderTest("picker", {
    preferencesGet: async () => ({ pickerLocations: { session: "/deleted" } }),
    listDir: async (path) => {
      calls.push(path);
      if (path) throw new Error("Folder unavailable");
      return { path: "/home" };
    },
  });
  const result = await picker.loadPickerStart("session", "/unmounted", false);
  assert.equal(result.listing.path, "/home");
  assert.equal(result.usedFallback, true);
  assert.deepEqual(calls, ["/unmounted", "/deleted", ""]);
});

test("picker tolerates an older server without preferences but reports unreadable Home", async () => {
  const picker = moduleUnderTest("picker", {
    preferencesGet: async () => { throw new Error("404"); },
    listDir: async () => { throw new Error("Permission denied"); },
  });
  await assert.rejects(picker.loadPickerStart("open", undefined, true), /Permission denied/);
});

test("a late history GET cannot erase an open; writes reach the server in order", async () => {
  const initial = deferred();
  const added = deferred();
  const removed = deferred();
  const calls = [];
  const store = moduleUnderTest("recents", {
    recentsList: () => { calls.push("list"); return initial.promise; },
    recentsAdd: () => { calls.push("add"); return added.promise; },
    recentsRemove: () => { calls.push("remove"); return removed.promise; },
  });
  await tick();
  store.addRecent({ root: "/notes.md", name: "notes.md", kind: "markdown" });
  initial.resolve([]);
  await tick();
  assert.equal(store.useRecents()[0].root, "/notes.md");
  store.removeRecent("/notes.md");
  await tick();
  assert.deepEqual(calls, ["list", "add"]);
  added.resolve([{ root: "/notes.md", name: "notes.md", kind: "markdown" }]);
  await tick();
  assert.equal(store.useRecents().length, 0);
  assert.deepEqual(calls, ["list", "add", "remove"]);
  removed.resolve([]);
  await tick();
  assert.equal(store.useRecents().length, 0);
});

test("failed recent writes are visible and do not stop later writes", async () => {
  let fail = true;
  const store = moduleUnderTest("recents", {
    recentsList: async () => [],
    recentsAdd: async (r) => { if (fail) throw new Error("Disk full"); return [r]; },
  });
  await tick();
  store.addRecent({ root: "/one", name: "One" });
  await tick();
  assert.match(store.useRecentsStatus().error, /could not be saved/);
  fail = false;
  store.addRecent({ root: "/two", name: "Two" });
  await tick();
  assert.equal(store.useRecentsStatus().error, null);
  assert.equal(store.useRecents()[0].root, "/two");
});

test("legacy client history and launch folders never migrate onto an SSH server", async () => {
  let writes = 0;
  const storage = new Map([
    ["skillviewer-recents", JSON.stringify([{ root: "/local/notes.md", name: "notes.md" }])],
    ["skillviewer-terminal-prefs", JSON.stringify({ "shell:cli": { cwd: "/local/project" } })],
  ]);
  const api = {
    remoteStatus: async () => ({ state: "connected" }),
    recentsList: async () => [],
    recentsAdd: async () => { writes++; return []; },
    preferencesGet: async () => ({ pickerLocations: {}, terminal: {}, lastAgent: null }),
    rememberTerminal: async () => { writes++; },
  };
  moduleUnderTest("recents", api, storage);
  const terminal = moduleUnderTest("terminalPrefs", api, storage);
  await terminal.loadTerminalPrefs();
  await tick();
  assert.equal(writes, 0);
  assert.equal(storage.size, 2);
});
