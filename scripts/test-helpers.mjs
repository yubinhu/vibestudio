import { readFileSync } from "node:fs";
import vm from "node:vm";
import ts from "typescript";

// Execute a complete frontend module in a fresh VM. Each suite owns its globals,
// dependency stubs and any source transform needed for Vite-only syntax.
export function loadWebModule(path, globals = {}, transformSource = (source) => source) {
  const source = readFileSync(new URL(`../client/web/${path}`, import.meta.url), "utf8");
  const { outputText } = ts.transpileModule(transformSource(source), {
    fileName: path,
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
  });
  const exports = {};
  vm.runInNewContext(outputText, { exports, ...globals }, { filename: path });
  return exports;
}
