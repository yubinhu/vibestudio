import assert from "node:assert/strict";
import test from "node:test";
import { loadWebModule } from "./test-helpers.mjs";

const bundled = { sourceUrl: "chrome-source", checkedAt: null, devices: [{ id: "offline", label: "Offline", group: "Phones", width: 390, height: 844, showByDefault: true, order: 0 }] };
const cached = { ...bundled, refreshing: false };
const settle = () => new Promise((resolve) => setImmediate(resolve));
function harness(reads, sync = { ...bundled, refreshing: true }) {
  const timers = new Map();
  let nextTimer = 0;
  let calls = 0;
  let syncs = 0;
  const store = loadWebModule("lib/comparisonDevices.ts", {
    require: (id) => {
      if (id === "react") return {};
      if (id === "./comparisonDevices.generated.json") return { default: bundled };
      if (id === "./api") return {
        comparisonDevices: async () => {
          const response = reads[calls++];
          if (response instanceof Error) throw response;
          return response;
        },
        comparisonDevicesRefresh: async () => { syncs++; if (sync instanceof Error) throw sync; return sync; },
      };
      throw new Error(`Unexpected import: ${id}`);
    },
    setTimeout: (callback, ms) => { const id = ++nextTimer; timers.set(id, { callback, ms }); return id; },
    clearTimeout: (id) => timers.delete(id),
  });
  return { store, timers, calls: () => calls, syncs: () => syncs };
}

test("opening the picker reads saved data once and schedules no background checks", async () => {
  const h = harness([cached]);
  const unsubscribe = h.store.subscribeComparisonDevices(() => {});
  await settle();
  assert.equal(h.calls(), 1);
  assert.equal(h.syncs(), 0);
  assert.equal(h.timers.size, 0);
  unsubscribe();
});

test("explicit sync polls only while requested work is pending and stops after completion", async () => {
  const fresh = { ...cached, checkedAt: 123, devices: [{ ...bundled.devices[0], id: "fresh" }] };
  const h = harness([cached, fresh]);
  const unsubscribe = h.store.subscribeComparisonDevices(() => {});
  await settle();
  await h.store.syncComparisonDevices();
  assert.equal(h.syncs(), 1);
  const [id, timer] = [...h.timers.entries()][0];
  assert.equal(timer.ms, 1000);
  h.timers.delete(id); timer.callback();
  await settle();
  assert.equal(h.store.comparisonDevicesSnapshot().devices[0].id, "fresh");
  assert.equal(h.timers.size, 0);
  unsubscribe();
});

test("offline reads and failed manual syncs preserve data without retries", async () => {
  const h = harness([new Error("offline")], new Error("sync offline"));
  const unsubscribe = h.store.subscribeComparisonDevices(() => {});
  await settle();
  assert.equal(h.store.comparisonDevicesSnapshot().devices[0].id, "offline");
  assert.equal(h.syncs(), 0);
  assert.equal(h.timers.size, 0);
  await h.store.syncComparisonDevices();
  assert.equal(h.syncs(), 1);
  assert.equal(h.store.comparisonDevicesSnapshot().devices[0].id, "offline");
  assert.equal(h.store.comparisonDevicesSnapshot().refreshing, false);
  assert.ok(h.store.comparisonDevicesSnapshot().error);
  assert.equal(h.timers.size, 0);
  unsubscribe();
});
