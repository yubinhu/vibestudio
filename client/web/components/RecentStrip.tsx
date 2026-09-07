import { useNavigate } from "react-router-dom";
import { FileIcon, FolderIcon } from "./FileIcon";
import { Spinner } from "./ui";
import { useRecents, useRecentsStatus, refreshRecents, removeRecent } from "@/lib/recents";
import { markdownPath, studioPath } from "@/lib/routes";

/** Compact, name-only shortcuts to the four most recently opened items. */
export default function RecentStrip() {
  const recents = useRecents();
  const { loading, error } = useRecentsStatus();
  const navigate = useNavigate();

  return (
    <section className="mt-6 flex min-w-0 flex-col gap-1 sm:flex-row sm:items-start sm:gap-3" aria-labelledby="recent-heading">
      <h2 id="recent-heading" className="shrink-0 text-xs font-medium text-fg sm:py-2.5">Recent</h2>

      <div className="min-w-0">
        {error && (
          <p role="status" className="mb-3 text-xs text-warn">
            {error} <button type="button" onClick={refreshRecents} className="font-medium underline">Retry</button>
          </p>
        )}
        {recents.length > 0 ? (
          <ul className="flex min-w-0 max-w-full flex-wrap items-center gap-x-3 gap-y-1">
            {recents.slice(0, 4).map((r) => (
              <li key={r.root} className="group flex min-w-0 max-w-full items-center gap-1 sm:max-w-64">
                <button
                  type="button"
                  onClick={() => navigate(r.kind === "markdown" ? markdownPath(r.root) : studioPath(r.root))}
                  className="flex min-w-0 flex-1 items-center gap-2 rounded-md px-1 py-2 text-left text-fg transition-colors hover:bg-panel focus-visible:outline-accent"
                  title={r.root}
                >
                  {r.kind === "markdown" ? <FileIcon name={r.name} /> : <FolderIcon open={false} name={r.name} />}
                  <span className="min-w-0 truncate text-sm font-medium">{r.name}</span>
                </button>
                <button
                  type="button"
                  onClick={() => removeRecent(r.root)}
                  aria-label={`Remove ${r.name} from recents`}
                  className="shrink-0 rounded-md px-2 py-1.5 text-faint hover:bg-panel hover:text-danger focus-visible:opacity-100 sm:opacity-0 sm:group-hover:opacity-100 sm:group-focus-within:opacity-100"
                >
                  ×
                </button>
              </li>
            ))}
          </ul>
        ) : loading ? (
          <div role="status" className="flex items-center gap-2 py-2 text-xs text-muted"><Spinner /> Loading recent items…</div>
        ) : !error ? (
          <p className="py-2 text-xs text-muted">Skills and Markdown files you open will appear here.</p>
        ) : null}
      </div>
    </section>
  );
}
