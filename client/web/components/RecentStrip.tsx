import { useNavigate } from "react-router-dom";
import { FileIcon, FolderIcon } from "./FileIcon";
import { Spinner } from "./ui";
import { useRecents, useRecentsStatus, refreshRecents, removeRecent } from "@/lib/recents";
import { markdownPath, studioPath } from "@/lib/routes";

/** Quiet shortcuts to the four most recently opened skills and documents. */
export default function RecentStrip() {
  const recents = useRecents();
  const { loading, error } = useRecentsStatus();
  const navigate = useNavigate();

  return (
    <section className="mt-6 min-w-0" aria-labelledby="recent-heading">
      <h2 id="recent-heading" className="mb-2 text-xs font-medium text-muted">Recent</h2>

      {error && (
        <p role="status" className="mb-3 text-xs text-warn">
          {error} <button type="button" onClick={refreshRecents} className="font-medium underline">Retry</button>
        </p>
      )}
      {recents.length > 0 ? (
        <ul className="grid gap-x-6 gap-y-1 sm:grid-cols-2 lg:grid-cols-4">
          {recents.slice(0, 4).map((r) => (
            <li key={r.root} className="group flex min-w-0 items-center gap-1">
              <button
                type="button"
                onClick={() => navigate(r.kind === "markdown" ? markdownPath(r.root) : studioPath(r.root))}
                className="flex min-w-0 flex-1 items-center gap-2.5 rounded-md px-1 py-2 text-left text-muted transition-colors hover:bg-panel hover:text-fg focus-visible:outline-accent"
                title={r.root}
              >
                {r.kind === "markdown" ? <FileIcon name={r.name} /> : <FolderIcon open={false} name={r.name} />}
                <span className="min-w-0">
                  <span className="block truncate text-sm font-medium">{r.name}</span>
                  <span className="mt-0.5 block truncate font-mono text-[0.68rem] text-faint">{r.root}</span>
                </span>
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
    </section>
  );
}
