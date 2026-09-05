"use client";

import { useEffect, useMemo, useRef, useState } from "react";
import { Modal } from "@/components/Modal";
import { btnDanger, btnGhost, btnPrimary, Spinner } from "@/components/ui";
import FolderPicker from "@/components/FolderPicker";
import * as api from "@/lib/api";
import type { ImportResult, SkillHome } from "@/lib/api";

/** Re-runnable import attempt (so a name conflict can retry with overwrite=true). */
type Run = (overwrite: boolean) => Promise<ImportResult>;

type Phase =
  | { t: "choose" }
  | { t: "importing" }
  | { t: "conflict"; run: Run; message: string }
  | { t: "secrets"; result: ImportResult; run: Run }
  | { t: "error"; message: string };

/** Read a File as bare base64 (strip the `data:…;base64,` prefix). */
function fileToBase64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => {
      const r = String(reader.result);
      resolve(r.includes(",") ? r.slice(r.indexOf(",") + 1) : r);
    };
    reader.onerror = () => reject(reader.error ?? new Error("Couldn’t read the file."));
    reader.readAsDataURL(file);
  });
}

/**
 * Import an existing skill into a chosen home — from a folder or a `.skill`/`.zip`
 * archive (the inverse of Export). Lands a copy under the skill's name; if it
 * carries a `.env`, offers to load those values into the secret store rather than
 * into the folder. (`.skill` is just a zip, so both extensions extract the same.)
 */
export default function ImportSkillDialog({
  onClose,
  onImported,
}: {
  onClose: () => void;
  onImported: (root: string) => void;
}) {
  const [homes, setHomes] = useState<SkillHome[] | null>(null);
  const [target, setTarget] = useState("universal");
  const [phase, setPhase] = useState<Phase>({ t: "choose" });
  const [pickerOpen, setPickerOpen] = useState(false);
  const [dragging, setDragging] = useState(false);
  const [busy, setBusy] = useState(false);
  const [selected, setSelected] = useState<Record<string, boolean>>({});
  const [gitUrl, setGitUrl] = useState("");
  const fileInputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    api
      .skillHomes()
      .then((h) => {
        const list = Array.isArray(h) ? h : [];
        setHomes(list);
        if (list.length && !list.some((x) => x.id === "universal")) setTarget(list[0].id);
      })
      .catch(() => setHomes([]));
  }, []);
  const home = useMemo(() => homes?.find((h) => h.id === target), [homes, target]);

  const done = (res: ImportResult) => onImported(res.root);

  // Run an import; route name conflicts to a confirm and .env to the secrets step.
  const attempt = async (run: Run, overwrite = false) => {
    setPhase({ t: "importing" });
    try {
      const res = await run(overwrite);
      if (res.env.length > 0) {
        const init: Record<string, boolean> = {};
        res.env.forEach((e) => (init[e.key] = true));
        setSelected(init);
        setPhase({ t: "secrets", result: res, run });
      } else {
        done(res);
      }
    } catch (e) {
      const message = e instanceof Error ? e.message : "Import failed.";
      if (!overwrite && /already exists/i.test(message)) setPhase({ t: "conflict", run, message });
      else setPhase({ t: "error", message });
    }
  };

  const chooseFolder = () => setPickerOpen(true);
  const chooseZip = () => fileInputRef.current?.click();
  const importZipFile = (file: File) =>
    void attempt(async (ow) => api.importSkillZipUpload(await fileToBase64(file), target, ow));
  // Clone a skill repository (GitHub/GitLab/any git URL). The clone keeps its
  // origin, so the imported skill arrives already connected for sync.
  const importFromUrl = () => {
    const url = gitUrl.trim();
    if (url) void attempt((ow) => api.importSkillFromRemote(url, target, ow));
  };

  // File drop onto the dropzone (expects a `.skill`/`.zip`); buttons cover folder too.
  const onDrop = (e: React.DragEvent) => {
    e.preventDefault();
    setDragging(false);
    const file = e.dataTransfer.files?.[0];
    if (file && /\.(skill|zip)$/i.test(file.name)) importZipFile(file);
    else if (file) setPhase({ t: "error", message: "Drop a .skill or .zip file (or use “Choose folder”)." });
  };

  const loadSecrets = async (result: ImportResult) => {
    setBusy(true);
    try {
      for (const s of result.env) if (selected[s.key]) await api.secretSet(s.key, s.value);
      done(result);
    } catch (e) {
      setBusy(false);
      setPhase({ t: "error", message: e instanceof Error ? e.message : "Couldn’t save secrets." });
    }
  };

  return (
    <>
      <Modal title="Import skill" onClose={onClose} dismissDisabled={pickerOpen}>
        <div className="space-y-4 px-5 py-4">
          {/* Location is chosen first so every source lands in the same home. */}
          <div>
            <label className="mb-1 block text-xs font-medium uppercase tracking-wider text-muted">Location</label>
            {homes === null ? (
              <p className="flex items-center gap-2 text-sm text-muted">
                <Spinner className="h-3.5 w-3.5" /> Loading…
              </p>
            ) : (
              <>
                <select
                  value={target}
                  onChange={(e) => setTarget(e.target.value)}
                  disabled={phase.t === "importing" || busy}
                  className="w-full rounded-md border border-border bg-surface px-2.5 py-1.5 text-sm text-fg outline-none focus:border-accent disabled:opacity-50"
                >
                  {homes.map((h) => (
                    <option key={h.id} value={h.id}>
                      {h.label}
                    </option>
                  ))}
                </select>
                {home && (
                  <p className="mt-1 truncate font-mono text-[0.7rem] text-faint" title={home.dir}>
                    {home.dir}/…
                  </p>
                )}
              </>
            )}
          </div>

          {phase.t === "secrets" ? (
            <SecretsStep
              result={phase.result}
              selected={selected}
              setSelected={setSelected}
              busy={busy}
              onSkip={() => done(phase.result)}
              onLoad={() => void loadSecrets(phase.result)}
            />
          ) : phase.t === "importing" ? (
            <p className="flex items-center gap-2 py-6 text-sm text-muted">
              <Spinner className="h-4 w-4" /> Importing…
            </p>
          ) : (
            <>
              {/* Dropzone (browser) + explicit source buttons (both modes). */}
              <button
                type="button"
                onClick={chooseZip}
                onDragOver={(e) => {
                  e.preventDefault();
                  setDragging(true);
                }}
                onDragLeave={() => setDragging(false)}
                onDrop={onDrop}
                className={`flex w-full flex-col items-center gap-1 rounded-lg border border-dashed px-4 py-7 text-center transition-colors ${
                  dragging ? "border-accent bg-accent-soft" : "border-border-strong hover:bg-panel"
                }`}
              >
                <span className="text-sm font-medium text-fg">
                  Drop a .skill or .zip here, or click to choose
                </span>
                <span className="text-xs text-muted">Packaged skills (SKILL.md inside)</span>
              </button>
              <div className="flex items-center gap-3">
                <span className="h-px flex-1 bg-border" />
                <span className="text-[0.7rem] uppercase tracking-wider text-faint">or</span>
                <span className="h-px flex-1 bg-border" />
              </div>
              <button type="button" onClick={chooseFolder} className={`${btnGhost} w-full`}>
                Choose a folder…
              </button>
              <div className="flex gap-2">
                <input
                  value={gitUrl}
                  onChange={(e) => setGitUrl(e.target.value)}
                  placeholder="https://github.com/user/skill.git"
                  spellCheck={false}
                  className="min-w-0 flex-1 rounded-md border border-border bg-surface px-2.5 py-1.5 font-mono text-xs text-fg outline-none focus:border-accent"
                  onKeyDown={(e) => e.key === "Enter" && importFromUrl()}
                />
                <button type="button" onClick={importFromUrl} disabled={!gitUrl.trim()} className={`${btnGhost} shrink-0`}>
                  Clone
                </button>
              </div>
              <p className="-mt-2 text-[0.7rem] text-faint">
                A skill repository’s clone URL (GitHub, GitLab, …) — it arrives connected, so Sync pulls the
                team’s updates.
              </p>
              <input
                ref={fileInputRef}
                type="file"
                accept=".skill,.zip,application/zip"
                className="hidden"
                onChange={(e) => {
                  const file = e.target.files?.[0];
                  e.target.value = "";
                  if (file) importZipFile(file);
                }}
              />

              {phase.t === "conflict" && (
                <div className="rounded-md border border-warn/40 bg-warn/10 px-3 py-2.5 text-xs">
                  <p className="text-fg">{phase.message}</p>
                  <p className="mt-0.5 text-muted">Replace the existing skill with the imported one?</p>
                  <div className="mt-2 flex justify-end gap-2">
                    <button type="button" onClick={() => setPhase({ t: "choose" })} className={btnGhost}>
                      Cancel
                    </button>
                    <button type="button" onClick={() => void attempt(phase.run, true)} className={btnDanger}>
                      Overwrite
                    </button>
                  </div>
                </div>
              )}
              {phase.t === "error" && <p className="text-xs text-danger">{phase.message}</p>}
            </>
          )}
        </div>
      </Modal>

      {pickerOpen && (
        <FolderPicker
          context="import"
          title="Choose a skill to import"
          selectLabel="Choose this folder"
          onSelect={(p) => {
            setPickerOpen(false);
            void attempt((ow) => api.importSkillFolder(p, target, ow));
          }}
          onClose={() => setPickerOpen(false)}
        />
      )}
    </>
  );
}

/** Follow-up step: an imported skill carried a `.env`; offer to load it into secrets. */
function SecretsStep({
  result,
  selected,
  setSelected,
  busy,
  onSkip,
  onLoad,
}: {
  result: ImportResult;
  selected: Record<string, boolean>;
  setSelected: (f: (s: Record<string, boolean>) => Record<string, boolean>) => void;
  busy: boolean;
  onSkip: () => void;
  onLoad: () => void;
}) {
  const anyChecked = result.env.some((e) => selected[e.key]);
  return (
    <div>
      <p className="text-sm text-fg">
        Imported <span className="font-medium">{result.name}</span>.
      </p>
      <p className="mt-1 text-xs text-muted">
        It included a <code className="rounded bg-panel px-1 font-mono text-[0.9em]">.env</code> — load these into your
        secret store? They’re kept out of the skill folder.
      </p>
      <ul className="mt-3 space-y-1.5">
        {result.env.map((e) => (
          <li key={e.key}>
            <label className="flex items-center gap-2 text-sm">
              <input
                type="checkbox"
                checked={!!selected[e.key]}
                onChange={(ev) => setSelected((s) => ({ ...s, [e.key]: ev.target.checked }))}
                className="accent-accent"
              />
              <span className="font-mono text-xs text-fg">{e.key}</span>
              {e.exists && (
                <span className="rounded-full bg-warn/15 px-1.5 py-0.5 text-[0.6rem] font-medium uppercase tracking-wide text-warn">
                  overwrites
                </span>
              )}
            </label>
          </li>
        ))}
      </ul>
      <div className="mt-4 flex justify-end gap-2">
        <button type="button" onClick={onSkip} disabled={busy} className={btnGhost}>
          Skip
        </button>
        <button type="button" onClick={onLoad} disabled={busy || !anyChecked} className={btnPrimary}>
          {busy ? "Saving…" : "Load & open"}
        </button>
      </div>
    </div>
  );
}
