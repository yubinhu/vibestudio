import { test, expect } from "./fixtures";

// These tests control response order with page.route. A service-worker-controlled
// page can bypass interception in WebKit; leave workers enabled in real API tests.
test.use({ serviceWorkers: "block" });

const entry = (id: string, title: string) => ({
  agent: "claude", sessionId: id, title, cwd: "/work/project",
  createdAt: 1_700_000_000, updatedAt: 1_800_000_000, canResume: true,
});
const historyRoute = /\/api\/terminal\/history(?:\?|$)/;

test("the latest history search survives an older response arriving last", async ({ page }) => {
  let releaseOlder!: () => void;
  const olderGate = new Promise<void>((resolve) => { releaseOlder = resolve; });
  await page.route(historyRoute, async (route) => {
    const query = new URL(route.request().url()).searchParams.get("query") ?? "";
    if (query === "older") await olderGate;
    await route.fulfill({ json: {
      sessions: [entry(query || "initial", query === "newest" ? "Newest match" : query === "older" ? "Stale older match" : "Initial conversation")],
      warnings: [], hasMore: false, truncated: false,
    } });
  });

  await page.goto("/#/");
  await page.getByRole("button", { name: "Session history", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "Session history" });
  const search = dialog.getByRole("searchbox", { name: "Search past sessions" });
  await expect(dialog.getByRole("button", { name: "Resume Initial conversation", exact: true })).toBeVisible();
  const olderRequest = page.waitForRequest((request) => new URL(request.url()).searchParams.get("query") === "older");
  await search.fill("older");
  await olderRequest;
  await search.fill("newest");
  await expect(dialog.getByRole("button", { name: "Resume Newest match", exact: true })).toBeVisible();

  const olderResponse = page.waitForResponse((response) => new URL(response.url()).searchParams.get("query") === "older");
  releaseOlder();
  await (await olderResponse).finished();
  // Let the completed fetch and React paint settle before checking that its
  // obsolete result cannot replace the current query's selectable row.
  await page.evaluate(() => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));
  await expect(dialog.getByRole("button", { name: "Resume Newest match", exact: true })).toBeVisible();
  await expect(dialog.getByRole("button", { name: "Resume Stale older match", exact: true })).toHaveCount(0);
});

test("history paginates matches but does not offer more for an incomplete scan", async ({ page }) => {
  const offsets: number[] = [];
  await page.route(historyRoute, async (route) => {
    const url = new URL(route.request().url());
    const offset = Number(url.searchParams.get("offset") ?? 0);
    offsets.push(offset);
    expect(url.searchParams.get("limit")).toBe("50");
    await route.fulfill({ json: offset === 0 ? {
      sessions: Array.from({ length: 50 }, (_, index) => entry(`page-one-${index}`, `Saved conversation ${index + 1}`)),
      warnings: [], hasMore: true, truncated: false,
    } : {
      sessions: [entry("last-entry", "Last available conversation")],
      warnings: [], hasMore: false, truncated: true,
    } });
  });

  await page.goto("/#/");
  await page.getByRole("button", { name: "Session history", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "Session history" });
  await dialog.getByRole("button", { name: "Load more", exact: true }).click();
  await expect(dialog.getByRole("button", { name: "Resume Last available conversation", exact: true })).toBeVisible();
  await expect(dialog.getByText("Some older sessions may be missing from this history.")).toBeVisible();
  await expect(dialog.getByRole("button", { name: "Load more", exact: true })).toHaveCount(0);
  expect(offsets).toEqual([0, 50]);
  await expect(dialog.getByRole("list", { name: "Past sessions" }).getByRole("listitem")).toHaveCount(51);
});
