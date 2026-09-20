// Agent identity helpers — map a skill to the coding agent it belongs to.
// `agentColor` keys off the discovery group label; `agentForPath` infers the
// agent from the canonical home dir in a skill's path (so a skill opened via
// Browse / path input — not just discovery — still shows its owner).

export const AGENT_COLORS: Record<string, string> = {
  "Claude Code": "#d97757",
  Codex: "#10a37f",
  Cursor: "#7c83ff",
  OpenClaw: "#a855f7",
  "Gemini CLI": "#4285f4",
  opencode: "#f59e0b",
  Hermes: "#c17d3f",
  Pi: "#ab70d6",
  // The Agent Skills standard shared dir (~/.agents/skills), read by many agents.
  "Agent Skills": "#0ea5e9",
};

export function agentColor(label: string): string {
  return AGENT_COLORS[label] ?? "var(--muted)";
}

// Extra context for a discovery group's header. Used for the shared
// `~/.agents/skills` standard dir, to make clear which agents read it (and which
// don't) since it isn't a single agent's private folder.
export interface AgentGroupInfo {
  /** Agents that read this shared location (rendered as colored chips). */
  sharedWith: string[];
  /** Notable agents that do NOT read it. */
  excludes: string[];
}

export const AGENT_GROUP_INFO: Record<string, AgentGroupInfo> = {
  "Agent Skills": {
    sharedWith: ["Codex", "Cursor", "Gemini CLI", "opencode", "Pi"],
    excludes: ["Claude Code", "Hermes"],
  },
};

const PATH_RULES: [RegExp, string][] = [
  [/(^|\/)\.hermes(\/|$)/, "Hermes"],
  [/(^|\/)\.pi(\/|$)/, "Pi"],
  [/(^|\/)\.claude(\/|$)/, "Claude Code"],
  [/(^|\/)\.codex(\/|$)/, "Codex"],
  [/(^|\/)\.cursor(\/|$)/, "Cursor"],
  [/(^|\/)\.openclaw(\/|$)/, "OpenClaw"],
  // opencode: its global skills home (~/.config/opencode) and the project marker.
  [/(^|\/)\.config\/opencode(\/|$)/, "opencode"],
  [/(^|\/)\.opencode(\/|$)/, "opencode"],
  // The Agent Skills standard shared dirs, ~/.agents/skills (and the singular variant).
  [/(^|\/)\.agents?(\/|$)/, "Agent Skills"],
];

/** Best-effort agent label for a skill path, or null for an unaffiliated folder. */
export function agentForPath(p: string): string | null {
  const s = p.replace(/\\/g, "/");
  for (const [re, label] of PATH_RULES) if (re.test(s)) return label;
  return null;
}

// --- skill provenance ---------------------------------------------------
// We rank skills by how "yours" they are: a skill you wrote/customized ranks
// above a first-party official one, which ranks above a third-party package.
// "studio" is the bundled activation skill this app ships (see isBootstrapSkill).
export type SkillKind = "personal" | "official" | "plugin" | "studio";

export interface KindMeta {
  kind: SkillKind;
  label: string;
  rank: number; // 0 = highest priority
}

export const KIND_META: Record<SkillKind, KindMeta> = {
  personal: { kind: "personal", label: "Personal", rank: 0 },
  official: { kind: "official", label: "Official", rank: 1 },
  // The VibeStudio activation skill tallies with official but keeps its own
  // badge; rank it just after official so the list order matches that tally.
  studio: { kind: "studio", label: "VibeStudio", rank: 2 },
  plugin: { kind: "plugin", label: "Plugin", rank: 3 },
};

/** Short pill label + classes for a kind tag shown on a skill card / header. */
export const KIND_TAG: Record<SkillKind, { label: string; cls: string }> = {
  personal: { label: "Yours", cls: "bg-accent-soft text-accent" },
  official: { label: "Official", cls: "bg-[color-mix(in_srgb,var(--ok)_16%,transparent)] text-ok" },
  plugin: { label: "Plugin", cls: "bg-panel text-muted" },
  studio: { label: "VibeStudio", cls: "bg-panel text-muted" },
};

export function kindMeta(kind: string): KindMeta {
  return KIND_META[(kind as SkillKind) in KIND_META ? (kind as SkillKind) : "personal"];
}

/** Best-effort provenance for a single skill from its path alone (used on the
 *  skill page, where we don't have the authoritative discovery `kind`). The
 *  discovery list uses the backend-computed `kind` instead — it can read the
 *  Cursor manifest, which a path can't reveal. */
export function skillKind(root: string): KindMeta {
  const s = root.replace(/\\/g, "/");
  if (/\/\.codex\/skills\/\.system\//.test(s)) return KIND_META.official;
  if (isBootstrapSkill(root)) return KIND_META.studio; // shipped by VibeStudio, not yours
  if (/\/\.cursor\/skills-cursor\//.test(s)) return KIND_META.official; // built-in Cursor skills

  const isPackaged =
    /\/plugins\//.test(s) || /\/marketplaces\//.test(s) || /\/remote\/plugins\//.test(s);
  if (!isPackaged) return KIND_META.personal;
  // The official marketplace's own `plugins/` are first-party; its
  // `external_plugins/` (and any other marketplace / remote release) are
  // third-party packages.
  const official =
    /\/marketplaces\/[^/]*official[^/]*\/plugins\//i.test(s) && !/\/external_plugins\//.test(s);
  return official ? KIND_META.official : KIND_META.plugin;
}

// --- bundled built-in skills --------------------------------------------
// Skills this app ships and installs into your shared skills dirs
// (~/.agents/skills, ~/.claude/skills, …): "load-secrets" (the secrets
// activation fallback; formerly "skill-studio") and "skill-miner". They land in
// personal dirs, so discovery would tag them "personal" and surface them as
// your own — but you didn't author them, so we give them the "studio" kind:
// they keep their folder names but tuck into the bundled dropdown with a
// "VibeStudio" tag instead of showing as your cards.
export const BUNDLED_SKILL_DIRNAMES = ["load-secrets", "skill-miner", "skill-studio"];

// Bundled skills the user is meant to make their own after install: editable
// and versionable in place (the quality bar, prompts and adapters are theirs to
// tune; a reinstall restores the official version, and with a `.git` present
// the update lands as reviewable uncommitted changes). The rest (load-secrets)
// stay locked down — they're plumbing that terminals and rc-file wiring depend
// on, with no customization upside.
const EDITABLE_BUNDLED_DIRNAMES = ["skill-miner"];

const lastSegment = (root: string) => root.replace(/\\/g, "/").split("/").filter(Boolean).pop() ?? "";

/** True for a bundled built-in skill, matched by its installed folder name. */
export function isBootstrapSkill(root: string): boolean {
  return BUNDLED_SKILL_DIRNAMES.includes(lastSegment(root));
}

/** True for a bundled skill the user may edit and version in place. */
export function isEditableBundledSkill(root: string): boolean {
  return EDITABLE_BUNDLED_DIRNAMES.includes(lastSegment(root));
}
