import { Badge, type Tone } from "./ui";
import { agentColor } from "@/lib/agents";
import type { ConnectorAgent, ConnectorInfo, ConnectorState } from "@/lib/api";

const STATUS: Record<ConnectorState, { label: string; tone: Tone; explanation: string }> = {
  configured: { label: "Configured", tone: "muted", explanation: "Found in settings or plugin files, or enabled for this account; the service connection has not been verified." },
  connected: { label: "Connected", tone: "ok", explanation: "The agent reported this connector connected at the last check." },
  needs_auth: { label: "Sign-in needed", tone: "warn", explanation: "This connector requires sign-in." },
  disabled: { label: "Disabled", tone: "muted", explanation: "This connector is disabled in this scope." },
  error: { label: "Error", tone: "danger", explanation: "A connector error was reported." },
};

export function ConnectorStatus({ state }: { state: ConnectorState }) {
  const meta = STATUS[state];
  return <Badge tone={meta.tone} title={meta.explanation}>{meta.label}</Badge>;
}

/** Identity comes from the server's registry; a new agent needs no UI branch.
 * Conflicting project/plugin states stay visible instead of claiming access. */
export function ConnectorAgents({ connector, agents, compact = false }: {
  connector: ConnectorInfo;
  agents: ConnectorAgent[];
  compact?: boolean;
}) {
  const ids = [...new Set(connector.availability.map((a) => a.agentId))];
  return (
    <span className="flex flex-wrap items-center gap-1.5">
      {ids.map((id) => {
        const agent = agents.find((a) => a.id === id);
        const entries = connector.availability.filter((a) => a.agentId === id);
        let states = [...new Set(entries.map((a) => a.state))];
        // Passive configuration is supporting evidence, not a conflicting live
        // state. Keep real conflicts (e.g. disabled in a project) visible.
        if (states.includes("connected")) states = states.filter((state) => state !== "configured");
        const meta = states.length === 1 ? STATUS[states[0]] : { label: "Mixed", tone: "warn" as Tone, explanation: "Availability differs between sources or project scopes. Expand for details." };
        const label = agent?.label ?? id;
        return (
          <Badge key={id} tone={compact ? "muted" : meta.tone} className="max-w-full" title={`${label}: ${meta.label}. ${meta.explanation}${agent?.clients.length ? ` Clients: ${agent.clients.join(", ")}.` : ""}`}>
            <span className="h-1.5 w-1.5 shrink-0 rounded-full" style={{ background: agentColor(label) }} aria-hidden />
            <span className="truncate">{label}</span>
            <span className={compact ? "sr-only" : "font-normal"}>· {meta.label}</span>
          </Badge>
        );
      })}
      {ids.length === 0 && <span className="text-xs text-faint">No agents configured</span>}
    </span>
  );
}
