import { chmod, copyFile, mkdir, readFile, unlink, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test, expect } from "./fixtures";

const olderId = "11111111-1111-4111-8111-111111111111";
const newerId = "22222222-2222-4222-8222-222222222222";

async function seedClaude(directory: string, id: string, title: string, cwd: string, timestamp: string) {
  const project = join(directory, "home/.claude/projects/history-fixture");
  await mkdir(project, { recursive: true });
  await writeFile(join(project, `${id}.jsonl`), [
    { type: "user", sessionId: id, cwd, timestamp, message: { role: "user", content: title } },
    { type: "custom-title", sessionId: id, customTitle: title },
  ].map((record) => JSON.stringify(record)).join("\n") + "\n");
}

test("past sessions search and resume the exact conversation, surviving terminal closure", async ({ page, request, app }) => {
  // Native records predate VibeStudio; both conversations deliberately share cwd.
  await seedClaude(app.directory, olderId, "Repair the older parser", app.directory, "2026-01-01T12:00:00Z");
  await seedClaude(app.directory, newerId, "Design the newer renderer", app.directory, "2026-02-01T12:00:00Z");
  await copyFile("e2e/codex.sh", join(app.binDirectory, "claude"));
  await chmod(join(app.binDirectory, "claude"), 0o700);

  await page.goto("/#/");
  await page.getByRole("button", { name: "Session history", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "Session history" });
  await expect(dialog.getByRole("button", { name: "Resume Repair the older parser", exact: true })).toBeVisible();
  const search = dialog.getByRole("searchbox", { name: "Search past sessions" });
  await search.fill("older parser");
  await expect(dialog.getByRole("button", { name: "Resume Design the newer renderer", exact: true })).toHaveCount(0);
  await dialog.getByRole("button", { name: "Resume Repair the older parser", exact: true }).click();
  await expect(dialog).not.toBeVisible();
  await expect(page.locator(".xterm-screen").first()).toBeVisible();
  await expect.poll(async () => readFile(join(app.directory, ".e2e-agent-args"), "utf8")).toBe(`--resume\n${olderId}\n`);

  const inventory = async () => {
    const response = await request.get("/api/terminal/list");
    expect(response.ok()).toBeTruthy();
    return await response.json() as Array<{ id: string; sessionId: string; cwd: string }>;
  };
  const live = await inventory();
  expect(live).toHaveLength(1);
  expect(live[0].sessionId).toBe(olderId);
  expect(live[0].cwd).toBe(app.directory);

  // Lost acknowledgements / repeated requests must open the same terminal.
  const duplicates = await Promise.all([0, 1].map(() => request.post("/api/terminal/history/resume", {
    data: { agent: "claude", sessionId: olderId },
  })));
  for (const response of duplicates) {
    expect(response.ok()).toBeTruthy();
    expect((await response.json()).id).toBe(live[0].id);
  }
  expect(await inventory()).toHaveLength(1);
  await page.getByRole("button", { name: "Session history", exact: true }).click();
  await expect(dialog.getByRole("button", { name: "Open Repair the older parser", exact: true })).toBeVisible();
  await dialog.getByRole("button", { name: "Open Repair the older parser", exact: true }).click();
  expect(await inventory()).toHaveLength(1);

  // Exiting the CLI preserves its scrollback shell, but must allow Resume.
  await writeFile(join(app.directory, ".e2e-agent-state"), "exit\n");
  await expect.poll(async () => {
    const result = await (await request.get("/api/terminal/history?query=older%20parser")).json();
    return result.sessions[0].activeTerminalId ?? null;
  }).toBeNull();
  await unlink(join(app.directory, ".e2e-agent-state"));
  await page.getByRole("button", { name: "Session history", exact: true }).click();
  await expect(dialog.getByRole("button", { name: "Resume Repair the older parser", exact: true })).toBeVisible();
  // Two fresh resumes race through real tmux creation, not just a live lookup.
  const restarted = await Promise.all([0, 1].map(async () => {
    const response = await request.post("/api/terminal/history/resume", { data: { agent: "claude", sessionId: olderId } });
    expect(response.ok()).toBeTruthy();
    return response.json();
  }));
  expect(restarted[0].id).toBe(restarted[1].id);
  expect(restarted[0].id).not.toBe(live[0].id);
  expect(await inventory()).toHaveLength(2);
  await dialog.getByRole("button", { name: "Resume Repair the older parser", exact: true }).click();
  await expect(dialog).not.toBeVisible();
  for (const terminal of await inventory()) {
    const killed = await request.post("/api/terminal/kill", { data: { id: terminal.id } });
    expect(killed.ok()).toBeTruthy();
  }
  await page.reload();
  await page.getByRole("button", { name: "Session history", exact: true }).click();
  await expect(dialog.getByRole("button", { name: "Resume Repair the older parser", exact: true })).toBeVisible();
  // Transcript remains owned by the provider; closing our terminal does not delete it.
  const history = await request.get("/api/terminal/history?query=older%20parser");
  const result = await history.json();
  expect(result.sessions).toHaveLength(1);
  expect(result.sessions[0].sessionId).toBe(olderId);
  expect(result.sessions[0].activeTerminalId).toBeUndefined();
});

test("history shows unavailable folders and rejects unknown conversations without launching", async ({ page, request, app }) => {
  await seedClaude(app.directory, olderId, "Work in a removed folder", join(app.directory, "removed"), "2026-01-01T12:00:00Z");
  await copyFile("e2e/codex.sh", join(app.binDirectory, "claude"));
  await chmod(join(app.binDirectory, "claude"), 0o700);
  await page.goto("/#/");
  await page.getByRole("button", { name: "Session history", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "Session history" });
  await expect(dialog.getByText("This session's working folder is no longer available.")).toBeVisible();
  await expect(dialog.getByRole("button", { name: "Resume Work in a removed folder", exact: true })).toBeDisabled();
  for (const sessionId of [olderId, newerId, "../../outside", "--latest"]) {
    const response = await request.post("/api/terminal/history/resume", { data: { agent: "claude", sessionId } });
    expect(response.ok()).toBeFalsy();
  }
  expect(await (await request.get("/api/terminal/list")).json()).toEqual([]);
});

test("Codex resumes a native rollout by exact ID and refreshes executable availability", async ({ request, app }) => {
  const sessions = join(app.directory, "home/.codex/sessions/2026/02/01");
  await mkdir(sessions, { recursive: true });
  await writeFile(join(sessions, `rollout-2026-02-01T12-00-00-${olderId}.jsonl`), [
    { type: "session_meta", timestamp: "2026-02-01T12:00:00Z", payload: {
      id: olderId, cwd: app.directory, source: "cli", timestamp: "2026-02-01T12:00:00Z",
    } },
    { type: "event_msg", timestamp: "2026-02-01T12:00:01Z", payload: {
      type: "user_message", message: "Repair the Codex parser",
    } },
  ].map((record) => JSON.stringify(record)).join("\n") + "\n");
  const history = async () => {
    const response = await request.get("/api/terminal/history?agent=codex");
    expect(response.ok()).toBeTruthy();
    return (await response.json()).sessions;
  };
  expect(await history()).toMatchObject([{ sessionId: olderId, canResume: true }]);
  const response = await request.post("/api/terminal/history/resume", { data: { agent: "codex", sessionId: olderId } });
  expect(response.ok()).toBeTruthy();
  const session = await response.json();
  expect(session.sessionId).toBe(olderId);
  await expect.poll(async () => readFile(join(app.directory, ".e2e-agent-args"), "utf8")).toBe(`resume\n${olderId}\n`);
  expect((await request.post("/api/terminal/kill", { data: { id: session.id } })).ok()).toBeTruthy();

  await unlink(join(app.binDirectory, "codex"));
  expect(await history()).toMatchObject([{ sessionId: olderId, canResume: false }]);
  await copyFile("e2e/codex.sh", join(app.binDirectory, "codex"));
  await chmod(join(app.binDirectory, "codex"), 0o700);
  expect(await history()).toMatchObject([{ sessionId: olderId, canResume: true }]);
});
