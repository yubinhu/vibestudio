import { createHash } from "node:crypto";
import { readFile, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import ts from "typescript";

export const CHROME_DEVICES_SOURCE = "https://raw.githubusercontent.com/ChromeDevTools/devtools-frontend/main/front_end/models/emulation/EmulatedDevices.ts";
const outputPath = fileURLToPath(new URL("../client/web/lib/comparisonDevices.generated.json", import.meta.url));

function property(object, key) {
  if (!ts.isObjectLiteralExpression(object)) throw new Error(`Expected an object for ${key}`);
  const matches = object.properties.filter((entry) => ts.isPropertyAssignment(entry)
    && (ts.isIdentifier(entry.name) || ts.isStringLiteral(entry.name)) && entry.name.text === key);
  if (matches.length > 1) throw new Error(`Duplicate device property: ${key}`);
  return matches[0]?.initializer;
}

function literal(node, kind, field) {
  if (kind === "string" && node && ts.isStringLiteral(node)) return node.text;
  if (kind === "number" && node && ts.isNumericLiteral(node)) return Number(node.text);
  if (kind === "boolean" && node?.kind === ts.SyntaxKind.TrueKeyword) return true;
  if (kind === "boolean" && node?.kind === ts.SyntaxKind.FalseKeyword) return false;
  throw new Error(`Device ${field} must be a literal ${kind}`);
}

/** Inspect the declaration as syntax. Never import, transpile, or execute upstream code. */
export function parseChromeDevices(source) {
  const file = ts.createSourceFile("EmulatedDevices.ts", source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TS);
  if (file.parseDiagnostics.length) throw new Error("Chrome device source could not be parsed as TypeScript");
  const declarations = file.statements.filter(ts.isVariableStatement)
    .flatMap((statement) => [...statement.declarationList.declarations])
    .filter((declaration) => ts.isIdentifier(declaration.name) && declaration.name.text === "emulatedDevices");
  if (declarations.length !== 1 || !declarations[0].initializer || !ts.isArrayLiteralExpression(declarations[0].initializer)) {
    throw new Error("Chrome source must declare one literal emulatedDevices array");
  }
  const devices = [];
  const ids = new Set();
  for (const object of declarations[0].initializer.elements) {
    if (!ts.isObjectLiteralExpression(object)) throw new Error("Device entries must be literal objects");
    const type = literal(property(object, "type"), "string", "type");
    // Named desktop emulation titles are functions; generic computer sizes stay UI-owned.
    if (type !== "phone" && type !== "tablet") continue;
    const label = literal(property(object, "title"), "string", "title");
    const id = label.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "");
    if (!id || ids.has(id)) throw new Error(`Invalid or duplicate device ID: ${id}`);
    const screen = property(object, "screen");
    const vertical = screen && property(screen, "vertical");
    if (!vertical) throw new Error(`Missing primary viewport: ${label}`);
    const width = literal(property(vertical, "width"), "number", "width");
    const height = literal(property(vertical, "height"), "number", "height");
    if (![width, height].every((value) => Number.isInteger(value) && value >= 240 && value <= 3840)) {
      throw new Error(`Unsupported CSS viewport dimensions: ${label}`);
    }
    const showByDefault = literal(property(object, "show-by-default"), "boolean", "show-by-default");
    const orderNode = property(object, "order");
    const order = orderNode ? literal(orderNode, "number", "order") : 0;
    if (!Number.isInteger(order) || order < 0 || order > 100_000) throw new Error(`Invalid device order: ${label}`);
    ids.add(id);
    devices.push({ id, label, group: type === "phone" ? "Phones" : "Tablets", width, height, showByDefault, order });
  }
  if (!devices.length) throw new Error("Chrome source contained no supported devices");
  return devices.sort((a, b) => a.order - b.order || (a.label < b.label ? -1 : a.label > b.label ? 1 : 0));
}

export function generateChromeCatalog(source) {
  return {
    sourceUrl: CHROME_DEVICES_SOURCE,
    sourceSha256: createHash("sha256").update(source).digest("hex"),
    checkedAt: null,
    devices: parseChromeDevices(source),
  };
}

async function main() {
  const args = process.argv.slice(2);
  if (args.length && !(args.length === 2 && args[0] === "--input")) {
    throw new Error("Usage: node scripts/update-comparison-devices.mjs [--input EmulatedDevices.ts]");
  }
  let source;
  if (args.length) source = await readFile(args[1], "utf8");
  else {
    const response = await fetch(CHROME_DEVICES_SOURCE, { signal: AbortSignal.timeout(20_000) });
    if (!response.ok) throw new Error(`Chrome source request failed: HTTP ${response.status}`);
    const reader = response.body?.getReader();
    if (!reader) throw new Error("Chrome source response was empty");
    const chunks = [];
    let size = 0;
    try {
      while (true) {
        const { value, done } = await reader.read();
        if (done) break;
        size += value.byteLength;
        if (size > 2_000_000) throw new Error("Chrome source exceeded 2 MB");
        chunks.push(value);
      }
      source = new TextDecoder("utf-8", { fatal: true }).decode(Buffer.concat(chunks));
    } finally {
      await reader.cancel();
    }
  }
  if (Buffer.byteLength(source) > 2_000_000) throw new Error("Chrome source exceeded 2 MB");
  const catalog = generateChromeCatalog(source);
  const output = `${JSON.stringify(catalog, null, 2)}\n`;
  const previous = await readFile(outputPath, "utf8").catch(() => null);
  if (previous === output) console.log(`Chrome device fallback is current (${catalog.devices.length} devices).`);
  else {
    await writeFile(outputPath, output);
    console.log(`Updated Chrome device fallback (${catalog.devices.length} devices).`);
  }
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  main().catch((error) => { console.error(error.message); process.exitCode = 1; });
}
