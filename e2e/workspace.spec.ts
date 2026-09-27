import { readFile } from "node:fs/promises";
import { join } from "node:path";
import type { Page } from "@playwright/test";
import { test, expect } from "./fixtures";

async function openFile(page: Page, name: string) {
  if ((page.viewportSize()?.width ?? 0) < 640) {
    await page.getByTitle("Files & versions", { exact: true }).click();
  }
  // The same file also appears under New Changes after it has been edited.
  await page.getByRole("button", { name, exact: true }).and(page.getByTitle(name, { exact: true })).click();
  await expect(page).toHaveURL((url) => url.hash.endsWith(`/file/${encodeURIComponent(name)}`));
}

test("Home searches discovered skills and remembers the opened skill after reload", async ({ page, app }) => {
  await page.goto("/");
  const results = page.locator("#skill-search-results");
  const primary = results.getByRole("button", { name: /^e2e-primary\b/ });
  const secondary = results.getByRole("button", { name: /^e2e-secondary\b/ });
  await expect(primary).toBeVisible();
  await expect(secondary).toBeVisible();

  await page.getByRole("searchbox", { name: "Search skills" }).fill("e2e-primary");
  await expect(primary).toBeVisible();
  await expect(secondary).toHaveCount(0);
  await page.getByRole("button", { name: "Clear search", exact: true }).click();
  await expect(secondary).toBeVisible();

  await primary.click();
  const skillURL = new URL(`/#/skills/${encodeURIComponent(app.skillRoot)}`, app.baseURL).href;
  await expect(page).toHaveURL(skillURL);
  await expect(page.getByRole("textbox", { name: "Skill name", exact: true })).toHaveValue("e2e-primary");
  await expect(page.getByRole("textbox", { name: "Skill description", exact: true })).toHaveValue("Primary E2E skill");

  await page.getByTitle("Back to home", { exact: true }).click();
  const recent = page.getByRole("region", { name: "Recent", exact: true })
    .getByRole("button", { name: "e2e-primary", exact: true });
  await expect(recent).toBeVisible();
  await page.reload();
  await expect(recent).toBeVisible();
  await recent.click();
  await expect(page).toHaveURL(skillURL);
  await expect(page.getByRole("textbox", { name: "Skill name", exact: true })).toHaveValue("e2e-primary");
});

test("skill and file edits survive navigation and reload", async ({ page, app }) => {
  await page.goto(`/#/skills/${encodeURIComponent(app.skillRoot)}`);
  const description = "Updated through the skill editor.";
  await page.getByRole("textbox", { name: "Skill description", exact: true }).fill(description);
  // Navigate immediately: leaving an editor must flush its pending autosave.
  await openFile(page, "notes.md");
  await expect.poll(() => readFile(join(app.skillRoot, "SKILL.md"), "utf8")).toContain(description);

  const editor = page.locator("main").getByRole("textbox");
  await expect(editor).toContainText("Original note.");
  const notes = "# Notes\n\nSaved through the file editor.";
  await editor.fill(notes);
  await page.getByTitle("Back to home", { exact: true }).click();
  await expect.poll(() => readFile(join(app.skillRoot, "notes.md"), "utf8")).toBe(notes);

  await page.getByRole("region", { name: "Recent", exact: true })
    .getByRole("button", { name: "e2e-primary", exact: true }).click();
  await expect(page).toHaveURL(new URL(`/#/skills/${encodeURIComponent(app.skillRoot)}`, app.baseURL).href);
  await page.reload();
  await expect(page.getByRole("textbox", { name: "Skill description", exact: true })).toHaveValue(description);
  await openFile(page, "notes.md");
  await page.reload();
  await expect(editor).toContainText("Saved through the file editor.");
});
