"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { Modal } from "@/components/Modal";
import { Spinner, btnGhost, btnPrimary } from "@/components/ui";
import * as api from "@/lib/api";
import { useRemote } from "@/lib/remote";

export function PhoneIcon({ className = "" }: { className?: string }) {
  return (
    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden className={className}>
      <rect x="7" y="2" width="10" height="20" rx="2" />
      <path d="M11 18h2" />
    </svg>
  );
}

function CopyButton({ text }: { text: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <button
      type="button"
      className={btnGhost}
      onClick={() => {
        void navigator.clipboard?.writeText(text);
        setCopied(true);
      }}
    >
      {copied ? "Copied" : "Copy"}
    </button>
  );
}

const codeCls = "rounded-md border border-border bg-panel px-2.5 py-1.5 font-mono text-xs text-fg";

type EnableFailure = Extract<api.PhoneEnableResult, { ok: false }>;

/**
 * "Open on your phone" — serves the app over the user's Tailscale network and
 * shows a QR code the phone scans. Opened from the Remote dialog, which only
 * offers it once `/api/phone/status` answered (the feature 404s on standalone
 * remote servers), or via the tray's `#/?phone=1` deep link.
 */
export default function PhoneModal({ onClose }: { onClose: () => void }) {
  const [status, setStatus] = useState<api.PhoneStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false); // enable/disable/login in flight
  const [fail, setFail] = useState<EnableFailure | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [login, setLogin] = useState<api.PhoneLoginResult | null>(null);
  // Auto-serve guards: front the server once Tailscale is ready without a click,
  // but never re-serve after the user turns it off, and never loop on a failure.
  const autoServed = useRef(false);
  const userDisabled = useRef(false);
  // While SSH-connected, /api/phone/* proxies to the remote server (tailscale runs
  // THERE), so the copy names the host that owns the durable phone endpoint.
  const { status: remote, workspaceHost } = useRemote();
  const remoteHost = workspaceHost ?? (remote.state === "connected" ? (remote.host ?? null) : null);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    setLogin(null);
    try {
      setStatus(await api.phoneStatus());
    } catch (e) {
      setError(e instanceof Error ? e.message : "Couldn't check phone access.");
    } finally {
      setLoading(false);
    }
  }, []);
  useEffect(() => {
    void load();
  }, [load]);

  const doEnable = useCallback(async () => {
    setBusy(true);
    setFail(null);
    setError(null);
    try {
      const r = await api.phoneEnable();
      if (r.ok) setStatus(r);
      else setFail(r);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Couldn't enable phone access.");
    } finally {
      setBusy(false);
    }
  }, []);

  // Once Tailscale is signed in, front the server automatically so the QR appears
  // without a separate "Enable" click. Guarded (autoServed/userDisabled/fail) so
  // it fires at most once and never undoes a manual "Turn off".
  useEffect(() => {
    if (!status || busy) return;
    if (status.serving) {
      autoServed.current = true;
      return;
    }
    if (status.tailscale !== "ok" || autoServed.current || userDisabled.current || fail) return;
    autoServed.current = true;
    void doEnable();
  }, [status, busy, fail, doEnable]);

  const doLogin = async () => {
    setBusy(true);
    setError(null);
    try {
      const r = await api.phoneLogin();
      setLogin(r);
      if (!r.ok) setError(r.message || "Couldn't start Tailscale sign-in.");
    } catch (e) {
      setError(e instanceof Error ? e.message : "Couldn't start Tailscale sign-in.");
    } finally {
      setBusy(false);
    }
  };

  const doDisable = async () => {
    userDisabled.current = true;
    setBusy(true);
    setError(null);
    try {
      const r = await api.phoneDisable();
      setFail(null);
      await load(); // resets error, so surface the failure after the reload
      if (!r.ok) setError(r.message || "Couldn't turn off phone access.");
    } catch (e) {
      setError(e instanceof Error ? e.message : "Couldn't turn off phone access.");
    } finally {
      setBusy(false);
    }
  };

  // Narrowed serving view-model; per the contract url/qrSvg are only set while
  // serving with tailscale "ok".
  const live =
    status && status.serving && status.tailscale === "ok" && status.url && status.qrSvg
      ? { url: status.url, qrSvg: status.qrSvg, server: status.server }
      : null;

  return (
    <Modal title="Open on your phone" titleLeading={<PhoneIcon />} onClose={onClose}>
      <div className="space-y-4 px-5 py-4">
        {loading ? (
          <p className="flex items-center gap-2 text-sm text-muted">
            <Spinner className="h-4 w-4" /> Checking…
          </p>
        ) : status == null ? (
          <>
            <p className="text-sm text-muted">{error ?? "Phone access isn't available on this server."}</p>
            {error && (
              <div className="flex justify-end pt-1">
                <button type="button" onClick={() => void load()} className={btnPrimary}>
                  Retry
                </button>
              </div>
            )}
          </>
        ) : status.tailscale === "missing" ? (
          <>
            <p className="text-sm text-fg">
              Phone access works over Tailscale, a free private network between your devices — it isn't installed on
              {remoteHost ? ` the remote (${remoteHost})` : " this computer"} yet.
            </p>
            <p className="text-sm text-muted">
              Install it {remoteHost ? "there" : "here"} and on your phone (same account), then check again.
            </p>
            <div className="flex justify-end gap-2 pt-1">
              <button type="button" onClick={() => void load()} className={btnGhost}>
                Check again
              </button>
              <a
                href="https://tailscale.com/download"
                target="_blank"
                rel="noreferrer noopener"
                className={`${btnPrimary} inline-block`}
              >
                Get Tailscale ↗
              </a>
            </div>
          </>
        ) : status.tailscale === "needs_login" ? (
          <>
            <p className="text-sm text-fg">
              Tailscale is installed{remoteHost ? ` on the remote (${remoteHost})` : ""} but you're not signed in
              yet.
            </p>
            <p className="text-sm text-muted">
              Sign in with your Tailscale account (it's free), then check again. You'll also want Tailscale on your
              phone, signed in to the same account.
            </p>
            {login?.message && <p className="text-sm text-muted">{login.message}</p>}
            {error && <p className="text-sm text-danger">{error}</p>}
            <div className="flex justify-end gap-2 pt-1">
              <button type="button" onClick={() => void load()} className={btnGhost}>
                Check again
              </button>
              {login?.loginUrl ? (
                <a
                  href={login.loginUrl}
                  target="_blank"
                  rel="noreferrer noopener"
                  className={`${btnPrimary} inline-block`}
                >
                  Open sign-in ↗
                </a>
              ) : login?.ok ? null : (
                <button type="button" onClick={() => void doLogin()} disabled={busy} className={btnPrimary}>
                  {busy ? "Starting…" : "Sign in to Tailscale"}
                </button>
              )}
            </div>
          </>
        ) : status.tailscale === "stopped" ? (
          <>
            <p className="text-sm text-fg">
              Tailscale is installed{remoteHost ? ` on the remote (${remoteHost})` : ""} but not running.
            </p>
            <p className="text-sm text-muted">
              Start it{remoteHost ? " there" : ""} with <code className={`${codeCls} px-1.5 py-0.5`}>tailscale up</code>,
              then check again.
            </p>
            <div className="flex justify-end pt-1">
              <button type="button" onClick={() => void load()} className={btnPrimary}>
                Check again
              </button>
            </div>
          </>
        ) : live ? (
          <>
            {/* While SSH-connected this SVG comes from the remote, so render it as an
                <img> (scripts never run) — never raw HTML. White padding for dark mode. */}
            <img
              className="mx-auto h-[200px] w-[200px] rounded-lg bg-white p-3"
              alt="QR code for the phone link"
              src={`data:image/svg+xml;utf8,${encodeURIComponent(live.qrSvg)}`}
            />
            <p className="text-center text-sm text-muted">
              Scan with your phone's camera. Works from any device signed in to your Tailscale network.
            </p>
            <p className="text-center text-xs text-faint">
              On iPhone: open the link in Safari, then Share → <b>Add to Home Screen</b>. The installed app can
              notify you when an agent finishes a turn.
            </p>
            <div className="flex items-center gap-2">
              <span className={`${codeCls} min-w-0 flex-1 select-all truncate`}>{live.url}</span>
              <CopyButton text={live.url} />
            </div>
            {error && <p className="text-sm text-danger">{error}</p>}
            <div className="flex items-center justify-between gap-3 pt-1">
              <p className="text-xs text-faint">
                Served by {remoteHost ?? "VibeStudio"}
                {live.server.version && !["dev", "0.0.0"].includes(live.server.version)
                  ? ` v${live.server.version}`
                  : ""}{" "}
                on port {live.server.port} —{" "}
                {remoteHost
                  ? "runs on the remote and stays reachable when this computer is off."
                  : "keeps running after the desktop quits; this machine must stay awake."}
              </p>
              <button type="button" onClick={() => void doDisable()} disabled={busy} className={`${btnGhost} shrink-0`}>
                {busy ? "Turning off…" : "Turn off"}
              </button>
            </div>
          </>
        ) : (
          <>
            <p className="text-sm text-fg">
              Get a QR code your phone can scan to open VibeStudio, over your Tailscale network.
            </p>
            {fail?.stage === "operator" && fail.command ? (
              <div className="space-y-2">
                <p className="text-sm text-muted">
                  Tailscale needs a one-time permission. Run this once
                  {remoteHost ? ` on the remote (${remoteHost})` : ""}, then retry:
                </p>
                <div className="flex items-center gap-2">
                  <code className={`${codeCls} min-w-0 flex-1 overflow-x-auto whitespace-nowrap`}>{fail.command}</code>
                  <CopyButton text={fail.command} />
                </div>
              </div>
            ) : fail?.stage === "consent" && fail.consentUrl ? (
              <p className="text-sm text-muted">
                Tailscale needs your approval to serve from{" "}
                {remoteHost ? `the remote (${remoteHost})` : "this computer"}. Approve it (you'll need to be an admin
                of your Tailscale network), then retry.
              </p>
            ) : fail ? (
              <p className="text-sm text-danger">{fail.message}</p>
            ) : null}
            {error && <p className="text-sm text-danger">{error}</p>}
            <div className="flex justify-end gap-2 pt-1">
              {fail?.stage === "consent" && fail.consentUrl ? (
                <>
                  <button type="button" onClick={() => void doEnable()} disabled={busy} className={btnGhost}>
                    {busy ? "Retrying…" : "Retry"}
                  </button>
                  <a
                    href={fail.consentUrl}
                    target="_blank"
                    rel="noreferrer noopener"
                    className={`${btnPrimary} inline-block`}
                  >
                    Approve on Tailscale ↗
                  </a>
                </>
              ) : (
                <button type="button" onClick={() => void doEnable()} disabled={busy} className={btnPrimary}>
                  {busy ? "Enabling…" : fail || error ? "Retry" : "Enable phone access"}
                </button>
              )}
            </div>
          </>
        )}
      </div>
    </Modal>
  );
}
