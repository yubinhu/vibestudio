import { useState } from "react";
import { useNavigate } from "react-router-dom";
import { FileIcon, FolderIcon } from "./FileIcon";
import FolderPicker from "./FolderPicker";
import RecentDialog from "./RecentDialog";
import { Spinner } from "./ui";
import { useRecents, useRecentsStatus, refreshRecents, removeRecent } from "@/lib/recents";
import { markdownPath, studioPath } from "@/lib/routes";

/** Immediate access to recent work, with the searchable history one click away. */
export default function RecentStrip() {
  const recents = useRecents();
  const { loading, error } = useRecentsStatus();
  const navigate = useNavigate();
  const [historyOpen, setHistoryOpen] = useState(false);
  const [pickerOpen, setPickerOpen] = useState(false);

  return (
    <section className="mt-10 min-w-0" aria-labelledby="recent-heading">
      <div className="mb-3 flex items-center gap-2.5">
        <h2 id="recent-heading" className="text-sm font-semibold tracking-wide text-fg">Recent</h2>
        {recents.length > 0 && <span className="text-xs text-faint">{recents.length}</span>}
        <div className="ml-auto flex items-center gap-4">
          <button type="button" onClick={() => setPickerOpen(true)} className="text-xs font-medium text-accent hover:underline">Open…</button>
          {recents.length > 0 && (
            <button type="button" onClick={() => setHistoryOpen(true)} className="text-xs font-medium text-accent hover:underline">View all →</button>
          )}
        </div>
      </div>

      {error && (
        <p role="status" className="mb-3 text-xs text-warn">
          {error} <button type="button" onClick={refreshRecents} className="font-medium underline">Retry</button>
        </p>
      )}
      {recents.length > 0 ? (
        <ul className="flex snap-x snap-proximity gap-3 overflow-x-auto overscroll-x-contain p-1 pb-3">
          {recents.slice(0, 6).map((r) => (
            <li key={r.root} className="group relative w-64 shrink-0 snap-start sm:w-[calc((100%_-_1.5rem)/3)] lg:w-[calc((100%_-_2.25rem)/4)]">
              <button
                type="button"
                onClick={() => navigate(r.kind === "markdown" ? markdownPath(r.root) : studioPath(r.root))}
                className="flex h-full w-full min-w-0 flex-col gap-2 rounded-xl border border-border bg-surface p-4 pr-8 text-left transition-colors hover:border-border-strong hover:bg-panel focus-visible:outline-accent"
                title={r.root}
              >
                <span className="flex w-full min-w-0 items-center gap-2">
                  {r.kind === "markdown" ? <FileIcon name={r.name} /> : <FolderIcon open={false} name={r.name} />}
                  <span className="truncate text-sm font-semibold text-fg">{r.name}</span>
                </span>
                <span className="w-full truncate font-mono text-[0.7rem] text-faint">{r.root}</span>
                <span className="flex w-full items-center gap-2 text-[0.7rem] text-muted">
                  <span>{r.kind === "markdown" ? "Markdown" : "Skill"}</span>
                  {r.openedAt && (
                    <time dateTime={new Date(r.openedAt * 1000).toISOString()} className="ml-auto">
                      {new Date(r.openedAt * 1000).toLocaleDateString(undefined, { month: "short", day: "numeric" })}
                    </time>
                  )}
                </span>
              </button>
              <button
                type="button"
                onClick={() => removeRecent(r.root)}
                aria-label={`Remove ${r.name} from recents`}
                className="absolute right-2 top-2 rounded p-1 text-faint hover:text-danger focus-visible:opacity-100 sm:opacity-0 sm:group-hover:opacity-100 sm:group-focus-within:opacity-100"
              >
                ×
              </button>
            </li>
          ))}
        </ul>
      ) : loading ? (
        <div role="status" className="flex items-center gap-2 rounded-xl border border-border p-4 text-sm text-muted"><Spinner /> Loading recent items…</div>
      ) : !error ? (
        <p className="rounded-xl border border-dashed border-border p-4 text-sm text-muted">Open a skill or Markdown file and it will appear here for quick access.</p>
      ) : null}

      {historyOpen && <RecentDialog onClose={() => setHistoryOpen(false)} />}
      {pickerOpen && (
        <FolderPicker
          onClose={() => setPickerOpen(false)}
          onSelect={(path) => { setPickerOpen(false); navigate(studioPath(path)); }}
          onSelectFile={(path) => { setPickerOpen(false); navigate(markdownPath(path)); }}
        />
      )}
    </section>
  );
}
