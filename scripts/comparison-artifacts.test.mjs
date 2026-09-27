import assert from "node:assert/strict";
import test from "node:test";
import { loadWebModule } from "./test-helpers.mjs";

const { comparisonBelongsToSession, comparisonArtifactForSession, comparisonOwnersMatch } = loadWebModule("lib/comparisonArtifacts.ts", {
  require: (id) => {
    assert.equal(id, "./sessionTitle");
    return loadWebModule("lib/sessionTitle.ts");
  },
});
const terminal = { id: "terminal-one", agent: "codex", sessionId: "conversation-one", cwd: "/same/path", label: "Agent", customTitle: "Settings redesign" };
const artifact = (owner) => ({ config: { repository: terminal.cwd, artifact: owner ? { title: "Settings", owner } : null } });

test("UI artifacts require an exact host and session association, never just the same directory", () => {
  assert.equal(comparisonBelongsToSession(artifact(null), terminal, "local"), false);
  assert.equal(comparisonBelongsToSession(artifact({ hostId: "remote", terminalId: terminal.id }), terminal, "local"), false);
  assert.equal(comparisonBelongsToSession(artifact({ hostId: "local", terminalId: "another-terminal" }), terminal, "local"), false);
  assert.equal(comparisonBelongsToSession(artifact({ hostId: "local", terminalId: terminal.id }), terminal, "local"), true);
});

test("a native conversation follows resume without matching another provider or conversation", () => {
  const owner = { hostId: "local", terminalId: "old-terminal", provider: "codex", conversationId: terminal.sessionId };
  assert.equal(comparisonBelongsToSession(artifact(owner), terminal, "local"), true);
  assert.equal(comparisonBelongsToSession(artifact({ ...owner, provider: "claude" }), terminal, "local"), false);
  assert.equal(comparisonBelongsToSession(artifact({ ...owner, terminalId: terminal.id, conversationId: "another" }), terminal, "local"), false);
});

test("new artifacts carry all available exact identity and review metadata", () => {
  const metadata = comparisonArtifactForSession(terminal, "user@server", " Settings UI ", " Review empty state ");
  assert.deepEqual(JSON.parse(JSON.stringify(metadata)), {
    title: "Settings UI", description: "Review empty state",
    owner: { hostId: "user@server", terminalId: terminal.id, provider: "codex", conversationId: terminal.sessionId },
  });
  assert.equal(comparisonArtifactForSession(terminal, "local").title, "Settings redesign · UI diff");
});


test("toolbar siblings use conversation identity before a reused terminal", () => {
  const owner = { hostId: "local", terminalId: terminal.id, provider: "codex", conversationId: terminal.sessionId };
  assert.equal(comparisonOwnersMatch(owner, { ...owner, conversationId: "another" }), false);
  assert.equal(comparisonOwnersMatch(owner, { ...owner, provider: "claude" }), false);
  assert.equal(comparisonOwnersMatch(owner, { ...owner, terminalId: "resumed-terminal" }), true);
  assert.equal(comparisonOwnersMatch(owner, { ...owner, hostId: "remote" }), false);
  assert.equal(comparisonOwnersMatch(owner, { hostId: "local", terminalId: terminal.id }), true);
  assert.equal(comparisonOwnersMatch(owner, { hostId: "local", terminalId: terminal.id, provider: "claude" }), false);
  assert.equal(comparisonOwnersMatch({ ...owner, provider: undefined }, { ...owner, provider: undefined }), false);
  assert.equal(comparisonOwnersMatch(undefined, owner), false);
});
