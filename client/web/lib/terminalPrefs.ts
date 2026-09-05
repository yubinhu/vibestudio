import * as api from "./api";

export type { TerminalPrefs } from "./api";

/** Agent choice, working directories and flags follow the active server. */
export async function loadTerminalPrefs(): Promise<api.WorkspacePreferences> {
  let prefs = await api.preferencesGet();
  // Lift old origin-only defaults only when the answering switchboard is known
  // to be Local. A remote/phone must never inherit this browser's local paths.
  const key = "skillviewer-terminal-prefs";
  try {
    if (!["localhost", "127.0.0.1", "[::1]"].includes(location.hostname)) return prefs;
    const raw = localStorage.getItem(key);
    if (!raw) return prefs;
    const status = await api.remoteStatus();
    if (status.state !== "idle") return prefs;
    const old: unknown = JSON.parse(raw);
    if (!old || typeof old !== "object" || Array.isArray(old)) return prefs;
    for (const [agentId, value] of Object.entries(old)) {
      if (prefs.terminal[agentId] || !value || typeof value !== "object") continue;
      const p = value as Partial<api.TerminalPrefs>;
      prefs = await api.rememberTerminal(agentId, {
        cwd: typeof p.cwd === "string" ? p.cwd : "",
        ide: !!p.ide, skip: !!p.skip, auto: !!p.auto,
        extra: typeof p.extra === "string" ? p.extra : "",
      });
    }
    localStorage.removeItem(key);
  } catch {
    // Keep the old copy when migration cannot finish; server prefs remain usable.
  }
  return prefs;
}

export const saveTerminalPrefs = api.rememberTerminal;
