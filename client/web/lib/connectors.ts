import { useCallback, useSyncExternalStore } from "react";
import * as api from "./api";

export interface ConnectorsSnapshot {
  inventory: api.ConnectorInventory | null;
  loading: boolean;
  refreshing: boolean;
  checking: boolean;
  error: string | null;
  /** Time of the last completed runtime check, retained through passive reads. */
  checkedAt: number | null;
}

interface Store {
  snapshot: ConnectorsSnapshot;
  listeners: Set<() => void>;
  inflight: Promise<void> | null;
  fetchedAt: number;
  queued: "refresh" | "check" | null;
  revision: number;
}

// Project scopes never share results. Switching active servers reloads the SPA,
// just as for skills/recents; revisiting Home shares only the global inventory.
const stores = new Map<string, Store>();
const STALE_MS = 30_000;
function storeFor(project?: string): Store {
  const key = project ?? "";
  let store = stores.get(key);
  if (!store) {
    store = {
      snapshot: { inventory: null, loading: true, refreshing: false, checking: false, error: null, checkedAt: null },
      listeners: new Set(),
      inflight: null,
      fetchedAt: 0,
      queued: null,
      revision: 0,
    };
    stores.set(key, store);
  }
  return store;
}

function update(store: Store, patch: Partial<ConnectorsSnapshot>) {
  store.snapshot = { ...store.snapshot, ...patch };
  for (const listener of store.listeners) listener();
}

/** Passive reads cannot rediscover account-only connectors. Retain runtime
 * observations with their original checkedAt until another explicit check.
 * Source provenance, rather than agent names, makes this work for new agents. */
function retainRuntime(fresh: api.ConnectorInventory, previous: api.ConnectorInventory | null): api.ConnectorInventory {
  if (!previous) return fresh;
  const runtimeSources = previous.sources.filter((source) => source.discovery === "runtime");
  const runtimeIds = new Set(runtimeSources.map((source) => source.id));
  if (!runtimeIds.size) return fresh;
  const connectors = fresh.connectors.map((connector) => ({ ...connector, availability: [...connector.availability] }));
  for (const old of previous.connectors) {
    const observed = old.availability.filter((access) => runtimeIds.has(access.sourceId));
    if (!observed.length) continue;
    const matching = connectors.find((connector) => connector.id === old.id);
    if (matching) {
      matching.availability = matching.availability.filter((access) => !runtimeIds.has(access.sourceId)).concat(observed);
    } else connectors.push({ ...old, availability: observed });
  }
  const agents = fresh.agents.map((agent) => ({ ...agent, clients: [...agent.clients] }));
  for (const old of previous.agents) {
    const matching = agents.find((agent) => agent.id === old.id);
    if (matching) matching.clients = [...new Set([...matching.clients, ...old.clients])];
    else if (connectors.some((connector) => connector.availability.some((access) => access.agentId === old.id))) agents.push(old);
  }
  return { ...fresh, connectors, agents, sources: fresh.sources.filter((source) => !runtimeIds.has(source.id)).concat(runtimeSources) };
}

function load(project: string | undefined, runtime: boolean): Promise<void> {
  const store = storeFor(project);
  // Explicit work arriving mid-read gets one trailing pass. A runtime check
  // subsumes a queued passive refresh; duplicate checks coalesce. The same
  // promise includes the trailing pass so mutation callers await fresh data.
  if (store.inflight) {
    if (runtime && !store.snapshot.checking) store.queued = "check";
    else if (!runtime && store.queued !== "check") store.queued = "refresh";
    return store.inflight;
  }
  update(store, { refreshing: true, checking: runtime, error: null });
  store.inflight = Promise.resolve().then(async () => {
    let mode: "refresh" | "check" | null = runtime ? "check" : "refresh";
    while (mode) {
      const checking = mode === "check";
      const revision = store.revision;
      update(store, { checking, error: null });
      try {
        const result = await (checking ? api.connectorsCheck(project) : api.connectorsDiscover(project));
        if (revision === store.revision) {
          const inventory = checking ? result : retainRuntime(result, store.snapshot.inventory);
          store.fetchedAt = Date.now();
          update(store, { inventory, ...(checking ? { checkedAt: Date.now() } : {}) });
        }
      } catch (error) {
        const detail = (error as { detail?: string; status?: number })?.detail;
        const message = (error as { status?: number })?.status === 404
          ? "This server does not support connector discovery yet. Update the server and refresh."
          : detail ?? (error instanceof Error ? error.message : "Couldn’t discover connectors.");
        if (revision === store.revision) update(store, { error: message });
      }
      mode = store.queued;
      store.queued = null;
    }
  }).finally(() => {
    store.inflight = null;
    update(store, { loading: false, refreshing: false, checking: false });
  });
  return store.inflight;
}

/** Passive configuration refresh; never starts an agent. */
export const refreshConnectors = (project?: string) => load(project, false);
/** Runtime checks are only initiated by an explicit user action. */
export const checkConnectors = (project?: string) => load(project, true);

/** A managed sign-in/deletion changes access across project scopes. Drop old
 * runtime observations and prevent an earlier request from restoring them.
 * Other project caches rescan passively when next visited. */
export function invalidateConnectors() {
  for (const store of stores.values()) {
    store.revision++;
    store.fetchedAt = 0;
    if (store.inflight) store.queued = "refresh";
    const previous = store.snapshot.inventory;
    const runtimeIds = new Set(previous?.sources.filter((source) => source.discovery === "runtime").map((source) => source.id));
    const inventory = previous ? {
      ...previous,
      connectors: previous.connectors.map((connector) => ({ ...connector, availability: connector.availability.filter((access) => !runtimeIds.has(access.sourceId)) }))
        .filter((connector) => connector.availability.length || connector.managedConnectionIds?.length),
      sources: previous.sources.filter((source) => !runtimeIds.has(source.id)),
    } : null;
    update(store, { inventory, checkedAt: null });
  }
}

export function useConnectors(project?: string): ConnectorsSnapshot {
  const store = storeFor(project);
  const subscribe = useCallback((listener: () => void) => {
    store.listeners.add(listener);
    // Refresh configured/account inventory on revisits even after a live check.
    // retainRuntime keeps dated runtime-only evidence without starting agents.
    if (!store.inflight && Date.now() - store.fetchedAt >= STALE_MS) void refreshConnectors(project);
    return () => { store.listeners.delete(listener); };
  }, [project, store]);
  const snapshot = useCallback(() => store.snapshot, [store]);
  return useSyncExternalStore(subscribe, snapshot, snapshot);
}

if (typeof window !== "undefined") window.addEventListener("vibestudio:workspace-restored", () => {
  for (const [project, store] of stores) {
    store.fetchedAt = 0;
    if (store.listeners.size) void refreshConnectors(project || undefined);
  }
});
