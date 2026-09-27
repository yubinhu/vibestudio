import type { ComparisonArtifact, ComparisonOwner, ComparisonSession, TermSession } from "./api";
import { sessionTitle } from "./sessionTitle";

/** Known conversations take precedence over a terminal that can be reused. */
export function comparisonOwnersMatch(a: ComparisonOwner | null | undefined, b: ComparisonOwner | null | undefined): boolean {
  if (!a || !b || a.hostId !== b.hostId) return false;
  if (a.conversationId && b.conversationId) {
    return Boolean(a.provider && a.provider === b.provider && a.conversationId === b.conversationId);
  }
  return Boolean(a.terminalId && a.terminalId === b.terminalId && (!a.provider || !b.provider || a.provider === b.provider));
}

/** A conversation survives terminal resume; a host boundary is never inferred from cwd. */
export function comparisonBelongsToSession(comparison: ComparisonSession, session: TermSession, hostId: string): boolean {
  return comparisonOwnersMatch(comparison.config.artifact?.owner, {
    hostId, terminalId: session.id, provider: session.agent, conversationId: session.sessionId,
  });
}

export function comparisonArtifactForSession(session: TermSession, hostId: string, title?: string, description?: string): ComparisonArtifact {
  return {
    title: title?.trim() || `${sessionTitle(session)} · UI diff`,
    ...(description?.trim() ? { description: description.trim() } : {}),
    owner: {
      hostId,
      terminalId: session.id,
      provider: session.agent,
      ...(session.sessionId ? { conversationId: session.sessionId } : {}),
    },
  };
}

export function comparisonTitle(comparison: ComparisonSession): string {
  return comparison.config.artifact?.title || `UI diff · ${comparison.config.repository.split(/[\\/]/).filter(Boolean).at(-1) || comparison.id}`;
}

export function comparisonStateLabel(comparison: ComparisonSession): string {
  if (comparison.state === "starting") return "Starting";
  if (comparison.state === "stopping") return "Stopping";
  if (comparison.state === "failed") return "Failed";
  if (comparison.state === "stopped") return "Stopped";
  if (comparison.windowOpen) return "Open";
  if (comparison.windowRequested) return "Opening";
  return "Ready";
}
