"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { Badge, Spinner } from "@/components/ui";
import { agentColor } from "@/lib/agents";
import { useConfirm } from "@/components/useConfirm";
import * as api from "@/lib/api";
import type { ImportedSecret, SecretEntry, SecretsStatus } from "@/lib/api";

const btnPrimary =
  "rounded-md bg-action px-3 py-1.5 text-sm font-medium text-action-fg transition-opacity hover:opacity-90 disabled:opacity-40";
const btnGhost =
  "rounded-md border border-border px-3 py-1.5 text-sm text-fg transition-colors hover:bg-panel disabled:opacity-40";

const mask = (v: string) => "•".repeat(Math.min(12, Math.max(4, v.length)));

function KeyIcon() {
  return (
    <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
      <circle cx="7.5" cy="15.5" r="5.5" />
      <path d="m21 2-9.6 9.6" />
      <path d="m15.5 7.5 3 3L22 7l-3-3" />
    </svg>
  );
}

/** The machine-local secret store — the one functional source today. The env vars
 *  a skill references are auto-detected and folded into its `metadata.required-env`
 *  on save and on open (the post-save pipeline), so there's nothing to scan from
 *  here; adding a secret refreshes a skill's required-env on its next save/reopen,
 *  not instantly. Carries the activation-skill installer because that skill is the
 *  local store's runtime mechanism — it's how agents load these vars. */
export default function LocalStoreCard() {
  const [status, setStatus] = useState<SecretsStatus | null>(null);
  const [secrets, setSecrets] = useState<SecretEntry[] | null>(null);
  const [reveal, setReveal] = useState<Set<string>>(new Set());
  const [newKey, setNewKey] = useState("");
  const [newValue, setNewValue] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);
  // ".env import": parsed entries awaiting the user's pick (null = no import open).
  const [importing, setImporting] = useState<ImportedSecret[] | null>(null);
  const [picked, setPicked] = useState<Record<string, boolean>>({});
  const envInputRef = useRef<HTMLInputElement>(null);
  const confirm = useConfirm();

  const refresh = useCallback(async () => {
    const [st, ls] = await Promise.all([
      api.secretsStatus().catch(() => null),
      api.secretsList().catch(() => [] as SecretEntry[]),
    ]);
    setStatus(st);
    setSecrets(ls);
  }, []);
  useEffect(() => {
    void refresh();
  }, [refresh]);

  const add = async (key: string, value: string) => {
    if (!key.trim()) return;
    setBusy(true);
    setErr(null);
    setNote(null);
    try {
      await api.secretSet(key.trim(), value);
      setNewKey("");
      setNewValue("");
      await refresh();
    } catch (e) {
      setErr(e instanceof Error ? e.message : "Couldn’t save secret");
    } finally {
      setBusy(false);
    }
  };

  const remove = async (key: string) => {
    if (
      !(await confirm({
        title: `Delete ${key}?`,
        body: "Skills using it will lose access.",
        confirmLabel: "Delete",
        danger: true,
      }))
    )
      return;
    setBusy(true);
    setErr(null);
    setNote(null);
    try {
      await api.secretDelete(key);
      await refresh();
    } catch (e) {
      setErr(e instanceof Error ? e.message : "Couldn’t delete secret");
    } finally {
      setBusy(false);
    }
  };

  // Read a chosen .env file, parse it server-side (same parser the zip import
  // uses), and stage the entries for review — applying is per-key secretSet.
  const previewEnvFile = async (file: File) => {
    setErr(null);
    setNote(null);
    try {
      const entries = await api.secretsPreviewEnv(await file.text());
      if (entries.length === 0) {
        setErr("No secrets found in that file — expected KEY=value lines.");
        return;
      }
      const init: Record<string, boolean> = {};
      entries.forEach((e) => (init[e.key] = true));
      setPicked(init);
      setImporting(entries);
    } catch (e) {
      setErr(e instanceof Error ? e.message : "Couldn’t read the file");
    }
  };

  const applyImport = async (entries: ImportedSecret[]) => {
    setBusy(true);
    setErr(null);
    try {
      let n = 0;
      for (const e of entries) {
        if (!picked[e.key]) continue;
        await api.secretSet(e.key, e.value);
        n += 1;
      }
      setImporting(null);
      setNote(`Imported ${n} secret${n === 1 ? "" : "s"}.`);
      await refresh();
    } catch (e) {
      setErr(e instanceof Error ? e.message : "Couldn’t save secrets");
    } finally {
      setBusy(false);
    }
  };

  const runSetup = async () => {
    setBusy(true);
    setErr(null);
    setNote(null);
    try {
      const r = await api.secretsSetup();
      setNote(
        r.installedAgents.length
          ? `Activation skill installed for ${r.installedAgents.join(", ")}.`
          : "Set up — no agents detected yet to install the activation skill into.",
      );
      await refresh();
    } catch (e) {
      setErr(e instanceof Error ? e.message : "Setup failed");
    } finally {
      setBusy(false);
    }
  };

  const loading = secrets === null || status === null;
  const installedAny = status?.agents.some((a) => a.hasSkill) ?? false;

  return (
    <section className="overflow-hidden rounded-xl border border-border bg-surface">
      <header className="flex items-center gap-3 border-b border-border px-5 py-4">
        <span className="grid h-9 w-9 shrink-0 place-items-center rounded-lg bg-panel text-muted" aria-hidden>
          <KeyIcon />
        </span>
        <div className="min-w-0">
          <h2 className="text-sm font-semibold text-fg">Your secrets</h2>
          <p className="text-xs text-muted">On this machine — cloud sync coming soon.</p>
        </div>
        {/* Gated on a real load: don't claim "Active" before we've reached the
            store (or if it's unreachable — refresh swallows fetch errors). */}
        {!loading && (
          <div className="ml-auto flex shrink-0 items-center gap-2">
            <Badge tone="ok">
              <span className="h-1.5 w-1.5 rounded-full bg-ok" aria-hidden />
              Active
            </Badge>
            <span className="text-xs text-faint">
              {secrets.length} {secrets.length === 1 ? "secret" : "secrets"}
            </span>
          </div>
        )}
      </header>

      {loading ? (
        <p className="flex items-center gap-2 px-5 py-6 text-sm text-muted">
          <Spinner className="h-3.5 w-3.5" /> Loading secrets…
        </p>
      ) : (
        <div className="space-y-5 px-5 py-5">
          <form
            onSubmit={(e) => {
              e.preventDefault();
              void add(newKey, newValue);
            }}
            className="flex gap-2"
          >
            <input
              value={newKey}
              onChange={(e) => setNewKey(e.target.value.toUpperCase())}
              placeholder="OPENAI_API_KEY"
              spellCheck={false}
              className="w-2/5 rounded-md border border-border bg-surface px-2.5 py-2 font-mono text-sm text-fg outline-none focus:border-accent"
            />
            <input
              value={newValue}
              onChange={(e) => setNewValue(e.target.value)}
              placeholder="value"
              type="password"
              spellCheck={false}
              autoComplete="off"
              className="min-w-0 flex-1 rounded-md border border-border bg-surface px-2.5 py-2 font-mono text-sm text-fg outline-none focus:border-accent"
            />
            <button type="submit" disabled={busy || !newKey.trim()} className={btnPrimary}>
              Save
            </button>
          </form>

          {importing ? (
            <div className="space-y-2 rounded-lg border border-border bg-panel px-3 py-2.5">
              <p className="text-xs text-muted">
                Found {importing.length} secret{importing.length === 1 ? "" : "s"} — import the checked ones:
              </p>
              <ul className="space-y-1.5">
                {importing.map((e) => (
                  <li key={e.key}>
                    <label className="flex items-center gap-2 text-sm">
                      <input
                        type="checkbox"
                        checked={!!picked[e.key]}
                        onChange={(ev) => setPicked((p) => ({ ...p, [e.key]: ev.target.checked }))}
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
              <div className="flex justify-end gap-2">
                <button type="button" onClick={() => setImporting(null)} disabled={busy} className={btnGhost}>
                  Cancel
                </button>
                <button
                  type="button"
                  onClick={() => void applyImport(importing)}
                  disabled={busy || !importing.some((e) => picked[e.key])}
                  className={btnPrimary}
                >
                  {busy ? "Importing…" : "Import"}
                </button>
              </div>
            </div>
          ) : (
            <div className="flex items-center gap-2">
              <button type="button" onClick={() => envInputRef.current?.click()} disabled={busy} className={btnGhost}>
                Import .env…
              </button>
              <span className="text-[0.7rem] text-faint">Load a teammate’s exported .env into your store.</span>
              <input
                ref={envInputRef}
                type="file"
                accept=".env,text/plain"
                className="hidden"
                onChange={(e) => {
                  const file = e.target.files?.[0];
                  e.target.value = "";
                  if (file) void previewEnvFile(file);
                }}
              />
            </div>
          )}

          {secrets.length > 0 ? (
            <ul className="space-y-0 overflow-hidden rounded-lg border border-border">
              {secrets.map((s) => {
                const shown = reveal.has(s.key);
                return (
                  <li key={s.key} className="flex items-center gap-2 border-t border-border px-3 py-2 text-sm first:border-t-0">
                    <code className="shrink-0 font-mono text-fg">{s.key}</code>
                    <span className="ml-auto min-w-0 truncate font-mono text-xs text-faint">{shown ? s.value : mask(s.value)}</span>
                    <button
                      type="button"
                      onClick={() =>
                        setReveal((r) => {
                          const n = new Set(r);
                          if (n.has(s.key)) n.delete(s.key);
                          else n.add(s.key);
                          return n;
                        })
                      }
                      className="shrink-0 text-xs text-faint hover:text-fg"
                    >
                      {shown ? "Hide" : "Show"}
                    </button>
                    <button
                      type="button"
                      onClick={() => void remove(s.key)}
                      disabled={busy}
                      aria-label={`Delete ${s.key}`}
                      className="shrink-0 text-faint hover:text-danger"
                    >
                      ✕
                    </button>
                  </li>
                );
              })}
            </ul>
          ) : (
            <p className="rounded-lg border border-dashed border-border px-3 py-6 text-center text-xs text-faint">
              No secrets stored yet. Add your first API key or token above.
            </p>
          )}

          <div className="space-y-2 rounded-lg border border-border bg-panel px-3 py-2.5">
            <div className="flex items-center justify-between gap-2">
              <span className="text-xs font-medium text-fg">Activation skill</span>
              <button type="button" onClick={() => void runSetup()} disabled={busy} className={btnGhost}>
                {busy ? "…" : installedAny ? "Reinstall" : "Set up"}
              </button>
            </div>
            <p className="text-[0.7rem] text-muted">
              Installs the <span className="font-mono">load-secrets</span> skill into the shared{" "}
              <span className="font-mono">~/.agents/skills</span> dir (and Claude Code’s own) so your agents can load these
              vars. Run again after installing a new agent.
            </p>
            <ul className="flex flex-wrap gap-1.5">
              {status.agents.filter((a) => a.installed).length === 0 ? (
                <li className="text-[0.7rem] text-faint">No agents detected on this machine.</li>
              ) : (
                status.agents
                  .filter((a) => a.installed)
                  .map((a) => (
                    <li
                      key={a.agent}
                      className={`flex items-center gap-1 rounded-full border border-border px-2 py-0.5 text-[0.7rem] ${
                        a.hasSkill ? "text-ok" : "text-faint"
                      }`}
                    >
                      <span className="h-1.5 w-1.5 rounded-full" style={{ background: agentColor(a.agent) }} aria-hidden />
                      {a.agent}
                      {a.hasSkill ? " ✓" : ""}
                    </li>
                  ))
              )}
            </ul>
          </div>

          {note && <p className="text-xs text-ok">{note}</p>}
          {err && <p className="text-xs text-danger">{err}</p>}
        </div>
      )}
    </section>
  );
}
