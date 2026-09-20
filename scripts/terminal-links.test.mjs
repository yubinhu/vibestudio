import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

const source = readFileSync(new URL("../client/web/lib/terminalLinks.ts", import.meta.url), "utf8");
const { outputText } = ts.transpileModule(source, {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
});
const exports = {};
vm.runInNewContext(outputText, { exports, URL }, { filename: "terminalLinks.ts" });
const { parseFileLink, detectFileLinks, webLinkUrl, fileLinksForBuffer, fileLinkProvider } = exports;
const plain = (value) => structuredClone(value);

function target(path, line, column) {
  return { path, line, column };
}

function links(text) {
  return plain(detectFileLinks(text));
}

// Explicit cell widths model xterm's buffer, including the width-zero cell after
// a wide glyph and multiple Unicode code points stored in one combining cell.
function line(glyphs, cols, isWrapped = false) {
  const cells = [];
  for (const glyph of glyphs) {
    const [chars, width, color] = Array.isArray(glyph) ? glyph : [glyph, 1];
    cells.push({
      getChars: () => chars, getWidth: () => width,
      isFgDefault: () => color === undefined,
      getFgColorMode: () => color === undefined ? 0 : 1,
      getFgColor: () => color ?? 0,
    });
    if (width === 2) cells.push({ getChars: () => "", getWidth: () => 0 });
  }
  while (cells.length < cols) cells.push({ getChars: () => "", getWidth: () => 1 });
  assert.equal(cells.length, cols, "fixture occupies exactly the requested columns");
  return {
    isWrapped, length: cells.length, getCell: (index) => cells[index],
    translateToString: (trimRight = false, start = 0, end = cells.length) => {
      const text = cells.slice(start, end).filter((cell) => cell.getWidth() > 0).map((cell) => cell.getChars() || " ").join("");
      return trimRight ? text.trimEnd() : text;
    },
  };
}

const colored = (text, color = 6) => [...text].map((char) => [char, 1, color]);

function buffer(rows) {
  return { getLine: (index) => rows[index] };
}

test("file locations support compiler, GitHub and editor syntax", () => {
  for (const [text, expected] of [
    ["src/main.ts:12:3", target("src/main.ts", 12, 3)],
    ["./src/main.ts:12", target("./src/main.ts", 12)],
    ["../src/main.ts#L12C3", target("../src/main.ts", 12, 3)],
    ["/tmp/main.ts#L12", target("/tmp/main.ts", 12)],
    ["src/main.ts(12, 3)", target("src/main.ts", 12, 3)],
    ["src/main.ts(12)", target("src/main.ts", 12)],
    ["~/My Project/main.ts", target("~/My Project/main.ts")],
    [String.raw`C:\work\main.ts:12:3`, target(String.raw`C:\work\main.ts`, 12, 3)],
  ]) assert.deepEqual(plain(parseFileLink(text)), expected, text);

  for (const text of ["main.ts:0", "main.ts:1:0", "main.ts#L0", "main.ts(0, 1)", "main.ts:9007199254740992"]) {
    assert.equal(parseFileLink(text), null, text);
  }
});

test("file URI paths decode once and retain locations on the workspace host", () => {
  for (const [text, expected] of [
    ["file:///tmp/my%20file.ts:12:3", target("/tmp/my file.ts", 12, 3)],
    ["file://remote-host/home/me/main.ts#L8C2", target("/home/me/main.ts", 8, 2)],
    ["file:///tmp/literal%253A12.ts", target("/tmp/literal%3A12.ts")],
    ["file:///tmp/name%3A12", target("/tmp/name:12")],
  ]) assert.deepEqual(plain(parseFileLink(text)), expected, text);

  for (const text of [
    "", "file:///tmp/bad%00.ts", "file:///tmp/bad%0a.ts", "file:///tmp/bad%1b.ts", "file:///tmp/bad%7f.ts",
    "file:///tmp/bad%xx.ts", "file:///tmp/main.ts?query=1", "file:///tmp/main.ts#fragment",
    "file://user:password@host/tmp/main.ts", "file://host:80/tmp/main.ts",
    "javascript:alert(1)", "https://example.test/main.ts", "vscode://file/tmp/main.ts", "data:text/plain,main.ts",
    "src/escape\u001b.ts", "src/tab\t.ts", "src/newline\n.ts", "src/delete\u007f.ts",
  ]) assert.equal(parseFileLink(text), null, JSON.stringify(text));
});

test("web links accept HTTP(S) and reject executable schemes, credentials and controls", () => {
  assert.equal(webLinkUrl("https://example.test/docs?q=hello#section"), "https://example.test/docs?q=hello#section");
  assert.equal(webLinkUrl("HTTP://EXAMPLE.TEST"), "http://example.test/");
  for (const text of [
    "javascript:alert(1)", "file:///tmp/main.ts", "data:text/html,test", "//example.test",
    "https://user:password@example.test/", "http://", "https://example.test/has space",
    "https://example.test/\npath", "https://example.test/\tpath", "https://example.test/\u001bpath", "https://example.test/\u007fpath",
  ]) assert.equal(webLinkUrl(text), null, JSON.stringify(text));
});

test("quoted paths and compiler locations with spaces keep accurate clickable ranges", () => {
  for (const quote of ['"', "'", "`"]) {
    const text = `error: ${quote}src/my file.ts${quote}:12:3`;
    const [link] = links(text);
    assert.deepEqual(link, { ...target("src/my file.ts", 12, 3), text: "src/my file.ts:12:3", start: 8, end: text.length });
    const quotedLocation = `error: ${quote}src/my file.ts(12, 3)${quote}`;
    const [inside] = links(quotedLocation);
    assert.deepEqual(inside, { ...target("src/my file.ts", 12, 3), text: "src/my file.ts(12, 3)", start: 8, end: quotedLocation.length - 1 });
  }
  const text = "src/main.ts(12, 3): error TS1000";
  const [link] = links(text);
  assert.deepEqual(link, { ...target("src/main.ts", 12, 3), text: "src/main.ts(12, 3)", start: 0, end: 18 });
  assert.equal(links('"src/O\'Brien/my file.ts":12')[0].path, "src/O'Brien/my file.ts");
  assert.equal(links("'src/with\"quotes/file.ts':12")[0].path, 'src/with"quotes/file.ts');
});

test("Python tracebacks retain their line outside a quoted filename", () => {
  const text = '  File "/tmp/my project/app.py", line 42, in main';
  assert.deepEqual(links(text), [{ ...target("/tmp/my project/app.py", 42), text: "/tmp/my project/app.py", start: 8, end: 30 }]);
  assert.deepEqual(links('File "app.py", line 0'), []);
});

test("paths in punctuation and markdown exclude surrounding prose", () => {
  const text = "See (src/main.ts:12:3), [notes](docs/README.md). Then `Makefile` and LICENSE.";
  const found = links(text);
  assert.deepEqual(found.map(({ path, line, column }) => ({ path, line, column })), [
    target("src/main.ts", 12, 3), target("docs/README.md"), target("Makefile"), target("LICENSE"),
  ]);
  for (const link of found) assert.equal(text.slice(link.start, link.end), link.text);
  assert.deepEqual(links("hello ordinary words"), []);
});

test("URLs never become partial filesystem links", () => {
  for (const text of [
    "https://example.test/src/main.ts:12", "http://[::1]/src/main.ts",
    "https://example.test/api?paths[]=src/main.ts", "vscode://file/tmp/main.ts",
    "ssh://[::1]/tmp/main.ts", "mailto:someone@example.test",
  ]) assert.deepEqual(links(text), [], text);
});

test("wide and combining glyphs before a path do not shift terminal hit targets", () => {
  const cols = 32;
  const active = buffer([line([["😀", 2], "e\u0301", ["中", 2], " ", ..."src/main.ts:12:3"], cols)]);
  const [link] = plain(fileLinksForBuffer(active, 1, cols));
  assert.equal(link.path, "src/main.ts");
  assert.deepEqual(link.range, { start: { x: 7, y: 1 }, end: { x: 22, y: 1 } });
});

test("a file split across wrapped rows is clickable from either row", () => {
  const cols = 10;
  const active = buffer([
    line([["😀", 2], "e\u0301", " ", ..."./src/"], cols),
    line([..."file.ts:42"], cols, true),
    line([..."unrelated"], cols),
  ]);
  for (const row of [1, 2]) {
    const [link] = plain(fileLinksForBuffer(active, row, cols));
    assert.deepEqual({ path: link.path, line: link.line, column: link.column }, target("./src/file.ts", 42));
    assert.deepEqual(link.range, { start: { x: 5, y: 1 }, end: { x: 10, y: 2 } });
  }
  assert.deepEqual(plain(fileLinksForBuffer(active, 3, cols)), []);
});

test("indented hard-wrapped colored paths open the complete file from either row", () => {
  const cols = 80;
  const prefix = "  May I apply these edits (";
  const first = "/tmp/vibestudio-doc-audit/readme-";
  const second = "proposal.patch";
  const active = buffer([
    line([...prefix, ...colored(first)], cols),
    line([..."  ", ...colored(second), ...")? They correct the README."], cols),
  ]);
  const term = { buffer: { active }, cols };
  const activations = [];
  const provider = fileLinkProvider(term, { activate: (_event, text) => activations.push(text) });
  for (const row of [1, 2]) {
    const [link] = plain(fileLinksForBuffer(active, row, cols));
    assert.equal(link.path, first + second);
    assert.equal(link.text, first + second);
    assert.deepEqual(link.range, row === 1
      ? { start: { x: prefix.length + 1, y: 1 }, end: { x: prefix.length + first.length, y: 1 } }
      : { start: { x: 3, y: 2 }, end: { x: second.length + 2, y: 2 } });
    provider.provideLinks(row, (provided) => provided[0].activate({ ctrlKey: true }, provided[0].text));
  }
  assert.deepEqual(activations, [first + second, first + second]);
});

test("hard-wrapped paths can span three rows and keep line and column suffixes", () => {
  const cols = 48;
  const active = buffer([
    line([..."  See (", ...colored("/tmp/my-project/")], cols),
    line([..."  ", ...colored("long-file-")], cols),
    line([..."  ", ...colored("name.ts:42:3"), ..."). Then other.ts."], cols),
  ]);
  for (const row of [1, 2, 3]) {
    const [link] = plain(fileLinksForBuffer(active, row, cols));
    assert.equal(link.path, "/tmp/my-project/long-file-name.ts");
    assert.equal(link.line, 42);
    assert.equal(link.column, 3);
    assert.equal(link.range.start.y, row);
    assert.equal(link.range.end.y, row);
  }
  assert.equal(fileLinksForBuffer(active, 3, cols)[1].path, "other.ts");
});

test("hard line boundaries do not combine unrelated file links or prose", () => {
  const cols = 64;
  for (const [first, second] of [
    [[..."See /tmp/readme-"], [..."  proposal.patch"]],
    [[..."See ", ...colored("/tmp/readme-", 6)], [..."  ", ...colored("proposal.patch", 5)]],
    [[..."See ", ...colored("/tmp/readme.patch")], [..."  ", ...colored("other.patch")]],
    [[..."See ", ...colored("/tmp/readme-")], [..."  Ordinary prose follows."]],
    [[..."See ", ...colored("/tmp/readme-")], [...colored("proposal.patch")]],
    [colored("Status -"), [..."  ", ...colored("output.patch")]],
    [colored("directory: /tmp/project/"), [..."  ", ...colored("Run the test.")]],
    [[..."See ", ...colored("/tmp/readme-")], colored("    ")],
    [[..."See ", ...colored("/tmp/readme-")], [..."  ", ...colored("- other.patch")]],
    ...["/tmp/other.patch", "~/other.patch", "./other.patch", "../other.patch", "C:\\other.patch", "file:///tmp/other.patch", "https://example.test/other.patch"]
      .map((next) => [[..."See ", ...colored("/tmp/readme-")], [..."  ", ...colored(next)]]),
  ]) {
    const active = buffer([line(first, cols), line(second, cols)]);
    for (const row of [1, 2]) {
      const separate = buffer([line(row === 1 ? first : second, cols)]);
      assert.deepEqual(
        plain(fileLinksForBuffer(active, row, cols)).map(({ text }) => text),
        plain(fileLinksForBuffer(separate, 1, cols)).map(({ text }) => text),
      );
    }
  }
});

test("hard-wrapped quoted paths and compiler locations retain embedded spaces", () => {
  const cols = 64;
  for (const [first, second, path, row, column] of [
    ['"/tmp/my project/readme-', 'proposal copy.patch"', "/tmp/my project/readme-proposal copy.patch"],
    ["/tmp/readme-", "proposal.patch(12, 3)", "/tmp/readme-proposal.patch", 12, 3],
  ]) {
    const active = buffer([
      line([..."See ", ...colored(first)], cols),
      line([..."  ", ...colored(second), ..." and continue."], cols),
    ]);
    for (const y of [1, 2]) {
      const [link] = plain(fileLinksForBuffer(active, y, cols));
      assert.deepEqual({ path: link.path, line: link.line, column: link.column }, target(path, row, column));
    }
  }
});

test("mixed hard and soft wraps preserve Unicode cell positions", () => {
  const cols = 20;
  const active = buffer([
    line([["😀", 2], "e\u0301", " ", ...colored("/tmp/readme-")], cols),
    line([..."  ", ...colored("proposal-with-long")], cols),
    line([...colored("file.ts:42:3")], cols, true),
  ]);
  for (const row of [1, 2, 3]) {
    const [link] = plain(fileLinksForBuffer(active, row, cols));
    assert.equal(link.path, "/tmp/readme-proposal-with-longfile.ts");
    assert.equal(link.line, 42);
    assert.equal(link.column, 3);
    assert.deepEqual(link.range, {
      start: { x: [5, 3, 1][row - 1], y: row },
      end: { x: [16, 20, 12][row - 1], y: row },
    });
  }
});

test("wide glyphs at a wrap skip unused cells and include the complete end glyph", () => {
  const cols = 10;
  const active = buffer([
    line([..."./folder/"], cols),
    line([["中", 2], ...".ts:5"], cols, true),
  ]);
  const [link] = plain(fileLinksForBuffer(active, 2, cols));
  assert.equal(link.path, "./folder/中.ts");
  assert.equal(link.line, 5);
  assert.deepEqual(link.range, { start: { x: 1, y: 1 }, end: { x: 7, y: 2 } });
  const [wideEnd] = plain(fileLinksForBuffer(buffer([line([..."./", ["中", 2]], cols)]), 1, cols));
  assert.deepEqual(wideEnd.range, { start: { x: 1, y: 1 }, end: { x: 4, y: 1 } });
});

test("large wrapped output is bounded and paths in different rows stay separate", () => {
  const cols = 10;
  const excessive = buffer(Array.from({ length: 35 }, (_, i) => line([..."src/aaaaaa"], cols, i > 0)));
  assert.deepEqual(plain(fileLinksForBuffer(excessive, 20, cols)), []);
  const hardWrapped = buffer(Array.from({ length: 35 }, () => line([..."  ", ...colored("src/aaa-")], cols)));
  assert.deepEqual(plain(fileLinksForBuffer(hardWrapped, 20, cols)), []);
  const separate = buffer([line([..."first.ts"], cols), line([..."second.ts"], cols)]);
  assert.equal(fileLinksForBuffer(separate, 1, cols)[0].path, "first.ts");
  assert.equal(fileLinksForBuffer(separate, 2, cols)[0].path, "second.ts");
});

test("provider activation preserves traceback locations and URI encoding", () => {
  for (const [printed, activated] of [
    ['File "/tmp/my project/app.py", line 42', "/tmp/my project/app.py:42"],
    ["file:///tmp/my%20file.ts:12:3", "file:///tmp/my%20file.ts:12:3"],
    ["src/main.ts(12, 3)", "src/main.ts(12, 3)"],
  ]) {
    const cols = 64;
    const term = { buffer: { active: buffer([line([...printed], cols)]) }, cols };
    const events = [];
    const hover = () => {};
    const leave = () => {};
    const provider = fileLinkProvider(term, { activate: (event, text) => events.push([event, text]), hover, leave });
    let provided;
    provider.provideLinks(1, (value) => { provided = value; });
    assert.equal(provided.length, 1, printed);
    assert.equal(provided[0].hover, hover);
    assert.equal(provided[0].leave, leave);
    const event = { ctrlKey: true };
    provided[0].activate(event, provided[0].text);
    assert.deepEqual(events, [[event, activated]]);
  }
});
