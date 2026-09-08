"use client";

import { useEffect, useState } from "react";
import { Modal } from "@/components/Modal";
import PhoneModal, { PhoneIcon } from "@/components/PhoneModal";
import { btnGhost, btnPrimary, Spinner } from "@/components/ui";
import { AddConnection, SavedConnections, ServerIcon } from "@/components/connections";
import * as api from "@/lib/api";
import { useRemote } from "@/lib/remote";
import { useSshProfiles } from "@/lib/sshProfiles";

const CONNECTING = new Set<api.RemoteState>(["detecting", "installing", "launching", "forwarding", "reconnecting"]);

/**
 * The connection control in the top chrome: a status pill ("Local" / "⟳ Connecting…"
 * / "● <host>") that opens a dialog to pick an SSH host. While connected, the entire
 * app runs on the remote (the local server proxies every `/api/*` to it). Hidden when
 * the server doesn't expose remoting (browser dev / the remote binary itself).
 */
export default function RemoteMenu() {
  const { status, available } = useRemote();
  const [open, setOpen] = useState(false);
  // The tray's `#/?phone=1` deep link is handled in AppShell, NOT here: a second
  // RemoteMenu lives inside the hidden always-mounted Sessions subtree, and its
  // copy of the listener would consume the one-shot param into an invisible modal.
  const [phoneOpen, setPhoneOpen] = useState(false);

  if (!available) return null;

  const connecting = CONNECTING.has(status.state);
  const connected = status.state === "connected";
  const errored = status.state === "error";
  const label = connected ? status.host || "Remote" : status.state === "reconnecting" ? "Reconnecting…" : connecting ? "Connecting…" : errored ? "Connection lost" : "Local";

  return (
    <>
      <button
        type="button"
        onClick={() => setOpen(true)}
        title={
          errored
            ? `${status.message || "Connection error"} — click to reconnect`
            : connected
              ? `Connected to ${status.host}`
              : "Connect to a remote host over SSH"
        }
        className={`flex items-center gap-1.5 rounded-md px-2 py-1 text-xs transition-colors ${
          connected ? "text-ok hover:bg-panel" : errored ? "text-warn hover:bg-panel" : "text-muted hover:bg-panel hover:text-fg"
        }`}
      >
        {connecting ? <Spinner className="h-3.5 w-3.5" /> : <ServerIcon />}
        <span className="hidden max-w-40 truncate sm:inline">{label}</span>
        {connected && <span className="h-1.5 w-1.5 rounded-full bg-ok" aria-hidden />}
        {errored && <span className="h-1.5 w-1.5 rounded-full bg-warn" aria-hidden />}
      </button>
      {open && (
        <RemoteDialog
          onClose={() => setOpen(false)}
          onOpenPhone={() => {
            setOpen(false);
            setPhoneOpen(true);
          }}
        />
      )}
      {phoneOpen && <PhoneModal onClose={() => setPhoneOpen(false)} />}
    </>
  );
}

/** The SSH-host connect/disconnect dialog. Exported so other surfaces (e.g. the
 *  home dashboard's Server card) can open the same control as the top-chrome pill.
 *  On the mobile app (the server has a credential store) the free-form host +
 *  ~/.ssh/config lists give way to saved connections with Keychain-held keys. */
export function RemoteDialog({ onClose, onOpenPhone }: { onClose: () => void; onOpenPhone: () => void }) {
  const { status, connect, disconnect, cancel } = useRemote();
  const [hosts, setHosts] = useState<api.RemoteHost[] | null>(null);
  const [value, setValue] = useState("");
  const [busy, setBusy] = useState(false); // disconnect in flight (reloads the page)
  const [starting, setStarting] = useState(false); // connect kickoff, before status updates
  const [error, setError] = useState<string | null>(null);

  const connecting = CONNECTING.has(status.state) || starting;
  const connected = status.state === "connected";
  const errored = status.state === "error";

  // WSL distros (Windows) are offered as `wsl:<distro>` targets; split them from the
  // ssh-config aliases so each gets its own labelled group.
  const wslHosts = (hosts ?? []).filter((h) => h.name.startsWith("wsl:"));
  const sshHosts = (hosts ?? []).filter((h) => !h.name.startsWith("wsl:"));

  // Whether this server has the phone feature — probed when the dialog opens
  // (never on page load), so the item only shows where /api/phone answers.
  const [phone, setPhone] = useState(false);
  // Saved connections (mobile only). Tri-state via the shared loader: `undefined` =
  // probe in flight (show a spinner, never the desktop form — this dialog remounts
  // on every open, so on the phone the desktop UI would otherwise flash first);
  // `null` = no credential store (desktop/standalone 404) → the desktop dialog; an
  // array = the mobile app.
  const { profiles, reload: reloadProfiles, loadError } = useSshProfiles();

  useEffect(() => {
    api.remoteList().then(setHosts).catch(() => setHosts([]));
    api.phoneStatus().then((s) => setPhone(s != null)).catch(() => setPhone(false));
  }, []);
  // Surface a saved-connections read failure (a corrupt profile file on the phone).
  useEffect(() => {
    if (loadError) setError(loadError);
  }, [loadError]);
  // Surface a backend connect failure inside the dialog.
  useEffect(() => {
    if (status.state === "error" && status.message) setError(status.message);
  }, [status.state, status.message]);

  const doConnect = async (host: string) => {
    const h = host.trim();
    if (!h) return;
    setError(null);
    setStarting(true);
    try {
      // Kicks off; the store's status drives the connecting view. On success it
      // reloads once "connected"; on failure status flips to "error" and the form
      // reappears below with the message — no stuck spinner.
      await connect(h);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Couldn't start connecting.");
    } finally {
      setStarting(false);
    }
  };

  // Abort an in-flight connect, or clear an "error" → back to Local (no reload).
  const doCancel = async () => {
    setError(null);
    await cancel();
  };

  const doDisconnect = async () => {
    setBusy(true);
    setError(null);
    try {
      await disconnect(); // reloads the page on success
    } catch (e) {
      setBusy(false);
      setError(e instanceof Error ? e.message : "Couldn't disconnect.");
    }
  };

  return (
    <Modal title="Remote host" titleLeading={<ServerIcon />} onClose={onClose}>
        <div className="space-y-4 px-5 py-4">
          {connected ? (
            <>
              <p className="text-sm text-fg">
                Connected to <span className="break-all font-mono text-ok">{status.host}</span>. The whole app — skills, files, git,
                secrets, and terminals — is running on this host.
              </p>
              <div className="flex justify-end gap-2">
                <button type="button" onClick={onClose} className={btnGhost}>
                  Close
                </button>
                <button type="button" onClick={() => void doDisconnect()} disabled={busy} className={btnPrimary}>
                  {busy ? "Disconnecting…" : "Disconnect"}
                </button>
              </div>
            </>
          ) : connecting ? (
            <>
              <p className="flex items-center gap-2 text-sm text-muted">
                <Spinner className="h-4 w-4" /> {status.message || "Connecting…"}
              </p>
              <p className="text-xs text-faint">
                Connecting to <span className="break-all font-mono">{status.host || value}</span>. First-time setup downloads a small
                server to the remote.
              </p>
              <div className="flex justify-end pt-1">
                <button type="button" onClick={() => void doCancel()} className={btnGhost}>
                  Cancel
                </button>
              </div>
            </>
          ) : profiles === undefined ? (
            <p className="flex items-center gap-2 text-sm text-muted">
              <Spinner className="h-3.5 w-3.5" /> Loading connections…
            </p>
          ) : profiles !== null ? (
            <>
              <SavedConnections
                profiles={profiles}
                onPick={doConnect}
                onChanged={() => void reloadProfiles()}
                onError={setError}
              />
              <AddConnection alwaysOpen={profiles.length === 0} onSaved={() => void reloadProfiles()} />

              {error && <p className="text-xs text-danger">{error}</p>}

              <div className="flex justify-end gap-2 pt-1">
                <button
                  type="button"
                  onClick={() => {
                    if (errored) void cancel(); // reset backend status → Local
                    onClose();
                  }}
                  className={btnGhost}
                >
                  {errored ? "Back to local" : "Close"}
                </button>
              </div>
            </>
          ) : (
            <>
              <div>
                <label className="mb-1 block text-xs font-medium uppercase tracking-wider text-muted">SSH host</label>
                <input
                  value={value}
                  onChange={(e) => setValue(e.target.value)}
                  onKeyDown={(e) => e.key === "Enter" && void doConnect(value)}
                  placeholder="user@host or a ~/.ssh/config alias"
                  spellCheck={false}
                  autoFocus
                  className="w-full rounded-md border border-border bg-surface px-2.5 py-1.5 font-mono text-xs text-fg outline-none focus:border-accent"
                />
              </div>

              {hosts === null ? (
                <p className="flex items-center gap-2 text-sm text-muted">
                  <Spinner className="h-3.5 w-3.5" /> Reading hosts…
                </p>
              ) : hosts.length > 0 ? (
                <div className="space-y-3">
                  <HostGroup label="WSL distros" hosts={wslHosts} display={(h) => h.name.replace(/^wsl:/, "")} onPick={doConnect} />
                  <HostGroup label="From ~/.ssh/config" hosts={sshHosts} display={(h) => h.name} onPick={doConnect} />
                </div>
              ) : (
                <p className="text-xs text-faint">No hosts in ~/.ssh/config — type one above.</p>
              )}

              {error && <p className="text-xs text-danger">{error}</p>}

              <div className="flex justify-end gap-2 pt-1">
                <button
                  type="button"
                  onClick={() => {
                    if (errored) void cancel(); // reset backend status → Local
                    onClose();
                  }}
                  className={btnGhost}
                >
                  {errored ? "Back to local" : "Cancel"}
                </button>
                <button type="button" onClick={() => void doConnect(value)} disabled={!value.trim()} className={btnPrimary}>
                  {errored ? "Try again" : "Connect"}
                </button>
              </div>
            </>
          )}
          {phone && (
            <div className="border-t border-border pt-2">
              <button
                type="button"
                onClick={onOpenPhone}
                className="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-sm text-muted transition-colors hover:bg-panel hover:text-fg"
              >
                <PhoneIcon className="shrink-0" />
                Open on your phone…
              </button>
            </div>
          )}
        </div>
    </Modal>
  );
}

/** A labelled list of pickable hosts; renders nothing when empty. */
function HostGroup({
  label,
  hosts,
  display,
  onPick,
}: {
  label: string;
  hosts: api.RemoteHost[];
  display: (h: api.RemoteHost) => string;
  onPick: (host: string) => void;
}) {
  if (hosts.length === 0) return null;
  return (
    <div>
      <label className="mb-1 block text-xs font-medium uppercase tracking-wider text-muted">{label}</label>
      <div className="max-h-48 space-y-1 overflow-auto">
        {hosts.map((h) => (
          <button
            key={h.name}
            type="button"
            onClick={() => void onPick(h.name)}
            className="flex w-full items-center gap-2 rounded-md border border-border px-2.5 py-1.5 text-left text-fg transition-colors hover:border-border-strong hover:bg-panel"
          >
            <ServerIcon className="shrink-0 text-muted" />
            <span className="truncate font-mono text-xs">{display(h)}</span>
            {h.detail && <span className="ml-auto truncate text-xs text-faint">{h.detail}</span>}
          </button>
        ))}
      </div>
    </div>
  );
}
