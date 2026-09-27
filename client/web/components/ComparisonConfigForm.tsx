import { useState, type FormEvent } from "react";
import * as api from "@/lib/api";
import { comparisonArtifactForSession } from "@/lib/comparisonArtifacts";
import { btnGhost, btnPrimary } from "./ui";
import ComparisonViewportPicker from "./ComparisonViewportPicker";

const inputClass = "mt-1 w-full rounded-md border border-border bg-surface px-2.5 py-1.5 text-sm text-fg outline-none focus:border-accent disabled:opacity-50";

export default function ComparisonConfigForm({ session, hostId, initial, busy, onSubmit, onCancel }: {
  session: api.TermSession;
  hostId: string;
  initial?: api.ComparisonSession;
  busy: boolean;
  onSubmit: (config: api.ComparisonConfig) => Promise<void>;
  onCancel: () => void;
}) {
  const config = initial?.config;
  const [title, setTitle] = useState(config?.artifact?.title ?? "UI changes");
  const [description, setDescription] = useState(config?.artifact?.description ?? "");
  const [repository, setRepository] = useState(config?.repository ?? (hostId === "local" ? session.cwd : ""));
  const [workingDirectory, setWorkingDirectory] = useState(config?.workingDirectory ?? "");
  const [baselineRef, setBaselineRef] = useState(initial?.baselineSha ?? config?.baselineRef ?? "");
  const [baselineWorktree, setBaselineWorktree] = useState(config?.baselineWorktree ?? "");
  const [route, setRoute] = useState(config?.route ?? "/");
  const [width, setWidth] = useState(config?.viewport.width ?? 390);
  const [height, setHeight] = useState(config?.viewport.height ?? 844);
  const [preset, setPreset] = useState(config?.viewport.preset ?? "custom");
  const [syncScroll, setSyncScroll] = useState(config?.syncScroll ?? true);
  const [baseline, setBaseline] = useState<api.ComparisonPreviewConfig>(config?.baseline ?? {});
  const [working, setWorking] = useState<api.ComparisonPreviewConfig>(config?.working ?? {});
  const [environment, setEnvironment] = useState("");
  const [baselineEnvironment, setBaselineEnvironment] = useState("");
  const [workingEnvironment, setWorkingEnvironment] = useState("");
  const [error, setError] = useState<string | null>(null);

  const parseEnvironment = (source: string): Record<string, string> => {
    const value: unknown = source.trim() ? JSON.parse(source) : {};
    if (!value || typeof value !== "object" || Array.isArray(value) || Object.values(value).some((item) => typeof item !== "string")) {
      throw new Error("Environment must be a JSON object with string values.");
    }
    return value as Record<string, string>;
  };
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (busy) return;
    setError(null);
    try {
      await onSubmit({
        repository: repository.trim(),
        workingDirectory: workingDirectory.trim() || undefined,
        baselineRef: baselineRef.trim() || undefined,
        baselineWorktree: baselineWorktree.trim() || undefined,
        baseline: { ...baseline, env: parseEnvironment(baselineEnvironment) },
        working: { ...working, env: parseEnvironment(workingEnvironment) },
        env: parseEnvironment(environment),
        route: route.trim() || "/",
        viewport: { width, height, orientation: width > height ? "landscape" : "portrait", preset },
        syncScroll,
        artifact: comparisonArtifactForSession(session, hostId, title, description),
      });
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Could not start this UI diff.");
    }
  };

  return (
    <form onSubmit={(event) => void submit(event)} className="space-y-4 p-4">
      <div>
        <h3 className="text-sm font-semibold">{initial ? "Reopen UI diff" : "New UI diff"}</h3>
        <p className="mt-1 text-xs leading-relaxed text-muted">Compare a pinned baseline with saved edits. Preview files and commands run on this desktop.</p>
        {initial && <p className="mt-2 text-xs text-muted">The session association, source directories, and pinned baseline stay with this artifact.</p>}
        {initial?.restoreRequired && <p className="mt-2 text-xs text-warn">Environment values are not saved with artifacts. Supply any required values again before reopening.</p>}
      </div>
      <fieldset disabled={busy} className="space-y-3">
        <label className="block text-xs text-muted">Title<input autoFocus readOnly={!!initial} required maxLength={200} value={title} onChange={(event) => setTitle(event.target.value)} className={inputClass} /></label>
        <label className="block text-xs text-muted">Description<textarea readOnly={!!initial} rows={2} maxLength={4000} value={description} onChange={(event) => setDescription(event.target.value)} className={inputClass} placeholder="What changed and what should be reviewed" /></label>
        <label className="block text-xs text-muted">Source repository<input readOnly={!!initial} required value={repository} onChange={(event) => setRepository(event.target.value)} className={`${inputClass} font-mono`} placeholder="/path/to/repository" /></label>
        {hostId !== "local" && <p className="text-xs text-muted">This session runs on {hostId}. Enter a repository path available on this desktop, or use forwarded preview URLs below.</p>}
        <div className="grid gap-3 sm:grid-cols-2">
          <label className="block text-xs text-muted">Working directory<input readOnly={!!initial} value={workingDirectory} onChange={(event) => setWorkingDirectory(event.target.value)} className={`${inputClass} font-mono`} placeholder="Same as source repository" /></label>
          <label className="block text-xs text-muted">Baseline ref<input readOnly={!!initial} value={baselineRef} onChange={(event) => setBaselineRef(event.target.value)} className={`${inputClass} font-mono`} placeholder="Mainline branch" /></label>
        </div>
        <label className="block text-xs text-muted">Route<input value={route} onChange={(event) => setRoute(event.target.value)} className={inputClass} placeholder="/" /></label>
        <label className="block text-xs text-muted">Device viewport<ComparisonViewportPicker
          viewport={{ width, height, preset, orientation: width > height ? "landscape" : "portrait" }}
          onChange={(viewport) => { setWidth(viewport.width); setHeight(viewport.height); setPreset(viewport.preset ?? "custom"); }}
          className={inputClass}
        /></label>
        <div className="flex flex-wrap items-end gap-3">
          <label className="min-w-0 flex-1 text-xs text-muted">Width (CSS px)<input required type="number" min={240} max={3840} value={width} onChange={(event) => { setWidth(Number(event.target.value)); setPreset("custom"); }} className={inputClass} /></label>
          <label className="min-w-0 flex-1 text-xs text-muted">Height (CSS px)<input required type="number" min={240} max={3840} value={height} onChange={(event) => { setHeight(Number(event.target.value)); setPreset("custom"); }} className={inputClass} /></label>
          <button type="button" className={btnGhost} onClick={() => { setWidth(height); setHeight(width); }}>Rotate</button>
        </div>
        <p className="text-[0.65rem] text-muted">CSS viewport size only. Both previews scale to fit the comparison window.</p>
        <label className="flex items-center gap-2 text-xs text-muted"><input type="checkbox" checked={syncScroll} onChange={(event) => setSyncScroll(event.target.checked)} /> Sync scrolling</label>
        <details open={!!initial} className="rounded-lg border border-border p-3">
          <summary className="cursor-pointer text-xs font-medium text-fg">Servers and advanced settings</summary>
          <div className="mt-3 space-y-4">
            <p className="text-xs leading-relaxed text-muted">Leave commands empty to use a detected Vite dev script, or supply your own commands. Use {'{port}'} in a custom command for the assigned port. An existing URL uses your running server.</p>
            {([
              ["Baseline", baseline, setBaseline, baselineEnvironment, setBaselineEnvironment],
              ["Working", working, setWorking, workingEnvironment, setWorkingEnvironment],
            ] as const).map(([label, preview, setPreview, env, setEnv]) => (
              <fieldset key={label} className="space-y-2 rounded-md bg-panel p-3">
                <legend className="text-xs font-medium text-fg">{label} preview</legend>
                <label className="block text-xs text-muted">{label} command<input value={preview.command ?? ""} onChange={(event) => setPreview({ ...preview, command: event.target.value || undefined })} className={`${inputClass} font-mono`} placeholder="npm run dev -- --host 127.0.0.1 --port {port}" /></label>
                <label className="block text-xs text-muted">{label} existing URL<input readOnly={!!initial && label === "Baseline"} type="url" value={preview.url ?? ""} onChange={(event) => setPreview({ ...preview, url: event.target.value || undefined })} className={inputClass} placeholder="http://127.0.0.1:3000" /></label>
                <div className="grid gap-2 sm:grid-cols-2">
                  <label className="block text-xs text-muted">{label} port<input type="number" min={1} max={65535} value={preview.port ?? ""} onChange={(event) => setPreview({ ...preview, port: event.target.value ? Number(event.target.value) : undefined })} className={inputClass} placeholder="Automatic" /></label>
                  <label className="block text-xs text-muted">{label} app subdirectory<input value={preview.directory ?? ""} onChange={(event) => setPreview({ ...preview, directory: event.target.value || undefined })} className={inputClass} placeholder="Repository root" /></label>
                </div>
                <label className="block text-xs text-muted">{label} startup timeout (seconds)<input type="number" min={1} max={600} value={preview.readyTimeoutSeconds ?? ""} onChange={(event) => setPreview({ ...preview, readyTimeoutSeconds: event.target.value ? Number(event.target.value) : undefined })} className={inputClass} placeholder="Default" /></label>
                <label className="block text-xs text-muted">{label} environment (JSON)<textarea rows={2} spellCheck={false} value={env} onChange={(event) => setEnv(event.target.value)} className={`${inputClass} font-mono`} placeholder={'{"PREVIEW_MODE":"true"}'} /></label>
              </fieldset>
            ))}
            <label className="block text-xs text-muted">Baseline worktree location<input value={baselineWorktree} onChange={(event) => setBaselineWorktree(event.target.value)} className={`${inputClass} font-mono`} placeholder="New temporary worktree" /></label>
            <label className="block text-xs text-muted">Shared environment (JSON)<textarea rows={2} spellCheck={false} value={environment} onChange={(event) => setEnvironment(event.target.value)} className={`${inputClass} font-mono`} placeholder={'{"PREVIEW_MODE":"true"}'} /></label>
          </div>
        </details>
      </fieldset>
      {error && <p role="alert" className="text-xs text-danger">{error}</p>}
      <div className="flex justify-end gap-2">
        <button type="button" disabled={busy} onClick={onCancel} className={btnGhost}>Cancel</button>
        <button type="submit" disabled={busy} className={btnPrimary}>{busy ? "Starting…" : initial ? "Reopen UI diff" : "Start UI diff"}</button>
      </div>
    </form>
  );
}
