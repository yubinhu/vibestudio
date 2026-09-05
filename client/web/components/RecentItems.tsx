import { useState } from "react";
import { useNavigate } from "react-router-dom";
import { FileIcon, FolderIcon } from "./FileIcon";
import { Spinner } from "./ui";
import { useRecents, useRecentsStatus, refreshRecents, removeRecent } from "@/lib/recents";
import { markdownPath, studioPath } from "@/lib/routes";

function openedLabel(timestamp?: number): string {
  if (!timestamp) return "";
  const days = Math.floor((Date.now() - timestamp * 1000) / 86400000);
  if (days < 1) return "Today";
  if (days === 1) return "Yesterday";
  if (days < 7) return `${days} days ago`;
  return new Date(timestamp * 1000).toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

export default function RecentItems({ onOpen }: { onOpen: () => void }) {
  const recents = useRecents();
  const { loading, error } = useRecentsStatus();
  const navigate = useNavigate();
  const [filter, setFilter] = useState("all");
  const [query, setQuery] = useState("");
  const [expanded, setExpanded] = useState(false);
  const matches = recents.filter((r) =>
    (filter === "all" || (r.kind ?? "skill") === filter) &&
    `${r.name} ${r.root}`.toLowerCase().includes(query.toLowerCase()),
  );
  const shown = expanded || query ? matches : matches.slice(0, 6);

  return (
    <div className="min-w-0">
        {recents.length > 0 && (
          <div className="flex flex-wrap items-center justify-between gap-3 border-b border-border px-4 py-3">
            <div className="flex gap-1" role="group" aria-label="Filter recent items">
              {[["all", "All"], ["skill", "Skills"], ["markdown", "Markdown"]].map(([value, label]) => (
                <button key={value} type="button" aria-pressed={filter === value} onClick={() => setFilter(value)}
                  className={`rounded-md px-2.5 py-1.5 text-xs font-medium ${filter === value ? "bg-accent-soft text-accent" : "text-muted hover:bg-panel hover:text-fg"}`}>
                  {label}
                </button>
              ))}
            </div>
            <input type="search" aria-label="Search recent items" placeholder="Find recent items…" value={query} onChange={(e) => setQuery(e.target.value)}
              className="min-w-0 flex-1 rounded-md border border-border bg-app px-2.5 py-1.5 text-xs outline-none focus:border-accent sm:max-w-44" />
          </div>
        )}
        {error && <div role="status" className="border-b border-border px-4 py-3 text-xs text-warn">{error} <button type="button" onClick={refreshRecents} className="font-medium underline">Retry</button></div>}
        {loading && !recents.length ? (
          <div role="status" className="flex items-center gap-2 p-6 text-sm text-muted"><Spinner /> Loading recent items…</div>
        ) : !recents.length ? (
          <div className="flex min-h-60 flex-col items-start justify-center p-6 sm:p-8">
            <span className="mb-4 rounded-xl bg-accent-soft p-3 text-accent"><FolderIcon open={false} name="work" /></span>
            <h3 className="font-semibold text-fg">Your work, within reach</h3>
            <p className="mt-2 max-w-sm text-sm leading-relaxed text-muted">Open a skill or Markdown file. It will appear here the next time you come back.</p>
            <button type="button" onClick={onOpen} className="mt-4 text-sm font-medium text-accent hover:underline">Browse files →</button>
          </div>
        ) : !shown.length ? (
          <p className="p-6 text-sm text-muted">No recent items match this filter.</p>
        ) : (
          <ul className="divide-y divide-border">
            {shown.map((r) => (
              <li key={r.root} className="group flex min-w-0 items-center gap-1 pr-2 hover:bg-panel">
                <button type="button" onClick={() => navigate(r.kind === "markdown" ? markdownPath(r.root) : studioPath(r.root))}
                  className="flex min-w-0 flex-1 items-center gap-3 px-4 py-4 text-left focus-visible:outline-accent" title={r.root}>
                  <span className="grid h-9 w-9 shrink-0 place-items-center rounded-lg border border-border bg-app">
                    {r.kind === "markdown" ? <FileIcon name={r.name} /> : <FolderIcon open={false} name={r.name} />}
                  </span>
                  <span className="min-w-0 flex-1">
                    <span className="block truncate text-sm font-medium text-fg">{r.name}</span>
                    <span className="mt-1 block truncate font-mono text-[0.68rem] text-muted">{r.root}</span>
                  </span>
                  <span className="hidden shrink-0 text-right text-[0.68rem] text-muted sm:block">
                    <span className="block">{r.kind === "markdown" ? "Markdown" : "Skill"}</span>
                    {r.openedAt && <time className="mt-1 block text-faint" dateTime={new Date(r.openedAt * 1000).toISOString()} title={new Date(r.openedAt * 1000).toLocaleString()}>{openedLabel(r.openedAt)}</time>}
                  </span>
                </button>
                <button type="button" onClick={() => removeRecent(r.root)} aria-label={`Remove ${r.name} from recents`} title="Remove from recents"
                  className="shrink-0 rounded-md px-2 py-1 text-muted hover:bg-surface hover:text-danger focus-visible:opacity-100 sm:opacity-0 sm:group-hover:opacity-100">×</button>
              </li>
            ))}
          </ul>
        )}
        {matches.length > 6 && !query && (
          <button type="button" onClick={() => setExpanded(!expanded)} className="w-full border-t border-border px-4 py-3 text-left text-xs font-medium text-accent hover:bg-panel">
            {expanded ? "Show less" : `Show all ${matches.length} recent items`} <span aria-hidden>→</span>
          </button>
        )}
    </div>
  );
}
