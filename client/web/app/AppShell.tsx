import { Suspense, lazy, useEffect, useState } from "react";
import { Outlet, useLocation } from "react-router-dom";
import { Spinner } from "@/components/ui";
import PhoneModal from "@/components/PhoneModal";
import SessionsHost from "@/pages/sessions/SessionsHost";
import UpdateBanner from "@/components/UpdateBanner";
import RemoteRecovery from "@/components/RemoteRecovery";
import { useRemote } from "@/lib/remote";
import { notifyPrime } from "@/lib/api";
import { log } from "@/lib/log";
import { useDiscardBlocker } from "./routeGuard";

// Mobile-only landing (the connect screen). Lazy so the desktop bundle never pulls
// it — the gate below only ever renders it on the iOS shell.
const MobileConnect = lazy(() => import("@/pages/MobileConnect"));

/**
 * The single persistent node under the router — it never unmounts across
 * navigation. Routed views (Home, Skill) render in the Outlet; Sessions is an
 * always-mounted sibling kept alive (its pty/xterm must survive navigation) and
 * shown only on `/sessions`. The unsaved-changes guard lives here so it covers
 * every navigation, including browser back/forward.
 *
 * Mobile has NO local workspace: a phone holds no skills/agents, so the switchboard
 * only exists to reach a remote. While the mobile app is not connected, the whole
 * shell IS the connect screen — the workspace (and its Outlet/Sessions) never mount
 * until a remote is up. Desktop is unaffected (`mobile` is false there).
 */
export default function AppShell() {
  const { pathname } = useLocation();
  const onSessions = pathname === "/sessions";
  const { mobile, status, workspaceHost, interrupted } = useRemote();
  // Home's navigation and connection controls stay usable during recovery.
  // Remote requests remain paused by the transport until the host is available.
  const lockWorkspace = interrupted && pathname !== "/";
  useDiscardBlocker();

  // A phone often joins agents started on the desktop. Ask when its workspace
  // connects, rather than requiring the user to create a new session on iOS.
  useEffect(() => {
    if (mobile && status.state === "connected") {
      void notifyPrime().catch((error) => {
        if (error?.status !== 404) log.warn("notify", "notification permission request failed", String(error));
      });
    }
  }, [mobile, status.state]);

  // The tray's "Open on your phone…" item deep-links to `#/?phone=1`. Handled
  // here — mounted exactly once and never display:none-hidden (RemoteMenu isn't:
  // a hidden Sessions copy would consume the one-shot param invisibly). With
  // HashRouter the query rides inside the hash, so parse it ourselves; open the
  // modal and strip the param (replaceState doesn't re-fire hashchange, so no loop).
  const [phoneOpen, setPhoneOpen] = useState(false);
  useEffect(() => {
    const check = () => {
      const hash = window.location.hash;
      const q = hash.indexOf("?");
      if (q === -1) return;
      const params = new URLSearchParams(hash.slice(q + 1));
      if (params.get("phone") !== "1") return;
      params.delete("phone");
      const rest = params.toString();
      const url = window.location.pathname + window.location.search + hash.slice(0, q) + (rest ? `?${rest}` : "");
      history.replaceState(null, "", url);
      setPhoneOpen(true);
    };
    check();
    window.addEventListener("hashchange", check);
    return () => window.removeEventListener("hashchange", check);
  }, []);

  // Until the one-time mobile probe resolves, render a neutral splash — so neither
  // the desktop workspace nor the mobile connect screen flashes on the wrong host.
  // Desktop resolves `mobile = false` in one loopback round-trip (imperceptible).
  if (mobile === undefined) {
    return <div className="grid h-dvh place-items-center"><Spinner /></div>;
  }
  // Mobile, not connected → the connect screen is the entire app.
  if (mobile && status.state !== "connected" && !workspaceHost) {
    return (
      <Suspense fallback={<div className="grid h-dvh place-items-center"><Spinner /></div>}>
        <MobileConnect />
      </Suspense>
    );
  }

  return (
    <>
      <div className="contents" inert={lockWorkspace}>
      <div style={{ display: onSessions ? "none" : "contents" }}>
        <Suspense fallback={<div className="grid h-dvh place-items-center"><Spinner /></div>}>
          <Outlet />
        </Suspense>
      </div>
      <SessionsHost active={onSessions} />
      <UpdateBanner />
      {phoneOpen && <PhoneModal onClose={() => setPhoneOpen(false)} />}
      </div>
      {lockWorkspace && <RemoteRecovery />}
    </>
  );
}
