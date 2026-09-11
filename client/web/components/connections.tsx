"use client";

// Saved SSH connections (mobile). Shared by two surfaces: the top-chrome RemoteMenu
// dialog (compact, shown when already connected — to switch/disconnect) and the
// full-screen MobileConnect landing (large, Termius-style — the phone's first
// entry). Each connection is a profile whose private key lives in the OS keystore;
// managed over the pinned-local /api/remote/profiles* + /api/ssh/keygen routes.
import { useState } from "react";
import { btnGhost, btnPrimary } from "@/components/ui";
import { useConfirm } from "@/components/useConfirm";
import * as api from "@/lib/api";
import { sshKeyInstallCommands } from "@/lib/sshKeyCommands";
import { copyText } from "@/lib/copyText";

export function ServerIcon({ className = "", size = 14 }: { className?: string; size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden className={className}>
      <rect x="3" y="4" width="18" height="7" rx="1.5" />
      <rect x="3" y="13" width="18" height="7" rx="1.5" />
      <path d="M7 7.5h.01M7 16.5h.01" />
    </svg>
  );
}

/** The saved-connection list: tap a card to connect; a trailing ✕ removes the
 *  profile AND its keystore-held key. Renders nothing when empty — the add form is
 *  the empty state. `large` = the full-screen touch styling (bigger cards + hit
 *  targets); default = the compact dialog rows. */
export function SavedConnections({
  profiles,
  onPick,
  onChanged,
  onError,
  large = false,
}: {
  profiles: api.SshProfile[];
  onPick: (id: string) => void;
  onChanged: () => void;
  onError: (msg: string) => void;
  large?: boolean;
}) {
  const confirm = useConfirm();
  if (profiles.length === 0) return null;
  const remove = async (p: api.SshProfile) => {
    const ok = await confirm({
      title: "Remove this connection?",
      body: `${p.id} and its key are removed from this device. The server's authorized_keys entry stays until you delete it there.`,
      confirmLabel: "Remove",
      danger: true,
    });
    if (!ok) return;
    try {
      await api.sshProfileDelete(p.id);
    } catch (e) {
      // A keystore delete can fail (400); say so rather than silently leaving the
      // entry after the confirm dialog closes.
      onError(e instanceof Error ? e.message : "Couldn't remove the connection.");
    } finally {
      onChanged();
    }
  };
  const card = large
    ? "flex min-w-0 flex-1 items-center gap-3 rounded-xl border border-border px-4 py-3.5 text-left text-fg transition-colors hover:border-border-strong hover:bg-panel active:bg-panel"
    : "flex min-w-0 flex-1 items-center gap-2 rounded-md border border-border px-2.5 py-1.5 text-left text-fg transition-colors hover:border-border-strong hover:bg-panel";
  const del = large
    ? "shrink-0 rounded-lg px-3 py-3 text-base text-muted transition-colors hover:bg-panel hover:text-danger"
    : "shrink-0 rounded-md px-2 py-1.5 text-xs text-muted transition-colors hover:bg-panel hover:text-danger";
  return (
    <div>
      {!large && <label className="mb-1 block text-xs font-medium uppercase tracking-wider text-muted">Saved connections</label>}
      <div className={large ? "space-y-2" : "max-h-48 space-y-1 overflow-auto"}>
        {profiles.map((p) => (
          <div key={p.id} className="flex items-center gap-1">
            <button type="button" onClick={() => void onPick(p.id)} className={card}>
              <ServerIcon size={large ? 18 : 14} className="shrink-0 text-muted" />
              <span className={large ? "truncate font-mono text-sm" : "truncate font-mono text-xs"}>{p.id}</span>
              {large && (
                <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden className="ml-auto shrink-0 text-faint">
                  <path d="M9 18l6-6-6-6" />
                </svg>
              )}
            </button>
            <button type="button" onClick={() => void remove(p)} title="Remove this connection" aria-label={`Remove ${p.id}`} className={del}>
              ✕
            </button>
          </div>
        ))}
      </div>
    </div>
  );
}

/** The Termius-style add-connection flow: host/user/port, generate an ed25519 key
 *  ON DEVICE, paste the shown public key into the server's authorized_keys, save.
 *  The private key goes straight from the keygen response into the keystore via the
 *  save route — it is never displayed. `large` = full-screen touch styling. */
export function AddConnection({
  alwaysOpen,
  onSaved,
  large = false,
}: {
  alwaysOpen: boolean;
  onSaved: () => void;
  large?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const [host, setHost] = useState("");
  const [user, setUser] = useState("");
  const [port, setPort] = useState("22");
  const [key, setKey] = useState<api.GeneratedSshKey | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const field = large
    ? "w-full rounded-lg border border-border bg-surface px-3 py-2.5 font-mono text-sm text-fg outline-none focus:border-accent"
    : "w-full rounded-md border border-border bg-surface px-2.5 py-1.5 font-mono text-xs text-fg outline-none focus:border-accent";
  const primary = large ? `${btnPrimary} w-full py-2.5 text-base` : btnPrimary;

  if (!alwaysOpen && !open) {
    return (
      <button type="button" onClick={() => setOpen(true)} className={large ? `${btnGhost} w-full py-2.5 text-base` : btnGhost}>
        Add connection…
      </button>
    );
  }

  const generate = async () => {
    setBusy(true);
    setError(null);
    try {
      const label = user.trim() && host.trim() ? `vibestudio-${user.trim()}@${host.trim()}` : "vibestudio";
      const generated = await api.sshKeygen(label);
      // Validate before rendering/copying a response; malformed key data belongs
      // in the existing error state, not in the user's terminal command.
      sshKeyInstallCommands(generated.publicKey);
      setKey({ ...generated, publicKey: generated.publicKey.trim() });
    } catch (e) {
      setError(e instanceof Error ? e.message : "Couldn't generate a key.");
    } finally {
      setBusy(false);
    }
  };

  const save = async () => {
    if (!key) return;
    // Validate the port here rather than silently coercing junk to 22 (which would
    // save a profile dialing a port the user never typed, then fail opaquely on
    // connect). The server rejects it too, but a specific message is friendlier.
    const portNum = Number(port.trim());
    if (!Number.isInteger(portNum) || portNum < 1 || portNum > 65535) {
      setError("Port must be a whole number between 1 and 65535.");
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await api.sshProfileSave({ host: host.trim(), user: user.trim(), port: portNum, privateKey: key.privateKey });
      setHost("");
      setUser("");
      setPort("22");
      setKey(null);
      setOpen(false);
      onSaved();
    } catch (e) {
      setError(e instanceof Error ? e.message : "Couldn't save the connection.");
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className={large ? "space-y-4 rounded-xl border border-border p-4" : "space-y-3 rounded-md border border-border p-3"}>
      <label className="block text-xs font-medium uppercase tracking-wider text-muted">Add connection</label>
      <div className="flex gap-2">
        <div className="min-w-0 flex-1">
          <label className="mb-1 block text-xs text-muted">Host</label>
          <input value={host} onChange={(e) => setHost(e.target.value)} placeholder="pi.local" spellCheck={false} autoCapitalize="none" autoCorrect="off" className={field} />
        </div>
        <div className={large ? "w-20 shrink-0" : "w-16 shrink-0"}>
          <label className="mb-1 block text-xs text-muted">Port</label>
          <input value={port} onChange={(e) => setPort(e.target.value)} inputMode="numeric" className={field} />
        </div>
      </div>
      <div>
        <label className="mb-1 block text-xs text-muted">User</label>
        <input value={user} onChange={(e) => setUser(e.target.value)} placeholder="harvey" spellCheck={false} autoCapitalize="none" autoCorrect="off" className={field} />
      </div>

      {key ? (
        <div className="space-y-2">
          <p className="text-xs text-muted">
            On <span className="break-all font-mono">{host.trim() || "your server"}</span>, open a terminal
            as <span className="break-all font-mono">{user.trim() || "your SSH login user"}</span> and paste
            these commands. Then save this connection below.
          </p>
          <pre className="max-h-56 select-text overflow-auto whitespace-pre-wrap break-all rounded-md border border-border bg-panel px-2.5 py-2 font-mono text-[11px] text-fg">{sshKeyInstallCommands(key.publicKey)}</pre>
          <p className="text-xs text-faint">
            Adds the key to <span className="font-mono">~/.ssh/authorized_keys</span> and sets its permissions.
            Existing keys are kept; running it again won’t add a duplicate.
          </p>
          <div className="flex flex-wrap items-center gap-2">
            <CopyButton text={sshKeyInstallCommands(key.publicKey)} label="Copy commands" />
            <CopyButton text={key.publicKey} label="Copy public key" />
          </div>
          <details className="text-xs text-muted">
            <summary className="cursor-pointer py-1">Show public key</summary>
            <div className="mt-1 select-text break-all rounded-md border border-border bg-panel px-2.5 py-1.5 font-mono text-[11px] text-fg">{key.publicKey}</div>
            <p className="mt-1 select-text break-all text-[11px] text-faint">{key.fingerprint}</p>
          </details>
        </div>
      ) : (
        <p className="text-xs text-faint">The key is created on this device; only its public half ever leaves it.</p>
      )}

      {error && <p className="text-xs text-danger">{error}</p>}

      {key === null ? (
        <button type="button" onClick={() => void generate()} disabled={busy} className={primary}>
          {busy ? "Generating…" : "Generate key"}
        </button>
      ) : (
        <button type="button" onClick={() => void save()} disabled={busy || !host.trim() || !user.trim()} className={primary}>
          {busy ? "Saving…" : "Save connection"}
        </button>
      )}
    </div>
  );
}

export function CopyButton({ text, label = "Copy" }: { text: string; label?: string }) {
  const [status, setStatus] = useState<"idle" | "copied" | "failed">("idle");
  const copy = async () => {
    // Only assert "Copied" once the write actually resolves — in a WKWebview the
    // Clipboard API can be absent or reject (NotAllowedError), and this is the
    // load-bearing paste-into-authorized_keys step, so a false "Copied" is a trap.
    try {
      await copyText(text);
      setStatus("copied");
    } catch {
      setStatus("failed");
    }
  };
  return (
    <span className="inline-flex flex-col items-start gap-1">
      <button type="button" className={btnGhost} onClick={() => void copy()} aria-live="polite">
        {status === "copied" ? "Copied" : status === "failed" ? "Copy failed — retry" : label}
      </button>
      {status === "failed" && (
        <span className="text-xs text-danger" role="status">
          Select the commands or expand “Show public key” to copy the text manually.
        </span>
      )}
    </span>
  );
}
