"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { listDir, rememberPicker, type DirListing, type PickerContext } from "@/lib/api";
import { loadPickerStart } from "@/lib/picker";
import { FileIcon, FolderIcon } from "./FileIcon";
import { Spinner } from "./ui";
import { Modal } from "./Modal";

// The shared picker browses the active server through HTTP in every client.
// If the caller passes `onSelectFile`, markdown files are listed alongside folders
// too and clicking one opens it — so the one Browse modal handles both a skill and
// a loose .md, with no separate entry point. Omit it for a folders-only picker.
export default function FolderPicker({
  onSelect,
  onClose,
  onSelectFile,
  context = "open",
  initialPath,
  title,
  selectLabel,
}: {
  /** A chosen skill folder. */
  onSelect: (path: string) => void;
  onClose: () => void;
  /** A chosen markdown file. Providing it opts this picker into listing files. */
  onSelectFile?: (path: string) => void;
  /** Each workflow remembers its own confirmed directory on the active server. */
  context?: PickerContext;
  /** The current form's directory wins over its remembered browsing location. */
  initialPath?: string;
  title?: string;
  selectLabel?: string;
}) {
  const allowFiles = !!onSelectFile;
  const [listing, setListing] = useState<DirListing | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [pathInput, setPathInput] = useState("");
  const [saving, setSaving] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const request = useRef(0);
  const startPath = useRef(initialPath);

  const load = useCallback(async (path: string) => {
    const id = ++request.current;
    setLoading(true);
    setError(null);
    setListing(null);
    setPathInput(path);
    setNotice(null);
    try {
      const l = await listDir(path, allowFiles);
      if (id !== request.current) return;
      setListing(l);
      setPathInput(l.path);
    } catch (e) {
      if (id === request.current) setError(e instanceof Error ? e.message : "Failed to list directory");
    } finally {
      if (id === request.current) setLoading(false);
    }
  }, [allowFiles]);

  useEffect(() => {
    const id = ++request.current;
    void loadPickerStart(context, startPath.current, allowFiles).then(({ listing: l, usedFallback }) => {
      if (id !== request.current) return;
      setListing(l);
      setPathInput(l.path);
      if (usedFallback) setNotice("The previous folder is unavailable. Choose another location.");
    }).catch((e) => {
      if (id === request.current) setError(e instanceof Error ? e.message : "Failed to list directory");
    }).finally(() => {
      if (id === request.current) setLoading(false);
    });
    return () => { request.current++; };
  }, [allowFiles, context]);

  const select = async (path: string, file = false) => {
    if (!listing || loading || saving || error) return;
    setSaving(true);
    // File selections remember their parent; folder selections remember the
    // folder itself. Browsing and cancelling do not change the saved default.
    await rememberPicker(context, file ? listing.path : path).catch(() => {});
    if (file) onSelectFile?.(path);
    else onSelect(path);
    setSaving(false);
  };

  const join = (name: string) => {
    if (!listing) return name;
    const separator = listing.path.includes("\\") ? "\\" : "/";
    return /[\\/]$/.test(listing.path) ? `${listing.path}${name}` : `${listing.path}${separator}${name}`;
  };

  return (
    <Modal title={title ?? (allowFiles ? "Open a skill or Markdown file" : "Choose a folder")} onClose={onClose} dismissDisabled={saving} widthClass="max-w-xl">
      <div className="flex max-h-[70dvh] flex-col">

        <form
          className="flex gap-2 border-b border-border px-4 py-2"
          onSubmit={(e) => {
            e.preventDefault();
            void load(pathInput.trim());
          }}
        >
          <input
            disabled={saving}
            value={pathInput}
            onChange={(e) => setPathInput(e.target.value)}
            spellCheck={false}
            aria-label="Folder path"
            className="w-full rounded-md border border-border bg-app px-2 py-1 font-mono text-xs text-fg outline-none focus:border-accent"
          />
          <button type="submit" disabled={saving} className="shrink-0 rounded-md border border-border px-2 py-1 text-xs hover:bg-panel">
            Go
          </button>
          <button type="button" onClick={() => void load("")} disabled={saving} className="shrink-0 rounded-md border border-border px-2 py-1 text-xs hover:bg-panel">Home</button>
        </form>

        {notice && <p role="status" className="px-4 pt-2 text-xs text-muted">{notice}</p>}
        <div className={`min-h-48 flex-1 overflow-auto px-2 py-2 ${saving ? "pointer-events-none opacity-60" : ""}`} inert={saving}>
          {loading ? (
            <div className="flex items-center gap-2 px-2 py-3 text-sm text-muted">
              <Spinner className="h-3.5 w-3.5" /> Loading…
            </div>
          ) : error ? (
            <p className="px-2 py-3 text-sm text-danger">{error}</p>
          ) : listing ? (
            <ul className="text-sm">
              {listing.parent && (
                <li>
                  <button
                    type="button"
                    onClick={() => load(listing.parent!)}
                    className="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-muted hover:bg-panel"
                  >
                    <span className="w-4 text-center" aria-hidden>
                      ↑
                    </span>
                    ..
                  </button>
                </li>
              )}
              {listing.entries
                .filter((e) => e.isDir || e.isMarkdown)
                .map((e) =>
                  e.isDir ? (
                    <li key={e.name} className="flex items-center gap-1">
                      <button
                        type="button"
                        onClick={() => load(join(e.name))}
                        className="flex min-w-0 flex-1 items-center gap-2 rounded-md px-2 py-1.5 text-left hover:bg-panel"
                      >
                        <FolderIcon open={false} name={e.name} />
                        <span className="truncate text-fg">{e.name}</span>
                        {e.isSkill && (
                          <span className="shrink-0 rounded bg-accent-soft px-1.5 py-0.5 text-[0.65rem] font-medium text-accent">
                            skill
                          </span>
                        )}
                      </button>
                      {e.isSkill && (
                        <button
                          type="button"
                          onClick={() => void select(join(e.name))}
                          className="shrink-0 rounded-md px-2 py-1 text-xs text-accent hover:bg-panel"
                        >
                          {selectLabel ?? "Open"}
                        </button>
                      )}
                    </li>
                  ) : (
                    <li key={e.name}>
                      <button
                        type="button"
                        onClick={() => void select(join(e.name), true)}
                        className="flex w-full min-w-0 items-center gap-2 rounded-md px-2 py-1.5 text-left hover:bg-panel"
                      >
                        <FileIcon name={e.name} />
                        <span className="truncate text-fg">{e.name}</span>
                      </button>
                    </li>
                  ),
                )}
              {listing.entries.filter((e) => e.isDir || e.isMarkdown).length === 0 && (
                <li className="px-2 py-3 text-sm text-muted">
                  {allowFiles ? "No folders or markdown files here." : "No subfolders."}
                </li>
              )}
            </ul>
          ) : null}
        </div>

        <div className="flex items-center gap-2 border-t border-border px-4 py-3">
          <span className="truncate font-mono text-xs text-faint">{listing?.path}</span>
          <button
            type="button"
            onClick={() => listing && void select(listing.path)}
            disabled={!listing || loading || saving || !!error}
            className="ml-auto shrink-0 rounded-md bg-accent px-3 py-1.5 text-sm font-medium text-accent-fg disabled:opacity-40"
          >
            {saving ? "Selecting…" : selectLabel ?? "Open this folder"}
          </button>
        </div>
      </div>
    </Modal>
  );
}
