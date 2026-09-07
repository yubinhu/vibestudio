import type { SessionAttention, TermSession } from "./api";

export function attentionBoot(a: SessionAttention): string {
  return a.sequence.slice(0, a.sequence.lastIndexOf(":"));
}

/** A late list response must not roll back a more recent SSE transition. */
export function newerAttention(next: SessionAttention, previous: SessionAttention): boolean {
  if (next.sequence === previous.sequence) return false;
  if (attentionBoot(next) !== attentionBoot(previous)) return true;
  try {
    return BigInt(next.sequence.slice(next.sequence.lastIndexOf(":") + 1)) >
      BigInt(previous.sequence.slice(previous.sequence.lastIndexOf(":") + 1));
  } catch {
    return next.changedAt > previous.changedAt;
  }
}

export function attentionLabel(s: TermSession): string | null {
  switch (s.attention?.state) {
    case "blocked": return "Needs input";
    case "working": return "Working";
    case "idle": return s.attention.kind === "done" ? "Finished" : "Idle";
    default: return null;
  }
}

/** Requests stay at the top even after viewing; manual order wins within a group. */
export function orderSessions(list: TermSession[], order: string[]): TermSession[] {
  const rank = new Map(order.map((id, i) => [id, i]));
  return [...list].sort((a, b) =>
    Number(b.attention?.state === "blocked") - Number(a.attention?.state === "blocked") ||
    (rank.get(a.id) ?? Number.MAX_SAFE_INTEGER) - (rank.get(b.id) ?? Number.MAX_SAFE_INTEGER) ||
    (Number(a.created) || 0) - (Number(b.created) || 0) ||
    (a.id < b.id ? -1 : a.id > b.id ? 1 : 0),
  );
}

export function attentionIsUnread(attention: SessionAttention, seenSequence?: string): boolean {
  return attention.kind !== null && attention.state !== "working" &&
    attention.state !== "unknown" && seenSequence !== undefined && seenSequence !== attention.sequence;
}

export function shouldSound(kind: "request" | "done", watching: boolean): boolean {
  return kind === "request" || !watching;
}
