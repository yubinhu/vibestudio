// App-update state for the UpdateBanner: one module-level store polling
// GET /api/update/status — a slow heartbeat at rest, fast while a download or
// restart is in flight. Same external-store idiom as lib/remote.ts. A 404 means
// this server has no updater (browser dev / remote binary) — stop for good.
import { useSyncExternalStore } from "react";
import { updateStatus, updateApply, type UpdateStatus } from "@/lib/api";
import { flushEditor } from "./editorState";

type UpdateSnapshot = Omit<UpdateStatus, "phase"> & { phase: UpdateStatus["phase"] | "saving" };
let status: UpdateSnapshot | null = null;
let unsupported = false;
let bootstrapped = false;
let timer: ReturnType<typeof setTimeout> | null = null;
const listeners = new Set<() => void>();
let refreshSequence = 0;

const IDLE_POLL_MS = 30 * 60 * 1000;
const ACTIVE_POLL_MS = 1500;

function emit() {
  for (const l of listeners) l();
}

function schedule(ms: number) {
  if (timer) clearTimeout(timer);
  timer = unsupported ? null : setTimeout(() => void refresh(), ms);
}

async function refresh(): Promise<void> {
  if (status?.phase === "saving") return;
  const sequence = ++refreshSequence;
  try {
    const next = await updateStatus();
    if (sequence !== refreshSequence) return;
    status = next;
    emit();
  } catch (e) {
    if (sequence !== refreshSequence) return;
    if ((e as { status?: number } | undefined)?.status === 404) {
      unsupported = true;
      status = null;
      emit();
      return;
    }
    // Mid-update the server restarts (or hiccups) and goes briefly
    // unreachable — keep the last in-flight status so the banner keeps showing
    // "Downloading…"/"Restarting…" on the fast poll until the server answers.
    // Any other failure hides the banner until it returns.
    if (status?.phase !== "ready" && status?.phase !== "downloading") {
      status = null;
      emit();
    }
  }
  const active = status?.phase === "downloading" || status?.phase === "ready";
  // The server's own first feed check runs ~5s after launch, after our first
  // fetch — one early follow-up so a fresh update shows in seconds, not after
  // the idle cadence.
  const followUp = !bootstrapped && !status?.available;
  bootstrapped = true;
  schedule(active ? ACTIVE_POLL_MS : followUp ? 20_000 : IDLE_POLL_MS);
}

/** Only the persistent-terminal accident guard is bypassed during an intentional
 * update. Editors retain their independent protection against failed saves. */
export function isUpdateInProgress(): boolean {
  return !!status?.canAuto && (status.phase === "downloading" || status.phase === "ready");
}

/** Persist the current buffer before requesting a restart, then track the native
 * install. A failed save leaves the app open and never starts the updater. */
export async function applyUpdate(): Promise<void> {
  if (!status?.canAuto || !status.available || status.phase === "saving" || isUpdateInProgress()) return;
  ++refreshSequence; // an older idle poll cannot erase the local save/download state
  status = { ...status, phase: "saving", error: null };
  emit();
  try {
    await flushEditor();
  } catch (e) {
    status = { ...status, phase: "error", progress: null,
      error: `Your changes couldn't be saved, so the update has not started. ${e instanceof Error ? e.message : "Try saving again."}` };
    emit();
    schedule(IDLE_POLL_MS);
    return;
  }
  status = { ...status, phase: "downloading", progress: null, error: null };
  emit();
  let requestError: unknown;
  try {
    await updateApply();
  } catch (e) {
    if ((e as { status?: number } | null)?.status) {
      // An HTTP rejection proves the request did not start an update. Restore
      // ordinary unload protection even if the following status poll would fail.
      status = { ...status, phase: "error", progress: null,
        error: e instanceof Error ? e.message : "The update could not start." };
      emit();
      schedule(IDLE_POLL_MS);
      return;
    }
    requestError = e;
  }
  await refresh();
  if (requestError && status && !isUpdateInProgress() && status.phase !== "error") {
    status = { ...status, phase: "error", error: requestError instanceof Error ? requestError.message : "The update could not start." };
    emit();
  }
}

/** The latest update status; null until the first fetch lands (or unsupported). */
export function useUpdate(): UpdateSnapshot | null {
  return useSyncExternalStore(
    (cb) => {
      listeners.add(cb);
      return () => listeners.delete(cb);
    },
    () => status,
    () => status,
  );
}

// First check waits out app startup; after that the schedule above takes over.
schedule(3000);
