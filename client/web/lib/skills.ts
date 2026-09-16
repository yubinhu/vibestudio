// Shared inventory for the home stat card and gallery. The server returns
// installed skills first and searches for project skills in the background;
// poll that snapshot while Home is mounted, without restarting the search.
import { useSyncExternalStore } from "react";
import * as api from "@/lib/api";
import type { AgentSkills, DiscoveredSkill } from "@/lib/api";
import { kindMeta } from "@/lib/agents";

const skillName = (skill: DiscoveredSkill) =>
  skill.name ?? skill.root.split(/[\\/]/).filter(Boolean).pop() ?? skill.root;

/** Keep origin priority; within each origin, show global skills before project
 * skills, then sort each scope by its displayed name. */
export function compareSkills(a: DiscoveredSkill, b: DiscoveredSkill): number {
  return kindMeta(a.kind).rank - kindMeta(b.kind).rank ||
    Number(Boolean(a.project)) - Number(Boolean(b.project)) ||
    skillName(a).localeCompare(skillName(b));
}

// A mount-driven background refresh is skipped when the cache is fresher than
// this — coalescing already dedups a same-tick mount storm; this also spares a
// redundant scan on a quick bounce back to home. An explicit refreshSkills()
// (after accept/discard/delete/mining) always runs regardless.
const STALE_MS = 2000;
const POLL_MS = 1000;

const nowMs = () => Date.now();

export interface SkillsSnap {
  groups: AgentSkills[];
  /** Roots with uncommitted changes (fills in just after `groups`, one batch call). */
  dirtyRoots: Set<string>;
  /** True only until the FIRST successful scan lands — drives the cold-start
   *  spinner. False forever after, so revisits show the cache, never a blank. */
  loading: boolean;
  /** A request or server-side search is in flight — drives the header spinner
   *  without blanking the grid. */
  scanning: boolean;
  /** Total skills across groups (incl. proposed drafts) — the stat card's value. */
  total: number;
}

// ─── store state ───
let groups: AgentSkills[] = [];
let dirtyRoots: Set<string> = new Set();
let loading = true;
let scanning = false;
let fetchedAt = 0;
const listeners = new Set<() => void>();

// Serialize inventory reads. A user action during a read queues one force
// refresh; another subscriber or background poll merely joins the current read.
let inflight: Promise<void> | null = null;
let queuedRefresh = false;
let serverScanning = false;
let pollTimer: ReturnType<typeof setTimeout> | null = null;
let revision = 0;

// Git badges must not hold up the next project-discovery snapshot. Keep at most
// one dirty request in flight and one latest request waiting; stale results can
// never replace badges for a newer inventory.
let checkingDirty = false;
let pendingDirty: { roots: string[]; revision: number } | null = null;

let snap: SkillsSnap = { groups, dirtyRoots, loading, scanning, total: 0 };

function rebuild() {
  const total = groups.reduce((n, g) => n + g.skills.length, 0);
  snap = { groups, dirtyRoots, loading, scanning, total };
  for (const l of listeners) l();
}

function clearPoll() {
  if (pollTimer !== null) clearTimeout(pollTimer);
  pollTimer = null;
}

function schedulePoll() {
  if (!listeners.size || !serverScanning || inflight || pollTimer !== null) return;
  pollTimer = setTimeout(() => {
    pollTimer = null;
    void readSkills();
  }, POLL_MS);
}

function refreshDirty() {
  pendingDirty = {
    roots: [...new Set(groups.flatMap((g) => g.skills.filter((s) => !s.proposed).map((s) => s.root)))],
    revision,
  };
  if (checkingDirty) return;
  checkingDirty = true;
  void (async () => {
    try {
      while (pendingDirty) {
        const next = pendingDirty;
        pendingDirty = null;
        try {
          const states = next.roots.length ? await api.gitDirtyMany(next.roots) : [];
          if (next.revision === revision) {
            dirtyRoots = new Set(states.filter((d) => d.dirty).map((d) => d.root));
            rebuild();
          }
        } catch {
          /* dirty badges are best-effort */
        }
      }
    } finally {
      checkingDirty = false;
    }
  })();
}

function readSkills(refresh = false): Promise<void> {
  if (inflight) {
    if (refresh) queuedRefresh = true;
    return inflight;
  }
  clearPoll();
  scanning = true;
  rebuild();
  inflight = (async () => {
    try {
      let force = refresh;
      do {
        queuedRefresh = false;
        try {
          const result = await api.discoverSkillSnapshot(force);
          const first = loading;
          groups = result.groups;
          serverScanning = result.scanning;
          loading = false;
          fetchedAt = nowMs();
          revision++;
          pendingDirty = null;
          rebuild();
          // Check the first quick inventory and the final one; intermediate polls
          // need no repeated git sweep. Publishing groups always comes first.
          if (first || !serverScanning) refreshDirty();
        } catch {
          /* keep the cache; a running server search is polled again */
        }
        force = queuedRefresh;
      } while (force);
    } finally {
      inflight = null;
      scanning = serverScanning;
      rebuild();
      schedulePoll();
    }
  })();
  return inflight;
}

/** Explicit action: refresh the inventory and request a new project search.
 * Resolves when the immediate snapshot lands; background results and git badges
 * continue independently, keeping the cached cards available throughout.
 * Automatic mining ticks pass false to read without restarting the search. */
export function refreshSkills(refresh = true): Promise<void> {
  return readSkills(refresh);
}

function subscribe(fn: () => void): () => void {
  listeners.add(fn);
  // Paint the cache immediately; refresh in the background. The cold start (no
  // scan yet) shows the spinner via loading=true; a fresh-enough cache skips the
  // redundant rescan (coalescing handles a same-tick double-mount).
  if (!inflight && (serverScanning || nowMs() - fetchedAt >= STALE_MS)) void readSkills();
  return () => {
    listeners.delete(fn);
    if (!listeners.size) clearPoll();
  };
}

/** The cached discovered skills; the home page's stat card and gallery share it.
 *  Reads the last result instantly on revisit while a background rescan runs. */
export function useSkills(): SkillsSnap {
  return useSyncExternalStore(subscribe, () => snap, () => snap);
}

if (typeof window !== "undefined") window.addEventListener("vibestudio:workspace-restored", () => {
  if (listeners.size) void refreshSkills(false);
});
