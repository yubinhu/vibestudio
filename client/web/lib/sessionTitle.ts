import type { TermSession } from "./api";

/** One display name across the rail, compact picker, Home cards, and editor. */
export function sessionTitle(session: Pick<TermSession, "customTitle" | "title" | "label">): string {
  return session.customTitle?.trim() || session.title?.trim() || session.label;
}
