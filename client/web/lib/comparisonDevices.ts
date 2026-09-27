import { useSyncExternalStore } from "react";
import * as api from "./api";
import chromeDevices from "./comparisonDevices.generated.json";

let snapshot: api.ComparisonDeviceCatalog = {
  ...chromeDevices, devices: chromeDevices.devices as api.ComparisonDevice[], refreshing: false,
};
const listeners = new Set<() => void>();
let timer: ReturnType<typeof setTimeout> | null = null;
let reading = false;
let syncing = false;
let generation = 0;

export const comparisonDevicesSnapshot = () => snapshot;

function publish(next: api.ComparisonDeviceCatalog) {
  snapshot = next;
  for (const listener of listeners) listener();
}

function pollPendingSync() {
  // This timer only observes a sync explicitly requested by a user. No periodic
  // check, network retry, or background fetch is scheduled after it finishes.
  if (timer !== null || !listeners.size || !snapshot.refreshing || syncing) return;
  timer = setTimeout(() => { timer = null; void readCached(); }, 1000);
}

async function readCached() {
  if (reading) return;
  reading = true;
  const version = generation;
  try {
    const next = await api.comparisonDevices();
    if (version === generation && !syncing) publish(next);
  } catch {
    // Initial reads retain offline data quietly. A failed observation of a
    // requested sync stops polling; the user can explicitly retry from the picker.
    if (version === generation && snapshot.refreshing && !syncing) {
      publish({ ...snapshot, refreshing: false, error: "Could not read the Chrome sync result. Using the saved device list." });
    }
  } finally {
    reading = false;
    pollPendingSync();
  }
}

/** The only path that asks the server to contact Chrome. */
export async function syncComparisonDevices(): Promise<void> {
  if (syncing || snapshot.refreshing) return;
  syncing = true;
  generation++;
  if (timer !== null) { clearTimeout(timer); timer = null; }
  publish({ ...snapshot, refreshing: true, error: null });
  try {
    publish(await api.comparisonDevicesRefresh());
  } catch (error) {
    publish({ ...snapshot, refreshing: false, error: error instanceof Error ? error.message : "Could not sync from Chrome. Using the saved device list." });
  } finally {
    syncing = false;
    pollPendingSync();
  }
}

export function subscribeComparisonDevices(listener: () => void): () => void {
  listeners.add(listener);
  if (listeners.size === 1) void readCached();
  return () => {
    listeners.delete(listener);
    if (!listeners.size && timer !== null) { clearTimeout(timer); timer = null; }
  };
}

export function useComparisonDevices(): api.ComparisonDeviceCatalog {
  return useSyncExternalStore(subscribeComparisonDevices, comparisonDevicesSnapshot, comparisonDevicesSnapshot);
}
