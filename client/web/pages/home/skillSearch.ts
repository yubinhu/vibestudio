import type { AgentSkills } from "@/lib/api";
import { AGENT_GROUP_INFO, kindMeta, KIND_TAG } from "@/lib/agents";

const normalize = (value: string) => value.normalize("NFKD").replace(/\p{M}/gu, "").toLowerCase().replace(/\\/g, "/");

/** Filter the current inventory without changing its order or shared objects.
 * Terms can match different fields, e.g. "codex project review". */
export function filterSkillGroups(groups: AgentSkills[], query: string): AgentSkills[] {
  const terms = normalize(query).trim().split(/\s+/).filter(Boolean);
  if (terms.length === 0) return groups;

  return groups.flatMap((group) => {
    const agent = [group.agent, ...(AGENT_GROUP_INFO[group.agent]?.sharedWith ?? [])];
    if (group.agent === "Agent Skills") agent.push("Standard Agent Skills", "shared");
    const skills = group.skills.filter((skill) => {
      const kind = kindMeta(skill.kind);
      const text = normalize([
        ...agent, skill.name, skill.description, skill.root, skill.project,
        skill.project ? "project" : "global", kind.kind, kind.label, KIND_TAG[kind.kind].label,
        skill.proposed ? "proposed draft" : "",
      ].filter(Boolean).join(" "));
      return terms.every((term) => text.includes(term));
    });
    return skills.length ? [{ ...group, skills }] : [];
  });
}
