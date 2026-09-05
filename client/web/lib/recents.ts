"use client";

import { useEffect, useSyncExternalStore } from "react";
import * as api from "./api";

export type { Recent } from "./api";
type Recent = api.Recent;

// The active server owns history; a connection change reloads the SPA. Serialize
// requests so fast open/remove actions reach the server in the user's order.
const listeners = new Set<() => void>();
let cache: Recent[] = [];
let status: { loading: boolean; error: string | null } = { loading: true, error: null };
let queue = Promise.resolve();
let revision = 0;
let pendingMutations = 0;
let refreshing = false;

function emit() { for (const l of listeners) l(); }
function subscribe(cb: () => void) {
  listeners.add(cb);
  return () => { listeners.delete(cb); };
}
function enqueue(task: () => Promise<void>) {
  queue = queue.then(task).catch(() => {
    status = { loading: false, error: "Recent history could not be saved. Try refreshing when the server is available." };
    emit();
  });
}

async function migrateLegacy(items: Recent[]): Promise<Recent[]> {
  const key = "skillviewer-recents";
  try {
    if (!["localhost", "127.0.0.1", "[::1]"].includes(location.hostname)) return items;
    const raw = localStorage.getItem(key);
    if (!raw || (await api.remoteStatus()).state !== "idle") return items;
    const old: unknown = JSON.parse(raw);
    if (!Array.isArray(old)) return items;
    // Never replay an old entry over an entry already opened on the server.
    const existing = new Set(items.map((r) => r.root));
    for (const r of [...old].reverse()) {
      if (!r || typeof r.root !== "string" || existing.has(r.root)) continue;
      items = await api.recentsAdd({ root: r.root, name: typeof r.name === "string" ? r.name : r.root, kind: r.kind === "markdown" ? "markdown" : "skill" });
    }
    localStorage.removeItem(key);
  } catch {
    // Keep legacy history for a later attempt if migration was interrupted.
  }
  return items;
}

export function refreshRecents() {
  if (refreshing) return;
  refreshing = true;
  enqueue(async () => {
    try {
      const items = await migrateLegacy(await api.recentsList());
      // A cold-start GET must not erase a newer optimistic open/remove.
      if (!pendingMutations) cache = items;
      status = { loading: false, error: null };
    } catch {
      status = { loading: false, error: "Recent history is unavailable. Check the server connection and try again." };
    } finally {
      refreshing = false;
      emit();
    }
  });
}

function mutate(call: () => Promise<Recent[]>) {
  const id = ++revision;
  pendingMutations++;
  emit();
  enqueue(async () => {
    try {
      const items = await call();
      if (id === revision) cache = items;
      status = { loading: false, error: null };
    } finally {
      pendingMutations--;
      emit();
    }
  });
}

export function addRecent(r: Recent) {
  cache = [{ ...r, openedAt: Math.floor(Date.now() / 1000) }, ...cache.filter((x) => x.root !== r.root)].slice(0, 30);
  mutate(() => api.recentsAdd(r));
}

export function removeRecent(root: string) {
  cache = cache.filter((x) => x.root !== root);
  mutate(() => api.recentsRemove(root));
}

export function useRecents(): Recent[] {
  useEffect(() => {
    refreshRecents();
    // Each mounted consumer owns its listener, so closing the history dialog
    // cannot remove the navbar/gallery's focus refresh registration.
    const onFocus = () => refreshRecents();
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, []);
  return useSyncExternalStore(subscribe, () => cache, () => cache);
}

export function useRecentsStatus() {
  return useSyncExternalStore(subscribe, () => status, () => status);
}

refreshRecents();
