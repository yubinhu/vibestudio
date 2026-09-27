import { useEffect, useRef, useState, type FormEvent } from "react";
import { useParams } from "react-router-dom";
import { Badge, btnGhost, btnPrimary, Spinner, type Tone } from "@/components/ui";
import * as api from "@/lib/api";
import { comparisonOwnersMatch } from "@/lib/comparisonArtifacts";
import ComparisonViewportPicker from "@/components/ComparisonViewportPicker";
import { rotateComparisonViewport, validComparisonViewportSize } from "@/lib/comparisonViewports";

const STATE_LABEL: Record<api.ComparisonSession["state"], string> = {
  starting: "Starting previews",
  ready: "Live",
  stopping: "Stopping",
  stopped: "Stopped",
  failed: "Failed",
};
const STATE_TONE: Record<api.ComparisonSession["state"], Tone> = {
  starting: "info", ready: "ok", stopping: "muted", stopped: "muted", failed: "danger",
};
const inputClass = "h-7 rounded-md border border-border bg-surface px-2 text-xs text-fg disabled:opacity-40";
const compactButton = `${btnGhost} h-7 px-2 py-0 text-xs`;

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function ComparisonToolbar({ id }: { id: string }) {
  const [session, setSession] = useState<api.ComparisonSession | null>(null);
  const [artifacts, setArtifacts] = useState<api.ComparisonSession[]>([]);
  const [readError, setReadError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [draftWidth, setDraftWidth] = useState("");
  const [draftHeight, setDraftHeight] = useState("");
  const [customDraft, setCustomDraft] = useState(false);
  // A read begun before an update/stop must not restore the old configuration.
  const mutationVersion = useRef(0);
  const operation = useRef(false);

  useEffect(() => {
    let disposed = false;
    let timer: ReturnType<typeof setTimeout>;
    async function poll() {
      const version = mutationVersion.current;
      try {
        const artifacts = await api.comparisonList();
        const next = artifacts.find((artifact) => artifact.id === id);
        if (!next) throw new Error("This UI diff is no longer available.");
        if (!disposed && version === mutationVersion.current && !operation.current) {
          setSession(next);
          setArtifacts(artifacts);
          setReadError(null);
        }
      } catch (error) {
        if (!disposed && version === mutationVersion.current) setReadError(message(error));
      }
      if (!disposed) timer = setTimeout(poll, 1000);
    }
    void poll();
    return () => { disposed = true; clearTimeout(timer); };
  }, [id]);

  async function mutate(action: () => Promise<api.ComparisonSession>) {
    if (operation.current) return;
    operation.current = true;
    mutationVersion.current += 1;
    setBusy(true);
    setActionError(null);
    try {
      setSession(await action());
      setReadError(null);
    } catch (error) {
      setActionError(message(error));
    } finally {
      mutationVersion.current += 1;
      operation.current = false;
      setBusy(false);
    }
  }

  const update = (changes: api.ComparisonUpdate) => mutate(() => api.comparisonUpdate(id, changes));
  const related = session
    ? artifacts.filter((artifact) => artifact.id === id || comparisonOwnersMatch(session.config.artifact?.owner, artifact.config.artifact?.owner))
    : [];
  const viewport = session?.config.viewport;
  useEffect(() => {
    setDraftWidth(viewport ? String(viewport.width) : "");
    setDraftHeight(viewport ? String(viewport.height) : "");
    setCustomDraft(false);
  }, [viewport?.width, viewport?.height, viewport?.preset]);
  const draftViewport: api.ComparisonViewport = {
    width: Number(draftWidth), height: Number(draftHeight),
    preset: customDraft ? "custom" : viewport?.preset,
    orientation: Number(draftWidth) > Number(draftHeight) ? "landscape" : "portrait",
  };
  const validDraft = validComparisonViewportSize(draftViewport.width, draftViewport.height);
  function changeViewport(next: api.ComparisonViewport) {
    setDraftWidth(String(next.width));
    setDraftHeight(String(next.height));
    setCustomDraft(next.preset === "custom");
    void update({ viewport: next });
  }
  const disabled = busy || !session || !["starting", "ready"].includes(session.state);
  const error = actionError ?? readError ?? session?.error;
  const workingLocation = session?.workingExternal
    ? session.workingUrl
    : session?.config.workingDirectory ?? session?.config.repository;

  function applySize(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!validDraft) return;
    changeViewport({ ...draftViewport, preset: "custom" });
  }

  function applyRoute(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const route = String(new FormData(event.currentTarget).get("route") ?? "").trim();
    void update({ route: route || "/" });
  }

  // This is only the 176px native toolbar child. The desktop shell owns the two
  // independent preview webviews below it; the frontend never embeds the pages.
  return (
    <main aria-label="Live UI comparison" className="h-[176px] min-w-[860px] overflow-hidden bg-surface text-fg">
      <header className="flex h-10 items-center gap-3 border-b border-border px-4">
        <h1 className="shrink-0 text-sm font-semibold">UI diff</h1>
        {related.length > 1 ? (
          <select
            aria-label="Switch UI diff"
            value={id}
            disabled={busy}
            className={`${inputClass} min-w-0 max-w-[260px]`}
            onChange={(event) => {
              const nextId = event.target.value;
              if (nextId !== id) void mutate(async () => {
                await api.comparisonOpen(nextId);
                return api.comparisonClose(id);
              });
            }}
          >
            {related.map((artifact) => (
              <option key={artifact.id} value={artifact.id} disabled={artifact.state === "stopping"}>
                {artifact.config.artifact?.title ?? artifact.id}
              </option>
            ))}
          </select>
        ) : session?.config.artifact ? (
          <span className="max-w-[260px] truncate text-sm" title={session.config.artifact.title}>{session.config.artifact.title}</span>
        ) : null}
        {session ? <Badge tone={STATE_TONE[session.state]}>{STATE_LABEL[session.state]}</Badge> : <Spinner />}
        <span className="min-w-0 flex-1 truncate text-xs text-muted" title={session?.config.repository}>
          {session?.config.repository ?? "Loading comparison…"}
        </span>
        <button
          type="button"
          className={compactButton}
          disabled={busy || !session || session.state === "stopping" || session.state === "stopped"}
          onClick={() => void mutate(() => api.comparisonStop(id))}
          title="Stop this comparison and clean up its temporary worktree and dev servers"
        >Stop</button>
        <button
          type="button"
          className={compactButton}
          disabled={busy || !session}
          onClick={() => void mutate(() => api.comparisonClose(id))}
          title="Close this window; keep the pinned baseline and preview servers available"
        >Close</button>
      </header>

      <div className="flex h-11 items-center gap-3 px-4">
        <div className="flex shrink-0 items-center gap-2">
          <span className="text-xs text-muted">Viewport</span>
          <ComparisonViewportPicker viewport={draftViewport} disabled={disabled} onChange={changeViewport} className={`${inputClass} w-[240px]`} />
        </div>
        <form onSubmit={applySize} className="flex items-center gap-1.5">
          <input aria-label="Viewport width (CSS px)" name="width" type="number" min="240" max="3840" step="1" required value={draftWidth} disabled={disabled} onChange={(event) => { setDraftWidth(event.target.value); setCustomDraft(true); }} className={`${inputClass} w-[70px]`} />
          <span aria-hidden className="text-xs text-muted">×</span>
          <input aria-label="Viewport height (CSS px)" name="height" type="number" min="240" max="3840" step="1" required value={draftHeight} disabled={disabled} onChange={(event) => { setDraftHeight(event.target.value); setCustomDraft(true); }} className={`${inputClass} w-[70px]`} />
          <span className="shrink-0 text-[11px] text-muted" title="CSS layout pixels; both previews scale to fit this window">CSS px</span>
          <button type="submit" className={compactButton} disabled={disabled}>Apply</button>
        </form>
        <button
          type="button"
          className={compactButton}
          disabled={disabled || !validDraft}
          title={`Rotate both viewports to ${draftViewport.orientation === "landscape" ? "portrait" : "landscape"}`}
          aria-label="Rotate both viewports"
          onClick={() => changeViewport(rotateComparisonViewport(draftViewport))}
        >Rotate ↻</button>
        <label className="ml-auto flex shrink-0 cursor-pointer items-center gap-1.5 text-xs">
          <input type="checkbox" checked={session?.config.syncScroll ?? true} disabled={disabled} onChange={(event) => void update({ syncScroll: event.target.checked })} />
          Sync scroll
        </label>
      </div>

      <div className="flex h-11 items-center gap-4 px-4">
        <form key={session?.config.route} onSubmit={applyRoute} className="flex min-w-0 flex-1 items-center gap-2">
          <label htmlFor="comparison-route" className="text-xs text-muted">Route</label>
          <input id="comparison-route" name="route" aria-label="Preview route" className={`${inputClass} min-w-0 flex-1 font-mono`} defaultValue={session?.config.route ?? "/"} disabled={disabled} placeholder="/" />
          <button type="submit" className={`${btnPrimary} h-7 px-2 py-0 text-xs`} disabled={disabled}>Go</button>
        </form>
        <div role={error ? "alert" : "status"} className={`min-w-0 flex-1 text-xs ${error ? "text-danger" : "text-muted"}`}>
          <p className="line-clamp-2 break-words" title={error ?? undefined}>
            {error || (busy ? "Applying changes…" : session?.state === "ready" ? "Saved edits update the working preview." : session?.state === "starting" ? "Preparing the baseline and starting previews…" : session?.state === "stopping" ? "Cleaning up comparison resources…" : session?.state === "stopped" ? "Comparison stopped. You can close this window." : "Waiting for the comparison…")}
          </p>
        </div>
      </div>

      <div className="grid h-12 grid-cols-2 border-y border-border bg-panel text-xs">
        <section aria-label="Baseline preview" className="flex min-w-0 flex-col justify-center gap-0.5 border-r border-border px-4">
          <div className="flex min-w-0 items-center gap-2">
            <span className="font-semibold">Baseline</span>
            <span className="truncate text-muted" title={session?.baselineSha ?? undefined}>
              {session?.baselineExternal ? "Existing URL" : session?.baselineSha ? `${session.config.baselineRef ?? "Mainline"} · ${session.baselineSha.slice(0, 8)} · pinned` : "Preparing pinned commit"}
            </span>
          </div>
          <span className="truncate font-mono text-[10px] text-muted" title={session?.baselineWorktree ?? session?.baselineUrl ?? undefined}>
            {session?.baselineWorktree ?? session?.baselineUrl ?? "Temporary worktree"}
          </span>
        </section>
        <section aria-label="Working preview" className="flex min-w-0 flex-col justify-center gap-0.5 px-4">
          <div className="flex items-center gap-2"><span className="font-semibold">Working</span><span className="text-muted">{session?.workingExternal ? "Existing URL" : "Saved, uncommitted changes"}</span></div>
          <span className="truncate font-mono text-[10px] text-muted" title={workingLocation ?? undefined}>{workingLocation ?? "Working directory"}</span>
        </section>
      </div>
    </main>
  );
}

export function Component() {
  const id = useParams().id ?? "";
  return <ComparisonToolbar key={id} id={id} />;
}
