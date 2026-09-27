import assert from "node:assert/strict";
import test from "node:test";
import { generateChromeCatalog, parseChromeDevices } from "./update-comparison-devices.mjs";

const device = (overrides = {}) => ({
  title: "Fixture Phone", type: "phone", order: 7, "show-by-default": true,
  screen: { vertical: { width: 390, height: 844 }, horizontal: { width: 844, height: 390 } }, ...overrides,
});
const source = (devices) => `const emulatedDevices = ${JSON.stringify(devices)};`;

test("generator extracts authoritative viewport metadata without unrelated emulation settings", () => {
  const catalog = generateChromeCatalog(source([device({ "user-agent": "ignored", screen: { vertical: { width: 390, height: 844 }, "device-pixel-ratio": 3 } }), device({ title: "Tablet / Fixture", type: "tablet", order: 2, "show-by-default": false })]));
  assert.deepEqual(catalog.devices.map(({ id, group, showByDefault }) => ({ id, group, showByDefault })), [
    { id: "tablet-fixture", group: "Tablets", showByDefault: false }, { id: "fixture-phone", group: "Phones", showByDefault: true },
  ]);
  assert.match(catalog.sourceSha256, /^[a-f0-9]{64}$/);
  assert.equal(catalog.checkedAt, null);
  assert.equal(JSON.stringify(catalog).includes("user-agent"), false);
});

test("upstream expressions are rejected without executing JavaScript", () => {
  const input = source([device()]).replace('"Fixture Phone"', '(() => { globalThis.catalogExecuted = true; return "Injected"; })()');
  assert.throws(() => parseChromeDevices(input), /literal string/);
  assert.equal(globalThis.catalogExecuted, undefined);
  const notebook = '{type:"notebook",title:i18nLazyString(UIStrings.laptop)}';
  assert.equal(parseChromeDevices(`const emulatedDevices = [${JSON.stringify(device())}, ${notebook}];`).length, 1);
});

test("malformed dimensions, duplicate IDs, and ambiguous source declarations cannot replace the bundle", () => {
  assert.throws(() => parseChromeDevices(source([device({ screen: { vertical: { width: 239, height: 844 } } })])), /dimensions/);
  assert.throws(() => parseChromeDevices(source([device(), device()])), /duplicate/);
  assert.throws(() => parseChromeDevices('const emulatedDevices = fetchDevices();'), /literal emulatedDevices/);
  assert.throws(() => parseChromeDevices(source([])), /no supported devices/);
});
