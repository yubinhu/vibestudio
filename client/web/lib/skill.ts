// Pure (no fs) helpers for parsing, serializing and validating Agent Skills.
// Shared between the server (route handlers) and the client (live editor).
//
// Reference: https://agentskills.io/specification

import { parse as parseYaml, stringify as stringifyYaml } from "yaml";

/** Frontmatter fields defined by the Agent Skills spec, plus any extras. */
export interface SkillFrontmatter {
  name?: string;
  description?: string;
  license?: string;
  compatibility?: string;
  metadata?: Record<string, string>;
  "allowed-tools"?: string;
  // Any additional, non-spec keys are preserved verbatim.
  [key: string]: unknown;
}

/** Frontmatter keys defined by the spec, in canonical serialization order. */
// Order matches the spec's frontmatter table (metadata before allowed-tools).
export const SPEC_FIELD_ORDER = [
  "name",
  "description",
  "license",
  "compatibility",
  "metadata",
  "allowed-tools",
] as const;

export const KNOWN_FIELDS = new Set<string>(SPEC_FIELD_ORDER);

export const LIMITS = {
  nameMax: 64,
  descriptionMax: 1024,
  compatibilityMax: 500,
  bodyMaxLines: 500,
  bodyMaxTokens: 5000,
} as const;

/** name: 1-64 chars, lowercase alphanumeric + single hyphens, no leading/trailing/consecutive hyphen. */
export const NAME_REGEX = /^[a-z0-9]+(?:-[a-z0-9]+)*$/;

export type IssueLevel = "error" | "warning" | "info";

export interface ValidationIssue {
  level: IssueLevel;
  field: string;
  message: string;
}

export interface ParsedSkill {
  hasFrontmatter: boolean;
  frontmatter: SkillFrontmatter;
  frontmatterRaw: string;
  body: string;
  parseError?: string;
}

const FRONTMATTER_RE = /^﻿?---[ \t]*\r?\n([\s\S]*?)\r?\n---[ \t]*(?:\r?\n|$)/;

/** Split a SKILL.md document into its YAML frontmatter block and Markdown body. */
export function splitFrontmatter(raw: string): {
  hasFrontmatter: boolean;
  frontmatterRaw: string;
  body: string;
} {
  const match = FRONTMATTER_RE.exec(raw);
  if (!match) {
    return { hasFrontmatter: false, frontmatterRaw: "", body: raw };
  }
  return {
    hasFrontmatter: true,
    frontmatterRaw: match[1],
    body: raw.slice(match[0].length),
  };
}

/** Parse a SKILL.md document into structured frontmatter + body. */
export function parseSkillMd(raw: string): ParsedSkill {
  const { hasFrontmatter, frontmatterRaw, body } = splitFrontmatter(raw);
  if (!hasFrontmatter) {
    return { hasFrontmatter: false, frontmatter: {}, frontmatterRaw: "", body };
  }

  let frontmatter: SkillFrontmatter = {};
  let parseError: string | undefined;
  try {
    const parsed = parseYaml(frontmatterRaw);
    if (parsed == null) {
      frontmatter = {};
    } else if (typeof parsed !== "object" || Array.isArray(parsed)) {
      parseError = "Frontmatter must be a YAML mapping (key: value pairs).";
    } else {
      frontmatter = parsed as SkillFrontmatter;
    }
  } catch (err) {
    parseError = err instanceof Error ? err.message : String(err);
  }

  return { hasFrontmatter: true, frontmatter, frontmatterRaw, body, parseError };
}

/**
 * Serialize frontmatter + body back into a SKILL.md document.
 * Spec fields are emitted first in canonical order; unknown fields are kept.
 */
export function serializeSkillMd(
  frontmatter: SkillFrontmatter,
  body: string,
): string {
  const ordered: Record<string, unknown> = {};

  for (const key of SPEC_FIELD_ORDER) {
    const value = frontmatter[key];
    if (value === undefined || value === null) continue;
    if (typeof value === "string" && value.trim() === "" && key !== "name") {
      continue;
    }
    if (key === "metadata") {
      const meta = normalizeMetadata(value);
      if (meta && Object.keys(meta).length > 0) ordered[key] = meta;
      continue;
    }
    ordered[key] = value;
  }

  // Preserve any extra (non-spec) fields after the known ones.
  for (const [key, value] of Object.entries(frontmatter)) {
    if (KNOWN_FIELDS.has(key)) continue;
    if (value === undefined) continue;
    ordered[key] = value;
  }

  const yaml = stringifyYaml(ordered, { lineWidth: 0 }).replace(/\n$/, "");
  // Trim leading/trailing *blank lines* only — preserve trailing spaces on the
  // final content line (a Markdown hard line break) so the body round-trips.
  const normalizedBody = body.replace(/^\n+/, "").replace(/\n+$/, "");
  return `---\n${yaml}\n---\n\n${normalizedBody}\n`;
}

/** Coerce a metadata value into a string→string map (spec requires string values). */
export function normalizeMetadata(value: unknown): Record<string, string> | null {
  if (value == null || typeof value !== "object" || Array.isArray(value)) {
    return null;
  }
  const out: Record<string, string> = {};
  for (const [k, v] of Object.entries(value as Record<string, unknown>)) {
    if (v == null) continue;
    out[k] = typeof v === "string" ? v : String(v);
  }
  return out;
}

/**
 * `metadata` key skills use to declare the env vars their scripts read —
 * VibeStudio's own portable contract, auto-detected on save (see
 * `reconcileRequiredEnv`). It powers the export-time "bundle these secrets?"
 * prompt and lets an import surface which secrets a skill needs before it runs.
 * Stored as a space-separated string (the spec types metadata values as
 * strings), so it stays valid under `skills-ref validate` and travels inside
 * SKILL.md wherever the skill goes.
 */
export const REQUIRED_ENV_KEY = "required-env";

/** A valid environment-variable name: leading letter/underscore, then word chars.
 *  Mirrors the backend `valid_key` check in `secrets.rs`. */
const ENV_NAME_RE = /^[A-Za-z_][A-Za-z0-9_]*$/;

/** Env var names the skill declares it needs (from `metadata.required-env`).
 *  Tokens that aren't valid env-var names are dropped, so a noisy declaration
 *  (e.g. prose lifted from the skill body) can't surface junk like `(or`,
 *  `claude/codex/gemini`, or `|` as if they were secrets. */
export function requiredEnv(fm: SkillFrontmatter): string[] {
  const raw = fm.metadata?.[REQUIRED_ENV_KEY];
  if (typeof raw !== "string") return [];
  return raw.trim().split(/\s+/).filter((t) => ENV_NAME_RE.test(t));
}

/** A frontmatter with `metadata.required-env` set to `names` (cleared if empty). */
export function withRequiredEnv(fm: SkillFrontmatter, names: string[]): SkillFrontmatter {
  const metadata: Record<string, string> = { ...(normalizeMetadata(fm.metadata) ?? {}) };
  if (names.length) metadata[REQUIRED_ENV_KEY] = names.join(" ");
  else delete metadata[REQUIRED_ENV_KEY];
  return { ...fm, metadata };
}

/** Rough token estimate (~4 chars/token) used only for the size advisory. */
export function estimateTokens(text: string): number {
  return Math.ceil(text.length / 4);
}

export function countLines(text: string): number {
  if (text === "") return 0;
  return text.split("\n").length;
}

/** Count Unicode code points (so astral chars/emoji count as 1, matching "chars"). */
export function cpLen(text: string): number {
  return [...text].length;
}

export interface ValidationInput {
  frontmatter: SkillFrontmatter;
  body: string;
  hasFrontmatter: boolean;
  parseError?: string;
  /** Parent directory name (for the name-must-match-folder rule). */
  dirName?: string;
  /** All relative file paths in the skill (for reference checking). */
  files?: string[];
}

/** Validate a parsed skill against the Agent Skills specification. */
export function validateSkill(input: ValidationInput): ValidationIssue[] {
  const issues: ValidationIssue[] = [];
  const { frontmatter: fm, body, hasFrontmatter, parseError, dirName, files } = input;

  if (!hasFrontmatter) {
    issues.push({
      level: "error",
      field: "frontmatter",
      message: "SKILL.md must begin with a YAML frontmatter block delimited by '---'.",
    });
    return issues;
  }
  if (parseError) {
    issues.push({ level: "error", field: "frontmatter", message: `YAML parse error: ${parseError}` });
    return issues;
  }

  // --- name -------------------------------------------------------------
  const name = fm.name;
  if (name == null || (typeof name === "string" && name.trim() === "")) {
    issues.push({ level: "error", field: "name", message: "`name` is required." });
  } else if (typeof name !== "string") {
    issues.push({ level: "error", field: "name", message: "`name` must be a string." });
  } else {
    if (cpLen(name) > LIMITS.nameMax) {
      issues.push({
        level: "error",
        field: "name",
        message: `\`name\` must be at most ${LIMITS.nameMax} characters (got ${cpLen(name)}).`,
      });
    }
    if (!NAME_REGEX.test(name)) {
      issues.push({
        level: "error",
        field: "name",
        message:
          "`name` must be lowercase alphanumeric with single hyphens, and may not start/end with or repeat a hyphen.",
      });
    }
    if (dirName && name !== dirName) {
      issues.push({
        level: "error",
        field: "name",
        message: `\`name\` ("${name}") must match the parent directory name ("${dirName}").`,
      });
    }
  }

  // --- description ------------------------------------------------------
  const desc = fm.description;
  if (desc == null || (typeof desc === "string" && desc.trim() === "")) {
    issues.push({ level: "error", field: "description", message: "`description` is required and must be non-empty." });
  } else if (typeof desc !== "string") {
    issues.push({ level: "error", field: "description", message: "`description` must be a string." });
  } else {
    if (cpLen(desc) > LIMITS.descriptionMax) {
      issues.push({
        level: "error",
        field: "description",
        message: `\`description\` must be at most ${LIMITS.descriptionMax} characters (got ${cpLen(desc)}).`,
      });
    }
    if (cpLen(desc) < 20) {
      issues.push({
        level: "warning",
        field: "description",
        message: "`description` is very short. Describe what the skill does AND when to use it for better activation.",
      });
    } else if (!/\b(use|when|for)\b/i.test(desc)) {
      issues.push({
        level: "info",
        field: "description",
        message: "Consider stating when to use the skill (e.g. \"Use when …\") so agents activate it reliably.",
      });
    }
  }

  // --- license ----------------------------------------------------------
  if (fm.license != null && typeof fm.license !== "string") {
    issues.push({ level: "warning", field: "license", message: "`license` should be a string." });
  }

  // --- compatibility ----------------------------------------------------
  if (fm.compatibility != null) {
    if (typeof fm.compatibility !== "string") {
      issues.push({ level: "warning", field: "compatibility", message: "`compatibility` should be a string." });
    } else if (cpLen(fm.compatibility) > LIMITS.compatibilityMax) {
      issues.push({
        level: "error",
        field: "compatibility",
        message: `\`compatibility\` must be at most ${LIMITS.compatibilityMax} characters (got ${cpLen(fm.compatibility)}).`,
      });
    }
  }

  // --- allowed-tools ----------------------------------------------------
  if (fm["allowed-tools"] != null && typeof fm["allowed-tools"] !== "string") {
    issues.push({
      level: "warning",
      field: "allowed-tools",
      message: "`allowed-tools` should be a space-separated string of tool names.",
    });
  }

  // --- metadata ---------------------------------------------------------
  if (fm.metadata != null) {
    if (typeof fm.metadata !== "object" || Array.isArray(fm.metadata)) {
      issues.push({ level: "warning", field: "metadata", message: "`metadata` should be a mapping of string keys to string values." });
    } else {
      for (const [k, v] of Object.entries(fm.metadata as Record<string, unknown>)) {
        if (typeof v !== "string") {
          issues.push({
            level: "warning",
            field: "metadata",
            message: `metadata.${k} is a ${typeof v}, not a string; the spec requires string values and it will be saved as "${String(v)}".`,
          });
        }
      }
    }
  }

  // --- unknown fields ---------------------------------------------------
  for (const key of Object.keys(fm)) {
    if (!KNOWN_FIELDS.has(key)) {
      issues.push({
        level: "info",
        field: key,
        message: `\`${key}\` is not a spec-defined field. Custom data belongs under \`metadata\`.`,
      });
    }
  }

  // --- body size --------------------------------------------------------
  const lines = countLines(body);
  if (lines > LIMITS.bodyMaxLines) {
    issues.push({
      level: "warning",
      field: "body",
      message: `Body is ${lines} lines; spec recommends keeping SKILL.md under ${LIMITS.bodyMaxLines}. Move detail into references/.`,
    });
  }
  const tokens = estimateTokens(body);
  if (tokens > LIMITS.bodyMaxTokens) {
    issues.push({
      level: "warning",
      field: "body",
      message: `Body is ~${tokens} tokens; spec recommends under ${LIMITS.bodyMaxTokens}. Consider progressive disclosure.`,
    });
  }
  if (body.trim() === "") {
    issues.push({ level: "warning", field: "body", message: "Body is empty. Add instructions for the agent to follow." });
  }

  // --- file references --------------------------------------------------
  if (files && files.length) {
    const fileSet = new Set(files.map((f) => f.replace(/^\.\//, "")));
    for (const ref of extractFileReferences(body, files)) {
      const normalized = ref.replace(/^\.\//, "");
      if (!fileSet.has(normalized)) {
        issues.push({
          level: "warning",
          field: "references",
          message: `Body references "${ref}" but no such file exists in the skill.`,
        });
      }
    }
  }

  return issues;
}

/** Remove fenced code blocks and inline code spans so refs inside them aren't matched. */
function stripCode(body: string): string {
  return body
    .replace(/```[\s\S]*?```/g, " ")
    .replace(/~~~[\s\S]*?~~~/g, " ")
    .replace(/`[^`\n]*`/g, " ");
}

/**
 * Heuristically extract relative file references from the Markdown body:
 * markdown links/images to relative paths, bare `dir/file.ext` mentions in any
 * subdirectory, and (when `files` is supplied) bare root-level filenames that
 * match a real file. References inside code spans/fences are ignored.
 */
export function extractFileReferences(body: string, files?: string[]): string[] {
  const refs = new Set<string>();
  const text = stripCode(body);

  // Markdown links / images: [text](path) or ![alt](path)
  const linkRe = /\]\(\s*<?([^)\s>]+)>?(?:\s+"[^"]*")?\s*\)/g;
  let m: RegExpExecArray | null;
  while ((m = linkRe.exec(text))) {
    const target = m[1].split("#")[0];
    if (isRelativeFileRef(target)) refs.add(target);
  }

  // Bare references in any subdirectory, e.g. `scripts/extract.py`, `workflows/build.md`.
  const bareRe = /(?<![\w/.-])([\w.-]+\/[\w./-]+\.[A-Za-z0-9]+)/g;
  while ((m = bareRe.exec(text))) {
    refs.add(m[1]);
  }

  // Bare root-level filenames (no slash) — only when they match a real file, to
  // avoid flagging prose words that happen to look like filenames.
  if (files && files.length) {
    const rootNames = new Set(files.filter((f) => !f.includes("/")));
    const nameRe = /(?<![\w/.-])([\w-]+\.[A-Za-z0-9]+)(?![\w/])/g;
    while ((m = nameRe.exec(text))) {
      if (rootNames.has(m[1])) refs.add(m[1]);
    }
  }

  return [...refs];
}

function isRelativeFileRef(target: string): boolean {
  if (!target) return false;
  if (/^[a-z]+:\/\//i.test(target)) return false; // http(s):, mailto:, etc.
  if (target.startsWith("#")) return false;
  if (target.startsWith("/")) return false; // absolute
  if (!/\.[A-Za-z0-9]+$/.test(target)) return false; // needs a file extension
  return true;
}

export function summarizeIssues(issues: ValidationIssue[]): {
  errors: number;
  warnings: number;
  infos: number;
  ok: boolean;
} {
  let errors = 0;
  let warnings = 0;
  let infos = 0;
  for (const i of issues) {
    if (i.level === "error") errors++;
    else if (i.level === "warning") warnings++;
    else infos++;
  }
  return { errors, warnings, infos, ok: errors === 0 };
}
