import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { Modal } from "./Modal";
import { btnGhost, Spinner } from "./ui";
import * as api from "@/lib/api";
import type { SessionHistoryEntry, TermSession } from "@/lib/api";
import { subscribeWorkspaceConnection, workspaceConnection } from "@/lib/workspaceConnection";

const PAGE_SIZE = 50;
const AGENT_LABELS: Record<string, string> = {
  claude: "Claude Code", codex: "Codex", cursor: "Cursor", gemini: "Gemini CLI", opencode: "opencode",
};
const entryKey = (entry: SessionHistoryEntry) => `${entry.agent}:${entry.sessionId}`;
const connectionEpoch = () => workspaceConnection().epoch;

export function SessionHistoryIcon() {
  return (
    <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
      <path d="M3 12a9 9 0 1 0 2.6-6.4L3 8" />
      <path d="M3 3v5h5M12 7v5l3 2" />
    </svg>
  );
}

function historyTime(seconds: number): string {
  const date = new Date(seconds * 1000);
  return seconds > 0 && Number.isFinite(date.getTime())
    ? date.toLocaleString(undefined, { month: "short", day: "numeric", year: "numeric", hour: "numeric", minute: "2-digit" })
    : "Time unavailable";
}

/** A picker for saved agent conversations. Resume returns to the ordinary terminal. */
export default function SessionHistoryDialog({ onClose, onResumed }: {
  onClose: () => void;
  onResumed: (session: TermSession) => void;
}) {
  const epoch = useSyncExternalStore(subscribeWorkspaceConnection, connectionEpoch);
  const [query, setQuery] = useState("");
  const [retry, setRetry] = useState(0);
  const [entries, setEntries] = useState<SessionHistoryEntry[]>([]);
  const [warnings, setWarnings] = useState<string[]>([]);
  const [offset, setOffset] = useState(0);
  const [hasMore, setHasMore] = useState(false);
  const [truncated, setTruncated] = useState(false);
  const [loading, setLoading] = useState(true);
  const [loadingMore, setLoadingMore] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [moreError, setMoreError] = useState<string | null>(null);
  const [resumeError, setResumeError] = useState<{ key: string; message: string } | null>(null);
  const [resuming, setResuming] = useState<string | null>(null);
  const requestGeneration = useRef(0);
  const resumeGeneration = useRef(0);
  const pagePending = useRef(false);
  const resumePending = useRef(false);
  const searchRef = useRef<HTMLInputElement>(null);
  const available = workspaceConnection().available;

  useEffect(() => {
    const previousFocus = document.activeElement;
    searchRef.current?.focus();
    return () => {
      resumeGeneration.current++;
      if (previousFocus instanceof HTMLElement && previousFocus.isConnected) previousFocus.focus();
    };
  }, []);

  useEffect(() => {
    const generation = ++requestGeneration.current;
    const requestEpoch = workspaceConnection().epoch;
    resumeGeneration.current++;
    resumePending.current = false;
    pagePending.current = false;
    setResuming(null);
    setResumeError(null);
    setEntries([]);
    setWarnings([]);
    setOffset(0);
    setHasMore(false);
    setTruncated(false);
    setMoreError(null);
    setLoadingMore(false);
    setError(null);
    setLoading(true);
    const current = () => generation === requestGeneration.current &&
      workspaceConnection().epoch === requestEpoch && workspaceConnection().available;

    if (!workspaceConnection().available) {
      setLoading(false);
      setError("Session history is unavailable while the server reconnects.");
      return () => { requestGeneration.current++; };
    }

    const timer = setTimeout(() => {
      void api.terminalHistory({ query: query.trim(), offset: 0, limit: PAGE_SIZE }).then((result) => {
        if (!current()) return;
        setEntries(result.sessions);
        setWarnings(result.warnings);
        setHasMore(result.hasMore);
        setTruncated(result.truncated);
      }).catch((cause: unknown) => {
        if (current()) setError(cause instanceof Error ? cause.message : "Could not load session history.");
      }).finally(() => {
        if (current()) setLoading(false);
      });
    }, query.trim() ? 250 : 0);
    return () => {
      clearTimeout(timer);
      requestGeneration.current++;
    };
  }, [query, epoch, retry]);

  const loadMore = async () => {
    if (pagePending.current || loading || !hasMore || !workspaceConnection().available) return;
    pagePending.current = true;
    const generation = ++requestGeneration.current;
    const requestEpoch = workspaceConnection().epoch;
    const nextOffset = offset + PAGE_SIZE;
    const current = () => generation === requestGeneration.current &&
      workspaceConnection().epoch === requestEpoch && workspaceConnection().available;
    setLoadingMore(true);
    setMoreError(null);
    try {
      const result = await api.terminalHistory({ query: query.trim(), offset: nextOffset, limit: PAGE_SIZE });
      if (!current()) return;
      setEntries((previous) => {
        const seen = new Set(previous.map(entryKey));
        return [...previous, ...result.sessions.filter((entry) => !seen.has(entryKey(entry)))];
      });
      setWarnings((previous) => [...new Set([...previous, ...result.warnings])]);
      setOffset(nextOffset);
      setHasMore(result.hasMore);
      setTruncated(result.truncated);
    } catch (cause) {
      if (current()) setMoreError(cause instanceof Error ? cause.message : "Could not load more sessions.");
    } finally {
      if (current()) {
        pagePending.current = false;
        setLoadingMore(false);
      }
    }
  };

  const resume = async (entry: SessionHistoryEntry) => {
    if (resumePending.current || loading || !workspaceConnection().available) return;
    resumePending.current = true;
    const generation = ++resumeGeneration.current;
    const requestEpoch = workspaceConnection().epoch;
    const key = entryKey(entry);
    const current = () => generation === resumeGeneration.current &&
      workspaceConnection().epoch === requestEpoch && workspaceConnection().available;
    setResuming(key);
    setResumeError(null);
    try {
      // The server checks the exact saved conversation and reuses its live
      // terminal when one exists; never substitute the cwd's latest session.
      const session = await api.terminalResumeHistory({ agent: entry.agent, sessionId: entry.sessionId });
      if (current()) onResumed(session);
    } catch (cause) {
      if (current()) setResumeError({ key, message: cause instanceof Error ? cause.message : "Could not resume this session." });
    } finally {
      if (current()) {
        resumePending.current = false;
        setResuming(null);
      }
    }
  };

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-label="Session history"
      onKeyDown={(event) => {
        if (event.key !== "Tab") return;
        const controls = Array.from(event.currentTarget.querySelectorAll<HTMLElement>("button:not(:disabled), input:not(:disabled), summary"))
          .filter((control) => control.getClientRects().length > 0);
        const first = controls[0];
        const last = controls[controls.length - 1];
        if (!first) event.preventDefault();
        else if (event.shiftKey && document.activeElement === first) {
          event.preventDefault();
          last.focus();
        } else if (!event.shiftKey && document.activeElement === last) {
          event.preventDefault();
          first.focus();
        }
      }}
    >
      <Modal title="Session history" onClose={onClose} dismissDisabled={resuming !== null} widthClass="max-w-2xl">
        <div className="flex max-h-[calc(100dvh-8rem)] min-h-0 flex-col">
          <div className="shrink-0 space-y-2 px-5 py-4">
            <input
              type="search"
              ref={searchRef}
              aria-label="Search past sessions"
              placeholder="Search titles, agents, or folders…"
              value={query}
              disabled={resuming !== null}
              onChange={(event) => {
                requestGeneration.current++;
                setLoading(true);
                setQuery(event.target.value);
              }}
              className="w-full rounded-md border border-border bg-surface px-3 py-2 text-sm text-fg outline-none focus:border-accent disabled:opacity-50"
            />
            <p className="text-xs text-muted">Saved Claude Code and Codex conversations on this server. Resume opens the agent terminal.</p>
            {truncated && <p className="text-xs text-muted">Some older sessions may be missing from this history.</p>}
            {warnings.length > 0 && (
              <details className="text-xs text-muted">
                <summary className="cursor-pointer">Some session history could not be read</summary>
                <ul className="mt-2 max-h-24 space-y-1 overflow-y-auto break-words">
                  {warnings.map((warning) => <li key={warning}>{warning}</li>)}
                </ul>
              </details>
            )}
          </div>
          <div className="min-h-0 overflow-y-auto border-t border-border px-5" aria-busy={loading}>
            {loading ? (
              <p role="status" className="flex items-center gap-2 py-8 text-sm text-muted"><Spinner className="h-4 w-4" /> Loading history…</p>
            ) : error ? (
              <div className="space-y-3 py-6">
                <p role="alert" className="text-sm text-danger">{error}</p>
                <button type="button" onClick={() => setRetry((value) => value + 1)} disabled={!available} className={btnGhost}>Retry</button>
              </div>
            ) : entries.length === 0 ? (
              <p role="status" className="py-8 text-sm text-muted">{query.trim() ? "No sessions match your search." : "No saved Claude Code or Codex sessions found on this server."}</p>
            ) : (
              <ul aria-label="Past sessions" className="divide-y divide-border">
                {entries.map((entry) => {
                  const key = entryKey(entry);
                  const title = entry.title || "Untitled session";
                  const isOpen = !!entry.activeTerminalId;
                  const canOpen = isOpen || entry.canResume;
                  const rowError = resumeError?.key === key ? resumeError.message : null;
                  return (
                    <li key={key} className="py-3">
                      <div className="flex items-start gap-3">
                        <div className="min-w-0 flex-1 space-y-1">
                          <p className="break-words text-sm font-medium text-fg">{title}</p>
                          <p className="break-all font-mono text-[0.7rem] text-faint" title={entry.cwd}>{entry.cwd || "Folder unavailable"}</p>
                          <p className="text-xs text-muted">{AGENT_LABELS[entry.agent] ?? entry.agent} · {historyTime(entry.updatedAt || entry.createdAt)}</p>
                        </div>
                        <button
                          type="button"
                          aria-label={`${isOpen ? "Open" : "Resume"} ${title}`}
                          disabled={!canOpen || resuming !== null || !available}
                          onClick={() => void resume(entry)}
                          className={`${btnGhost} shrink-0`}
                        >
                          {resuming === key ? (isOpen ? "Opening…" : "Resuming…") : (isOpen ? "Open" : "Resume")}
                        </button>
                      </div>
                      {!canOpen && <p className="mt-2 text-xs text-muted">{entry.resumeUnavailableReason || "This agent does not support resuming saved sessions."}</p>}
                      {rowError && <p role="alert" className="mt-2 text-xs text-danger">{rowError}</p>}
                    </li>
                  );
                })}
              </ul>
            )}
            {!loading && !error && hasMore && (
              <div className="space-y-2 border-t border-border py-3">
                {moreError && <p role="alert" className="text-xs text-danger">{moreError}</p>}
                <button type="button" onClick={() => void loadMore()} disabled={loadingMore || resuming !== null || !available} className={btnGhost}>
                  {loadingMore ? "Loading…" : moreError ? "Retry loading more" : "Load more"}
                </button>
              </div>
            )}
          </div>
        </div>
      </Modal>
    </div>
  );
}
