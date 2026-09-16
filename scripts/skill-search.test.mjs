import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

function load(path, dependencies = {}) {
  const source = readFileSync(new URL(`../client/web/${path}`, import.meta.url), "utf8");
  const { outputText } = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  });
  const exports = {};
  vm.runInNewContext(outputText, {
    exports, require: (id) => {
      if (id in dependencies) return dependencies[id];
      throw new Error(`Unexpected import ${id}`);
    },
  });
  return exports;
}
const { filterSkillGroups } = load("pages/home/skillSearch.ts", { "@/lib/agents": load("lib/agents.ts") });
const skill = (name, fields = {}) => ({ name, description: "", root: `/skills/${name}`, kind: "personal", proposed: false, ...fields });
const inventory = [
  { agent: "Codex", skills: [
    skill("browser-check", { description: "Inspect café layouts with keyboard navigation" }),
    skill("deploy", { description: "Review release artifacts", root: "/repos/harbor/.codex/skills/deploy", project: "harbor" }),
    skill("skill-creator", { kind: "official", description: "Create reusable agent instructions" }),
    skill("reports", { kind: "plugin", root: "C:\\Users\\Casey\\plugins\\reports", description: "Export data tables" }),
    skill("load-secrets", { kind: "studio" }),
    skill("release-guide", { proposed: true, description: "Prepare a changelog" }),
  ] },
  { agent: "Agent Skills", skills: [skill("shared-review", { description: "Review pull requests" })] },
  { agent: "Claude Code", skills: [skill("storybook", { description: "Inspect UI components" })] },
];
const names = (groups) => Array.from(groups, (group) => Array.from(group.skills, (item) => item.name)).flat();

test("empty or whitespace-only searches keep the complete inventory and its identity", () => {
  for (const query of ["", "  ", "\t\n"]) assert.equal(filterSkillGroups(inventory, query), inventory);
});

test("search combines terms across names, descriptions, agents, scopes, and projects", () => {
  assert.deepEqual(names(filterSkillGroups(inventory, "  CODEX   project review  ")), ["deploy"]);
  assert.deepEqual(names(filterSkillGroups(inventory, "global browser")), ["browser-check"]);
  assert.deepEqual(names(filterSkillGroups(inventory, "harbor")), ["deploy"]);
  assert.deepEqual(names(filterSkillGroups(inventory, "CLAUDE inspect")), ["storybook"]);
  assert.deepEqual(names(filterSkillGroups(inventory, "codex storybook")), []);
});

test("search matches displayed kinds and proposals, including normally collapsed skills", () => {
  assert.deepEqual(names(filterSkillGroups(inventory, "official")), ["skill-creator"]);
  assert.deepEqual(names(filterSkillGroups(inventory, "plugin export")), ["reports"]);
  assert.deepEqual(names(filterSkillGroups(inventory, "VibeStudio")), ["load-secrets"]);
  assert.deepEqual(names(filterSkillGroups(inventory, "yours changelog")), ["release-guide"]);
  assert.deepEqual(names(filterSkillGroups(inventory, "proposed")), ["release-guide"]);
});

test("shared-standard skills are searchable by their displayed group and compatible agents", () => {
  for (const query of ["standard", "shared", "Cursor", "Gemini review"]) {
    assert.deepEqual(names(filterSkillGroups(inventory, query)), ["shared-review"]);
  }
  assert.deepEqual(names(filterSkillGroups(inventory, "Claude shared")), []);
});

test("paths and accented descriptions match ordinary text without requiring regex syntax", () => {
  assert.deepEqual(names(filterSkillGroups(inventory, "cafe KEYBOARD")), ["browser-check"]);
  assert.deepEqual(names(filterSkillGroups(inventory, "C:/users/casey")), ["reports"]);
  assert.deepEqual(names(filterSkillGroups(inventory, "C:\\Users\\Casey")), ["reports"]);
  assert.deepEqual(names(filterSkillGroups(inventory, "/repos/harbor/.codex")), ["deploy"]);
  assert.deepEqual(names(filterSkillGroups(inventory, ".*")), []);
});

test("missing optional metadata and unfamiliar kinds remain searchable by their root", () => {
  const groups = [{ agent: "Future Agent", skills: [{ root: "/opt/agent/skills/reconcile", kind: "custom", proposed: false }] }];
  assert.equal(filterSkillGroups(groups, "future reconcile personal")[0].skills[0], groups[0].skills[0]);
});

test("filtering preserves group and skill order without mutating the shared discovery cache", () => {
  const before = JSON.stringify(inventory);
  const result = filterSkillGroups(inventory, "review");
  assert.deepEqual(Array.from(result, (group) => group.agent), ["Codex", "Agent Skills"]);
  assert.deepEqual(names(result), ["deploy", "shared-review"]);
  assert.equal(result[0].skills[0], inventory[0].skills[1]);
  assert.equal(JSON.stringify(inventory), before);
});

test("the same active query includes project matches arriving in progressive discovery", () => {
  const initial = [{ agent: "Codex", skills: [inventory[0].skills[0]] }];
  assert.deepEqual(names(filterSkillGroups(initial, "project review")), []);
  const later = [{ ...initial[0], skills: [...initial[0].skills, inventory[0].skills[1]] }];
  assert.deepEqual(names(filterSkillGroups(later, "project review")), ["deploy"]);
  assert.deepEqual(names(initial), ["browser-check"]);
});
