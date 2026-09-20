"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { clearEditorStatus, isAutosaveHeld, publishEditorStatus } from "@/lib/editorState";

export type SaveStatus = "saved" | "dirty" | "saving" | "error";

/** Idle delay before a pause in typing is flushed to disk. */
const AUTOSAVE_DELAY = 800;

/**
 * Notion-style autosave for a derived string `value`. The user never saves by
 * hand: edits flush to disk on their own a beat after typing stops, on blur, and
 * on unmount (navigating away). There's no Save button and no unsaved-changes
 * prompt — only a *failure* surfaces (via the shared editor store) so a dropped
 * write is never silent.
 *
 * Guarantees:
 * - Writes never overlap (one in flight at a time) and the LATEST value always
 *   wins — edits made mid-write are re-saved on completion; the unmount flush is
 *   chained after any in-flight write so it can't clobber with stale content.
 * - A failed write stops the retry loop (no storm) until the next edit, while
 *   leaving `error` set so the chrome can offer a manual retry.
 *
 * `onSaved` fires after each successful write (fire-and-forget) — the hook the
 * host uses to run the post-save pipeline.
 */
export function useAutosave(
  value: string,
  save: (value: string) => Promise<void>,
  enabled = true,
  onSaved?: () => void,
) {
  const [savedValue, setSavedValue] = useState(value);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const dirty = enabled && value !== savedValue;

  // Live refs so the stable runSave/listeners always see current values. Synced
  // in an effect (React forbids writing refs during render). `inFlightRef` (the
  // actual pending write promise) is the single source of truth for "a write is
  // happening"; `savedRef` is ALSO written imperatively on success so a chained
  // re-run sees the just-saved value before the next render commits.
  const valueRef = useRef(value);
  const savedRef = useRef(savedValue);
  const enabledRef = useRef(enabled);
  const saveImplRef = useRef(save);
  const onSavedRef = useRef(onSaved);
  const inFlightRef = useRef<Promise<void> | null>(null);
  useEffect(() => {
    valueRef.current = value;
    savedRef.current = savedValue;
    enabledRef.current = enabled;
    saveImplRef.current = save;
    onSavedRef.current = onSaved;
  });

  const runSave = useCallback(() => {
    if (!enabledRef.current || isAutosaveHeld() || inFlightRef.current) return; // a write is in flight → it re-runs on completion
    const v = valueRef.current;
    if (v === savedRef.current) return; // nothing to save
    setSaving(true);
    const p = Promise.resolve(saveImplRef.current(v))
      .then(() => {
        setSavedValue(v);
        savedRef.current = v; // so the re-run check below sees it immediately
        setError(null);
        // Fire the post-save pipeline without blocking (it may reload the skill);
        // a clean save means value === savedValue, so it's safe.
        onSavedRef.current?.();
        // Edits arrived while we were writing → save the newest value too. The
        // re-run is queued here but runs AFTER .finally clears inFlightRef below,
        // so its in-flight guard passes.
        if (valueRef.current !== savedRef.current) queueMicrotask(runSave);
      })
      .catch((e) => setError(e instanceof Error ? e.message : "Save failed"))
      .finally(() => {
        setSaving(false);
        if (inFlightRef.current === p) inFlightRef.current = null;
      });
    inFlightRef.current = p;
  }, []);

  // Awaitable flush: persist the latest buffer and resolve once it's on disk.
  // Chained after any in-flight write so the newest content lands last. Used
  // before a deliberate working-tree swap (version preview) so pending edits are
  // saved — and thus stashed — rather than dropped when the editor remounts.
  const flush = useCallback(async () => {
    while (enabledRef.current) {
      const pending = inFlightRef.current;
      if (pending) {
        await pending.catch(() => {});
        continue;
      }
      if (valueRef.current === savedRef.current) return;
      if (isAutosaveHeld()) throw new Error("Wait for the current file operation to finish, then try again.");
      const v = valueRef.current;
      setSaving(true);
      const p = Promise.resolve()
        .then(() => saveImplRef.current(v))
        .then(() => {
          setSavedValue(v);
          savedRef.current = v;
          setError(null);
          onSavedRef.current?.();
        })
        .catch((e) => {
          setError(e instanceof Error ? e.message : "Save failed");
          throw e;
        })
        .finally(() => {
          setSaving(false);
          if (inFlightRef.current === p) inFlightRef.current = null;
        });
      inFlightRef.current = p;
      await p;
      // Edits can arrive while a save is in flight. An intentional restart must
      // wait for the latest buffer, not just the value captured before that save.
    }
  }, []);

  // A pending error is moot once the buffer matches disk again (e.g. the user
  // undid back to the last good state) — clear it so a dead "retry" doesn't linger.
  useEffect(() => {
    if (error && value === savedValue) setError(null);
  }, [error, value, savedValue]);

  // Debounced autosave: each edit (re)arms the timer, so the write lands a beat
  // after typing stops. A clean buffer schedules nothing. Errors don't reschedule
  // here (value is unchanged) — the next edit retries, so a failing backend never
  // spins.
  useEffect(() => {
    if (!enabled || value === savedValue) return;
    const t = setTimeout(runSave, AUTOSAVE_DELAY);
    return () => clearTimeout(t);
  }, [value, savedValue, enabled, runSave]);

  // Save promptly when the window loses focus or the tab is hidden (switch away /
  // closing) — both fire while the page is still alive (unlike beforeunload), so
  // the write completes. Cheap insurance against the debounce window on top of the
  // per-edit timer.
  useEffect(() => {
    if (!enabled) return;
    const flush = () => runSave();
    const onVisibility = () => document.visibilityState === "hidden" && runSave();
    window.addEventListener("blur", flush);
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      window.removeEventListener("blur", flush);
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [enabled, runSave]);

  // Publish state to the shared store (drives the failure indicator + lets other
  // panels refresh when a write lands; `runSave` is the manual-retry handle).
  useEffect(() => {
    if (enabled) publishEditorStatus({ present: true, dirty, saving, error }, runSave, flush);
    else clearEditorStatus();
  }, [enabled, dirty, saving, error, runSave, flush]);

  // Flush the final buffer on unmount (e.g. navigating to another file). Chained
  // after any in-flight write so the latest content lands LAST and can't be
  // clobbered by an older write completing after it.
  useEffect(
    () => () => {
      // Skip while autosave is held: the working tree is being swapped under us
      // (version preview), so flushing this now-stale buffer would clobber it.
      if (enabledRef.current && !isAutosaveHeld() && valueRef.current !== savedRef.current) {
        const v = valueRef.current;
        const prev = inFlightRef.current ?? Promise.resolve();
        prev
          .then(() => saveImplRef.current(v))
          .then(() => onSavedRef.current?.())
          .catch(() => {});
      }
      clearEditorStatus();
    },
    [],
  );

  // Best-effort flush before a hard tab close / reload; warn only if a save is
  // actually failing (otherwise stay silent — autosave has it covered).
  useEffect(() => {
    const onBeforeUnload = (e: BeforeUnloadEvent) => {
      if (enabledRef.current && !isAutosaveHeld() && valueRef.current !== savedRef.current) {
        void Promise.resolve(saveImplRef.current(valueRef.current)).catch(() => {});
      }
      if (error) {
        e.preventDefault();
        e.returnValue = "";
      }
    };
    window.addEventListener("beforeunload", onBeforeUnload);
    return () => window.removeEventListener("beforeunload", onBeforeUnload);
  }, [error]);

  // Adopt `v` as the on-disk baseline WITHOUT writing — used when the host swaps in
  // an externally-changed version it just read from disk, so autosave sees a clean
  // buffer instead of trying to write that just-read content straight back.
  const markClean = useCallback((v: string) => {
    setSavedValue(v);
    savedRef.current = v;
    setError(null);
  }, []);

  const status: SaveStatus = error ? "error" : saving ? "saving" : dirty ? "dirty" : "saved";
  return { status, dirty, saving, error, save: runSave, markClean };
}
