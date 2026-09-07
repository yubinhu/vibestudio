// Exercise the real skill store and HTTP adapter with deferred requests and a
// controlled clock. No live server, git process, or React DOM is needed.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

const tick = () => new Promise((resolve) => setImmediate(resolve));
function deferred() {
  let resolve;
  const promise = new Promise((yes) => { resolve = yes; });
  return { promise, resolve };
}
function groups(...roots) {
  return [{ agent: "Agent Skills", skills: roots.map((root) => ({ root, name: root, kind: "personal", proposed: false })) }];
}
function compile(name) {
  const source = readFileSync(new URL(`../client/web/lib/${name}.ts`, import.meta.url), "utf8")
    .replaceAll("import.meta.env", "({})");
  return ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  }).outputText;
}
function storeUnderTest(api) {
  let clock = 100_000;
  let nextTimer = 0;
  let subscribe;
  const timers = new Map();
  const exports = {};
  vm.runInNewContext(compile("skills"), {
    exports, Promise, Set, Error, Date: { now: () => clock },
    setTimeout: (callback, delay) => {
      const id = ++nextTimer;
      timers.set(id, { callback, at: clock + delay });
      return id;
    },
    clearTimeout: (id) => timers.delete(id),
    require: (id) => {
      if (id === "@/lib/api") return { gitDirtyMany: async () => [], ...api };
      if (id === "react") return {
        useSyncExternalStore: (listen, snapshot) => { subscribe = listen; return snapshot(); },
      };
      throw new Error(`Unexpected import ${id}`);
    },
  }, { filename: "skills.ts" });
  exports.useSkills();
  return {
    ...exports,
    mount: (listener = () => {}) => subscribe(listener),
    pendingTimers: () => timers.size,
    advance: async (ms) => {
      const end = clock + ms;
      for (;;) {
        const due = [...timers].sort((a, b) => a[1].at - b[1].at)[0];
        if (!due || due[1].at > end) break;
        timers.delete(due[0]);
        clock = due[1].at;
        due[1].callback();
        await tick();
      }
      clock = end;
      await tick();
    },
  };
}

test("installed skills paint before project discovery and slow git badges cannot block completion", async () => {
  const initial = deferred();
  const firstDirty = deferred();
  const finalDirty = deferred();
  const calls = [];
  const dirtyCalls = [];
  const initialGroups = groups("/installed");
  const finalGroups = groups("/installed", "/project");
  const replies = [initial.promise, { groups: initialGroups, scanning: true }, { groups: finalGroups, scanning: false }];
  const store = storeUnderTest({
    discoverSkillSnapshot: async (refresh) => { calls.push(refresh); return replies.shift(); },
    gitDirtyMany: (roots) => {
      dirtyCalls.push(Array.from(roots));
      return dirtyCalls.length === 1 ? firstDirty.promise : finalDirty.promise;
    },
  });
  const unmount = store.mount();
  assert.equal(store.useSkills().loading, true);
  initial.resolve({ groups: initialGroups, scanning: true });
  await tick();
  assert.equal(store.useSkills().loading, false);
  assert.equal(store.useSkills().scanning, true);
  assert.equal(store.useSkills().total, 1);
  assert.deepEqual(dirtyCalls, [["/installed"]]);
  await store.advance(1000);
  assert.equal(dirtyCalls.length, 1, "intermediate polls must not repeat git work");
  await store.advance(1000);
  assert.equal(store.useSkills().total, 2, "final cards must paint while the first git request is pending");
  assert.equal(store.useSkills().scanning, false);
  assert.deepEqual(calls, [false, false, false], "polling must not restart the server's scan");
  assert.equal(store.pendingTimers(), 0);
  firstDirty.resolve([{ root: "/installed", dirty: true }]);
  await tick();
  assert.equal(store.useSkills().dirtyRoots.size, 0, "a stale dirty response must not overwrite the final inventory");
  assert.deepEqual(dirtyCalls, [["/installed"], ["/installed", "/project"]]);
  finalDirty.resolve([{ root: "/installed", dirty: false }, { root: "/project", dirty: true }]);
  await tick();
  assert.deepEqual([...store.useSkills().dirtyRoots], ["/project"]);
  unmount();
});

test("two home subscribers share a mount request and quick revisits reuse the cache", async () => {
  const response = deferred();
  let calls = 0;
  const store = storeUnderTest({ discoverSkillSnapshot: async () => { calls++; return response.promise; } });
  const offCard = store.mount();
  const offGallery = store.mount();
  assert.equal(calls, 1);
  response.resolve({ groups: groups("/installed"), scanning: false });
  await tick();
  offCard();
  offGallery();
  await store.advance(1000);
  const offRevisit = store.mount();
  assert.equal(calls, 1);
  assert.equal(store.useSkills().total, 1);
  offRevisit();
  await store.advance(1100);
  const offStale = store.mount();
  assert.equal(calls, 2);
  await tick();
  offStale();
});

test("explicit refresh during an in-flight read coalesces to one awaited force request", async () => {
  const initial = deferred();
  const forced = deferred();
  const calls = [];
  const store = storeUnderTest({
    discoverSkillSnapshot: (refresh) => { calls.push(refresh); return calls.length === 1 ? initial.promise : forced.promise; },
  });
  const unmount = store.mount();
  const first = store.refreshSkills();
  const second = store.refreshSkills();
  assert.equal(first, second);
  assert.deepEqual(calls, [false]);
  initial.resolve({ groups: groups("/old"), scanning: true });
  await tick();
  assert.deepEqual(calls, [false, true]);
  assert.equal(store.useSkills().groups[0].skills[0].root, "/old");
  assert.equal(store.useSkills().scanning, true);
  let resolved = false;
  void second.then(() => { resolved = true; });
  await tick();
  assert.equal(resolved, false, "refresh callers must wait for the queued force request");
  forced.resolve({ groups: groups("/new"), scanning: false });
  await second;
  assert.equal(store.useSkills().groups[0].skills[0].root, "/new");
  assert.equal(store.useSkills().scanning, false);
  assert.equal(store.pendingTimers(), 0);
  unmount();
});

test("the last unmount cancels polling and an in-flight response cannot restart it", async () => {
  const second = deferred();
  let calls = 0;
  const store = storeUnderTest({ discoverSkillSnapshot: async () => {
    calls++;
    return calls === 1 ? { groups: groups("/installed"), scanning: true } : second.promise;
  } });
  const firstUnmount = store.mount();
  await tick();
  assert.equal(store.pendingTimers(), 1);
  firstUnmount();
  assert.equal(store.pendingTimers(), 0);
  await store.advance(5000);
  assert.equal(calls, 1);
  const secondUnmount = store.mount();
  assert.equal(calls, 2, "a revisit resumes unfinished discovery immediately");
  secondUnmount();
  second.resolve({ groups: groups("/installed"), scanning: true });
  await tick();
  assert.equal(store.pendingTimers(), 0);
  await store.advance(5000);
  assert.equal(calls, 2);
});

test("the adapter accepts legacy arrays and tags Studio skills in both response formats", async () => {
  const legacy = groups("/load-secrets", "/personal");
  const replies = [legacy, { groups: legacy, scanning: true }, legacy];
  const urls = [];
  const exports = {};
  vm.runInNewContext(compile("api"), {
    exports, Promise, Set, Error,
    fetch: async (url) => { urls.push(url); return { ok: true, json: async () => replies.shift() }; },
    require: (id) => {
      if (id === "@/lib/skill") return {};
      if (id === "@/lib/agents") return { isBootstrapSkill: (root) => root === "/load-secrets" };
      if (id === "@/lib/log") return { log: { debug: () => {} } };
      throw new Error(`Unexpected import ${id}`);
    },
  }, { filename: "api.ts" });
  const old = await exports.discoverSkillSnapshot();
  assert.equal(old.scanning, false);
  assert.equal(old.groups[0].skills[0].kind, "studio");
  assert.equal(old.groups[0].skills[1].kind, "personal");
  const current = await exports.discoverSkillSnapshot(true);
  assert.equal(current.scanning, true);
  assert.equal(current.groups[0].skills[0].kind, "studio");
  const original = await exports.discoverSkills();
  assert.equal(original[0].skills[0].kind, "studio");
  assert.equal(legacy[0].skills[0].kind, "personal", "retagging must not mutate the response cache");
  assert.deepEqual(urls, [
    "/api/skills/discover?progressive=true",
    "/api/skills/discover?progressive=true&refresh=true",
    "/api/skills/discover",
  ]);
});
