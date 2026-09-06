"use client";

import { useCallback, useEffect, useState } from "react";
import { useNavigate } from "react-router-dom";
import NavBar from "@/components/NavBar";
import { Modal } from "@/components/Modal";
import { btnGhost, Spinner } from "@/components/ui";
import MineDialog from "@/components/MineDialog";
import * as api from "@/lib/api";
import type { AgentOption, MineFile, MineFiles, MineHistoryEntry } from "@/lib/api";
import type { FileData } from "@/lib/types";
import { refreshMining, useMining } from "@/lib/mining";
import { sessionsPath } from "@/lib/routes";

function PickaxeIcon() {
  return (
    <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
      <path d="M14.5 12.5 6.6 20.4a1 1 0 1 1-3-3l7.9-7.9" />
      <path d="M15.7 4.3A12.5 12.5 0 0 0 5.5 3a1 1 0 0 0 .1 1.8 22 22 0 0 1 6.3 3.4" />
      <path d="M17.7 3.7a1 1 0 0 0-1.4 0l-4.6 4.6a1 1 0 0 0 0 1.4l2.6 2.6a1 1 0 0 0 1.4 0l4.6-4.6a1 1 0 0 0 0-1.4z" />
      <path d="M19.7 8.3a12.5 12.5 0 0 1 1.3 10.2 1 1 0 0 1-1.7-.1 22 22 0 0 0-3.4-6.3" />
    </svg>
  );
}

function timeAgo(unix: number): string {
  const s = Math.max(0, Math.floor(Date.now() / 1000 - unix));
  if (s < 60) return "just now";
  const m = Math.floor(s / 60);
  if (m < 60) return `${m} min ago`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h} hour${h === 1 ? "" : "s"} ago`;
  const d = Math.floor(h / 24);
  return `${d} day${d === 1 ? "" : "s"} ago`;
}

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

const STATUS_TONE: Record<string, string> = {
  running: "text-info",
  ended: "text-fg",
};

// "reviewing" is the open-conversation steady state (it can last days once
// the report landed), so don't present it as in-flight work.
const STAGE_LABEL: Record<string, string> = {
  scanning: "scanning",
  analyzing: "analyzing",
  reviewing: "conversation open",
};

// What each well-known artifact is, so the listing reads as a story rather
// than a bag of filenames.
const FILE_HINTS: Record<string, string> = {
  "run.json": "Run record (settings, status, dirty-skill snapshot)",
  "out/inventory.jsonl": "Discovered sessions, one per line",
  "out/conversations.jsonl": "Distilled conversations (theme, topics, feedback)",
};

/** One labeled value in the run summary row. */
function Fact({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <dt className="text-[0.7rem] font-medium uppercase tracking-wider text-faint">{label}</dt>
      <dd className="mt-0.5 text-sm text-fg">{value}</dd>
    </div>
  );
}

/**
 * The mining page (route: /mining): the active run's record and the live
 * contents of its run dir, plus a display-only list of past runs (each
 * archived under history/<id>/ when the next mine starts). Reachable from the
 * Home MineCard.
 */
export function Component() {
  const navigate = useNavigate();
  const mining = useMining();
  const [agents, setAgents] = useState<AgentOption[]>([]);
  const [files, setFiles] = useState<MineFiles | null>(null);
  const [history, setHistory] = useState<MineHistoryEntry[] | null>(null);
  const [viewing, setViewing] = useState<FileData | null>(null);
  const [opening, setOpening] = useState<string | null>(null);
  const [continuing, setContinuing] = useState(false);
  const [mineOpen, setMineOpen] = useState(false);

  const refreshFiles = useCallback(() => {
    api
      .mineFiles()
      .then(setFiles)
      .catch(() => setFiles({ runDir: "", files: [] }));
  }, []);
  const refreshHistory = useCallback(() => {
    api
      .mineHistory()
      .then(setHistory)
      .catch(() => setHistory([]));
  }, []);
  useEffect(() => {
    refreshFiles();
    void refreshMining();
    api
      .terminalAgents()
      .then(setAgents)
      .catch(() => {});
  }, [refreshFiles]);
  // The archive grows only when a new run starts (which re-stamps the active
  // run); re-list past runs whenever that happens.
  const startedUnix = mining?.startedUnix;
  useEffect(() => {
    refreshHistory();
  }, [refreshHistory, startedUnix]);
  // Artifacts appear as the run progresses; re-list on status flips and on a
  // slow tick while running (the state poll itself is the fast one).
  const status = mining?.status;
  useEffect(() => {
    refreshFiles();
    if (status !== "running") return;
    const t = setInterval(refreshFiles, 5000);
    return () => clearInterval(t);
  }, [status, refreshFiles]);

  const openFile = async (f: MineFile) => {
    if (!files || opening) return;
    setOpening(f.rel);
    try {
      setViewing(await api.readFile(files.runDir, f.rel));
    } catch {
      // Likely deleted by a new run starting; refresh the listing instead.
      refreshFiles();
    } finally {
      setOpening(null);
    }
  };

  const continueRun = async () => {
    setContinuing(true);
    try {
      const { terminalId } = await api.mineContinue();
      void refreshMining();
      navigate(sessionsPath(terminalId));
    } catch {
      navigate(sessionsPath(mining?.terminalId));
    } finally {
      setContinuing(false);
    }
  };

  const agentLabel = (id?: string) => {
    if (!id) return "—";
    const a = agents.find((x) => x.id === id);
    return a ? `${a.label} (${a.flavorLabel})` : id;
  };

  const hasRun = mining != null && mining.status !== "idle";

  return (
    <div className="flex min-h-dvh flex-col">
      <NavBar
        breadcrumb={
          <>
            <span className="text-faint" aria-hidden>
              /
            </span>
            <span className="truncate font-medium text-fg">Mining</span>
          </>
        }
      />

      <main className="mx-auto w-full max-w-3xl flex-1 px-6 pb-24 pt-10">
        <div className="flex flex-col items-start gap-4 sm:flex-row sm:justify-between">
          <div>
            <h1 className="text-2xl font-semibold tracking-tight text-fg">Mining</h1>
            <p className="mt-1.5 max-w-prose text-sm text-muted">
              The most recent mining run and its working files. Starting a new mine archives the previous run below
              and begins a fresh one.
            </p>
          </div>
          <button
            type="button"
            onClick={() => setMineOpen(true)}
            title="Mine your past agent sessions to create or update skills"
            className="inline-flex shrink-0 items-center gap-2 rounded-lg bg-action px-3.5 py-2 text-sm font-medium text-action-fg transition-colors hover:bg-action-hover"
          >
            <PickaxeIcon />
            Mine your sessions
          </button>
        </div>

        {mining === null ? (
          <p className="mt-8 flex items-center gap-2 text-sm text-muted">
            <Spinner className="h-3.5 w-3.5" /> Loading…
          </p>
        ) : !hasRun ? (
          <p className="mt-8 text-sm text-muted">No mining run on record yet — hit “Mine your sessions” to start one.</p>
        ) : (
          <section className="mt-8 rounded-xl border border-border bg-surface p-4">
            <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
              <span className={`text-sm font-semibold capitalize ${STATUS_TONE[mining.status] ?? "text-fg"}`}>
                {mining.status === "running" && <Spinner className="mr-1.5 inline-block h-3 w-3" />}
                {mining.status}
                {mining.status === "running" && mining.stage
                  ? ` — ${STAGE_LABEL[mining.stage] ?? mining.stage}`
                  : ""}
              </span>
              {mining.startedUnix != null && (
                <span className="text-xs text-faint">started {timeAgo(mining.startedUnix)}</span>
              )}
              <span className="ml-auto flex items-center gap-2.5">
                {mining.status === "running" ? (
                  <>
                    <button
                      type="button"
                      onClick={() => navigate(sessionsPath(mining.terminalId))}
                      className="text-xs font-medium text-accent hover:opacity-80"
                    >
                      Watch
                    </button>
                    <button
                      type="button"
                      onClick={() => void api.mineStop().then(() => refreshMining())}
                      className="text-xs font-medium text-faint hover:text-danger"
                    >
                      Stop
                    </button>
                  </>
                ) : (
                  <button
                    type="button"
                    disabled={continuing}
                    onClick={() => void continueRun()}
                    title="Reopens the mining conversation (revived if its terminal was closed)"
                    className="text-xs font-medium text-accent hover:opacity-80 disabled:opacity-50"
                  >
                    {continuing ? "Opening…" : "Continue the conversation"}
                  </button>
                )}
              </span>
            </div>
            <dl className="mt-4 grid grid-cols-2 gap-x-6 gap-y-3 sm:grid-cols-3">
              <Fact label="Agent" value={agentLabel(mining.agent)} />
              <Fact label="Model" value={mining.model || "Default"} />
              <Fact label="Effort" value={mining.effort || "Default"} />
            </dl>
            {/* The exact launched prompt — the source of truth for what the run
                actually looked at (window, scope), which a hand-edited prompt can
                make a derived "Last N days" misrepresent. Older records that
                predate prompt capture fall back to the window. */}
            {mining.prompt ? (
              <dl className="mt-4">
                <dt className="text-[0.7rem] font-medium uppercase tracking-wider text-faint">Prompt</dt>
                <dd className="mt-1">
                  <pre className="max-h-48 overflow-auto whitespace-pre-wrap break-words rounded-md bg-panel p-3 font-mono text-xs leading-relaxed text-fg">
                    {mining.prompt}
                  </pre>
                </dd>
              </dl>
            ) : (
              mining.days != null && (
                <dl className="mt-4">
                  <Fact label="Window" value={`Last ${mining.days} days`} />
                </dl>
              )
            )}
            {(mining.sources?.length ?? 0) > 0 && (
              <p className="mt-3 text-xs text-muted">Sources: {mining.sources?.join(", ")}</p>
            )}
          </section>
        )}

        {history && history.length > 0 && (
          <section className="mt-8">
            <h2 className="mb-2 text-sm font-semibold text-fg">Past runs</h2>
            <ul className="divide-y divide-border overflow-hidden rounded-xl border border-border bg-surface">
              {history.map((h) => (
                <li key={h.id} className="flex items-center gap-3 px-4 py-2.5">
                  <span className="min-w-0 flex-1">
                    <span className="block truncate text-sm text-fg">{agentLabel(h.agent)}</span>
                    <span className="block truncate font-mono text-[0.7rem] text-faint">{h.id}</span>
                  </span>
                  <span className="shrink-0 text-xs text-muted">Last {h.days} days</span>
                  <span className="w-24 shrink-0 text-right text-xs text-faint">{timeAgo(h.startedUnix)}</span>
                </li>
              ))}
            </ul>
          </section>
        )}

        <section className="mt-8">
          <div className="mb-2 flex items-baseline justify-between">
            <h2 className="text-sm font-semibold text-fg">Run folder</h2>
            {files && files.files.length > 0 && (
              <button type="button" onClick={refreshFiles} className="text-xs text-faint hover:text-fg">
                Refresh
              </button>
            )}
          </div>
          {files && files.runDir && (
            <p className="mb-3 truncate font-mono text-xs text-faint" title={files.runDir}>
              {files.runDir}
            </p>
          )}
          {files === null ? (
            <p className="flex items-center gap-2 text-sm text-muted">
              <Spinner className="h-3.5 w-3.5" /> Listing…
            </p>
          ) : files.files.length === 0 ? (
            <p className="text-sm text-muted">Empty — no run has left files here yet.</p>
          ) : (
            <ul className="divide-y divide-border overflow-hidden rounded-xl border border-border bg-surface">
              {files.files.map((f) => (
                <li key={f.rel}>
                  <button
                    type="button"
                    onClick={() => void openFile(f)}
                    disabled={opening !== null}
                    className="flex w-full items-center gap-3 px-4 py-2.5 text-left transition-colors hover:bg-panel disabled:opacity-60"
                  >
                    <span className="min-w-0 flex-1">
                      <span className="block truncate font-mono text-sm text-fg">{f.rel}</span>
                      {FILE_HINTS[f.rel] && (
                        <span className="block truncate text-[0.7rem] text-faint">{FILE_HINTS[f.rel]}</span>
                      )}
                    </span>
                    {opening === f.rel && <Spinner className="h-3 w-3 shrink-0" />}
                    <span className="w-16 shrink-0 text-right text-xs text-muted">{formatSize(f.size)}</span>
                    <span className="w-24 shrink-0 text-right text-xs text-faint">{timeAgo(f.modifiedUnix)}</span>
                  </button>
                </li>
              ))}
            </ul>
          )}
        </section>
      </main>

      {viewing && (
        <Modal title={viewing.rel} onClose={() => setViewing(null)} widthClass="max-w-3xl">
          <div className="px-5 py-4">
            {viewing.tooLarge ? (
              <p className="text-sm text-muted">
                Too large to preview here ({formatSize(viewing.size)}). Open it from a terminal at the run dir.
              </p>
            ) : (
              <pre className="max-h-[60vh] overflow-auto whitespace-pre-wrap break-all rounded-md bg-panel p-3 font-mono text-xs leading-relaxed text-fg">
                {viewing.content ?? ""}
              </pre>
            )}
            <div className="mt-3 flex justify-end">
              <button type="button" onClick={() => setViewing(null)} className={btnGhost}>
                Close
              </button>
            </div>
          </div>
        </Modal>
      )}

      {mineOpen && (
        <MineDialog
          onClose={() => setMineOpen(false)}
          onStarted={(terminalId) => {
            setMineOpen(false);
            void refreshMining();
            navigate(sessionsPath(terminalId));
          }}
        />
      )}
    </div>
  );
}
