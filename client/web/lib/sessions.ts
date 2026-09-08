// Shared sessions, unread state, and attention notifications outlive mounted
// workspaces. Server detector transitions drive status, request priority, and
// request/done sounds. SSE and polling share a sequence ledger so they cannot
// announce the same transition twice; startup/reconnect baselines stay silent.
// Older servers keep their terminal-bell notifications and seen timestamps.
import { useSyncExternalStore } from "react";
import * as api from "@/lib/api";
import type { TermEvent, TermSession } from "@/lib/api";
import { log } from "@/lib/log";
import { canPush, enablePushInGesture } from "@/lib/push";
import { sessionsPath } from "@/lib/routes";
import { attentionBoot, attentionIsUnread, newerAttention, orderSessions, shouldSound } from "./sessionAttention";
import { playAttentionSound, unlockAttentionSound } from "./attentionSound";
import { subscribeWorkspaceConnection, workspaceConnection } from "./workspaceConnection";

/** Per-session "last viewed" marks (id → unix secs) for the unread dot.
 *  Legacy key string — keep the old "terminals" word so existing marks survive the rename. */
const SEEN_KEY = "skillviewer-terminals-seen";
const ATTENTION_SEEN_KEY = "vibestudio-session-attention-seen";

function readAttentionSeen(): Record<string, string> {
  try {
    const value: unknown = JSON.parse(localStorage.getItem(ATTENTION_SEEN_KEY) ?? "{}");
    if (!value || typeof value !== "object" || Array.isArray(value)) return {};
    return Object.fromEntries(Object.entries(value).filter(([, v]) => typeof v === "string"));
  } catch { return {}; }
}

function persistAttentionSeen(): void {
  try { localStorage.setItem(ATTENTION_SEEN_KEY, JSON.stringify(attentionSeen)); } catch { /* best effort */ }
}

/** Wall-clock seconds, to compare against tmux bell timestamps. */
const nowSecs = () => Math.floor(Date.now() / 1000);

const bellOf = (s: TermSession) => Number(s.bellAt) || 0;

function readSeen(): Record<string, number> {
  try {
    const raw = localStorage.getItem(SEEN_KEY);
    const v = raw ? JSON.parse(raw) : null;
    return v && typeof v === "object" ? (v as Record<string, number>) : {};
  } catch {
    return {};
  }
}

function persistSeen() {
  try {
    localStorage.setItem(SEEN_KEY, JSON.stringify(seen));
  } catch {
    /* ignore */
  }
}

/** The user's manual rail order (session ids, see `reorder`). Legacy "terminals"
 *  word kept so the key matches SEEN_KEY / the rail-width key across the rename. */
const ORDER_KEY = "skillviewer-terminals-order";

function readOrder(): string[] {
  try {
    const raw = localStorage.getItem(ORDER_KEY);
    const v = raw ? JSON.parse(raw) : null;
    return Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : [];
  } catch {
    return [];
  }
}

function persistOrder() {
  try {
    localStorage.setItem(ORDER_KEY, JSON.stringify(order));
  } catch {
    /* ignore */
  }
}

/**
 * Requests first, then the user's order within each priority group. The stable
 * chronological fallback keeps tmux's pid-led list from shuffling the rail.
 */
function sortSessions(list: TermSession[]): TermSession[] {
  return orderSessions(list, order);
}

/** Attention uses exact transition IDs; legacy servers use bell timestamps.
 *  Raw terminal activity never counts as unread. */
export function isUnread(
  s: TermSession,
  seenMap: Record<string, number>,
  activeId: string | null,
): boolean {
  if (s.id === activeId) return false; // the one you're watching is never "new"
  if (s.attention) return attentionIsUnread(s.attention, attentionSeen[s.id]);
  return bellOf(s) > (seenMap[s.id] ?? bellOf(s));
}

// ─── store state ───

let sessions: TermSession[] = [];
let loading = true;
let seen: Record<string, number> = readSeen();
let order: string[] = readOrder();
let attentionSeen: Record<string, string> = readAttentionSeen();
const latestAttention = new Map<string, api.SessionAttention>();
const createdHere = new Set<string>();
let attentionReady = false;
let attentionEpoch = 0;
const pendingAttention = new Map<string, ReturnType<typeof setTimeout>>();
/** The session a VISIBLE workspace is currently showing (null = none visible). */
let watchedId: string | null = null;
const listeners = new Set<() => void>();

export interface SessionsSnap {
  sessions: TermSession[];
  loading: boolean;
  seen: Record<string, number>;
  /** Sessions with a bell newer than their seen mark, excluding the watched one
   *  — the NavBar aggregate dot / dock badge count. */
  unreadCount: number;
}

let snap: SessionsSnap = { sessions, loading, seen, unreadCount: 0 };

function rebuild() {
  const unreadCount = sessions.filter((s) => isUnread(s, seen, watchedId)).length;
  snap = { sessions, loading, seen, unreadCount };
  syncBadge();
  for (const l of listeners) l();
}

export function useSessions(): SessionsSnap {
  return useSyncExternalStore(
    (cb) => {
      listeners.add(cb);
      return () => listeners.delete(cb);
    },
    () => snap,
    () => snap,
  );
}

/** Freeze a session at "viewed now" (switching away from it, killing it). */
export function markSeen(id: string | null): void {
  if (!id) return;
  seen = { ...seen, [id]: nowSecs() };
  const attention = latestAttention.get(id);
  if (attention) {
    attentionSeen = { ...attentionSeen, [id]: attention.sequence };
    persistAttentionSeen();
  }
  persistSeen();
  rebuild();
}

/** Report the session a VISIBLE workspace is showing. The watched session never
 *  counts unread, and its seen mark is stamped at every moment attention is
 *  KNOWN — watch start, watch end, window blur, and each fetch that lands while
 *  focused — so a turn the user watched land (or just read) can never re-dot,
 *  re-badge, or toast after they move on. */
export function setWatched(id: string | null): void {
  if (id === watchedId) return;
  watchedId = id;
  if (id && !document.hidden && document.hasFocus()) {
    markSeen(id); // it's on screen right now — caught up by definition
  } else {
    rebuild();
  }
}

/** Release the watch IF this instance holds it — an unmounting/hidden workspace
 *  must not clobber a watch another (visible) workspace just reported. */
export function releaseWatched(id: string | null): void {
  if (id && id === watchedId) {
    watchedId = null;
    markSeen(id); // the user was looking at it until this instant
  }
}

/** Optimistic insert for a just-created session (the rail selects it at once;
 *  the next refresh reconciles). */
export function noteCreated(s: TermSession): void {
  if (sessions.some((p) => p.id === s.id)) return;
  createdHere.add(s.id);
  observeAttention(s, attentionReady);
  sessions = sortSessions([...sessions, s]);
  if (seen[s.id] == null) {
    seen = { ...seen, [s.id]: bellOf(s) };
    persistSeen();
  }
  rebuild();
}

/** Apply the user's manual rail order (drag- or keyboard-reorder). `orderedIds`
 *  is the full visual order of the currently-listed sessions; sessions not in it
 *  (new ones) fall to the end chronologically until placed. */
export function reorder(orderedIds: string[]): void {
  order = orderedIds.slice();
  persistOrder();
  sessions = sortSessions(sessions);
  rebuild();
}

// One refresh in flight, one queued: an event burst mid-fetch still lands one
// trailing fetch, without stacking requests.
let inflight: Promise<void> | null = null;
let queued = false;

export function refresh(): Promise<void> {
  if (!workspaceConnection().available) return Promise.resolve();
  if (inflight) {
    queued = true;
    return inflight;
  }
  const epoch = attentionEpoch;
  const requestedFrom = new Map(latestAttention);
  inflight = (async () => {
    try {
      const list = await api.terminalList();
      if (epoch === attentionEpoch) {
        setSessions(list, requestedFrom);
        attentionReady = true;
      }
    } catch {
      /* transient — the poll or the next event retries */
    } finally {
      loading = false;
      inflight = null;
      if (queued) {
        queued = false;
        void refresh();
      } else {
        rebuild();
      }
    }
  })();
  return inflight;
}

function setSessions(list: TermSession[], requestedFrom: Map<string, api.SessionAttention>) {
  list = list.map((s) => {
    const current = latestAttention.get(s.id);
    if (current && (
      requestedFrom.get(s.id)?.sequence !== current.sequence || (s.attention && !newerAttention(s.attention, current))
    )) return { ...s, attention: current };
    if (!s.attention && current) {
      latestAttention.delete(s.id);
      const timer = pendingAttention.get(s.id);
      if (timer) clearTimeout(timer);
      pendingAttention.delete(s.id);
    }
    observeAttention(s, attentionReady);
    return s;
  });
  // Poll-path notification backstop: detect bell transitions here too (a server
  // without /api/events still toasts, ≤5s late). The dedup set in maybeNotify
  // absorbs the overlap when the SSE path already announced the same bell.
  const prevById = new Map(sessions.map((s) => [s.id, s]));
  for (const s of list) {
    const p = prevById.get(s.id);
    if (p && !s.attention && bellOf(s) > bellOf(p)) {
      maybeNotify({ id: s.id, label: s.label, agent: s.agent, cwd: s.cwd, at: s.bellAt });
    }
  }

  sessions = sortSessions(list);
  const live = new Set(list.map((s) => s.id));
  for (const id of latestAttention.keys()) {
    if (!live.has(id)) latestAttention.delete(id);
  }
  for (const [id, timer] of pendingAttention) {
    if (!live.has(id)) { clearTimeout(timer); pendingAttention.delete(id); }
  }

  // Seed a seen mark for each newly-listed session (start it at its own bell)
  // and prune marks for sessions that are gone. Viewed sessions keep the stamps
  // markSeen gave them. An empty list is left alone — a transient tmux hiccup
  // must not wipe every mark.
  if (list.length > 0) {
    const next: Record<string, number> = {};
    let changed = Object.keys(seen).length !== list.length;
    for (const s of list) {
      if (seen[s.id] == null) changed = true;
      next[s.id] = seen[s.id] ?? bellOf(s);
    }
    if (changed) {
      seen = next;
      persistSeen();
    }
  }

  // Watching a focused, visible session keeps it read: a turn you watched land
  // must not dot the rail or count in the badge after you switch away.
  if (watchedId && !document.hidden && document.hasFocus()) {
    const w = list.find((s) => s.id === watchedId);
    if (w?.attention && attentionSeen[w.id] !== w.attention.sequence) {
      attentionSeen = { ...attentionSeen, [w.id]: w.attention.sequence };
      persistAttentionSeen();
    }
    if (w && bellOf(w) > (seen[w.id] ?? 0)) {
      seen = { ...seen, [w.id]: nowSecs() };
      persistSeen();
    }
  }
}

/** Snapshot and SSE share one sequence ledger. Initial observations and reconnect
 * baselines update the rail silently; only new live transitions announce. */
function observeAttention(s: Pick<TermSession, "id" | "label" | "attention">, announce: boolean): void {
  const next = s.attention;
  if (!next) return;
  const previous = latestAttention.get(s.id);
  if (previous && !newerAttention(next, previous)) return;
  const timer = pendingAttention.get(s.id);
  if (timer) clearTimeout(timer);
  pendingAttention.delete(s.id);
  latestAttention.set(s.id, next);
  const newlyCreated = createdHere.has(s.id);
  if (next.state !== "unknown") createdHere.delete(s.id);
  if (attentionSeen[s.id] === undefined) {
    attentionSeen = { ...attentionSeen, [s.id]: next.sequence };
    persistAttentionSeen();
  }
  if ((!announce && !newlyCreated) || !next.kind ||
    ((!previous || attentionBoot(previous) !== attentionBoot(next)) && !newlyCreated)) return;
  const kind = next.kind;
  const epoch = attentionEpoch;
  const current = () => attentionEpoch === epoch && latestAttention.get(s.id)?.sequence === next.sequence;
  // A short settle window cancels prompts that disappear before the user could act.
  pendingAttention.set(s.id, setTimeout(() => {
    pendingAttention.delete(s.id);
    if (!current()) return;
    const watching = () => watchedId === s.id && !document.hidden && document.hasFocus();
    if (shouldSound(kind, watching())) {
      void playAttentionSound(kind, () => current() && shouldSound(kind, watching()));
    }
    if (shouldNotify(s.id)) {
      void deliver(s.label || s.id, kind === "request" ? "Your input is needed." : "Your turn — the agent finished.", s.id,
        () => current() && shouldNotify(s.id));
    }
  }, 120));
}

// ─── the notifier ───

/** Bells already decided on (`id:bellAt`), so the SSE path and the poll backstop
 *  can't double-toast the same turn. */
const notified = new Set<string>();
const latestBell = new Map<string, string>();

/** The newest bell actually toasted per session — the "one banner per session
 *  until seen" ledger. Deliberately NOT derived from `sessions` (which lags a
 *  refresh behind and can miss one on a transport blip). */
const announced = new Map<string, number>();

/** Native-surface capability: null = not probed yet, false = 404 (no shell on
 *  this origin — browser mode), true = the native shell answers /api/notify. */
let nativeNotify: boolean | null = null;
let notifyWhileVisible: boolean | null = null;
let nativeProbe: Promise<void> | null = null;

function shouldNotify(id: string): boolean {
  return document.hidden || !document.hasFocus() || (notifyWhileVisible === true && watchedId !== id);
}

function maybeNotify(e: TermEvent): void {
  if (e.attention || latestAttention.has(e.id)) return; // semantic attention supersedes terminal bells
  const key = `${e.id}:${e.at}`;
  if (notified.has(key)) return;
  notified.add(key);
  latestBell.set(e.id, e.at);
  if (!attentionReady) return;
  const bell = Number(e.at) || 0;
  const seenAt = seen[e.id];
  // Unknown session (never listed) or already viewed past this bell → no toast;
  // the seen-seeding rule keeps reconnects/restarts silent by construction.
  if (seenAt == null || bell <= seenAt) return;
  const epoch = attentionEpoch;
  const current = () => attentionEpoch === epoch && latestBell.get(e.id) === e.at && !latestAttention.has(e.id) &&
    sessions.some((s) => s.id === e.id) &&
    !(watchedId === e.id && !document.hidden && document.hasFocus());
  if (current()) void playAttentionSound("done", current);
  // One banner per session until it's seen: a toast for an earlier still-unread
  // bell already summoned the user for this session.
  if ((announced.get(e.id) ?? 0) > seenAt) return;
  // Desktop/browser banners summon a background window. Mobile also announces
  // other sessions while the app is visible; the watched session stays quiet.
  if (!shouldNotify(e.id)) return;
  announced.set(e.id, bell);
  // Body = the agent's last line (SSE bell frames carry it); the poll backstop and
  // an empty pane fall back to the fixed summons.
  void deliver(e.label || e.id, e.last?.trim() || "Your turn — the agent finished.", e.id,
    () => current() && shouldNotify(e.id));
}

async function deliver(title: string, body: string, tag: string, current: () => boolean = () => true): Promise<void> {
  if (!current()) return;
  // A push-capable surface (installed PWA / push-subscribed browser) already gets
  // the server's Web Push for this same bell — raising a local notification too is
  // the "exactly two per turn" double. Web Push is the authoritative channel there,
  // so defer to it outside the native shell. Some native webviews expose these
  // browser APIs without a working push subscription; prefer their OS channel.
  if (nativeNotify !== true && canPush() && Notification.permission === "granted") return;
  if (nativeNotify !== false) {
    try {
      await api.notifyNative(title, body);
      nativeNotify = true;
      return;
    } catch (err) {
      if ((err as { status?: number } | undefined)?.status === 404) {
        nativeNotify = false; // no shell on this origin — web fallback below
      } else {
        // The shell exists but the OS refused (permission denied, no DBus
        // daemon, unsigned dev build): quiet failure, the dot still shows.
        log.warn("notify", "native notification failed", err instanceof Error ? err.message : String(err));
        return;
      }
    }
  }
  if (current()) webNotify(title, body, tag);
}

function webNotify(title: string, body: string, tag: string): void {
  // Feature-detect: WKWebView has no Notification API at all, and iOS Safari
  // tabs don't either — those quietly keep the dot only.
  if (typeof Notification === "undefined" || Notification.permission !== "granted") return;
  try {
    const n = new Notification(title, { body, tag });
    n.onclick = () => {
      window.focus();
      window.location.hash = `#${sessionsPath(tag)}`;
      n.close();
    };
  } catch {
    /* some embedders throw on construction — quiet */
  }
}

// Dock/taskbar badge follows the unread count (desktop shell only).
let lastBadge = -1;
function syncBadge(): void {
  if (nativeNotify !== true) return;
  const n = snap.unreadCount;
  if (n === lastBadge) return;
  lastBadge = n;
  api.notifyBadge(n).catch(() => {
    lastBadge = -1; // retry on the next change
  });
}

function probeNative(): Promise<void> {
  if (notifyWhileVisible !== null) return Promise.resolve();
  if (nativeProbe) return nativeProbe;
  nativeProbe = (async () => {
    try {
      const status = await api.notifyStatus();
      nativeNotify = status.native;
      notifyWhileVisible = status.native && status.notifyWhileVisible === true;
      syncBadge();
    } catch (e) {
      // Retry unknown capabilities on reconnect/resume. A successful toast
      // alone cannot tell us whether this shell supports foreground banners.
      if ((e as { status?: number } | undefined)?.status === 404) {
        nativeNotify = false;
        notifyWhileVisible = false;
      }
    }
  })().finally(() => { nativeProbe = null; });
  return nativeProbe;
}

/** Ask for notification permission at a user-legible moment — call this from
 *  the gesture that creates a session, so the OS prompt has an obvious "why".
 *  Browser mode needs the actual user gesture, so call it synchronously from
 *  the click handler, not after an await. */
export function primeNotifications(): void {
  unlockAttentionSound();
  if (nativeNotify !== false) {
    api.notifyPrime().catch(() => {});
  }
  // Web Push / Web Notification only where there's NO native shell (phone,
  // browser). A desktop shell — including Windows WebView2, which exposes
  // serviceWorker/PushManager but has no working Push API — already has real OS
  // toasts via notifyPrime above, so prompting for push there is a dead-end.
  if (nativeNotify === false) {
    if (canPush()) {
      void enablePushInGesture();
    } else if (typeof Notification !== "undefined" && Notification.permission === "default") {
      void Notification.requestPermission();
    }
  }
}

/** The probed native-shell state (true = desktop OS toasts, false = none,
 *  null = not yet probed) — lets the UI hide push affordances on the desktop. */
export function nativeNotifyState(): boolean | null {
  return nativeNotify;
}

// ─── the /api/events subscription ───

let esHandle: { close(): void } | null = null;
let eventRetry: ReturnType<typeof setTimeout> | null = null;

function rebaselineAttention(): void {
  attentionEpoch++;
  attentionReady = false;
  for (const timer of pendingAttention.values()) clearTimeout(timer);
  pendingAttention.clear();
}

function connectEvents(): void {
  if (eventRetry !== null) clearTimeout(eventRetry);
  eventRetry = null;
  esHandle?.close();
  if (!workspaceConnection().available) return;
  esHandle = api.terminalEvents(
    (kind, e) => {
      if (kind === "bell") maybeNotify(e);
      if (kind === "closed") {
        createdHere.delete(e.id);
        latestAttention.delete(e.id);
        latestBell.delete(e.id);
        const timer = pendingAttention.get(e.id);
        if (timer) clearTimeout(timer);
        pendingAttention.delete(e.id);
      }
      if (kind === "attention" && e.attention) {
        observeAttention(e, attentionReady);
        sessions = sortSessions(sessions.map((s) => s.id === e.id ? { ...s, attention: latestAttention.get(e.id) } : s));
        if (watchedId === e.id && !document.hidden && document.hasFocus()) markSeen(e.id);
        rebuild();
      }
      void refresh();
    },
    () => {
      // Retry an unavailable events route slowly. A recovered workspace restarts
      // this subscription immediately without replacing the session cache.
      esHandle = null;
      eventRetry = setTimeout(connectEvents, 30_000);
    },
    // Catch up without sounding historical transitions after a network gap.
    () => {
      rebaselineAttention();
      void probeNative();
      void refresh();
    },
    rebaselineAttention,
  );
}

subscribeWorkspaceConnection(() => {
  rebaselineAttention();
  connectEvents();
  if (workspaceConnection().available) void refresh();
});

// ─── boot (module side effects, like lib/updates.ts) ───

void probeNative();
connectEvents();
// A baseline fetch so the NavBar dot and the notifier's seen marks exist even
// before any Sessions surface mounts; waits out app startup.
setTimeout(() => void refresh(), 1500);
// Attention edges. Losing it stamps the watched session — everything up to this
// instant was on screen, so a bell that raced the blur (turn finished while the
// user was still looking, event delivered just after) must not toast or dot.
// Gaining it stamps too, and catches up the list (hidden webviews get their
// timers throttled, so the poll may have stretched).
window.addEventListener("blur", () => {
  if (watchedId) markSeen(watchedId);
});
window.addEventListener("focus", () => {
  void probeNative();
  if (watchedId && !document.hidden) markSeen(watchedId);
});
document.addEventListener("visibilitychange", () => {
  if (document.hidden) {
    if (watchedId) markSeen(watchedId);
  } else {
    void probeNative();
    void refresh();
  }
});
