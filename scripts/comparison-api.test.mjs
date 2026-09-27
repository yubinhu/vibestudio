// Comparisons belong to the local desktop even when its selected workspace is
// unavailable. Starting owns processes/worktrees, so transport retries are unsafe.
import assert from "node:assert/strict";
import test from "node:test";
import { loadWebModule } from "./test-helpers.mjs";

function client(reply) {
  const requests = [];
  const api = loadWebModule("lib/api.ts", {
    Error,
    fetch: async (url, options) => {
      requests.push({ url, method: options.method, body: options.body ? JSON.parse(options.body) : undefined });
      return reply();
    },
    require(id) {
      if (["@/lib/skill", "@/lib/agents", "./terminalAttachment"].includes(id)) return {};
      if (id === "@/lib/log") return { log: { debug() {} } };
      if (id === "./workspaceConnection") return {
        workspaceConnection: () => ({ available: false, epoch: 4 }),
        workspaceUnavailable: () => new Error("Selected workspace is unavailable"),
      };
      throw new Error(`Unexpected import ${id}`);
    },
  }, (source) => source.replaceAll("import.meta.env", "({})"));
  return { api, requests };
}

test("comparison controls remain available while the selected remote workspace is unavailable", async () => {
  const session = { id: "comparison-1", state: "starting" };
  const { api, requests } = client(() => ({ ok: true, json: async () => session }));
  const config = { repository: "/repo", workingDirectory: "/tmp/agent-tree", baselineRef: "main", working: { command: "npm run dev" } };
  assert.equal(await api.comparisonStart(config), session);
  await api.comparisonUpdate(session.id, { syncScroll: false, route: "/account" });
  await api.comparisonStatus("comparison /1");
  await api.comparisonList();
  await api.comparisonStop(session.id);
  await api.comparisonOpen(session.id);
  await api.comparisonClose(session.id);
  await api.comparisonCapabilities();
  await api.comparisonDevices();
  await api.comparisonDevicesRefresh();
  assert.deepEqual(requests, [
    { url: "/api/comparison/start", method: "POST", body: config },
    { url: "/api/comparison/update", method: "POST", body: { id: session.id, syncScroll: false, route: "/account" } },
    { url: "/api/comparison/status?id=comparison%20%2F1", method: "GET", body: undefined },
    { url: "/api/comparison/list", method: "GET", body: undefined },
    { url: "/api/comparison/stop", method: "POST", body: { id: session.id } },
    { url: "/api/comparison/open", method: "POST", body: { id: session.id } },
    { url: "/api/comparison/close", method: "POST", body: { id: session.id } },
    { url: "/api/comparison/capabilities", method: "GET", body: undefined },
    { url: "/api/comparison/devices", method: "GET", body: undefined },
    { url: "/api/comparison/devices/refresh", method: "POST", body: {} },
  ]);
  await assert.rejects(api.readFile("/repo", "README.md"), /workspace is unavailable/);
  assert.equal(requests.length, 10, "ordinary workspace requests must still be blocked");
});

test("lost comparison mutation responses never replay resource creation or configuration changes", async () => {
  for (const operation of [
    (api) => api.comparisonStart({ repository: "/repo" }),
    (api) => api.comparisonUpdate("comparison-1", { route: "/next" }),
    (api) => api.comparisonStop("comparison-1"),
    (api) => api.comparisonOpen("comparison-1", { repository: "/repo", env: { PREVIEW: "fixture" } }),
    (api) => api.comparisonClose("comparison-1"),
    (api) => api.comparisonDevicesRefresh(),
  ]) {
    const error = new TypeError("Connection lost");
    const { api, requests } = client(() => { throw error; });
    await assert.rejects(operation(api), (actual) => actual === error);
    assert.equal(requests.length, 1);
  }
});
