// Persisted Studio layout: which sidebar sections are open, any user-pinned
// section heights, and the agent panel width — so the visual setup survives
// closing and reopening a skill. Same client-local approach as theme.ts.
// One global setup (not per skill): it's the user's preferred
// workbench shape.

export interface StudioLayoutPrefs {
  open: { changes: boolean; versions: boolean; github: boolean };
  /** User-pinned px heights from dragging a divider (null = content-sized). */
  filesH: number | null;
  changesH: number | null;
  remoteH: number | null;
  /** Agent panel width in px (null = default). */
  agentW: number | null;
}

const KEY = "skillviewer-studio-layout";

// Remote starts collapsed: publishing is occasional; the changes + history
// above it are the everyday surface.
function defaults(): StudioLayoutPrefs {
  return {
    open: { changes: true, versions: true, github: false },
    filesH: null,
    changesH: null,
    remoteH: null,
    agentW: null,
  };
}

function px(v: unknown): number | null {
  return typeof v === "number" && Number.isFinite(v) && v > 0 ? v : null;
}

export function loadStudioLayout(): StudioLayoutPrefs {
  const d = defaults();
  try {
    const raw = localStorage.getItem(KEY);
    const parsed: unknown = raw ? JSON.parse(raw) : null;
    if (!parsed || typeof parsed !== "object") return d;
    const rec = parsed as Record<string, unknown>;
    const open = (rec.open && typeof rec.open === "object" ? rec.open : {}) as Record<string, unknown>;
    return {
      open: {
        changes: typeof open.changes === "boolean" ? open.changes : d.open.changes,
        versions: typeof open.versions === "boolean" ? open.versions : d.open.versions,
        github: typeof open.github === "boolean" ? open.github : d.open.github,
      },
      filesH: px(rec.filesH),
      changesH: px(rec.changesH),
      remoteH: px(rec.remoteH),
      agentW: px(rec.agentW),
    };
  } catch {
    return d;
  }
}

export function saveStudioLayout(patch: Partial<StudioLayoutPrefs>) {
  const next = { ...loadStudioLayout(), ...patch };
  try {
    localStorage.setItem(KEY, JSON.stringify(next));
  } catch {
    /* ignore */
  }
}
