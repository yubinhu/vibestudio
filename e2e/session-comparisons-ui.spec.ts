import { test, expect } from "./fixtures";
import type { ComparisonConfig, ComparisonDeviceCatalog, ComparisonSession, TermSession } from "../client/web/lib/api";
import type { Page } from "@playwright/test";
import { readFileSync } from "node:fs";
const deviceCatalog = JSON.parse(readFileSync(new URL("../client/web/lib/comparisonDevices.generated.json", import.meta.url), "utf8")) as ComparisonDeviceCatalog;

test.use({ serviceWorkers: "block" });

function comparison(id: string, title: string, session: TermSession, overrides: Partial<ComparisonSession> = {}): ComparisonSession {
  return {
    id, state: "ready", windowOpen: false, windowRequested: false, presentationRevision: 0, restoreRequired: false,
    config: {
      repository: "/work/source", workingDirectory: "/tmp/agent-worktree", baselineRef: "main", baseline: {}, working: {}, env: {}, route: "/settings",
      viewport: { width: 390, height: 844, orientation: "portrait", preset: "phone" }, syncScroll: true,
      artifact: { title, description: "Review the settings layout and empty state.", owner: { hostId: "local", terminalId: session.id, provider: session.agent } },
    },
    baselineSha: "a".repeat(40), baselineWorktree: "/tmp/baseline", baselineUrl: "http://127.0.0.1:3001/settings", workingUrl: "http://127.0.0.1:3002/settings",
    baselineExternal: false, workingExternal: false, baselineLog: "/tmp/baseline.log", workingLog: "/tmp/working.log", error: null,
    createdAt: 1_800_000_000_000, updatedAt: 1_800_000_000_000, ...overrides,
  };
}

async function comparisonApi(page: Page, initial: ComparisonSession[]) {
  let entries = initial;
  const actions: Array<{ operation: string; body: Record<string, unknown> }> = [];
  await page.route(/\/api\/comparison\//, async (route) => {
    const operation = new URL(route.request().url()).pathname.split("/").at(-1)!;
    if (operation === "capabilities") return route.fulfill({ json: { available: true, protocol: 1, nativeWebviews: true, sessionArtifacts: true } });
    if (operation === "list") return route.fulfill({ json: entries });
    if (operation === "devices") return route.fulfill({ json: { ...deviceCatalog, refreshing: false } });
    const body = route.request().postDataJSON() as Record<string, unknown>;
    actions.push({ operation, body });
    const found = entries.find((entry) => entry.id === body.id);
    if (operation === "start") {
      const fresh = { ...initial[0], id: "new-artifact", config: body as unknown as ComparisonSession["config"], state: "starting" as const, windowRequested: true, windowOpen: false };
      entries = [fresh, ...entries];
      return route.fulfill({ json: fresh });
    }
    if (!found) return route.fulfill({ status: 404, json: { error: "Missing comparison" } });
    const next: ComparisonSession = { ...found, updatedAt: Date.now() };
    if (operation === "open") { next.windowOpen = true; next.windowRequested = true; next.state = "ready"; }
    if (operation === "close") { next.windowOpen = false; next.windowRequested = false; }
    if (operation === "stop") { next.windowOpen = false; next.windowRequested = false; next.state = "stopped"; }
    if (operation === "update") next.config = { ...next.config, ...body };
    entries = entries.map((entry) => entry.id === next.id ? next : entry);
    return route.fulfill({ json: next });
  });
  return actions;
}

test("session artifacts are scoped, browsable and independently opened, closed and reopened", async ({ page, request, app }) => {
  const created = await request.post("/api/terminal/create", { data: { agent: "codex:cli", cwd: app.directory, cols: 80, rows: 24 } });
  expect(created.ok()).toBeTruthy();
  const session = await created.json() as TermSession;
  const first = comparison("first", "Settings layout", session, { createdAt: 1_800_000_002_000 });
  const second = comparison("second", "Empty state", session);
  const foreign = comparison("other-host", "Other host secret", session);
  foreign.config.artifact!.owner.hostId = "harvey@other-host";
  const unrelated = comparison("other-session", "Other session", session);
  unrelated.config.artifact!.owner.terminalId = "unrelated-terminal";
  const legacy = comparison("legacy", "Legacy preview", session);
  legacy.config.artifact = null;
  const actions = await comparisonApi(page, [first, second, foreign, unrelated, legacy]);
  await page.goto(`/#/sessions?id=${encodeURIComponent(session.id)}`);
  const diffControl = page.getByRole("button", { name: /^UI diffs for .+ \(2\)$/ });
  await expect(diffControl).toBeVisible();
  await expect(page.locator(".xterm-screen")).toBeVisible();
  const isNarrow = (page.viewportSize()?.width ?? 0) < 640;
  expect(await diffControl.evaluate((button) => button.closest("header") !== null)).toBe(!isNarrow);
  if (isNarrow) {
    expect(await diffControl.evaluate((button) => button.parentElement?.querySelector('select[aria-label="Session"]') !== null)).toBe(true);
  }
  // The terminal starts directly in its existing workspace; UI diffs add no row.
  const terminalBox = await page.locator(".xterm-screen").boundingBox();
  const workspaceBox = await page.locator("main").boundingBox();
  expect(terminalBox!.y - workspaceBox!.y).toBeCloseTo(6, 0);
  await diffControl.click();
  const dialog = page.getByRole("dialog", { name: /^UI diffs for / });
  expect(await dialog.evaluate((element) => element.parentElement === document.body)).toBe(true);
  await expect(dialog.getByRole("list", { name: "UI diff artifacts" }).getByRole("listitem")).toHaveCount(2);
  await expect(dialog.getByText("Other host secret")).toHaveCount(0);
  await expect(dialog.getByText("Other session", { exact: true })).toHaveCount(0);
  const detail = dialog.getByRole("region", { name: "Selected UI diff" });
  await expect(detail.getByRole("heading", { name: "Settings layout" })).toBeVisible();
  await expect(detail.getByText("a".repeat(40))).toBeVisible();
  await expect(detail.getByText("/tmp/agent-worktree", { exact: true })).toBeVisible();
  await detail.getByRole("button", { name: "Open UI diff", exact: true }).click();
  await expect(detail.getByRole("button", { name: "Focus window" })).toBeVisible();
  await detail.getByRole("button", { name: "Close window" }).click();
  await expect(detail.getByRole("button", { name: "Open UI diff", exact: true })).toBeVisible();
  await dialog.getByRole("button", { name: "Select Empty state" }).click();
  await expect(detail.getByRole("heading", { name: "Empty state" })).toBeVisible();
  await detail.getByRole("button", { name: "Stop previews" }).click();
  await detail.getByRole("button", { name: "Reopen UI diff" }).click();
  await expect(detail.getByRole("button", { name: "Focus window" })).toBeVisible();
  await dialog.getByRole("tab", { name: "Unassigned (1)" }).click();
  await detail.getByRole("button", { name: "Attach to this session" }).click();
  await expect(dialog.getByRole("list", { name: "UI diff artifacts" }).getByRole("listitem")).toHaveCount(3);
  expect(actions.filter((action) => action.operation !== "update").map((action) => [action.operation, action.body.id])).toEqual([
    ["open", "first"], ["close", "first"], ["stop", "second"], ["open", "second"],
  ]);
  const attachment = actions.find((action) => action.operation === "update" && action.body.id === "legacy");
  expect(attachment?.body.artifact).toMatchObject({ owner: { hostId: "local", terminalId: session.id, provider: session.agent } });
  await dialog.getByRole("button", { name: "Close", exact: true }).click();
  await expect(page.getByRole("button", { name: /^UI diffs for .+ \(3\)$/ })).toBeFocused();
});

test("new UI diffs preserve explicit paths and settings and attach exact session identity", async ({ page, request, app }) => {
  const created = await request.post("/api/terminal/create", { data: { agent: "codex:cli", cwd: app.directory, cols: 80, rows: 24 } });
  const session = await created.json() as TermSession;
  const template = comparison("template", "Template", session);
  const actions = await comparisonApi(page, [template]);
  await page.goto(`/#/sessions?id=${encodeURIComponent(session.id)}`);
  await page.getByRole("button", { name: /^UI diffs for / }).click();
  const dialog = page.getByRole("dialog", { name: /^UI diffs for / });
  await dialog.getByRole("button", { name: "＋ New UI diff" }).click();
  await dialog.getByLabel("Title", { exact: true }).fill("Phone navigation");
  await dialog.getByLabel("Description", { exact: true }).fill("Review the new drawer and selected item.");
  await dialog.getByLabel("Source repository", { exact: true }).fill("/work/source");
  await dialog.getByLabel("Working directory", { exact: true }).fill("/tmp/different-agent-worktree");
  await dialog.getByLabel("Baseline ref", { exact: true }).fill("origin/review");
  await dialog.getByLabel("Route", { exact: true }).fill("/navigation");
  await dialog.getByRole("combobox", { name: "Device preset" }).selectOption("ipad-mini");
  await dialog.getByRole("button", { name: "Rotate", exact: true }).click();
  await expect(dialog.getByLabel("Width (CSS px)", { exact: true })).toHaveValue("1024");
  await expect(dialog.getByLabel("Height (CSS px)", { exact: true })).toHaveValue("768");
  await dialog.getByText("Servers and advanced settings", { exact: true }).click();
  await dialog.getByLabel("Baseline existing URL", { exact: true }).fill("http://127.0.0.1:4111");
  await dialog.getByLabel("Working command", { exact: true }).fill("npm run preview -- --port {port}");
  await dialog.getByLabel("Working port", { exact: true }).fill("4222");
  await dialog.getByLabel("Baseline worktree location", { exact: true }).fill("/tmp/custom-baseline");
  await dialog.getByLabel("Shared environment (JSON)", { exact: true }).fill('{"PREVIEW_MODE":"true"}');
  await dialog.getByRole("button", { name: "Start UI diff", exact: true }).click();
  await expect(dialog.getByRole("heading", { name: "Phone navigation" })).toBeVisible();
  const config = actions.find((action) => action.operation === "start")?.body as unknown as ComparisonConfig;
  expect(config).toMatchObject({
    repository: "/work/source", workingDirectory: "/tmp/different-agent-worktree", baselineRef: "origin/review", baselineWorktree: "/tmp/custom-baseline",
    viewport: { preset: "ipad-mini", width: 1024, height: 768, orientation: "landscape" },
    route: "/navigation", baseline: { url: "http://127.0.0.1:4111" }, working: { command: "npm run preview -- --port {port}", port: 4222 }, env: { PREVIEW_MODE: "true" },
    artifact: { title: "Phone navigation", description: "Review the new drawer and selected item.", owner: { hostId: "local", terminalId: session.id, provider: session.agent } },
  });
});

test("session UI hides native comparison controls on an unsupported server", async ({ page, request, app }) => {
  const created = await request.post("/api/terminal/create", { data: { agent: "codex:cli", cwd: app.directory, cols: 80, rows: 24 } });
  const session = await created.json() as TermSession;
  await page.route("**/api/comparison/capabilities", (route) => route.fulfill({ status: 404, json: { error: "Desktop comparison is unavailable." } }));
  const capabilities = page.waitForResponse("**/api/comparison/capabilities");
  await page.goto(`/#/sessions?id=${encodeURIComponent(session.id)}`);
  await capabilities;
  await expect(page.locator(".xterm-screen")).toBeVisible();
  await expect(page.getByRole("button", { name: /^UI diffs for / })).toHaveCount(0);
});


test("comparison toolbar selects named CSS viewports, rotates and applies custom dimensions", async ({ page }, testInfo) => {
  test.skip(testInfo.project.name === "phone", "The native comparison toolbar is a desktop surface.");
  await page.setViewportSize({ width: 860, height: 480 });
  const session: TermSession = { id: "viewport-owner", label: "Viewport review", agent: "codex", cwd: "/work/source", created: "1", activity: "1", bellAt: "0" };
  const actions = await comparisonApi(page, [comparison("viewport", "Viewport review", session)]);
  await page.goto("/#/comparison/viewport");
  const picker = page.getByRole("combobox", { name: "Device preset" });
  const width = page.getByRole("spinbutton", { name: "Viewport width (CSS px)" });
  const height = page.getByRole("spinbutton", { name: "Viewport height (CSS px)" });
  await expect(picker).toHaveValue("custom");
  await expect(picker.locator("option:checked")).toHaveText("Responsive · custom size");
  await picker.selectOption("iphone-14");
  await expect(picker.locator("option:checked")).toHaveText("iPhone 14 · 390 × 844 CSS px");
  await picker.selectOption("pixel-7");
  await expect(width).toHaveValue("412");
  await expect(height).toHaveValue("915");
  await page.getByRole("button", { name: "Rotate both viewports" }).click();
  await expect(picker).toHaveValue("pixel-7");
  await expect(width).toHaveValue("915");
  await expect(height).toHaveValue("412");
  await expect(picker.locator("option:checked")).toHaveText("Pixel 7 · 915 × 412 CSS px");
  await width.fill("700");
  await expect(picker).toHaveValue("custom");
  await height.fill("900");
  await page.getByRole("button", { name: "Apply", exact: true }).click();
  await expect.poll(() => actions.filter((action) => action.operation === "update").at(-1)?.body.viewport).toEqual({ preset: "custom", width: 700, height: 900, orientation: "portrait" });
  await picker.selectOption("laptop");
  await expect(width).toHaveValue("1280");
  await expect(height).toHaveValue("800");
  await expect(picker.locator("option:checked")).toHaveText("Laptop (generic) · 1280 × 800 CSS px");
  const toolbar = await page.getByRole("main", { name: "Live UI comparison" }).boundingBox();
  expect(toolbar?.height).toBe(176);
  const sync = await page.getByRole("checkbox", { name: "Sync scroll" }).boundingBox();
  expect(sync!.x + sync!.width).toBeLessThan(860);
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(860);
});


test("explicit Chrome sync adds models without changing the selected viewport", async ({ page }, testInfo) => {
  test.skip(testInfo.project.name === "phone", "The native comparison toolbar is a desktop surface.");
  const session: TermSession = { id: "catalog-owner", label: "Catalog review", agent: "codex", cwd: "/work/source", created: "1", activity: "1", bellAt: "0" };
  const actions = await comparisonApi(page, [comparison("catalog", "Catalog review", session)]);
  let requests = 0;
  let syncs = 0;
  await page.route("**/api/comparison/devices/refresh", async (route) => {
    expect(route.request().method()).toBe("POST");
    syncs++;
    await route.fulfill({ json: { ...deviceCatalog, refreshing: true } });
  });
  await page.route("**/api/comparison/devices", async (route) => {
    requests++;
    await route.fulfill({ json: {
      ...deviceCatalog,
      refreshing: false,
      checkedAt: syncs ? Date.now() : null,
      devices: !syncs ? deviceCatalog.devices : [...deviceCatalog.devices, {
        id: "catalog-fixture-phone", label: "Catalog fixture phone", group: "Phones", width: 401, height: 877, order: 0, showByDefault: true,
      }],
    } });
  });
  await page.goto("/#/comparison/catalog");
  const picker = page.getByRole("combobox", { name: "Device preset" });
  const width = page.getByRole("spinbutton", { name: "Viewport width (CSS px)" });
  const height = page.getByRole("spinbutton", { name: "Viewport height (CSS px)" });
  await expect(width).toHaveValue("390");
  await expect.poll(() => requests).toBe(1);
  expect(syncs).toBe(0);
  await picker.selectOption("__sync_chrome_devices__");
  await expect(picker).toHaveValue("custom");
  await expect(picker.locator('option[value="catalog-fixture-phone"]')).toHaveCount(1);
  await expect(width).toHaveValue("390");
  await expect(height).toHaveValue("844");
  await picker.selectOption("catalog-fixture-phone");
  await expect.poll(() => actions.filter((action) => action.operation === "update").at(-1)?.body.viewport).toEqual({ preset: "catalog-fixture-phone", width: 401, height: 877, orientation: "portrait" });
  await expect(picker.locator("option:checked")).toHaveText("Catalog fixture phone · 401 × 877 CSS px");
  expect(requests).toBe(2);
  expect(syncs).toBe(1);
});


test("device picker remains usable from its bundled catalog when refresh is unavailable", async ({ page }, testInfo) => {
  test.skip(testInfo.project.name === "phone", "The native comparison toolbar is a desktop surface.");
  const session: TermSession = { id: "offline-owner", label: "Offline review", agent: "codex", cwd: "/work/source", created: "1", activity: "1", bellAt: "0" };
  const actions = await comparisonApi(page, [comparison("offline", "Offline review", session)]);
  await page.route("**/api/comparison/devices", (route) => route.fulfill({ status: 404, json: { error: "Unavailable" } }));
  await page.goto("/#/comparison/offline");
  const picker = page.getByRole("combobox", { name: "Device preset" });
  const device = deviceCatalog.devices.find((item) => item.showByDefault)!;
  await picker.selectOption(device.id);
  await expect.poll(() => actions.filter((action) => action.operation === "update").at(-1)?.body.viewport).toMatchObject({ preset: device.id, width: device.width, height: device.height });
  await expect(page.getByRole("spinbutton", { name: "Viewport width (CSS px)" })).toHaveValue(String(device.width));
});
