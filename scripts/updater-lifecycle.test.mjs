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
function load(path, dependencies, globals = {}) {
  const source = readFileSync(new URL(`../client/web/${path}.ts`, import.meta.url), "utf8");
  const { outputText } = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  });
  const exports = {};
  vm.runInNewContext(outputText, {
    exports, Error, Promise, Set, queueMicrotask, ...globals,
    require: (id) => {
      if (id in dependencies) return dependencies[id];
      throw new Error(`Unexpected import ${id}`);
    },
  }, { filename: `${path}.ts` });
  return exports;
}
const storeReact = { useSyncExternalStore: (_subscribe, snapshot) => snapshot() };
function unloadEvent() {
  return { defaultPrevented: false, preventDefault() { this.defaultPrevented = true; }, returnValue: undefined };
}

async function updater({ flush = async () => {}, apply, canAuto = true } = {}) {
  let next = { current: "1.2.9", available: { version: "1.2.10" }, canAuto,
    phase: "idle", progress: null, error: null, releaseUrl: "https://example.test/release" };
  let answer = () => next;
  let requests = 0;
  let timerId = 0;
  const timers = new Map();
  const store = load("lib/updates", {
    react: storeReact,
    "./editorState": { flushEditor: flush },
    "@/lib/api": {
      updateStatus: async () => answer(),
      updateApply: async () => {
        requests++;
        if (apply) await apply();
        next = { ...next, phase: "downloading" };
      },
    },
  }, {
    setTimeout: (fn, ms) => { const id = ++timerId; timers.set(id, { fn, ms }); return id; },
    clearTimeout: (id) => timers.delete(id),
  });
  const terminal = load("lib/terminalUnload", { "./updates": store });
  const poll = async () => {
    const entry = [...timers].at(-1);
    assert.ok(entry, "an updater poll must be scheduled");
    timers.delete(entry[0]); entry[1].fn();
    await tick();
  };
  await poll();
  return { store, poll, requests: () => requests,
    set: (patch) => { next = { ...next, ...patch }; answer = () => next; },
    respond: (fn) => { answer = fn; },
    guardsClose: () => { const event = unloadEvent(); terminal.guardTerminalUnload(event); return event.defaultPrevented; } };
}

test("restart waits for saves, bypasses only the terminal accident guard, and resets on failure", async () => {
  const saved = deferred();
  const h = await updater({ flush: () => saved.promise });
  assert.equal(h.guardsClose(), true, "ordinary browser Ctrl+W protection remains active");
  const pending = h.store.applyUpdate();
  assert.equal(h.store.useUpdate().phase, "saving");
  assert.equal(h.guardsClose(), true, "the guard remains while an editor has not yet saved");
  await h.store.applyUpdate();
  assert.equal(h.requests(), 0, "repeated clicks cannot bypass or duplicate the pending save");
  saved.resolve(); await pending;
  assert.equal(h.requests(), 1);
  assert.equal(h.guardsClose(), false);
  h.set({ phase: "ready" }); await h.poll();
  assert.equal(h.guardsClose(), false);
  h.respond(() => { throw new Error("server is restarting"); }); await h.poll();
  assert.equal(h.guardsClose(), false, "a restart connection gap must not resurrect the prompt");
  h.set({ phase: "error", error: "Installer failed" }); await h.poll();
  assert.equal(h.guardsClose(), true, "a failed update restores ordinary browser protection");
});

test("failed saves and rejected apply requests remain visible and never leave an unload bypass", async () => {
  const save = await updater({ flush: async () => { throw new Error("Disk full"); } });
  await save.store.applyUpdate();
  assert.equal(save.requests(), 0);
  assert.match(save.store.useUpdate().error, /changes couldn't be saved.*Disk full/);
  assert.equal(save.guardsClose(), true);

  const http = await updater({ apply: async () => { throw Object.assign(new Error("Update rejected"), { status: 400 }); } });
  http.respond(() => { throw new Error("status endpoint unavailable"); });
  await http.store.applyUpdate();
  assert.equal(http.requests(), 1);
  assert.equal(http.store.useUpdate().phase, "error");
  assert.equal(http.store.useUpdate().error, "Update rejected");
  assert.equal(http.guardsClose(), true);
});

test("stale idle polls cannot overwrite an update and manual-download hosts never bypass guards", async () => {
  const h = await updater();
  const old = deferred();
  h.respond(() => old.promise);
  await h.poll();
  h.set({ phase: "idle" });
  await h.store.applyUpdate();
  old.resolve({ current: "1.2.9", available: { version: "1.2.10" }, canAuto: true, phase: "idle" });
  await tick();
  assert.equal(h.store.useUpdate().phase, "downloading");
  assert.equal(h.guardsClose(), false);

  const manual = await updater({ canAuto: false });
  manual.set({ phase: "downloading" }); await manual.poll();
  await manual.store.applyUpdate();
  assert.equal(manual.requests(), 0);
  assert.equal(manual.guardsClose(), true);
});

// Exercise useAutosave itself with committed renders/effects and controlled save
// promises. This checks the actual editor guard and flush sequencing without a
// browser or production filesystem.
function editor(save) {
  const slots = [];
  let cursor = 0, value = "on disk", hook, result, queued = false, effects = [];
  const windowEvents = new Map();
  const eventTarget = (events) => ({
    addEventListener(name, callback) {
      if (!events.has(name)) events.set(name, new Set());
      events.get(name).add(callback);
    },
    removeEventListener(name, callback) { events.get(name)?.delete(callback); },
  });
  const scheduleRender = () => {
    if (queued) return;
    queued = true;
    queueMicrotask(() => { queued = false; render(); });
  };
  const same = (a, b) => a && b && a.length === b.length && a.every((item, i) => Object.is(item, b[i]));
  const react = {
    ...storeReact,
    useState(initial) {
      const index = cursor++;
      if (!slots[index]) slots[index] = { value: initial };
      return [slots[index].value, (next) => {
        const value = typeof next === "function" ? next(slots[index].value) : next;
        if (Object.is(value, slots[index].value)) return;
        slots[index].value = value; scheduleRender();
      }];
    },
    useRef(initial) { const index = cursor++; return slots[index] ??= { current: initial }; },
    useCallback(fn, deps) {
      const index = cursor++;
      if (!same(slots[index]?.deps, deps)) slots[index] = { fn, deps };
      return slots[index].fn;
    },
    useEffect(fn, deps) {
      const index = cursor++;
      const previous = slots[index];
      if (previous && same(previous.deps, deps)) return;
      const slot = { deps, cleanup: previous?.cleanup };
      slots[index] = slot;
      effects.push(() => { slot.cleanup?.(); slot.cleanup = fn(); });
    },
  };
  const state = load("lib/editorState", { react });
  hook = load("components/useAutosave", { react, "@/lib/editorState": state }, {
    window: eventTarget(windowEvents), document: { ...eventTarget(new Map()), visibilityState: "visible" },
    setTimeout: () => 1, clearTimeout() {},
  }).useAutosave;
  function render() {
    cursor = 0; effects = [];
    result = hook(value, save);
    for (const run of effects) run();
  }
  render();
  return { state, edit: (next) => { value = next; render(); }, save: () => result.save(),
    beforeUnload: (event) => { for (const listener of windowEvents.get("beforeunload") ?? []) listener(event); } };
}

test("concurrent editor flushes serialize writes and persist edits arriving during a save", async () => {
  const first = deferred();
  const events = [];
  let active = 0;
  const e = editor(async (value) => {
    assert.equal(active++, 0, "two flushes must never overlap writes");
    events.push(value);
    if (value === "first") await first.promise;
    active--;
  });
  e.edit("first");
  const h = await updater({ flush: e.state.flushEditor, apply: async () => { events.push("update"); } });
  const update = h.store.applyUpdate();
  const concurrent = e.state.flushEditor();
  await tick();
  assert.deepEqual(events, ["first"]);
  e.edit("latest"); first.resolve();
  await Promise.all([update, concurrent]);
  assert.deepEqual(events, ["first", "latest", "update"]);
  assert.equal(e.state.isEditorDirty(), false);
});

test("real editor save failures block update and keep their own beforeunload safeguard", async () => {
  let failing = true;
  const e = editor(async () => { if (failing) throw new Error("Conflict on disk"); });
  e.edit("unsaved");
  const h = await updater({ flush: e.state.flushEditor });
  await h.store.applyUpdate(); await tick();
  assert.equal(h.requests(), 0);
  assert.equal(e.state.hasSaveError(), true);
  const blocked = unloadEvent(); e.beforeUnload(blocked);
  assert.equal(blocked.defaultPrevented, true);

  failing = false;
  await h.store.applyUpdate(); await tick();
  assert.equal(h.requests(), 1);
  assert.equal(h.guardsClose(), false);
  failing = true;
  e.edit("edit during download"); e.save(); await tick();
  const stillBlocked = unloadEvent(); e.beforeUnload(stillBlocked);
  assert.equal(stillBlocked.defaultPrevented, true, "the terminal exemption must not disable unsaved-editor protection");
});
