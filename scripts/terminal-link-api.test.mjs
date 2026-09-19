// Exercise the real HTTP adapter against mixed-version host responses. No
// server, filesystem fallback, or live terminal is involved.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

const source = readFileSync(new URL("../client/web/lib/api.ts", import.meta.url), "utf8")
  .replaceAll("import.meta.env", "({})");
const { outputText } = ts.transpileModule(source, {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
});

function client(reply) {
  const requests = [];
  const exports = {};
  vm.runInNewContext(outputText, {
    exports, Error,
    fetch: async (url, options) => {
      requests.push({ url, method: options.method, body: JSON.parse(options.body) });
      return reply();
    },
    require: (id) => {
      if (["@/lib/skill", "@/lib/agents", "./terminalAttachment"].includes(id)) return {};
      if (id === "@/lib/log") return { log: { debug() {} } };
      if (id === "./workspaceConnection") return { workspaceConnection: () => ({ available: true, epoch: 0 }) };
      throw new Error(`Unexpected import ${id}`);
    },
  }, { filename: "api.ts" });
  return { api: exports, requests };
}

function response(status, body) {
  return { ok: status === 200, status, json: async () => body };
}

test("missing file-link routes explain the host upgrade without guessing a directory", async () => {
  const { api, requests } = client(() => response(404, { error: "unknown API route /api/terminal/resolve-link" }));
  await assert.rejects(api.terminalResolveLink("ass-1-2-3", "plans/terminal-links-research.md"), (error) => {
    assert.match(error.message, /server v1\.2\.8 or newer on this host/);
    assert.match(error.message, /Update and restart its host service/);
    assert.match(error.message, /terminal sessions stay running/);
    assert.equal(error.cause.status, 404);
    return true;
  });
  assert.deepEqual(requests, [{
    url: "/api/terminal/resolve-link",
    method: "POST",
    body: { id: "ass-1-2-3", path: "plans/terminal-links-research.md" },
  }], "a missing resolver must not fall back to the saved launch directory or start an upgrade");
});

test("missing files, access errors and host outages retain their original details", async () => {
  for (const [status, message] of [
    [400, "File not found on the terminal host: absent.md"],
    [403, "Access denied"],
    [503, "Reconnecting to the host service…"],
  ]) {
    const { api, requests } = client(() => response(status, { error: message, message: "additional detail" }));
    await assert.rejects(api.terminalResolveLink("ass-1-2-3", "absent.md"), (error) => {
      assert.equal(error.message, message);
      assert.equal(error.status, status);
      assert.equal(error.detail, "additional detail");
      return true;
    });
    assert.equal(requests.length, 1);
  }
  const failure = new TypeError("Failed to fetch");
  const { api, requests } = client(() => { throw failure; });
  await assert.rejects(api.terminalResolveLink("ass-1-2-3", "file.md"), (error) => error === failure);
  assert.equal(requests.length, 1, "transport failures do not retry the POST");
});

test("supported hosts resolve the session and return its authoritative file location", async () => {
  const file = { path: "/remote/current/file.md", root: "/remote/current", rel: "file.md" };
  const { api, requests } = client(() => response(200, file));
  assert.equal(await api.terminalResolveLink("ass-1-2-3", "file.md"), file);
  assert.deepEqual(requests[0].body, { id: "ass-1-2-3", path: "file.md" });
});
