// Same-host recovery preserves the mounted workspace and its selected session.
// Changing hosts or explicitly disconnecting still reloads to bind every cache.
import { useSyncExternalStore } from "react";
import * as api from "./api";
import { flushEditor } from "./editorState";
import { setWorkspaceAvailable } from "./workspaceConnection";

const CONNECTING: ReadonlySet<api.RemoteState> = new Set([
  "detecting",
  "installing",
  "launching",
  "forwarding",
  "reconnecting",
]);

export interface RemoteSnapshot {
  status: api.RemoteStatus;
  /** The host whose data the mounted workspace owns, even during an outage. */
  workspaceHost: string | null;
  interrupted: boolean;
  /** Whether the remote control is shown. False ONLY when the server explicitly has no
   *  remoting (`/api/remote/*` 404s — a network-exposed server / the remote binary).
   *  A transport error (the local server is down/unreachable) keeps this true, so the
   *  connect dialog stays reachable and we recover when the server returns. */
  available: boolean;
  /** Is this the mobile switchboard app? True when the server exposes a credential
   *  store (`/api/remote/profiles` answers) — only the iOS shell does. `undefined`
   *  until the one-time probe resolves; the shell shows a neutral splash until then,
   *  so neither the desktop workspace nor the mobile connect screen flashes wrongly.
   *  Mobile has NO local workspace — disconnected mobile shows the connect screen. */
  mobile: boolean | undefined;
}

let snapshot: RemoteSnapshot = { status: { state: "idle" }, workspaceHost: null, interrupted: false, available: false, mobile: undefined };
let pendingConnect = false; // a user-initiated connect is awaiting "connected"
const listeners = new Set<() => void>();
let pollTimer: ReturnType<typeof setInterval> | null = null;
let pollMs = 0; // current poll interval (0 = not polling)

/** Merge a partial update into the snapshot so unrelated fields (notably `mobile`,
 *  resolved by its own probe) survive a status/availability refresh. */
function update(next: Partial<RemoteSnapshot>) {
  snapshot = { ...snapshot, ...next };
  for (const l of listeners) l();
}

/** One-time probe: does this server have a credential store (→ the mobile app)?
 *  `api.sshProfiles()` resolves to an array on the iOS shell, `null` on a 404
 *  (desktop/standalone — no store), and throws only on a real error: a store-side
 *  400 (corrupt profile file — still mobile) or a transport failure. An HTTP answer
 *  resolves `mobile` immediately; a transport error (server not up yet) retries a
 *  few times, then defaults to `false` so the shell never hangs on its splash — a
 *  truly unreachable server can't render the workspace either way. Mobile has NO
 *  local workspace. */
async function probeMobile(): Promise<void> {
  for (let attempt = 0; attempt < 5; attempt++) {
    try {
      const profiles = await api.sshProfiles();
      update({ mobile: profiles !== null });
      return;
    } catch (e) {
      if ((e as { status?: number } | null)?.status) {
        update({ mobile: true }); // any HTTP status = the store route exists = mobile
        return;
      }
      await new Promise((r) => setTimeout(r, 300 * (attempt + 1))); // transport error — retry
    }
  }
  update({ mobile: false }); // give up → behave as the non-mobile workspace
}

/** Poll fast while connecting, slowly while connected (to catch a dropped tunnel),
 *  not at all when idle/errored. Only resets the timer when the cadence changes. */
function setPoll(ms: number) {
  if (ms === pollMs) return;
  pollMs = ms;
  if (pollTimer != null) {
    clearInterval(pollTimer);
    pollTimer = null;
  }
  if (ms > 0) pollTimer = setInterval(() => void refresh(), ms);
}

// Whether a status has been OBSERVED in this page lifetime — distinguishes a real
// transition (…→connected must rebind) from a page that loaded already-connected
// (every fetch it ever made was proxied; reloading would loop forever).
let observed = false;

let refreshSequence = 0;
export async function refresh(): Promise<void> {
  const sequence = ++refreshSequence;
  let status: api.RemoteStatus;
  try {
    status = await api.remoteStatus();
  } catch (e) {
    if (sequence !== refreshSequence) return;
    observed = true;
    const noRemoting = (e as { status?: number } | undefined)?.status === 404;
    const host = snapshot.workspaceHost;
    setPoll(noRemoting && !host ? 0 : 3000);
    if (host) {
      setWorkspaceAvailable(false);
      update({
        status: { state: noRemoting ? "error" : "reconnecting", host, message: "Waiting for the connection to return…" },
        interrupted: true, available: true,
      });
    } else {
      update({ status: { state: "idle" }, available: !noRemoting });
    }
    return;
  }
  if (sequence !== refreshSequence) return;
  const wasObserved = observed;
  observed = true;
  const boundHost = snapshot.workspaceHost;
  const host = status.host ?? null;
  if (status.state === "connected") {
    // A page loaded already connected has never read another machine's data.
    // Any later host change must invalidate all host-owned caches together.
    if ((boundHost && host !== boundHost) || (!boundHost && (wasObserved || pendingConnect))) {
      setWorkspaceAvailable(false);
      window.location.reload();
      return;
    }
    pendingConnect = false;
    update({ status, workspaceHost: host, interrupted: false, available: true });
    setWorkspaceAvailable(true);
    setPoll(3000);
    return;
  }
  if (boundHost && status.state === "idle") {
    // An explicit disconnect from another viewer also changes the environment.
    setWorkspaceAvailable(false);
    window.location.reload();
    return;
  }
  const interrupted = boundHost !== null;
  setWorkspaceAvailable(!interrupted && status.state === "idle");
  update({ status, interrupted, available: true });
  setPoll(CONNECTING.has(status.state) ? 1200 : interrupted ? 3000 : 0);
  if (status.state === "error" || status.state === "idle") pendingConnect = false;
}

export async function retry(): Promise<void> {
  ++refreshSequence;
  setWorkspaceAvailable(false);
  await api.remoteRetry();
  await refresh();
}

export async function connect(host: string): Promise<void> {
  if (host === snapshot.workspaceHost) return retry();
  ++refreshSequence;
  pendingConnect = true;
  setWorkspaceAvailable(false);
  try {
    await api.remoteConnect(host);
  } catch (e) {
    pendingConnect = false;
    await refresh();
    throw e;
  }
  await refresh(); // picks up "detecting" and starts polling
}

/** Abort an in-flight connect, or clear an "error" state, returning to Local —
 *  WITHOUT a reload. A failed/aborted connect never set a target (we were never
 *  proxying), so there's nothing to rebind; just reset the backend status to idle. */
export async function cancel(): Promise<void> {
  if (snapshot.workspaceHost) return disconnect();
  ++refreshSequence;
  pendingConnect = false;
  try {
    await api.remoteDisconnect();
  } catch {
    /* ignore — best-effort reset */
  }
  await refresh();
}

export async function disconnect(): Promise<void> {
  // Flush any pending editor buffer to the REMOTE before we tear the tunnel down (we're
  // still connected here), then reload so the window re-binds to the local host.
  ++refreshSequence;
  try {
    if (snapshot.status.state === "connected") await flushEditor();
  } catch {
    /* best-effort — don't block disconnect on a flush failure */
  }
  await api.remoteDisconnect();
  window.location.reload();
}

export function useRemote(): RemoteSnapshot & {
  connect: typeof connect;
  disconnect: typeof disconnect;
  cancel: typeof cancel;
  retry: typeof retry;
} {
  const snap = useSyncExternalStore(
    (cb) => {
      listeners.add(cb);
      return () => listeners.delete(cb);
    },
    () => snapshot,
    () => snapshot,
  );
  return { ...snap, connect, disconnect, cancel, retry };
}

// Auto-reconnect on launch (VS Code-style). The server remembers the host we last
// connected to (and didn't explicitly disconnect from); if we're currently Local,
// reconnect to it through the NORMAL connect path — so the same pendingConnect→reload
// rebinds the whole window (skills, files, git, secrets, terminals, recents) to the
// remote. At most once per launch: sessionStorage survives the connect-triggered
// reload but resets on a fresh app start, so disconnecting stays Local for the session.
const RESUME_KEY = "vibestudio-remote-resumed";
async function maybeResume(): Promise<void> {
  if (!snapshot.available) return; // this server has no remoting
  if (snapshot.status.state !== "idle") return; // already connecting/connected/errored
  // Only on a loopback origin (the desktop webview / browser-local dev). On a
  // shared origin — a tailscale-served or LAN server — one viewer's page load
  // must not silently flip the server onward to the last-connected SSH host
  // under every other viewer.
  if (!["127.0.0.1", "localhost", "[::1]", "::1"].includes(window.location.hostname)) return;
  try {
    if (sessionStorage.getItem(RESUME_KEY)) return; // already attempted this launch
  } catch {
    return;
  }
  let host: string | null = null;
  try {
    host = (await api.remoteLast()).host;
  } catch {
    return; // couldn't read the remembered host — stay Local
  }
  if (!host) return; // last state was Local → stay Local
  try {
    sessionStorage.setItem(RESUME_KEY, host);
  } catch {
    /* private mode — the idle guard above still bounds re-entry */
  }
  void connect(host).catch(() => {
    /* failure flips status to "error"; the menu surfaces it, the user can retry */
  });
}

// Resolve availability + current status on first import (cold start / post-reload),
// so the pill is correct immediately and a still-connecting session keeps polling,
// then resume the last connection if we came up Local. In parallel, probe whether
// this is the mobile app (drives the connect-first shell) — independent of status.
void refresh().then(maybeResume);
void probeMobile();
