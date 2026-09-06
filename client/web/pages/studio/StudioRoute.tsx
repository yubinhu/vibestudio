import { useCallback, useEffect, useRef, useState } from "react";
import { useNavigate, useParams } from "react-router-dom";
import { Spinner } from "@/components/ui";
import NavBar from "@/components/NavBar";
import { addRecent } from "@/lib/recents";
import { skillKind } from "@/lib/agents";
import { loadSkill, gitInfo, gitEnterVersion, gitExitVersion, gitKeepVersion } from "@/lib/api";
import type { SkillData } from "@/lib/types";
import { reconcileRequiredEnv, runSaveHooks } from "./saveHooks";
import { isEditorDirty, flushEditor, holdAutosave, releaseAutosave } from "@/lib/editorState";
import { StudioProvider, skillName, type VersionPreview } from "./StudioContext";
import { useEagerCommitDraft } from "./useEagerCommitDraft";
import StudioLayout from "./StudioLayout";

/** Idle delay before the post-save pipeline (reconcile) runs, so a burst of
 *  autosaves coalesces into one reconcile instead of one per keystroke pause. */
const HOOK_DELAY = 2000;

/**
 * Route element for `/studio/:root`. Owns the loaded skill: it loads (and
 * reconciles required-env) on every `:root` change, holds the post-save pipeline,
 * provides StudioContext, and renders the layout (or a loading / error shell). The
 * nested file routes render inside the layout's Outlet.
 */
export function Component() {
  const root = useParams().root!; // already decoded by the router
  const [data, setData] = useState<SkillData | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [docVersion, setDocVersion] = useState(0);
  const [gitVersion, setGitVersion] = useState(0);
  // Non-null while viewing a past version (its content is checked out into the
  // working tree). Reset on every skill change; re-detected below for recovery.
  const [preview, setPreview] = useState<VersionPreview | null>(null);

  // Live `data` for async callbacks that may resolve after a navigation.
  const dataRef = useRef<SkillData | null>(null);
  useEffect(() => {
    dataRef.current = data;
  });
  // Serialize version transitions so two concurrent ones (e.g. rapid version
  // clicks) can't interleave git stash/checkout commands on the same repo.
  const transitionRef = useRef(false);

  // (Re)load whenever the routed skill root changes.
  useEffect(() => {
    let cancelled = false;
    setError(null);
    setData(null);
    setPreview(null);
    (async () => {
      try {
        // Reconcile required-env on open so the declaration is current before edits.
        const { data: sd } = await reconcileRequiredEnv(root);
        if (cancelled) return;
        setData(sd);
        addRecent({ root: sd.root, name: skillName(sd) });
        // Recovery: a skill left mid-preview (detached HEAD, e.g. after a reload)
        // has an old version as its working tree — surface the banner so the user
        // can return to current. Best-effort; a non-preview detach still offers it.
        void gitInfo(sd.root)
          .then((info) => {
            if (cancelled || transitionRef.current) return;
            // Only surface the recovery banner if we don't ALREADY have a real
            // preview — a version click can win this race, and its {sha, number}
            // must not be clobbered by this generic {sha:"", number:0}.
            if (info.isRepo && !info.branch) setPreview((prev) => prev ?? { sha: "", number: 0 });
          })
          .catch(() => {});
      } catch (e) {
        if (cancelled) return;
        setError(e instanceof Error ? e.message : "Failed to load skill");
      }
    })();
    return () => {
      cancelled = true;
      if (hookTimer.current) clearTimeout(hookTimer.current); // drop a pending reconcile for the old skill
    };
  }, [root]);

  // Post-save pipeline. Autosave fires often, so we DEBOUNCE the (relatively
  // heavy) reconcile — it scans the skill's files and may rewrite SKILL.md — to
  // run once edits settle rather than on every keystroke pause. When a hook
  // rewrote the skill (same root only) we swap the reloaded data in and remount
  // the editor (docVersion bump) only when it isn't mid-edit, so keystrokes typed
  // after the save aren't dropped. `saveSeq` drops a stale reconcile if a newer
  // one started before it resolved.
  const saveSeq = useRef(0);
  const hookTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const runPipeline = useCallback(async (rel: string | null) => {
    const cur = dataRef.current;
    if (!cur) return;
    const seq = ++saveSeq.current;
    const effects = await runSaveHooks({ root: cur.root, kind: skillKind(cur.root).kind, rel });
    if (seq !== saveSeq.current) return; // a newer save superseded this one
    let reloaded: SkillData | undefined;
    for (const e of effects) if (e.reloaded) reloaded = e.reloaded;
    if (reloaded && dataRef.current?.root === reloaded.root) {
      setData(reloaded);
      dataRef.current = reloaded;
      if (!isEditorDirty()) setDocVersion((v) => v + 1);
    }
  }, []);
  const afterSave = useCallback(
    (rel: string | null) => {
      if (hookTimer.current) clearTimeout(hookTimer.current);
      hookTimer.current = setTimeout(() => void runPipeline(rel), HOOK_DELAY);
    },
    [runPipeline],
  );

  const bumpGit = useCallback(() => setGitVersion((v) => v + 1), []);

  // Eagerly draft a commit message in the background once edits settle, so the
  // Save dialog opens with it ready (no ~10s wait). Keyed off the routed root,
  // which is always present — so this hook runs above the loading/error returns.
  useEagerCommitDraft(root);

  // Git changed on disk from within the app (commit / discard / version swap).
  // Bump gitVersion so open diff overlays refetch their HEAD baseline; re-read the
  // skill so the editor reflects the new content, remounting it (docVersion) when
  // not mid-edit — or always when `force` (the working tree was deliberately
  // swapped, so the in-memory buffer is stale and must be replaced).
  const reloadAsync = useCallback(async (force = false) => {
    const cur = dataRef.current;
    if (!cur) return;
    setGitVersion((v) => v + 1);
    try {
      const sd = await loadSkill(cur.root);
      if (dataRef.current?.root !== sd.root) return;
      setData(sd);
      dataRef.current = sd;
      if (force || !isEditorDirty()) setDocVersion((v) => v + 1);
    } catch {
      /* a transient read failure just leaves the current data in place */
    }
  }, []);
  const reload = useCallback((force = false) => void reloadAsync(force), [reloadAsync]);

  // Soft refresh after an out-of-band new file (a pasted/dropped asset): re-read
  // the skill and patch ONLY the file tree + counts into the live `data`, then
  // bump gitVersion so the source-control panel sees the new untracked change.
  // Deliberately NEVER touches docVersion (the open buffer is mid-edit and must
  // not remount) and NEVER overwrites raw/frontmatter/body/validation — the editor
  // owns those, and the disk SKILL.md can lag the unsaved buffer, so swapping them
  // in would clobber pending edits. Only `files`/`tree`/`stats` come from disk.
  const refreshData = useCallback(() => {
    const cur = dataRef.current;
    if (!cur) return;
    void loadSkill(cur.root)
      .then((sd) => {
        const prev = dataRef.current;
        if (!prev || prev.root !== sd.root) return;
        const merged: SkillData = {
          ...prev,
          tree: sd.tree,
          files: sd.files,
          stats: {
            ...prev.stats,
            fileCount: sd.stats.fileCount,
            dirCount: sd.stats.dirCount,
            totalBytes: sd.stats.totalBytes,
          },
        };
        setData(merged);
        dataRef.current = merged;
        setGitVersion((v) => v + 1);
      })
      .catch(() => {
        /* a transient read failure just leaves the current data in place */
      });
  }, []);

  // ---- version preview: view/edit a past version through the full editor -----
  // Each transition swaps the working tree under the mounted editor, so we (1)
  // flush pending edits to disk first (they get stashed, not lost), (2) hold
  // autosave so the remount's unmount-flush can't clobber the freshly checked-out
  // version, and (3) release after the remount commits (deferred a tick).
  const enterVersion = useCallback(
    async (sha: string, number: number) => {
      const cur = dataRef.current;
      if (!cur || transitionRef.current) return; // drop a click while one is in flight
      transitionRef.current = true;
      await flushEditor();
      holdAutosave();
      try {
        await gitEnterVersion(cur.root, sha);
        setPreview({ sha, number });
        await reloadAsync(true);
      } finally {
        transitionRef.current = false;
        setTimeout(releaseAutosave, 0);
      }
    },
    [reloadAsync],
  );
  const exitVersion = useCallback(async () => {
    const cur = dataRef.current;
    if (!cur || transitionRef.current) return;
    transitionRef.current = true;
    holdAutosave();
    try {
      await gitExitVersion(cur.root);
      setPreview(null);
      await reloadAsync(true);
    } finally {
      transitionRef.current = false;
      setTimeout(releaseAutosave, 0);
    }
  }, [reloadAsync]);
  const keepVersion = useCallback(
    async (message: string) => {
      const cur = dataRef.current;
      if (!cur) return;
      // Share the transition lock with enter/exit so a version action can't
      // interleave its git mutations with this save. Throw (not silently return)
      // so doCommit surfaces it instead of reporting a phantom success.
      if (transitionRef.current) throw new Error("Another version action is in progress — try again.");
      transitionRef.current = true;
      await flushEditor();
      holdAutosave();
      try {
        await gitKeepVersion(cur.root, message);
        setPreview(null);
        await reloadAsync(true);
      } finally {
        transitionRef.current = false;
        setTimeout(releaseAutosave, 0);
      }
    },
    [reloadAsync],
  );

  if (error) return <SkillErrorShell root={root} message={error} />;
  if (!data) return <SkillLoadingShell />;
  return (
    <StudioProvider
      value={{ data, docVersion, gitVersion, bumpGit, afterSave, reload, refreshData, preview, enterVersion, exitVersion, keepVersion }}
    >
      <StudioLayout />
    </StudioProvider>
  );
}

function SkillLoadingShell() {
  return (
    <div className="flex h-dvh flex-col bg-app text-fg">
      <NavBar />
      <div role="status" aria-live="polite" className="flex flex-1 items-center justify-center text-muted">
        <Spinner /> <span className="ml-2">Loading skill…</span>
      </div>
    </div>
  );
}

function SkillErrorShell({ root, message }: { root: string; message: string }) {
  const navigate = useNavigate();
  return (
    <div className="flex h-dvh flex-col bg-app text-fg">
      <NavBar />
      <div className="flex flex-1 flex-col items-center justify-center gap-3 px-6 text-center">
        <p className="text-sm text-danger">{message}</p>
        <p className="max-w-md break-all font-mono text-xs text-faint">{root}</p>
        <button
          type="button"
          onClick={() => navigate("/")}
          className="rounded-md bg-action px-3 py-1.5 text-sm font-medium text-action-fg hover:opacity-90"
        >
          Back to home
        </button>
      </div>
    </div>
  );
}
