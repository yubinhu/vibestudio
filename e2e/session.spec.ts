import { rename, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test, expect } from "./fixtures";

test("session status follows the agent and stays finished after reading and idle redraws", async ({ page, request, app }) => {
  const created = await request.post("/api/terminal/create", {
    data: { agent: "codex:cli", cwd: app.directory, cols: 80, rows: 24 },
  });
  expect(created.ok()).toBeTruthy();
  const session = await created.json() as { id: string; label: string };
  const setState = async (state: string) => {
    const path = join(app.directory, ".e2e-agent-state");
    await writeFile(`${path}.tmp`, `${state}\n`);
    await rename(`${path}.tmp`, path);
  };
  const getSession = async () => {
    const response = await request.get("/api/terminal/list");
    expect(response.ok()).toBeTruthy();
    const sessions = await response.json() as Array<{
      id: string;
      activity: string;
      attention: { state: string; kind: string | null; changedAt: number };
    }>;
    const current = sessions.find((item) => item.id === session.id);
    expect(current).toBeDefined();
    return current!;
  };

  await page.goto("/#/");
  const card = page.locator("main").getByRole("button").filter({ hasText: session.label });
  await expect(card).toContainText("Codex · Working");

  await setState("blocked");
  await expect(card).toContainText("Needs input");
  await setState("working");
  await expect(card).toContainText("Codex · Working");
  await setState("finished");
  await expect(card).toContainText("Finished just now");
  await expect(card).toContainText("Your turn");
  const finished = await getSession();

  await card.click();
  await expect(page).toHaveURL(new RegExp(`/sessions\\?id=${session.id}$`));
  await expect(page.locator(".xterm-screen")).toBeVisible();
  await page.getByTitle("Back to home", { exact: true }).click();
  await expect(card).toContainText("Finished");
  await expect(card).not.toContainText("Your turn");

  // Prove output really advanced, without a timer-based sleep, and ensure it
  // neither starts a new turn nor moves the recorded completion time forward.
  await expect.poll(async () => Number((await getSession()).activity)).toBeGreaterThan(Number(finished.activity));
  expect((await getSession()).attention).toEqual(finished.attention);
  await page.reload();
  await expect(card).toContainText("Finished");
  await expect(card).not.toContainText("Your turn");
});
