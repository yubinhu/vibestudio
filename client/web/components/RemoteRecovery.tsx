import { useState } from "react";
import { useRemote } from "@/lib/remote";
import { btnGhost, btnPrimary, Spinner } from "./ui";

/** The existing workspace remains mounted and visible beneath this input gate. */
export default function RemoteRecovery() {
  const { status, workspaceHost, retry, disconnect } = useRemote();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const run = async (action: () => Promise<void>) => {
    setBusy(true);
    setError(null);
    try { await action(); }
    catch (e) { setError(e instanceof Error ? e.message : "The connection could not be updated."); }
    finally { setBusy(false); }
  };
  const attention = status.state === "error";
  return (
    <div className="fixed inset-0 z-[100] flex items-start justify-center bg-bg/35 px-4 pt-[max(5rem,env(safe-area-inset-top))]">
      <section role="alertdialog" aria-modal="true" aria-labelledby="remote-recovery-title" aria-describedby="remote-recovery-detail"
        className="w-full max-w-md rounded-xl border border-border bg-surface p-5 shadow-xl">
        <div className="flex items-center gap-2">
          {!attention && <Spinner className="h-4 w-4" />}
          <h2 id="remote-recovery-title" className="text-sm font-semibold text-fg">
            {attention ? "Connection needs attention" : "Reconnecting…"}
          </h2>
        </div>
        <p className="mt-2 break-all font-mono text-xs text-muted">{workspaceHost}</p>
        <p id="remote-recovery-detail" className="mt-2 text-sm text-muted">
          {status.message || "Waiting for the remote machine."} Your current view is preserved. Input is paused until the connection returns.
        </p>
        {error && <p role="alert" className="mt-2 text-xs text-danger">{error}</p>}
        <div className="mt-4 flex justify-end gap-2">
          <button type="button" disabled={busy} className={btnGhost} onClick={() => void run(disconnect)}>Disconnect</button>
          <button type="button" disabled={busy} className={btnPrimary} onClick={() => void run(retry)}>Retry</button>
        </div>
      </section>
    </div>
  );
}
