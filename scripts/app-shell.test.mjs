import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import * as React from "react";
import * as jsxRuntime from "react/jsx-runtime";
import { renderToReadableStream } from "react-dom/server";
import ts from "typescript";

function load(path, dependencies) {
  const source = readFileSync(new URL(`../client/web/${path}.tsx`, import.meta.url), "utf8");
  const { outputText } = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
  });
  const exports = {};
  vm.runInNewContext(outputText, {
    exports,
    require: (id) => {
      if (id in dependencies) return dependencies[id];
      throw new Error(`Unexpected import ${id}`);
    },
  }, { filename: `${path}.tsx` });
  return exports;
}

// Render the real shell and recovery dialog with React. Stub the workspace
// contents so the test measures input gating without loading terminals or HTTP.
async function shell({ pathname = "/", mobile = true, state = "reconnecting", host = "workstation" } = {}) {
  const empty = () => null;
  const dependencies = {
    react: React,
    "react/jsx-runtime": jsxRuntime,
    "react-router-dom": {
      useLocation: () => ({ pathname }),
      Outlet: () => React.createElement("button", { type: "button" }, "Workspace navigation"),
    },
    "@/components/ui": {
      Spinner: () => React.createElement("span", { role: "status" }, "Loading"),
      btnGhost: "ghost",
      btnPrimary: "primary",
    },
    "@/components/PhoneModal": { default: empty },
    "@/components/UpdateBanner": { default: empty },
    "@/pages/sessions/SessionsHost": {
      default: ({ active }) => React.createElement("section", { "data-sessions-active": active }, "Mounted sessions"),
    },
    "@/pages/MobileConnect": {
      default: () => React.createElement("button", { type: "button" }, "Connect to a computer"),
    },
    "@/lib/remote": {
      useRemote: () => ({
        mobile,
        status: { state, host },
        workspaceHost: host,
        interrupted: host !== null && state !== "connected",
        retry: async () => {},
        disconnect: async () => {},
      }),
    },
    "@/lib/api": { notifyPrime: async () => {} },
    "@/lib/log": { log: { warn() {} } },
    "./routeGuard": { useDiscardBlocker() {} },
  };
  dependencies["./ui"] = dependencies["@/components/ui"];
  dependencies["@/components/RemoteRecovery"] = load("components/RemoteRecovery", dependencies);
  const { default: AppShell } = load("app/AppShell", dependencies);
  const stream = await renderToReadableStream(React.createElement(AppShell));
  await stream.allReady;
  return new Response(stream).text();
}

function assertUnlocked(html) {
  assert.doesNotMatch(html, /\sinert(?:[=>\s])/);
  assert.doesNotMatch(html, /role="alertdialog"/);
}

for (const mobile of [true, false]) {
  test(`${mobile ? "mobile" : "desktop"} Home navigation remains available during remote recovery`, async () => {
    for (const state of ["reconnecting", "error"]) {
      const html = await shell({ mobile, state });
      assertUnlocked(html);
      assert.match(html, /<button type="button">Workspace navigation<\/button>/);
      assert.match(html, /Mounted sessions/);
    }
  });
}

for (const pathname of ["/sessions", "/skills/example"]) {
  test(`${pathname} preserves and locks its remote workspace during recovery`, async () => {
    for (const mobile of [true, false]) {
      for (const state of ["reconnecting", "error"]) {
        const html = await shell({ pathname, mobile, state });
        assert.match(html, /<div class="contents" inert="">/);
        assert.match(html, /role="alertdialog"/);
        assert.match(html, /Mounted sessions/);
        assert.match(html, /Workspace navigation/);
        assert.match(html, />Retry<\/button>/);
        assert.match(html, />Disconnect<\/button>/);
      }
    }
  });
}

test("connected remote workspaces are unlocked on every route", async () => {
  for (const pathname of ["/", "/sessions", "/skills/example"]) {
    const html = await shell({ pathname, state: "connected" });
    assertUnlocked(html);
    assert.match(html, /Workspace navigation/);
    assert.match(html, /Mounted sessions/);
  }
});

test("disconnected mobile opens its connection screen without mounting a workspace", async () => {
  for (const state of ["idle", "error"]) {
    const html = await shell({ state, host: null });
    assertUnlocked(html);
    assert.match(html, />Connect to a computer<\/button>/);
    assert.doesNotMatch(html, /Workspace navigation|Mounted sessions/);
  }
});

test("desktop Home remains usable without a remote connection", async () => {
  const html = await shell({ mobile: false, state: "idle", host: null });
  assertUnlocked(html);
  assert.match(html, /Workspace navigation/);
  assert.doesNotMatch(html, /Connect to a computer/);
});
