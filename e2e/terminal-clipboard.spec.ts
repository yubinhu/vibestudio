import { readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test, expect } from "./fixtures";

// xterm may wrap user paste data in the terminal's bracketed-paste sequences.
const decodeInput = (data: string) => Buffer.from(data, "base64").toString("utf8")
  .replace("\x1b[200~", "").replace("\x1b[201~", "");

test.beforeEach(async ({ page, request, app }) => {
  await writeFile(join(app.binDirectory, "codex"), `#!/bin/sh
if [ "\${1:-}" = --version ]; then printf 'codex 0.0.0\\n'; exit 0; fi
stty -echo
: > .e2e-clipboard-input
printf '\\033[2J\\033[Hclipboardword\\r\\n'
while IFS= read -r line; do
  printf '%s\\n' "$line" >> .e2e-clipboard-input
  printf 'received:%s\\r\\n' "$line"
done
`);
  const created = await request.post("/api/terminal/create", {
    data: { agent: "codex:cli", cwd: app.directory, cols: 80, rows: 24 },
  });
  expect(created.ok()).toBeTruthy();
  const session = await created.json() as { id: string };
  await page.goto(`/#/sessions?id=${session.id}`);
  await expect(page.locator(".xterm-rows")).toContainText("clipboardword");
  await expect(page.getByText("Connecting terminal…", { exact: true })).toHaveCount(0);
});

test("touch selection offers contextual Copy and Paste connected to the real terminal", async ({ page, app }) => {
  // Replace only the OS clipboard boundary. The selection, menu buttons and
  // resulting paste still use the real component, HTTP transport and tmux.
  await page.evaluate(() => {
    const clipboard = {
      copied: [] as string[],
      async writeText(text: string) { this.copied.push(text); },
      async readText() { return "menu text paste\n"; },
    };
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: clipboard });
  });
  const screen = page.locator(".xterm-screen");
  const row = page.locator(".xterm-rows > div").filter({ hasText: "clipboardword" });
  const box = await row.boundingBox();
  expect(box).not.toBeNull();
  const point = { clientX: box!.x + 15, clientY: box!.y + box!.height / 2 };
  const menu = page.getByRole("group", { name: "Terminal selection actions" });
  const selectWord = async () => {
    await screen.evaluate((element, point) => {
      element.dispatchEvent(Object.assign(new Event("touchstart", { bubbles: true }), {
        touches: [point], changedTouches: [point],
      }));
    }, point);
    await expect(screen.locator(".xterm-selection > div").first()).toBeVisible();
    await screen.evaluate((element, point) => {
      element.dispatchEvent(Object.assign(new Event("touchend", { bubbles: true, cancelable: true }), {
        touches: [], changedTouches: [point],
      }));
    }, point);
    await expect(menu).toBeVisible();
  };

  await selectWord();
  await expect(menu.getByRole("button", { name: "Copy", exact: true })).toBeEnabled();
  await expect(menu.getByRole("button", { name: "Paste", exact: true })).toBeEnabled();
  await menu.getByRole("button", { name: "Copy", exact: true }).click();
  await expect.poll(() => page.evaluate(() =>
    (navigator.clipboard as Clipboard & { copied: string[] }).copied)).toEqual(["clipboardword"]);
  await expect(menu).toHaveCount(0);
  await expect(screen.locator(".xterm-selection > div")).toHaveCount(0);

  await selectWord();
  await menu.getByRole("button", { name: "Paste", exact: true }).click();
  await expect.poll(() => readFile(join(app.directory, ".e2e-clipboard-input"), "utf8"))
    .toBe("menu text paste\n");
  await expect(menu).toHaveCount(0);
});

test("native paste events send text and uploaded image paths to the real terminal", async ({ page, app }) => {
  const input = page.locator(".xterm-helper-textarea");
  const received = () => readFile(join(app.directory, ".e2e-clipboard-input"), "utf8");
  await input.evaluate((element) => {
    const data = new DataTransfer();
    data.setData("text/plain", "native text paste\n");
    element.dispatchEvent(new ClipboardEvent("paste", { bubbles: true, cancelable: true, clipboardData: data }));
  });
  await expect.poll(received).toBe("native text paste\n");

  const png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+a4n0AAAAASUVORK5CYII=";
  const upload = page.waitForResponse((response) => response.url().endsWith("/api/terminal/paste-image") && response.request().method() === "POST");
  const imageInput = page.waitForResponse((response) => {
    if (!response.url().endsWith("/api/terminal/input") || response.request().method() !== "POST") return false;
    const { data } = response.request().postDataJSON() as { data: string };
    return decodeInput(data).endsWith(".png");
  });
  await input.evaluate((element, png) => {
    const data = new DataTransfer();
    const bytes = Uint8Array.from(atob(png), (character) => character.charCodeAt(0));
    data.items.add(new File([bytes], "clipboard.png", { type: "image/png" }));
    element.dispatchEvent(new ClipboardEvent("paste", { bubbles: true, cancelable: true, clipboardData: data }));
  }, png);
  const response = await upload;
  expect(response.ok()).toBeTruthy();
  const { path } = await response.json() as { path: string };
  expect(await readFile(path)).toEqual(Buffer.from(png, "base64"));
  // Wait until the uploaded path reaches terminal input before submitting it.
  const pasted = await imageInput;
  expect(pasted.ok()).toBeTruthy();
  expect(decodeInput(pasted.request().postDataJSON().data)).toBe(path);
  await input.press("Enter");
  await expect.poll(received).toBe(`native text paste\n${path}\n`);
});
