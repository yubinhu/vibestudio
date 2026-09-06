"use client";

import { useCallback, useEffect, useState } from "react";
import { useNavigate } from "react-router-dom";
import { Spinner } from "@/components/ui";
import { agentColor, isEditableBundledSkill, KIND_TAG, type SkillKind } from "@/lib/agents";
import { useConfirm } from "@/components/useConfirm";
import * as api from "@/lib/api";
import type { GitInfo, SyncTarget } from "@/lib/api";
import { credentialsPath } from "@/lib/routes";
import { useStudio } from "./StudioContext";

const btnGhost =
  "rounded-md border border-border px-3 py-1.5 text-sm text-fg transition-colors hover:bg-panel disabled:opacity-40";

function Section({ title, badge, children }: { title: string; badge?: React.ReactNode; children: React.ReactNode }) {
  return (
    <section className="border-t border-border px-5 py-4 first:border-t-0">
      <div className="mb-3 flex items-center gap-2">
        <h3 className="text-xs font-semibold uppercase tracking-wider text-muted">{title}</h3>
        {badge}
      </div>
      {children}
    </section>
  );
}

// ---- Sync -------------------------------------------------------------------
function SyncSection({ root }: { root: string }) {
  const [targets, setTargets] = useState<SyncTarget[] | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [link, setLink] = useState(false); // copy by default; link = one shared copy
  const [msg, setMsg] = useState<{ ok: boolean; text: string } | null>(null);
  const confirm = useConfirm();

  const refresh = useCallback(() => {
    api
      .syncTargets(root)
      .then(setTargets)
      .catch(() => setTargets([]));
  }, [root]);
  useEffect(() => {
    refresh();
  }, [refresh]);

  const doSync = async (t: SyncTarget, overwrite: boolean) => {
    if (
      overwrite &&
      !(await confirm({
        title: "Replace existing copy?",
        body: `Replace the existing copy in “${t.label}” with this version?`,
        confirmLabel: "Replace",
        danger: true,
      }))
    )
      return;
    setBusy(t.id);
    setMsg(null);
    try {
      const r = await api.syncSkill(root, t.id, overwrite, link);
      setMsg({ ok: true, text: `${r.linked ? "Linked" : "Copied"} to ${t.label}.` });
      refresh();
    } catch (e) {
      setMsg({ ok: false, text: e instanceof Error ? e.message : "Sync failed" });
    } finally {
      setBusy(null);
    }
  };

  if (targets === null) {
    return (
      <p className="flex items-center gap-2 text-sm text-muted">
        <Spinner className="h-3.5 w-3.5" /> Checking…
      </p>
    );
  }
  return (
    <div className="space-y-2.5">
      <p className="text-xs text-muted">
        Make this skill available to your other agents. Most agents read the shared{" "}
        <span className="font-medium text-fg">Agent Skills</span> directory; Claude Code keeps its own.
      </p>

      {/* Copy vs link mode */}
      <div className="inline-flex rounded-md border border-border p-0.5 text-xs">
        {[
          { v: false, label: "Copy", hint: "Independent duplicate" },
          { v: true, label: "Link", hint: "One shared copy — edits sync everywhere" },
        ].map((o) => (
          <button
            key={o.label}
            type="button"
            onClick={() => setLink(o.v)}
            title={o.hint}
            className={`rounded px-2 py-0.5 ${link === o.v ? "bg-action text-action-fg" : "text-muted hover:text-fg"}`}
          >
            {o.label}
          </button>
        ))}
      </div>

      <ul className="space-y-2">
        {targets.map((t) => (
          <li key={t.id} className="flex items-start gap-2 text-sm">
            <span className="mt-1 h-2 w-2 shrink-0 rounded-full" style={{ background: agentColor(t.reaches[0] ?? t.label) }} aria-hidden />
            <span className="min-w-0">
              <span className="block text-fg">{t.label}</span>
              <span className="block text-[0.7rem] text-faint">
                Read by {t.reaches.join(", ")}
                {t.id === "universal" ? " & more" : ""}
              </span>
            </span>
            <span className="ml-auto flex shrink-0 items-center gap-2 pt-0.5">
              {t.isSource ? (
                <span className="text-xs text-faint" title={t.reachedVia ? `Lives in ${t.reachedVia}` : undefined}>
                  Source
                </span>
              ) : t.present ? (
                <>
                  <span className="text-xs text-ok">✓ {t.linked ? "Linked" : "Added"}</span>
                  <button type="button" onClick={() => doSync(t, true)} disabled={busy === t.id} className="text-xs text-faint hover:text-fg">
                    Update
                  </button>
                </>
              ) : (
                <button type="button" onClick={() => doSync(t, false)} disabled={busy === t.id} className={btnGhost}>
                  {busy === t.id ? "…" : link ? "Link" : "Copy"}
                </button>
              )}
            </span>
          </li>
        ))}
      </ul>
      {msg && <p className={`text-xs ${msg.ok ? "text-ok" : "text-danger"}`}>{msg.text}</p>}
    </div>
  );
}

// ---- Version tracking -------------------------------------------------------
// Begin or end this skill's local version history. Personal skills auto-track on
// discovery, so this is mainly the opt-out (and the re-track path after one).
// Opting out deletes the local .git, so it's gated behind a danger confirm and
// the choice is remembered server-side so discovery won't re-create the repo.
// `onChanged` (bumpGit) lets the sidebar's Source Control panel re-read state —
// it shows nothing for an untracked skill, so opting out collapses it away.
function VersionTrackingSection({ root, kind, onChanged }: { root: string; kind: SkillKind; onChanged: () => void }) {
  const [info, setInfo] = useState<GitInfo | null>(null);
  const [loaded, setLoaded] = useState(false);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const confirm = useConfirm();
  const versionable = kind === "personal" || isEditableBundledSkill(root);

  const refresh = useCallback(() => {
    setLoaded(false);
    api
      .gitInfo(root)
      .then(setInfo)
      .catch(() => setInfo(null))
      .finally(() => setLoaded(true));
  }, [root]);
  useEffect(() => {
    refresh();
  }, [refresh]);

  const start = async () => {
    setBusy(true);
    setErr(null);
    try {
      setInfo(await api.gitTrack(root));
      onChanged();
    } catch (e) {
      setErr(e instanceof Error ? e.message : "Couldn’t start tracking");
    } finally {
      setBusy(false);
    }
  };

  const stop = async () => {
    if (
      !(await confirm({
        title: "Stop tracking this skill?",
        body: "This deletes all saved versions and the local version history for this skill. Your current files stay, but past versions can’t be recovered.",
        confirmLabel: "Stop tracking",
        danger: true,
      }))
    )
      return;
    setBusy(true);
    setErr(null);
    try {
      await api.gitUntrack(root);
      setInfo(null); // drop the stale isRepo=true so the recheck shows the spinner, not a momentarily re-enabled "Stop tracking"
      refresh();
      onChanged();
    } catch (e) {
      setErr(e instanceof Error ? e.message : "Couldn’t stop tracking");
    } finally {
      setBusy(false);
    }
  };

  if (!loaded && info === null) {
    return (
      <p className="flex items-center gap-2 text-sm text-muted">
        <Spinner className="h-3.5 w-3.5" /> Checking…
      </p>
    );
  }
  if (!info || !info.available) {
    return <p className="text-xs text-muted">Git isn’t installed — install git to enable version history.</p>;
  }
  if (info.inParentRepo) {
    return <p className="text-xs text-muted">Tracked by a parent repository — manage its versions there.</p>;
  }
  if (!versionable) {
    return (
      <p className="text-xs text-muted">
        Version history is for your own skills. Use <span className="font-medium text-fg">Sync</span> above to make an
        editable copy you can version.
      </p>
    );
  }

  return (
    <div className="space-y-2">
      {info.isRepo ? (
        <>
          <p className="text-xs text-muted">
            This skill keeps a local version history. Stopping deletes its saved versions — your current files stay,
            but past versions can’t be recovered.
          </p>
          <button
            type="button"
            onClick={stop}
            disabled={busy}
            className="rounded-md border border-danger/40 px-3 py-1.5 text-sm font-medium text-danger transition-colors hover:bg-danger/10 disabled:opacity-40"
          >
            {busy ? "Stopping…" : "Stop tracking"}
          </button>
        </>
      ) : (
        <>
          <p className="text-xs text-muted">Not version-tracked. Start a local history to save versions of this skill.</p>
          <button type="button" onClick={start} disabled={busy} className={btnGhost}>
            {busy ? "Starting…" : "Start tracking"}
          </button>
        </>
      )}
      {err && <p className="text-xs text-danger">{err}</p>}
    </div>
  );
}

// ---- Delete -----------------------------------------------------------------
function DeleteSection({
  root,
  dirName,
  kind,
  onDeleted,
}: {
  root: string;
  dirName: string;
  kind: SkillKind;
  onDeleted: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const confirm = useConfirm();

  const doDelete = async () => {
    const lead =
      kind === "personal"
        ? `Delete “${dirName}”?`
        : `“${dirName}” is a ${KIND_TAG[kind].label.toLowerCase()} skill. Delete it anyway?`;
    if (
      !(await confirm({
        title: lead,
        body: "This permanently removes the skill folder from disk. A synced link is just unlinked; the original isn't touched.",
        confirmLabel: "Delete",
        danger: true,
      }))
    ) {
      return;
    }
    setBusy(true);
    setErr(null);
    try {
      await api.deleteSkill(root);
      onDeleted();
    } catch (e) {
      setErr(e instanceof Error ? e.message : "Delete failed");
      setBusy(false);
    }
  };

  return (
    <div className="space-y-2">
      <p className="text-xs text-muted">Permanently remove this skill from disk. This can’t be undone.</p>
      <button
        type="button"
        onClick={doDelete}
        disabled={busy}
        className="rounded-md border border-danger/40 px-3 py-1.5 text-sm font-medium text-danger transition-colors hover:bg-danger/10 disabled:opacity-40"
      >
        {busy ? "Deleting…" : "Delete skill"}
      </button>
      {err && <p className="text-xs text-danger">{err}</p>}
    </div>
  );
}

// ---- Secrets (this skill's referenced env vars) -----------------------------
// `declared` is the skill's reconciled metadata.required-env (detected on save).
// We cross-check it against the store so the panel shows, at a glance, which of
// this skill's secrets are set and which are still missing — then jump to manage.
// Values never travel with the repo, so "Export .env" is the explicit hand-off
// for collaborators: a plain-text file the user shares over a channel they trust.
function SecretsSection({ root, declared, onOpen }: { root: string; declared: string[]; onOpen: () => void }) {
  const [stored, setStored] = useState<Set<string> | null>(null);
  useEffect(() => {
    api
      .secretsList()
      .then((ls) => setStored(new Set(ls.map((s) => s.key))))
      .catch(() => setStored(new Set<string>()));
  }, []);

  const missing = stored ? declared.filter((k) => !stored.has(k)) : [];
  const exportable = stored ? declared.filter((k) => stored.has(k)) : [];

  return (
    <div className="space-y-2.5">
      {declared.length === 0 ? (
        <p className="text-xs text-muted">
          This skill doesn’t reference any secrets yet. The env vars it uses are detected on save and shown here.
        </p>
      ) : (
        <>
          <p className="text-xs text-muted">
            This skill uses {declared.length} secret{declared.length === 1 ? "" : "s"}
            {stored &&
              (missing.length === 0 ? " — all set in your store." : `, ${missing.length} not in your store yet.`)}
          </p>
          <ul className="flex flex-wrap gap-1.5">
            {declared.map((k) => {
              const known = stored !== null;
              const have = stored?.has(k) ?? false;
              return (
                <li
                  key={k}
                  title={!known ? "Checking your store…" : have ? "In your store" : "Not in your store — add it"}
                  className={`flex items-center gap-1 rounded-full border px-2 py-0.5 font-mono text-[0.7rem] ${
                    !known ? "border-border text-faint" : have ? "border-ok/40 text-ok" : "border-warn/40 text-warn"
                  }`}
                >
                  {k}
                  {known && have ? " ✓" : ""}
                </li>
              );
            })}
          </ul>
        </>
      )}
      <div className="flex flex-wrap items-center gap-2">
        <button type="button" onClick={onOpen} className={btnGhost}>
          Open Credentials →
        </button>
        {exportable.length > 0 && (
          <button
            type="button"
            onClick={() => api.downloadEnv(root, exportable)}
            className={btnGhost}
            title="Download this skill's secrets as a .env file"
          >
            Export .env
          </button>
        )}
      </div>
      {exportable.length > 0 && (
        <p className="text-[0.7rem] text-faint">
          The .env is plain text — share it over a channel you trust, and never commit it. A teammate imports
          it on the Credentials page.
        </p>
      )}
    </div>
  );
}

export default function ManagePanel({
  root,
  dirName,
  kind,
  declared,
  onClose,
  onDeleted,
}: {
  root: string;
  dirName: string;
  kind: SkillKind;
  /** Env vars this skill declares it needs (its reconciled metadata.required-env). */
  declared: string[];
  onClose: () => void;
  /** Called after the skill folder is deleted, so the host can navigate away. */
  onDeleted: () => void;
}) {
  const navigate = useNavigate();
  const { bumpGit } = useStudio();
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  return (
    <div className="fixed inset-0 z-50 flex justify-end bg-black/40" onClick={onClose}>
      <div
        className="flex h-full w-full max-w-md flex-col overflow-hidden border-l border-border bg-surface shadow-xl"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-center gap-2 border-b border-border px-5 py-3">
          <span className="text-sm font-semibold text-fg">Manage</span>
          <span className="truncate font-mono text-xs text-faint">{dirName}</span>
          <button type="button" onClick={onClose} aria-label="Close" className="ml-auto rounded-md p-1 text-faint hover:bg-panel hover:text-fg">
            ✕
          </button>
        </div>

        <div className="min-h-0 flex-1 overflow-auto">
          <Section title="Secrets">
            <SecretsSection
              root={root}
              declared={declared}
              onOpen={() => {
                onClose();
                navigate(credentialsPath());
              }}
            />
          </Section>
          <Section title="Sync to another agent">
            <SyncSection root={root} />
          </Section>
          <Section title="Version tracking">
            <VersionTrackingSection root={root} kind={kind} onChanged={bumpGit} />
          </Section>
          <Section title="Delete">
            <DeleteSection root={root} dirName={dirName} kind={kind} onDeleted={onDeleted} />
          </Section>
        </div>
      </div>
    </div>
  );
}
