"use client";

import { useEffect, useMemo, useState } from "react";
import { Modal } from "@/components/Modal";
import { btnGhost, btnPrimary, Spinner } from "@/components/ui";
import FolderPicker from "@/components/FolderPicker";
import * as api from "@/lib/api";
import type { AgentOption } from "@/lib/api";
import { loadTerminalPrefs, saveTerminalPrefs } from "@/lib/terminalPrefs";
import { primeNotifications } from "@/lib/sessions";

function optionLabel(a: AgentOption): string {
  if (a.agent === "shell") return "Shell (bash)";
  const ver = a.version ? ` ${a.version}` : "";
  return `${a.label} — ${a.flavorLabel}${ver}`;
}

/** Split an args string into argv, honoring single/double quotes so a value with
 *  spaces (e.g. --append-system-prompt "be brief") stays one argument. */
function tokenizeArgs(input: string): string[] {
  const out: string[] = [];
  let cur = "";
  let quote: '"' | "'" | null = null;
  let started = false;
  for (const ch of input) {
    if (quote) {
      if (ch === quote) quote = null;
      else {
        cur += ch;
        started = true;
      }
    } else if (ch === '"' || ch === "'") {
      quote = ch;
      started = true;
    } else if (ch === " " || ch === "\t") {
      if (started) {
        out.push(cur);
        cur = "";
        started = false;
      }
    } else {
      cur += ch;
      started = true;
    }
  }
  if (started) out.push(cur);
  return out;
}

/**
 * Start a new managed session: pick an agent (each detected flavor — PATH CLI
 * and editor-extension build — is its own option), a working directory, and
 * optional flags, then create a detached tmux-backed session.
 */
export default function NewSessionDialog({
  onClose,
  onCreated,
  defaultCwd,
}: {
  onClose: () => void;
  onCreated: (s: api.TermSession) => void;
  /** Initial working directory (e.g. the open skill's folder when the dialog
   *  is reached from the studio's Agent panel); overrides remembered prefs. */
  defaultCwd?: string;
}) {
  const [agents, setAgents] = useState<AgentOption[] | null>(null);
  const [agentId, setAgentId] = useState("");
  const [cwd, setCwd] = useState("");
  const [skip, setSkip] = useState(false);
  const [auto, setAuto] = useState(false);
  const [extra, setExtra] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [pickerOpen, setPickerOpen] = useState(false);
  const [prefs, setPrefs] = useState<api.WorkspacePreferences | null>(null);
  const [prefsError, setPrefsError] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    void Promise.all([
      api.terminalAgents(),
      loadTerminalPrefs().catch(() => {
        if (alive) setPrefsError("Saved session defaults are unavailable. You can still start a session.");
        return null;
      }),
    ]).then(([a, p]) => {
        if (!alive) return;
        setPrefs(p);
        setAgents(a);
        setAgentId((a.find((x) => x.id === p?.lastAgent) ?? a.find((x) => x.agent !== "shell") ?? a[0])?.id ?? "");
      })
      .catch((e) => { if (alive) setError(e instanceof Error ? e.message : "Couldn't list agents."); });
    return () => { alive = false; };
  }, []);
  // Restore the agent's last-used config when it's selected (incl. the initial
  // default); falls back to an empty form for agents never launched before.
  useEffect(() => {
    if (!agentId) return;
    const p = prefs?.terminal[agentId];
    const wantAuto = p?.auto ?? false;
    setCwd(defaultCwd ?? p?.cwd ?? "");
    setAuto(wantAuto);
    // auto and skip are mutually exclusive; auto wins, matching the checkbox
    // handlers and the create() gating, in case stored prefs ever have both.
    setSkip((p?.skip ?? false) && !wantAuto);
    setExtra(p?.extra ?? "");
  }, [agentId, defaultCwd, prefs]);

  const selected = useMemo(() => agents?.find((a) => a.id === agentId), [agents, agentId]);

  const chooseCwd = () => setPickerOpen(true);

  const create = async () => {
    if (!selected) return;
    // Ask for notification permission NOW, while the OS prompt has an obvious
    // "why" (you just launched an agent) and the click still counts as a user
    // gesture (the browser's requestPermission needs one — no awaits first).
    primeNotifications();
    setBusy(true);
    setError(null);
    try {
      const s = await api.terminalCreate({
        agent: selected.id,
        cwd: cwd.trim(),
        cols: 80,
        rows: 24,
        ide: false,
        skipPermissions: skip && !auto && selected.agent === "claude",
        autoMode: auto && selected.agent === "claude",
        extraArgs: tokenizeArgs(extra),
      });
      // Preferences failing must not make a created session look like a failed
      // launch (which could cause a duplicate on retry).
      await saveTerminalPrefs(selected.id, { cwd: s.cwd, ide: false, skip, auto, extra }).catch(() => {});
      onCreated(s);
    } catch (e) {
      setBusy(false);
      setError(e instanceof Error ? e.message : "Couldn't start the session.");
    }
  };

  return (
    <>
      <Modal title="New session" onClose={onClose} dismissDisabled={pickerOpen}>
        <div className="space-y-4 px-5 py-4">
          {/* Agent */}
          <div>
            <label className="mb-1 block text-xs font-medium uppercase tracking-wider text-muted">Agent</label>
            {agents === null ? (
              <p className="flex items-center gap-2 text-sm text-muted">
                <Spinner className="h-3.5 w-3.5" /> Detecting…
              </p>
            ) : (
              <select
                value={agentId}
                onChange={(e) => setAgentId(e.target.value)}
                disabled={busy}
                className="w-full rounded-md border border-border bg-surface px-2.5 py-1.5 text-sm text-fg outline-none focus:border-accent disabled:opacity-50"
              >
                {agents.map((a) => (
                  <option key={a.id} value={a.id}>
                    {optionLabel(a)}
                  </option>
                ))}
              </select>
            )}
            {selected && selected.agent !== "shell" && (
              <p className="mt-1 truncate font-mono text-[0.7rem] text-faint" title={selected.bin}>
                {selected.bin}
              </p>
            )}
          </div>

          {/* Working directory */}
          <div>
            <label className="mb-1 block text-xs font-medium uppercase tracking-wider text-muted">
              Working directory
            </label>
            <div className="flex gap-2">
              <input
                value={cwd}
                onChange={(e) => setCwd(e.target.value)}
                placeholder="~ (home)"
                spellCheck={false}
                disabled={busy}
                className="w-full rounded-md border border-border bg-surface px-2.5 py-1.5 font-mono text-xs text-fg outline-none focus:border-accent disabled:opacity-50"
              />
              <button type="button" onClick={chooseCwd} disabled={busy} className={`${btnGhost} shrink-0`}>
                Browse…
              </button>
            </div>
          </div>

          {/* Per-agent options */}
          {selected?.agent === "claude" && (
            <div className="space-y-2">
              <label className="flex items-center gap-2 text-sm text-fg">
                <input
                  type="checkbox"
                  checked={auto}
                  onChange={(e) => {
                    setAuto(e.target.checked);
                    if (e.target.checked) setSkip(false);
                  }}
                  className="accent-accent"
                />
                Auto mode (<code className="font-mono text-[0.85em]">--permission-mode auto</code>)
                <span className="rounded-full bg-accent/15 px-1.5 py-0.5 text-[0.6rem] font-medium uppercase tracking-wide text-accent">
                  preview
                </span>
              </label>
              <label className="flex items-center gap-2 text-sm text-fg">
                <input
                  type="checkbox"
                  checked={skip}
                  onChange={(e) => {
                    setSkip(e.target.checked);
                    if (e.target.checked) setAuto(false);
                  }}
                  className="accent-accent"
                />
                Skip permission prompts
                <span className="rounded-full bg-warn/15 px-1.5 py-0.5 text-[0.6rem] font-medium uppercase tracking-wide text-warn">
                  risky
                </span>
              </label>
            </div>
          )}

          {selected && selected.agent !== "shell" && (
            <div>
              <label className="mb-1 block text-xs font-medium uppercase tracking-wider text-muted">
                Extra arguments <span className="font-normal normal-case text-faint">(optional)</span>
              </label>
              <input
                value={extra}
                onChange={(e) => setExtra(e.target.value)}
                placeholder="--model …"
                spellCheck={false}
                disabled={busy}
                className="w-full rounded-md border border-border bg-surface px-2.5 py-1.5 font-mono text-xs text-fg outline-none focus:border-accent disabled:opacity-50"
              />
            </div>
          )}

          {prefsError && <p role="status" className="text-xs text-muted">{prefsError}</p>}
          {error && <p className="text-xs text-danger">{error}</p>}

          <div className="flex justify-end gap-2 pt-1">
            <button type="button" onClick={onClose} disabled={busy} className={btnGhost}>
              Cancel
            </button>
            <button type="button" onClick={() => void create()} disabled={busy || !selected} className={btnPrimary}>
              {busy ? "Starting…" : "Start session"}
            </button>
          </div>
        </div>
      </Modal>

      {pickerOpen && (
        <FolderPicker
          context="session"
          initialPath={cwd || undefined}
          title="Choose a working directory"
          selectLabel="Use this folder"
          onSelect={(p) => {
            setPickerOpen(false);
            setCwd(p);
          }}
          onClose={() => setPickerOpen(false)}
        />
      )}
    </>
  );
}
