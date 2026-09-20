import assert from "node:assert/strict";
import test from "node:test";
import * as React from "react";
import * as jsxRuntime from "react/jsx-runtime";
import { renderToReadableStream } from "react-dom/server";
import { loadWebModule } from "./test-helpers.mjs";

const NOW = 1_800_000_000_000;
const nowSecs = NOW / 1000;
const session = (overrides = {}) => ({
  id: "ass-example", label: "Review project", agent: "codex", cwd: "/work/project",
  created: String(nowSecs - 86400), activity: String(nowSecs), bellAt: String(nowSecs),
  ...overrides,
});
const attention = (state, kind = null, changedAt = NOW - 7200_000) => ({
  state, kind, changedAt, sequence: "boot:2",
});

// Render the real dashboard and its cards. Only unrelated panels, HTTP and
// stores are stubbed; state labels and relative-time formatting execute normally.
async function dashboard(s, unread = false) {
  const empty = () => null;
  const dependencies = {
    react: React,
    "react/jsx-runtime": jsxRuntime,
    "react-router-dom": { useNavigate: () => () => {} },
    "@/components/ui": { Spinner: empty },
    "@/components/RemoteMenu": { RemoteDialog: empty },
    "@/components/useSessionTitleMenu": {
      useSessionTitleMenu: () => ({ titleProps: () => ({}), onTitleKeyDown() {}, menu: null }),
    },
    "@/lib/api": {},
    "@/lib/sessionTitle": loadWebModule("lib/sessionTitle.ts"),
    "@/lib/sessionAttention": loadWebModule("lib/sessionAttention.ts"),
    "@/lib/sessions": {
      useSessions: () => ({ sessions: [s], seen: {} }),
      isUnread: () => unread,
      refresh: async () => {},
      noteCreated() {},
      nativeNotifyState: () => true,
    },
    "@/lib/mining": { useMining: () => ({ status: "idle" }) },
    "@/lib/skills": { useSkills: () => ({ total: 0, dirtyRoots: new Set(), loading: false }) },
    "@/lib/connectors": { useConnectors: () => ({ inventory: { connectors: [] } }) },
    "@/lib/push": {},
    "@/lib/remote": { useRemote: () => ({ status: { state: "idle" } }) },
    "@/lib/routes": {},
  };
  for (const path of [
    "components/NavBar", "components/PhoneModal", "components/NewSessionDialog",
    "components/RenameSessionDialog", "components/RecentStrip", "components/OpenSkillDialog",
    "pages/home/SkillGallery",
  ]) dependencies[`@/${path}`] = { default: empty };
  const { Component } = loadWebModule("pages/home/DashboardRoute.tsx", {
    Date: class extends Date { static now() { return NOW; } },
    require: (id) => {
      if (id in dependencies) return dependencies[id];
      throw new Error(`Unexpected import ${id}`);
    },
  });
  const stream = await renderToReadableStream(React.createElement(Component));
  await stream.allReady;
  // React inserts comments between adjacent text expressions during SSR.
  return (await new Response(stream).text()).replace(/<!--.*?-->/g, "");
}

test("finished sessions stay finished after being read despite fresh terminal repainting", async () => {
  const s = session({ attention: attention("idle", "done") });
  for (const unread of [false, true]) {
    const html = await dashboard(s, unread);
    assert.match(html, /Codex · Finished 2h ago/);
    assert.doesNotMatch(html, /active just now|Working|Finished just now/);
    assert.equal(html.includes("Your turn"), unread);
  }
});

test("semantic working and input states override bell timestamps and read markers", async () => {
  for (const [state, kind, label] of [["working", null, "Working"], ["blocked", "request", "Needs input"]]) {
    for (const unread of [false, true]) {
      const html = await dashboard(session({ attention: attention(state, kind) }), unread);
      assert.ok(html.includes(`Codex · ${label}`));
      assert.doesNotMatch(html, /[Ff]inished|active just now/);
    }
  }
});

test("initial idle and unknown observations do not invent completion times", async () => {
  for (const [state, label] of [["idle", "Idle"], ["unknown", "Status unavailable"]]) {
    const html = await dashboard(session({ attention: attention(state, null, NOW) }));
    assert.ok(html.includes(`Codex · ${label}`));
    assert.doesNotMatch(html, /[Ff]inished|just now|Working/);
  }
  for (const changedAt of [0, NaN, Infinity]) {
    const html = await dashboard(session({ attention: attention("idle", "done", changedAt) }));
    assert.match(html, /Codex · Finished<\/span>/);
    assert.doesNotMatch(html, /NaN|Infinity|ago|just now/);
  }
});

test("legacy sessions describe the last completion without inferring current work", async () => {
  for (const unread of [false, true]) {
    const html = await dashboard(session({ bellAt: String(nowSecs - 10800) }), unread);
    assert.match(html, /Codex · Last finished 3h ago/);
    assert.doesNotMatch(html, /active just now|Working/);
  }
  for (const bellAt of ["0", "", "invalid", "Infinity"]) {
    const html = await dashboard(session({ bellAt }));
    assert.match(html, /Codex · Status unavailable/);
    assert.doesNotMatch(html, /[Ff]inished|active just now|Working/);
  }
  const shell = await dashboard(session({ agent: "shell", bellAt: "0" }));
  assert.match(shell, /Shell · Terminal open/);
  assert.doesNotMatch(shell, /active just now|Working/);
});
