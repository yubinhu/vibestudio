"use client";

import { type PointerEvent as ReactPointerEvent, useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { useLocation, useNavigate } from "react-router-dom";
import NavBar from "@/components/NavBar";
import NewSessionDialog from "@/components/NewSessionDialog";
import ResizeHandle from "@/components/ResizeHandle";
import TerminalPane from "@/components/TerminalPane";
import * as api from "@/lib/api";
import type { TermSession } from "@/lib/api";
import { log } from "@/lib/log";
import * as push from "@/lib/push";
import * as store from "@/lib/sessions";

// Legacy key string — keep the old "terminals" word so existing users' saved rail width survives the rename.
const RAIL_KEY = "skillviewer-terminals-rail";

function readRailW(): number {
  try {
    const v = Number(localStorage.getItem(RAIL_KEY));
    return Number.isFinite(v) && v > 0 ? v : 240;
  } catch {
    return 240;
  }
}

function PlusIcon() {
  return (
    <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
      <path d="M12 5v14M5 12h14" />
    </svg>
  );
}

// "Open externally" glyph for the open-in-VS-Code button; the tooltip names the
// editor, so the icon stays a neutral, on-theme monochrome stroke.
function OpenExternalIcon() {
  return (
    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
      <path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6" />
      <path d="M15 3h6v6" />
      <path d="M10 14 21 3" />
    </svg>
  );
}

/**
 * The Sessions workspace: a rail of live tmux-backed sessions plus the
 * active terminal. Sessions persist across UI disconnects and are reaped when
 * the backend process exits (see skill-term). The list lives in the shared
 * store (lib/sessions.ts), pushed fresh by its /api/events stream; this
 * component adds a 5s poll as the backstop, so externally exited /
 * watchdog-reaped sessions drop out even without the stream.
 *
 * Two render modes, one implementation: the full /sessions page (NavBar,
 * `?id=` deep link), and `embedded` — chrome-less, fills its parent — for
 * hosts like the studio's Agent side panel. In tight horizontal layouts
 * (a phone, the panel at its default width) the sessions rail collapses
 * into a dropdown row above the terminal, measured on the workspace itself.
 */
export default function SessionsWorkspace({
  visible,
  embedded = false,
  focusId,
  defaultCwd,
  onActiveChange,
}: {
  visible: boolean;
  /** Chrome-less variant for embedding: no NavBar/deep-link, compact rail
   *  with its own New button, h-full instead of h-dvh. */
  embedded?: boolean;
  /** Select this session whenever it's set (e.g. the mining conversation). */
  focusId?: string | null;
  /** Initial working directory for the New-session dialog. */
  defaultCwd?: string;
  /** Reports the selected session — e.g. so an embedding host's "open full
   *  page" affordance can carry the selection along. */
  onActiveChange?: (id: string | null) => void;
}) {
  // Sessions, the seen marks, and the unread math live in the shared store
  // (lib/sessions.ts) so the NavBar dot and the turn-finish notifier keep
  // working when no workspace is mounted; this component renders that store and
  // owns only its own selection + layout state.
  const { sessions, loading, seen } = store.useSessions();
  const [activeId, setActiveId] = useState<string | null>(null);
  const [newOpen, setNewOpen] = useState(false);
  // Web Push offer — shown until the user decides (mainly the installed phone
  // app, where watching desktop-started agents is the whole point).
  const [pushOffer, setPushOffer] = useState(() => push.canOfferPush());

  const location = useLocation();
  const navigate = useNavigate();

  // "Open in VS Code" is offered whenever a local VS Code is reachable AND we're on
  // this machine's own client. It opens the session's folder — locally, or, when a
  // remote is connected, on the remote over VS Code Remote-SSH (the server decides
  // which and resolves the ssh host; see EditorControl). No remote coupling here:
  // the status route runs locally (never proxied) and 404s for a tailscale-fronted
  // phone client, so the button stays hidden there.
  const [vscode, setVscode] = useState<{ available: boolean; name?: string } | null>(null);
  useEffect(() => {
    let alive = true;
    api.editorStatus().then(
      (e) => void (alive && setVscode(e)),
      () => void (alive && setVscode(null)),
    );
    return () => {
      alive = false;
    };
  }, []);
  const canOpenEditor = !!vscode?.available;
  const editorName = vscode?.name ?? "VS Code";
  const openInEditor = useCallback((s: TermSession) => {
    api.editorOpen(s.cwd).catch((e) =>
      log.warn("sessions", "open in editor failed", e instanceof Error ? e.message : String(e)),
    );
  }, []);
  const editorTitle = () => `Open this session's folder in ${editorName}`;

  // Unread dot — the predicate (and its bell-not-activity rationale) lives with
  // the store; this just binds it to this workspace's own selection.
  const unread = useCallback(
    (s: TermSession) => store.isUnread(s, seen, activeId),
    [activeId, seen],
  );

  // Freeze the session you're leaving at "now" — synchronously, before paint, so
  // switching away never flashes a dot on the pane you were just watching (its
  // attach repaint bumped `activity` above the last poll's snapshot). A plain
  // effect would let one painted frame through with the stale mark.
  const prevActiveRef = useRef<string | null>(null);
  useLayoutEffect(() => {
    const prev = prevActiveRef.current;
    if (prev && prev !== activeId) store.markSeen(prev);
    prevActiveRef.current = activeId;
  }, [activeId]);

  // Tell the store which session is on-screen (feeds the NavBar count and the
  // watched session's auto-mark-seen); release on hide/unmount without
  // clobbering a watch another visible workspace holds.
  useEffect(() => {
    if (!visible) return;
    store.setWatched(activeId);
    return () => store.releaseWatched(activeId);
  }, [visible, activeId]);

  const refresh = useCallback(() => store.refresh(), []);

  // Keep the selection valid as the store's list changes (killed/reaped sessions
  // drop out; the first session is selected once the initial fetch lands).
  useEffect(() => {
    if (loading) return;
    setActiveId((cur) => (cur && sessions.some((s) => s.id === cur) ? cur : sessions[0]?.id ?? null));
  }, [sessions, loading]);

  useEffect(() => {
    void refresh();
  }, [refresh]);
  // 5s poll: the backstop behind the store's /api/events stream (and the only
  // signal against a server that doesn't have it).
  useEffect(() => {
    const t = setInterval(() => void refresh(), 5000);
    return () => clearInterval(t);
  }, [refresh]);

  // Deep link: /sessions?id=<session> selects that session (e.g. "Continue
  // the conversation" from the mining card — possibly created a moment ago,
  // hence the refresh). Consumed once — the param is dropped so later visits
  // don't keep forcing the selection.
  useEffect(() => {
    if (!visible || embedded) return;
    const want = new URLSearchParams(location.search).get("id");
    if (want) {
      setActiveId(want);
      void refresh();
      navigate("/sessions", { replace: true });
    }
  }, [visible, embedded, location.search, navigate, refresh]);

  // Programmatic selection from the embedding host (e.g. the studio panel
  // focusing the mining conversation, possibly created a moment ago).
  useEffect(() => {
    if (!focusId) return;
    setActiveId(focusId);
    void refresh();
  }, [focusId, refresh]);

  useEffect(() => {
    onActiveChange?.(activeId);
  }, [activeId, onActiveChange]);

  // A failed close must be visible — a silent ✕ is indistinguishable from a
  // dead button (bitten once by the tmux locale bug). Attributed to its row's
  // label, cleared on success or once the session is gone anyway.
  const [killError, setKillError] = useState<{ id: string; msg: string } | null>(null);
  const kill = async (id: string) => {
    try {
      await api.terminalKill(id);
      setKillError(null);
    } catch (e) {
      const label = sessions.find((s) => s.id === id)?.label ?? id;
      setKillError({ id, msg: `${label} — ${e instanceof Error ? e.message : String(e)}` });
    }
    await refresh();
  };
  useEffect(() => {
    if (killError && !loading && !sessions.some((s) => s.id === killError.id)) setKillError(null);
  }, [sessions, loading, killError]);

  // Manual rail reorder — no handle, no affordance: press a row and drag it. The
  // row itself is the drag surface (native HTML5 drag is unreliable in the macOS
  // webview, so we use pointer events). A click only becomes a drag past a small
  // move threshold, so a plain click still selects; below the threshold nothing
  // happens. On each move the picked-up id is spliced to the row under the
  // pointer — the list reflowing under you is the only feedback — and the new
  // order commits to the store (and localStorage) once on release.
  const ulRef = useRef<HTMLUListElement>(null);
  const [drag, setDrag] = useState<{ id: string; ids: string[] } | null>(null);

  const startRowDrag = useCallback(
    (id: string, e: ReactPointerEvent) => {
      if (e.button !== 0) return; // left button / primary touch only
      const startX = e.clientX;
      const startY = e.clientY;
      let dragging = false;
      const move = (ev: PointerEvent) => {
        if (!dragging) {
          if (Math.hypot(ev.clientX - startX, ev.clientY - startY) < 5) return;
          dragging = true;
          setDrag({ id, ids: sessions.map((s) => s.id) });
          document.body.style.userSelect = "none";
        }
        const ul = ulRef.current;
        if (!ul) return;
        const rows = Array.from(ul.children) as HTMLElement[];
        let to = rows.findIndex((r) => {
          const rect = r.getBoundingClientRect();
          return ev.clientY < rect.top + rect.height / 2;
        });
        if (to < 0) to = rows.length;
        setDrag((d) => {
          if (!d) return d;
          const from = d.ids.indexOf(d.id);
          if (from < 0) return d;
          const next = [...d.ids];
          next.splice(from, 1);
          next.splice(to > from ? to - 1 : to, 0, d.id);
          return { ...d, ids: next };
        });
      };
      const stop = () => {
        window.removeEventListener("pointermove", move);
        window.removeEventListener("pointerup", stop);
        if (!dragging) return; // it was a plain click — let it select
        document.body.style.userSelect = "";
        setDrag((d) => {
          if (d) store.reorder(d.ids);
          return null;
        });
        // Swallow the click this drag-release synthesizes so it can't select a row.
        const swallow = (ce: MouseEvent) => {
          ce.stopPropagation();
          ce.preventDefault();
          document.removeEventListener("click", swallow, true);
        };
        document.addEventListener("click", swallow, true);
        setTimeout(() => document.removeEventListener("click", swallow, true), 250);
      };
      window.addEventListener("pointermove", move);
      window.addEventListener("pointerup", stop);
    },
    [sessions],
  );

  // Tight horizontal layouts (a phone, or the studio's Agent panel at its
  // default width) trade the sessions rail for a dropdown row above the
  // terminal. Measured on the workspace itself, not the viewport, so the
  // embedded panel adapts as it's resized.
  const rootRef = useRef<HTMLDivElement>(null);
  const [narrow, setNarrow] = useState(embedded);
  useEffect(() => {
    const el = rootRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setNarrow(el.clientWidth > 0 && el.clientWidth < 640));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  // Draggable sessions rail on the full page (the embedded panel is sized by its
  // host, AgentPanel). Width persists across visits; the ResizeHandle is the
  // divider, so the rail drops its own border. Clamped to keep the terminal usable.
  const railRef = useRef<HTMLElement>(null);
  const [railW, setRailW] = useState(readRailW);
  const dragRail = useCallback((clientX: number) => {
    const left = railRef.current?.getBoundingClientRect().left;
    if (left == null) return;
    const w = Math.round(Math.max(180, Math.min(520, clientX - left)));
    setRailW(w);
    try {
      localStorage.setItem(RAIL_KEY, String(w));
    } catch {
      /* ignore */
    }
  }, []);

  const active = sessions.find((s) => s.id === activeId) ?? null;
  // Collapsed (narrow) layout hides the rail, so surface a single dot when any
  // hidden session has new output.
  const anyUnread = sessions.some(unread);
  // Mid-drag the rail renders the live-reordered order; otherwise the store's.
  const view = drag
    ? (drag.ids.map((id) => sessions.find((s) => s.id === id)).filter(Boolean) as TermSession[])
    : sessions;

  const pane = active ? (
    <TerminalPane key={active.id} id={active.id} visible={visible} />
  ) : (
    <div className="flex h-full items-center justify-center px-6 text-center">
      <div>
        <p className="text-sm text-muted">No session selected.</p>
        <button
          type="button"
          onClick={() => setNewOpen(true)}
          className="mt-3 rounded-md bg-action px-3 py-1.5 text-sm font-medium text-action-fg hover:opacity-90"
        >
          ＋ New session
        </button>
      </div>
    </div>
  );

  return (
    <div ref={rootRef} className={`flex ${embedded ? "h-full" : "h-dvh"} flex-col bg-app text-fg`}>
      {!embedded && (
        <NavBar
          breadcrumb={
            <>
              <span className="text-faint" aria-hidden>
                /
              </span>
              <span className="truncate font-medium text-fg">Sessions</span>
            </>
          }
        >
          {active && canOpenEditor && (
            <button
              type="button"
              onClick={() => openInEditor(active)}
              title={editorTitle()}
              aria-label={`Open ${active.label} in ${editorName}`}
              className="flex items-center gap-1.5 rounded-md px-2 py-1 text-muted hover:bg-panel hover:text-fg"
            >
              <OpenExternalIcon />
              <span className="hidden text-xs sm:inline">Open in {editorName}</span>
            </button>
          )}
          <button
            type="button"
            onClick={() => setNewOpen(true)}
            title="New session"
            className="flex items-center gap-1.5 rounded-md px-2 py-1 text-muted hover:bg-panel hover:text-fg"
          >
            <PlusIcon />
            <span className="hidden text-xs sm:inline">New session</span>
          </button>
        </NavBar>
      )}

      {narrow ? (
        // Tight layout: the rail becomes a dropdown row above the terminal.
        <div className="flex min-h-0 flex-1 flex-col">
          <div className="flex items-center gap-1.5 border-b border-border bg-surface py-1.5 pl-2 pr-1.5">
            {anyUnread && (
              <span
                className="h-1.5 w-1.5 shrink-0 rounded-full bg-info"
                title="New output in a background session"
                aria-label="New output in a background session"
              />
            )}
            <select
              value={activeId ?? ""}
              onChange={(e) => setActiveId(e.target.value)}
              disabled={sessions.length === 0}
              aria-label="Session"
              title={active?.cwd}
              className="h-7 min-w-0 flex-1 rounded-md border border-border bg-surface px-2 text-xs text-fg outline-none focus:border-accent disabled:opacity-50"
            >
              {sessions.length === 0 ? (
                <option value="">{loading ? "Loading…" : "No sessions yet"}</option>
              ) : (
                sessions.map((s) => (
                  <option key={s.id} value={s.id}>
                    {unread(s) ? "● " : ""}
                    {s.label}
                  </option>
                ))
              )}
            </select>
            {pushOffer && store.nativeNotifyState() !== true && (
              <button
                type="button"
                onClick={() => void push.enablePushInGesture().then(() => setPushOffer(push.canOfferPush()))}
                title="Get notified when an agent finishes a turn"
                className="shrink-0 rounded-md border border-border px-2 py-1 text-xs text-muted hover:bg-panel hover:text-fg"
              >
                Notify me
              </button>
            )}
            {active && (
              <button
                type="button"
                onClick={() => void kill(active.id)}
                aria-label={`Kill ${active.label}`}
                title="Kill session"
                className="shrink-0 rounded p-1 text-faint hover:text-danger"
              >
                ✕
              </button>
            )}
            {active && canOpenEditor && (
              <button
                type="button"
                onClick={() => openInEditor(active)}
                aria-label={`Open ${active.label} in ${editorName}`}
                title={editorTitle()}
                className="shrink-0 rounded p-1 text-muted hover:bg-panel hover:text-fg"
              >
                <OpenExternalIcon />
              </button>
            )}
            <button
              type="button"
              onClick={() => setNewOpen(true)}
              title="New session"
              className="shrink-0 rounded p-1 text-muted hover:bg-panel hover:text-fg"
            >
              <PlusIcon />
            </button>
          </div>
          {killError && (
            <p className="border-b border-border bg-surface px-2 py-1 text-xs text-danger">{killError.msg}</p>
          )}
          <main className="min-h-0 flex-1 bg-surface">{pane}</main>
        </div>
      ) : (
        <div className="flex min-h-0 flex-1">
          <aside
            ref={railRef}
            style={embedded ? undefined : { width: railW }}
            className={`flex shrink-0 flex-col bg-surface ${embedded ? "w-44 border-r border-border" : ""}`}
          >
            {embedded && (
              <div className="flex items-center justify-between border-b border-border py-1 pl-3 pr-1.5">
                <span className="text-[0.65rem] font-semibold uppercase tracking-wider text-muted">Sessions</span>
                <div className="flex items-center gap-0.5">
                  {active && canOpenEditor && (
                    <button
                      type="button"
                      onClick={() => openInEditor(active)}
                      aria-label={`Open ${active.label} in ${editorName}`}
                      title={editorTitle()}
                      className="rounded p-1 text-muted hover:bg-panel hover:text-fg"
                    >
                      <OpenExternalIcon />
                    </button>
                  )}
                  <button
                    type="button"
                    onClick={() => setNewOpen(true)}
                    title="New session"
                    className="rounded p-1 text-muted hover:bg-panel hover:text-fg"
                  >
                    <PlusIcon />
                  </button>
                </div>
              </div>
            )}
            <div className="min-h-0 flex-1 overflow-auto p-2">
              {killError && <p className="px-2 pb-1 text-xs text-danger">{killError.msg}</p>}
              {loading ? (
                <p className="px-2 py-3 text-sm text-muted">Loading…</p>
              ) : sessions.length === 0 ? (
                <p className="px-2 py-3 text-xs leading-relaxed text-muted">
                  No sessions yet. Start one to run Claude Code, Codex, opencode, or a shell on this machine.
                </p>
              ) : (
                <ul ref={ulRef} className="space-y-1">
                  {view.map((s) => (
                    <li key={s.id} className="group flex items-center gap-1">
                      <button
                        type="button"
                        onPointerDown={(e) => startRowDrag(s.id, e)}
                        onClick={() => setActiveId(s.id)}
                        className={`flex min-w-0 flex-1 flex-col items-start rounded-md px-2 py-1.5 text-left transition-colors ${
                          s.id === activeId ? "bg-panel text-fg" : "text-muted hover:bg-panel hover:text-fg"
                        }`}
                      >
                        <span className="flex w-full items-center gap-1.5">
                          {/* Reserved slot so the label never shifts as the dot toggles. */}
                          <span
                            className={`h-1.5 w-1.5 shrink-0 rounded-full ${unread(s) ? "bg-info" : "bg-transparent"}`}
                            title={unread(s) ? "New output" : undefined}
                            aria-label={unread(s) ? "New output" : undefined}
                          />
                          <span className="min-w-0 flex-1 truncate text-sm">{s.label}</span>
                        </span>
                        <span className="w-full truncate pl-3 font-mono text-[0.65rem] text-faint" title={s.cwd}>
                          {s.cwd}
                        </span>
                      </button>
                      <button
                        type="button"
                        onClick={() => void kill(s.id)}
                        aria-label={`Kill ${s.label}`}
                        title="Kill session"
                        className="hidden shrink-0 appearance-none rounded p-1 text-faint hover:text-danger group-hover:inline-block"
                      >
                        ✕
                      </button>
                    </li>
                  ))}
                </ul>
              )}
            </div>
          </aside>

          {!embedded && <ResizeHandle axis="col" onDragTo={dragRail} />}

          <main className="min-w-0 flex-1 bg-surface">{pane}</main>
        </div>
      )}

      {newOpen && (
        <NewSessionDialog
          defaultCwd={defaultCwd}
          onClose={() => setNewOpen(false)}
          onCreated={(s) => {
            setNewOpen(false);
            store.noteCreated(s);
            setActiveId(s.id);
          }}
        />
      )}
    </div>
  );
}
