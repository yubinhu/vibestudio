import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { delimiter, join } from "node:path";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

function load(name, globals = {}) {
  const source = readFileSync(new URL(`../client/web/lib/${name}.ts`, import.meta.url), "utf8");
  const { outputText } = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  });
  const exports = {};
  vm.runInNewContext(outputText, { exports, ...globals }, { filename: `${name}.ts` });
  return exports;
}

const { sshKeyInstallCommands } = load("sshKeyCommands");
const key = "ssh-ed25519 AAAATEST vibestudio-O'Brien@host;$(touch injected)$HOME`touch injected2`" +
  String.raw`\literal\\double\'quote` + "\\";

function fixture(run) {
  const directory = mkdtempSync(join(tmpdir(), "vibestudio-ssh-key-test-"));
  try {
    return run(directory);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
}

function commands(publicKey = key) {
  // Exercise the generated shell verbatim except for redirecting its home-file
  // paths to the disposable fixture; never change the runner's HOME or SSH files.
  return sshKeyInstallCommands(publicKey)
    .replaceAll('"$HOME/.ssh', '"$VIBESTUDIO_SSH_TEST_HOME/.ssh');
}

function run(shell, directory, command, env = {}) {
  return execFileSync(shell, ["-c", command], {
    cwd: directory,
    env: { ...process.env, VIBESTUDIO_SSH_TEST_HOME: directory, ...env },
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
}

// /bin/sh and bash run on Linux CI; macOS also exercises its default zsh. Fish
// joins automatically on developer/CI hosts where it is installed.
for (const shell of ["/bin/sh", "/bin/bash", "/bin/zsh", "/usr/bin/fish"]) {
  test(`SSH commands preserve keys, shell data, permissions and umask in ${shell}`, {
    skip: !existsSync(shell),
  }, () => {
    for (const existing of [undefined, "ssh-ed25519 AAAAold keep-me", `${key}\n`]) {
      fixture((directory) => {
        const sshDirectory = join(directory, ".ssh");
        const authorizedKeys = join(sshDirectory, "authorized_keys");
        if (existing !== undefined) {
          mkdirSync(sshDirectory, { mode: 0o755 });
          writeFileSync(authorizedKeys, existing, { mode: 0o644 });
        }
        const beforeAfter = run(shell, directory, `umask\n${commands()}\numask`).trim().split("\n");
        assert.equal(beforeAfter.length, 2);
        assert.equal(beforeAfter[0], beforeAfter[1], "the caller's umask is unchanged");
        const first = readFileSync(authorizedKeys, "utf8");
        run(shell, directory, commands());
        assert.equal(readFileSync(authorizedKeys, "utf8"), first, "repeat installs do not append");
        assert.equal(first.split("\n").filter((line) => line === key).length, 1);
        if (existing !== undefined) assert.ok(first.startsWith(existing), "existing bytes survive");
        assert.equal(statSync(sshDirectory).mode & 0o777, 0o700);
        assert.equal(statSync(authorizedKeys).mode & 0o777, 0o600);
        assert.equal(existsSync(join(directory, "injected")), false);
        assert.equal(existsSync(join(directory, "injected2")), false);
      });
    }
  });
}

test("SSH install aborts on a failed duplicate check instead of appending", {
  skip: !existsSync("/bin/sh"),
}, () => fixture((directory) => {
  const sshDirectory = join(directory, ".ssh");
  const authorizedKeys = join(sshDirectory, "authorized_keys");
  mkdirSync(sshDirectory);
  writeFileSync(authorizedKeys, "existing key without newline");
  const bin = join(directory, "bin");
  mkdirSync(bin);
  writeFileSync(join(bin, "grep"), "#!/bin/sh\nexit 2\n", { mode: 0o700 });
  assert.throws(() => run("/bin/sh", directory, commands(), {
    PATH: `${bin}${delimiter}${process.env.PATH ?? "/usr/bin:/bin"}`,
  }), (error) => error.status === 1);
  assert.equal(readFileSync(authorizedKeys, "utf8"), "existing key without newline");
}));

test("SSH install stops on setup failure without replacing an existing .ssh file", {
  skip: !existsSync("/bin/sh"),
}, () => fixture((directory) => {
  writeFileSync(join(directory, ".ssh"), "preserve this file");
  assert.throws(() => run("/bin/sh", directory, commands()), (error) => error.status !== 0);
  assert.equal(readFileSync(join(directory, ".ssh"), "utf8"), "preserve this file");
}));

test("public-key commands reject extra entries and terminal control characters", () => {
  for (const value of [
    "", "not a public key", "ssh-ed25519 invalid;data comment",
    `${key}\nssh-ed25519 AAAAextra second-entry`, `${key}\rsecond-line`,
    `${key}\0`, `${key}\x1b[2K`, `${key}\x7f`,
  ]) {
    assert.throws(() => sshKeyInstallCommands(value), /generated public key is invalid/);
  }
  assert.equal(sshKeyInstallCommands(`${key}\n`), sshKeyInstallCommands(key));
});

function clipboard(clipboardApi, legacyResult = true) {
  const events = [];
  class Element {
    focus(options) { events.push(["focus", options.preventScroll]); }
  }
  const textarea = {
    style: {},
    setAttribute(name, value) { events.push([name, value]); },
    select() { events.push(["select", this.value, this.readOnly]); },
    setSelectionRange(start, end) { events.push(["range", start, end]); },
    remove() { events.push(["remove"]); },
  };
  const module = load("copyText", {
    navigator: { clipboard: clipboardApi },
    HTMLElement: Element,
    document: {
      activeElement: new Element(),
      createElement: () => textarea,
      body: { appendChild: () => events.push(["append"]) },
      execCommand: (name) => {
        events.push(["exec", name]);
        if (legacyResult instanceof Error) throw legacyResult;
        return legacyResult;
      },
    },
  });
  return { copy: module.copyText, events };
}

test("copy waits for successful Clipboard API completion before reporting success", async () => {
  let resolve;
  const writes = [];
  const h = clipboard({ writeText: (text) => {
    writes.push(text);
    return new Promise((done) => { resolve = done; });
  } });
  let finished = false;
  const result = h.copy(key).then(() => { finished = true; });
  await Promise.resolve();
  assert.equal(finished, false);
  assert.deepEqual(writes, [key]);
  resolve();
  await result;
  assert.equal(finished, true);
  assert.deepEqual(h.events, [], "modern copy does not disturb focus or selection");
});

test("copy falls back when the Clipboard API is missing or rejects", async () => {
  for (const api of [undefined, { writeText: () => Promise.reject(new Error("NotAllowedError")) }]) {
    const h = clipboard(api);
    await h.copy(key);
    assert.ok(h.events.some((event) => event[0] === "select" && event[1] === key && event[2] === true));
    assert.ok(h.events.some((event) => event[0] === "exec" && event[1] === "copy"));
    assert.deepEqual(h.events.slice(-2), [["remove"], ["focus", true]]);
  }
});

test("copy rejects when both methods fail and still removes the fallback textarea", async () => {
  for (const result of [false, new Error("copy blocked")]) {
    const h = clipboard({ writeText: () => Promise.reject(new Error("NotAllowedError")) }, result);
    await assert.rejects(h.copy(key));
    assert.deepEqual(h.events.slice(-2), [["remove"], ["focus", true]]);
  }
});
