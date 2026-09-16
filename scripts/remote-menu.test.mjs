import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

const tick = () => new Promise((resolve) => setImmediate(resolve));

// Render the dialog's real handlers with controlled hooks and transport; no DOM
// dependency or live SSH/WSL process is needed to exercise a failed connection.
async function dialog(initial = { state: "idle" }) {
  let status = initial, cursor = 0, dirty = true, tree;
  const hooks = [], effects = [], calls = [];
  const react = {
    useState(initialValue) {
      const index = cursor++;
      if (!(index in hooks)) hooks[index] = initialValue;
      return [hooks[index], (value) => {
        if (!Object.is(hooks[index], value)) { hooks[index] = value; dirty = true; }
      }];
    },
    useEffect(callback, dependencies) {
      const index = cursor++;
      if (!hooks[index] || dependencies.some((value, i) => !Object.is(value, hooks[index][i]))) {
        hooks[index] = dependencies;
        effects.push(callback);
      }
    },
  };
  const empty = () => null;
  const dependencies = {
    react,
    "react/jsx-runtime": {
      jsx: (type, props) => ({ type, props }),
      jsxs: (type, props) => ({ type, props }),
      Fragment: "fragment",
    },
    "@/components/Modal": { Modal: ({ children }) => children },
    "@/components/PhoneModal": { default: empty, PhoneIcon: empty },
    "@/components/ui": { btnGhost: "ghost", btnPrimary: "primary", Spinner: empty },
    "@/components/connections": { AddConnection: empty, SavedConnections: empty, ServerIcon: empty },
    "@/lib/api": {
      remoteList: async () => [{ name: "wsl:Ubuntu", detail: "WSL2" }, { name: "dev" }],
      phoneStatus: async () => null,
    },
    "@/lib/sshProfiles": { useSshProfiles: () => ({ profiles: null }) },
    "@/lib/remote": { useRemote: () => ({
      status,
      connect: async (host) => {
        calls.push(host);
        status = { state: "error", host, message: "The host service identity changed." };
        dirty = true;
      },
    }) },
  };
  const source = readFileSync(new URL("../client/web/components/RemoteMenu.tsx", import.meta.url), "utf8");
  const { outputText } = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
  });
  const exports = {};
  vm.runInNewContext(outputText, {
    exports, require: (id) => {
      if (id in dependencies) return dependencies[id];
      throw new Error(`Unexpected import ${id}`);
    },
  });
  function nodes(node) {
    if (Array.isArray(node)) return node.flatMap(nodes);
    if (!node || typeof node !== "object") return [];
    if (typeof node.type === "function") return nodes(node.type(node.props));
    return [node, ...nodes(node.props.children)];
  }
  async function render() {
    // Async discovery and connection results update hooks after the current turn.
    do {
      dirty = false;
      cursor = 0;
      tree = exports.RemoteDialog({ onClose() {}, onOpenPhone() {} });
      for (const effect of effects.splice(0)) effect();
      await tick();
    } while (dirty);
  }
  await render();
  return {
    calls, render,
    input: () => nodes(tree).find((node) => node.type === "input").props,
    button: (label) => nodes(tree).find((node) => node.type === "button"
      && nodes(node).some((child) => child.props.children === label)).props,
  };
}

for (const [label, host] of [["Ubuntu", "wsl:Ubuntu"], ["dev", "dev"]]) {
  test(`a failed ${label} list selection keeps Try again enabled for that host`, async () => {
    const h = await dialog();
    assert.equal(h.button("Connect").disabled, true);
    h.button(label).onClick();
    await h.render();
    assert.equal(h.input().value, host);
    assert.equal(h.button("Try again").disabled, false);
    h.button("Try again").onClick();
    await h.render();
    assert.deepEqual(h.calls, [host, host]);
  });
}

test("reopening a failed WSL connection restores its target and accepts a replacement", async () => {
  const h = await dialog({ state: "error", host: "wsl:Ubuntu", message: "The host service identity changed." });
  assert.equal(h.input().value, "wsl:Ubuntu");
  assert.equal(h.button("Try again").disabled, false);
  h.button("Try again").onClick();
  await h.render();
  h.input().onChange({ target: { value: "  user@other-host  " } });
  await h.render();
  h.input().onKeyDown({ key: "Enter" });
  await h.render();
  assert.deepEqual(h.calls, ["wsl:Ubuntu", "user@other-host"]);
  assert.equal(h.input().value, "user@other-host");
});
