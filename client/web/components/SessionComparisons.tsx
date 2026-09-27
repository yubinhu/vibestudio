import { useCallback, useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import * as api from "@/lib/api";
import { comparisonArtifactForSession, comparisonBelongsToSession, comparisonStateLabel, comparisonTitle } from "@/lib/comparisonArtifacts";
import { useRemote } from "@/lib/remote";
import { sessionTitle } from "@/lib/sessionTitle";
import { Modal } from "./Modal";
import { Badge, btnGhost, btnPrimary, Spinner, type Tone } from "./ui";
import ComparisonConfigForm from "./ComparisonConfigForm";

function ComparisonIcon() {
  return <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" aria-hidden><rect x="3" y="4" width="18" height="16" rx="2" /><path d="M12 4v16M6 8h3M15 8h3M6 12h3M15 12h3" /></svg>;
}

function timeLabel(milliseconds: number): string {
  const date = new Date(milliseconds);
  return Number.isFinite(date.getTime()) ? date.toLocaleString(undefined, { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" }) : "Unavailable";
}

function statusTone(comparison: api.ComparisonSession): Tone {
  if (comparison.state === "failed") return "danger";
  if (comparison.state === "starting" || comparison.state === "stopping") return "warn";
  return comparison.windowOpen ? "ok" : "muted";
}

function Detail({ label, children, mono = false }: { label: string; children: React.ReactNode; mono?: boolean }) {
  return <div className="min-w-0"><dt className="text-[0.65rem] text-muted">{label}</dt><dd className={`mt-0.5 break-all text-xs text-fg ${mono ? "font-mono" : ""}`}>{children || "—"}</dd></div>;
}

/** Local artifacts stay available even when the selected SSH host is reconnecting. */
export default function SessionComparisons({ session, visible, compact = false }: { session: api.TermSession; visible: boolean; compact?: boolean }) {
  const { workspaceHost } = useRemote();
  const hostId = workspaceHost ?? "local";
  const [available, setAvailable] = useState<boolean | null>(null);
  const [supported, setSupported] = useState(true);
  const [comparisons, setComparisons] = useState<api.ComparisonSession[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [open, setOpen] = useState(false);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [tab, setTab] = useState<"session" | "unassigned">("session");
  const [form, setForm] = useState<"new" | api.ComparisonSession | null>(null);
  const [busy, setBusy] = useState(false);
  const refreshPending = useRef(false);
  const mutationPending = useRef(false);
  const generation = useRef(0);
  const enriched = useRef(new Set<string>());
  const trigger = useRef<HTMLButtonElement>(null);
  const dialog = useRef<HTMLDivElement>(null);

  const refresh = useCallback(async () => {
    if (refreshPending.current || mutationPending.current) return;
    refreshPending.current = true;
    const currentGeneration = generation.current;
    try {
      const capabilities = await api.comparisonCapabilities();
      if (currentGeneration !== generation.current) return;
      setAvailable(capabilities.available);
      setSupported(capabilities.sessionArtifacts === true);
      if (!capabilities.available) return;
      const entries = await api.comparisonList();
      if (currentGeneration !== generation.current) return;
      setComparisons(entries.sort((a, b) => b.createdAt - a.createdAt));
      setError(null);
    } catch (cause) {
      if (currentGeneration !== generation.current) return;
      if ((cause as { status?: number })?.status === 404) setAvailable(false);
      else setError(cause instanceof Error ? cause.message : "Could not load UI diffs.");
    } finally {
      refreshPending.current = false;
      if (currentGeneration === generation.current) setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (!visible) return;
    void refresh();
    const timer = setInterval(() => void refresh(), open ? 1500 : 5000);
    const onFocus = () => void refresh();
    window.addEventListener("focus", onFocus);
    return () => { clearInterval(timer); window.removeEventListener("focus", onFocus); };
  }, [visible, open, refresh]);
  useEffect(() => () => { generation.current++; }, []);
  useEffect(() => {
    setOpen(false);
    setForm(null);
    setSelectedId(null);
    setTab("session");
  }, [session.id, hostId, visible]);
  useEffect(() => {
    if (!open) return;
    dialog.current?.querySelector<HTMLElement>("button:not(:disabled)")?.focus();
    return () => { trigger.current?.focus(); };
  }, [open]);

  // A terminal's native conversation ID can become known after the artifact was
  // created. Save that exact identity so resuming the conversation retains its diffs.
  useEffect(() => {
    if (!supported || !session.sessionId) return;
    for (const comparison of comparisons) {
      const artifact = comparison.config.artifact;
      if (!artifact || artifact.owner.conversationId || !comparisonBelongsToSession(comparison, session, hostId)) continue;
      const key = `${comparison.id}:${session.sessionId}`;
      if (enriched.current.has(key)) continue;
      enriched.current.add(key);
      void api.comparisonUpdate(comparison.id, {
        artifact: { ...artifact, owner: { ...artifact.owner, provider: session.agent, conversationId: session.sessionId } },
      }).then(() => refresh()).catch(() => { enriched.current.delete(key); });
    }
  }, [comparisons, session, hostId, supported, refresh]);

  const owned = comparisons.filter((comparison) => comparisonBelongsToSession(comparison, session, hostId));
  const unassigned = comparisons.filter((comparison) => !comparison.config.artifact?.owner);
  const entries = tab === "session" ? owned : unassigned;
  const selected = entries.find((comparison) => comparison.id === selectedId) ?? entries[0] ?? null;

  const mutate = async (action: () => Promise<api.ComparisonSession>) => {
    if (mutationPending.current) return;
    mutationPending.current = true;
    generation.current++;
    setBusy(true);
    setError(null);
    try {
      const result = await action();
      setComparisons((previous) => [result, ...previous.filter((comparison) => comparison.id !== result.id)].sort((a, b) => b.createdAt - a.createdAt));
      setSelectedId(result.id);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Could not update this UI diff.");
    } finally {
      mutationPending.current = false;
      setBusy(false);
    }
  };
  const saveConfig = async (config: api.ComparisonConfig) => {
    if (mutationPending.current) return;
    mutationPending.current = true;
    generation.current++;
    setBusy(true);
    setError(null);
    try {
      const result = form && form !== "new" ? await api.comparisonOpen(form.id, config) : await api.comparisonStart(config);
      setComparisons((previous) => [result, ...previous.filter((comparison) => comparison.id !== result.id)]);
      setSelectedId(result.id);
      setForm(null);
      setTab("session");
    } finally {
      mutationPending.current = false;
      setBusy(false);
    }
  };
  const closeDialog = () => { if (!busy) { setOpen(false); setForm(null); } };

  if (available === false || !visible) return null;
  return (
    <>
      <button
        ref={trigger}
        type="button"
        aria-label={`UI diffs for ${sessionTitle(session)} (${owned.length})`}
        title={`UI diffs for ${sessionTitle(session)} (${owned.length})`}
        aria-haspopup="dialog"
        onClick={() => { setOpen(true); void refresh(); }}
        className={`relative flex shrink-0 items-center gap-1.5 rounded-md text-muted hover:bg-panel hover:text-fg ${compact ? "p-1" : "px-2 py-1"}`}
      >
        <ComparisonIcon />
        {!compact && <span className="text-xs">UI diffs</span>}
        {owned.length > 0 && <span aria-hidden className={`rounded-full bg-accent-soft font-medium text-accent ${compact ? "absolute -right-1 -top-1 min-w-3 px-0.5 text-[0.55rem] leading-3" : "px-1.5 text-[0.65rem] leading-4"}`}>{owned.length}</span>}
      </button>
      {open && createPortal(
        <div ref={dialog} role="dialog" aria-modal="true" aria-label={`UI diffs for ${sessionTitle(session)}`} onKeyDown={(event) => {
          if (event.key !== "Tab") return;
          const controls = Array.from(event.currentTarget.querySelectorAll<HTMLElement>("button:not(:disabled), input:not(:disabled), textarea:not(:disabled), select:not(:disabled), summary"))
            .filter((control) => control.getClientRects().length > 0);
          const first = controls[0];
          const last = controls.at(-1);
          if (!first || !last) event.preventDefault();
          else if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last.focus(); }
          else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first.focus(); }
        }}>
          <Modal title="UI diffs" titleLeading={<ComparisonIcon />} onClose={closeDialog} dismissDisabled={busy} widthClass="max-w-4xl">
            <div className="flex max-h-[calc(100dvh-7rem)] min-h-0 flex-col">
              <div className="flex flex-wrap items-center gap-2 border-b border-border px-4 py-3">
                <div className="min-w-0 flex-1"><p className="truncate text-sm font-medium" title={sessionTitle(session)}>{sessionTitle(session)}</p><p className="truncate text-[0.65rem] text-muted">{hostId === "local" ? "Local session" : hostId} · {session.agent}</p></div>
                {!form && supported && <button type="button" disabled={busy || loading} onClick={() => setForm("new")} className={btnGhost}>＋ New UI diff</button>}
              </div>
              {error && <div className="flex items-center gap-3 border-b border-border px-4 py-3"><p role="alert" className="min-w-0 flex-1 text-xs text-danger">{error}</p><button type="button" onClick={() => void refresh()} disabled={busy} className={btnGhost}>Retry</button></div>}
              {!supported ? <p className="p-5 text-sm text-muted">Restart VibeStudio desktop to manage session UI diffs.</p> : form ? (
                <div className="min-h-0 overflow-y-auto"><ComparisonConfigForm key={form === "new" ? "new" : form.id} session={session} hostId={hostId} initial={form === "new" ? undefined : form} busy={busy} onSubmit={saveConfig} onCancel={() => setForm(null)} /></div>
              ) : loading ? <p role="status" className="flex items-center gap-2 p-6 text-sm text-muted"><Spinner /> Loading UI diffs…</p> : (
                <>
                  {unassigned.length > 0 && <div className="flex gap-1 border-b border-border px-4 py-2" role="tablist" aria-label="Artifact association">
                    {([ ["session", `This session (${owned.length})`], ["unassigned", `Unassigned (${unassigned.length})`] ] as const).map(([value, label]) => <button key={value} role="tab" type="button" aria-selected={tab === value} onClick={() => { setTab(value); setSelectedId(null); }} className={`rounded-md px-3 py-1.5 text-xs ${tab === value ? "bg-accent-soft text-accent" : "text-muted hover:bg-panel"}`}>{label}</button>)}
                  </div>}
                  {!selected ? <div className="space-y-3 p-6 text-center"><p className="text-sm font-medium">No UI diffs for this session yet.</p><p className="text-xs leading-relaxed text-muted">UI comparisons created by the agent appear here. You can also start one from this session.</p><button type="button" onClick={() => setForm("new")} className={btnPrimary}>New UI diff</button></div> : (
                    <div className="flex min-h-0 flex-col overflow-y-auto md:grid md:grid-cols-[15rem_minmax(0,1fr)]">
                      <ul aria-label="UI diff artifacts" className="max-h-44 shrink-0 overflow-y-auto border-b border-border bg-panel/30 p-2 md:max-h-none md:border-b-0 md:border-r">
                        {entries.map((comparison) => <li key={comparison.id}><button type="button" aria-label={`Select ${comparisonTitle(comparison)}`} aria-pressed={selected.id === comparison.id} onClick={() => setSelectedId(comparison.id)} className={`mb-1 flex w-full flex-col items-start gap-1.5 rounded-lg p-3 text-left ${selected.id === comparison.id ? "bg-accent-soft" : "hover:bg-panel"}`}><span className="break-words text-xs font-medium">{comparisonTitle(comparison)}</span><span className="flex flex-wrap items-center gap-2"><Badge tone={statusTone(comparison)} className="px-2 text-[0.6rem]">{comparisonStateLabel(comparison)}</Badge><span className="text-[0.6rem] text-muted">{timeLabel(comparison.createdAt)}</span></span></button></li>)}
                      </ul>
                      <section aria-label="Selected UI diff" className="min-w-0 shrink-0 space-y-4 p-4">
                        <div className="space-y-2"><h3 className="break-words text-sm font-semibold">{comparisonTitle(selected)}</h3>{selected.config.artifact?.description && <p className="whitespace-pre-wrap break-words text-xs leading-relaxed text-muted">{selected.config.artifact.description}</p>}</div>
                        <div className="flex flex-wrap items-center gap-2">
                          <button type="button" disabled={busy || selected.state === "stopping"} onClick={() => {
                            if (selected.restoreRequired) setForm(selected);
                            else void mutate(() => api.comparisonOpen(selected.id));
                          }} className={btnPrimary}>{selected.windowOpen ? "Focus window" : selected.state === "stopped" || selected.state === "failed" ? "Reopen UI diff" : "Open UI diff"}</button>
                          {(selected.windowOpen || selected.windowRequested) && <button type="button" disabled={busy} onClick={() => void mutate(() => api.comparisonClose(selected.id))} className={btnGhost}>Close window</button>}
                          {(selected.state === "ready" || selected.state === "starting") && <button type="button" disabled={busy} onClick={() => void mutate(() => api.comparisonStop(selected.id))} className={`${btnGhost} text-muted`}>Stop previews</button>}
                        </div>
                        {tab === "unassigned" && <div className="space-y-2 rounded-lg border border-border bg-panel p-3"><p className="text-xs text-muted">This UI diff has no session association.</p><button type="button" disabled={busy} onClick={() => void mutate(async () => { const result = await api.comparisonUpdate(selected.id, { artifact: comparisonArtifactForSession(session, hostId, comparisonTitle(selected), selected.config.artifact?.description ?? undefined) }); setTab("session"); return result; })} className={btnGhost}>Attach to this session</button></div>}
                        {selected.error && <p role="alert" className="break-words rounded-md bg-panel p-3 text-xs text-danger">{selected.error}</p>}
                        <dl className="grid gap-3 sm:grid-cols-2">
                          <Detail label="Baseline commit" mono>{selected.baselineSha || (selected.baselineExternal ? "External preview" : selected.state === "starting" ? "Resolving baseline…" : "Unavailable")}</Detail><Detail label="Baseline ref" mono>{selected.config.baselineRef || "Mainline branch"}</Detail>
                          <Detail label="Source repository" mono>{selected.config.repository}</Detail><Detail label="Working directory" mono>{selected.config.workingDirectory || selected.config.repository}</Detail>
                          <Detail label="Baseline worktree" mono>{selected.baselineWorktree || selected.config.baselineWorktree || (selected.baselineExternal ? "Existing preview URL" : "Temporary worktree")}</Detail><Detail label="Route" mono>{selected.config.route}</Detail>
                          <Detail label="Viewport">{selected.config.viewport.width} × {selected.config.viewport.height} CSS px · {selected.config.viewport.orientation}</Detail><Detail label="Scroll synchronization">{selected.config.syncScroll ? "On" : "Off"}</Detail>
                          <Detail label="Created">{timeLabel(selected.createdAt)}</Detail><Detail label="Updated">{timeLabel(selected.updatedAt || selected.createdAt)}</Detail>
                        </dl>
                        <details className="rounded-lg border border-border p-3"><summary className="cursor-pointer text-xs text-muted">Session and preview details</summary><dl className="mt-3 grid gap-3 sm:grid-cols-2">
                          <Detail label="Artifact ID" mono>{selected.id}</Detail><Detail label="Session host" mono>{selected.config.artifact?.owner.hostId}</Detail>
                          <Detail label="Terminal ID" mono>{selected.config.artifact?.owner.terminalId}</Detail><Detail label="Agent conversation" mono>{selected.config.artifact?.owner.conversationId}</Detail>
                          <Detail label="Baseline URL" mono>{selected.baselineUrl || selected.config.baseline.url}</Detail><Detail label="Working URL" mono>{selected.workingUrl || selected.config.working.url}</Detail>
                          <Detail label="Baseline command" mono>{selected.config.baseline.command || "Automatic"}</Detail><Detail label="Working command" mono>{selected.config.working.command || "Automatic"}</Detail>
                          <Detail label="Baseline server log" mono>{selected.baselineLog}</Detail><Detail label="Working server log" mono>{selected.workingLog}</Detail>
                        </dl></details>
                        <p className="text-[0.65rem] leading-relaxed text-muted">Closing the window keeps previews running. Stop previews to release their servers and temporary worktree; this artifact stays with the session.</p>
                      </section>
                    </div>
                  )}
                </>
              )}
            </div>
          </Modal>
        </div>,
        document.body,
      )}
    </>
  );
}
