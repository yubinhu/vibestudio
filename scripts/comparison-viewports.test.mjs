import assert from "node:assert/strict";
import test from "node:test";
import { loadWebModule } from "./test-helpers.mjs";
import { readFileSync } from "node:fs";

const bundled = JSON.parse(readFileSync(new URL("../client/web/lib/comparisonDevices.generated.json", import.meta.url), "utf8"));
const viewports = loadWebModule("lib/comparisonViewports.ts", { require: (id) => {
  assert.equal(id, "./comparisonDevices.generated.json");
  return { default: bundled };
} });
const viewport = (preset, width, height) => ({ preset, width, height, orientation: width > height ? "landscape" : "portrait" });

test("legacy generic presets stay responsive while explicit device identities require matching dimensions", () => {
  assert.equal(viewports.comparisonViewportPresetId(viewport("phone", 390, 844)), "custom");
  assert.equal(viewports.comparisonViewportPresetId(viewport("phone", 844, 390)), "custom");
  assert.equal(viewports.comparisonViewportPresetId(viewport("tablet", 768, 1024)), "custom");
  assert.equal(viewports.comparisonViewportPresetId(viewport("desktop", 1440, 900)), "desktop");
  assert.equal(viewports.comparisonViewportPresetId(viewport("iphone-14", 390, 844)), "iphone-14");
  for (const value of [viewport("phone", 400, 800), viewport("iphone-14", 400, 800), viewport("unknown", 390, 844), viewport("custom", 390, 844)]) {
    assert.equal(viewports.comparisonViewportPresetId(value), "custom");
  }
});

test("rotation retains device identity and labels the actual CSS dimensions", () => {
  const original = viewports.comparisonViewportFromPreset("pixel-7");
  const rotated = viewports.rotateComparisonViewport(original);
  assert.deepEqual(JSON.parse(JSON.stringify(rotated)), { preset: "pixel-7", width: 915, height: 412, orientation: "landscape" });
  const preset = viewports.COMPARISON_VIEWPORT_PRESETS.find((entry) => entry.id === "pixel-7");
  assert.equal(viewports.comparisonViewportOptionLabel(preset, rotated), "Pixel 7 · 915 × 412 CSS px");
  assert.deepEqual(viewports.rotateComparisonViewport(rotated), original);
});

test("viewport sizes enforce backend bounds and generic computer presets do not claim a model", () => {
  for (const [width, height] of [[239, 900], [1440, 3841], [NaN, 900], [390.5, 844]]) {
    assert.equal(viewports.validComparisonViewportSize(width, height), false);
  }
  assert.equal(viewports.validComparisonViewportSize(240, 3840), true);
  assert.equal(viewports.comparisonViewportFromPreset("missing"), null);
  for (const preset of viewports.COMPARISON_VIEWPORT_PRESETS.filter((entry) => entry.group === "Computers")) {
    assert.match(preset.label, /generic/);
  }
});


test("refreshed catalogs keep removed saved presets available and prefer current Chrome defaults", () => {
  const fresh = { id: "catalog-fixture", label: "Catalog fixture", group: "Phones", width: 401, height: 877, order: 1, showByDefault: true };
  const presets = viewports.comparisonViewportPresets([fresh]);
  assert.equal(presets.find((device) => device.id === "catalog-fixture").showByDefault, true);
  assert.equal(presets.find((device) => device.id === "iphone-14").showByDefault, false);
  assert.equal(viewports.comparisonViewportPresetId(viewport("catalog-fixture", 877, 401), presets), fresh.id);
  assert.equal(viewports.comparisonViewportOptionLabel(fresh, viewport("catalog-fixture", 877, 401)), "Catalog fixture · 877 × 401 CSS px");
});
