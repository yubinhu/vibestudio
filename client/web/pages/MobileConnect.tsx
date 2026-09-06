"use client";

// The mobile app's landing + only screen while disconnected. A phone has no local
// workspace — no skills, terminals, or engine live on it — so the switchboard's job
// is purely to reach a computer over SSH. This is the Termius-style connect screen:
// big saved-connection cards to tap, and an on-device key-generating add flow. Shown
// by AppShell whenever `mobile && status !== connected`; once connected, the normal
// (remote-backed) workspace takes over, and disconnecting returns here.
import { useEffect, useState } from "react";
import { VibeStudioMark } from "@/components/VibeStudioMark";
import { Spinner } from "@/components/ui";
import { AddConnection, SavedConnections } from "@/components/connections";
import { useRemote } from "@/lib/remote";
import { useSshProfiles } from "@/lib/sshProfiles";
import type { RemoteState } from "@/lib/api";

const CONNECTING = new Set<RemoteState>(["detecting", "installing", "launching", "forwarding"]);

function Wordmark() {
  return (
    <div className="flex items-center gap-2">
      <VibeStudioMark className="h-9 w-9 shrink-0" />
      <span className="text-lg font-semibold tracking-tight text-fg">VibeStudio</span>
    </div>
  );
}

export default function MobileConnect() {
  const { status, connect, cancel } = useRemote();
  const { profiles, reload, loadError } = useSshProfiles();
  const [error, setError] = useState<string | null>(null);

  const connecting = CONNECTING.has(status.state);
  const errored = status.state === "error";

  // Surface backend connect + saved-connection read failures.
  useEffect(() => {
    if (errored && status.message) setError(status.message);
  }, [errored, status.message]);
  useEffect(() => {
    if (loadError) setError(loadError);
  }, [loadError]);

  const doConnect = (id: string) => {
    setError(null);
    void connect(id).catch((e) => setError(e instanceof Error ? e.message : "Couldn't start connecting."));
  };

  // Connecting: a focused progress view with a way out. On success remote.ts reloads
  // the page and AppShell hands over to the workspace, so we never render "connected".
  if (connecting) {
    return (
      <main className="mx-auto flex h-dvh max-w-md flex-col items-center justify-center gap-4 px-6 text-center">
        <Spinner className="h-6 w-6" />
        <div>
          <p className="text-sm text-fg">{status.message || "Connecting…"}</p>
          <p className="mt-1 text-xs text-faint">
            Connecting to <span className="break-all font-mono">{status.host}</span>. First-time setup installs a small server on the computer.
          </p>
        </div>
        <button type="button" onClick={() => void cancel()} className="text-sm text-muted underline-offset-2 hover:underline">
          Cancel
        </button>
      </main>
    );
  }

  return (
    <main className="mx-auto flex min-h-dvh max-w-md flex-col gap-6 px-6 pb-10 pt-[calc(3rem+env(safe-area-inset-top))]">
      <header className="space-y-2">
        <Wordmark />
        <h1 className="text-2xl font-bold tracking-tight text-fg">Connect to a computer</h1>
        <p className="text-sm text-muted">
          VibeStudio runs your agents and skills on your own machine. Add an SSH connection to reach it from here.
        </p>
      </header>

      {error && (
        <p className="rounded-lg border border-danger/40 bg-danger/10 px-3 py-2 text-sm text-danger" role="alert">
          {error}
        </p>
      )}

      {profiles === undefined ? (
        <p className="flex items-center gap-2 text-sm text-muted">
          <Spinner className="h-4 w-4" /> Loading connections…
        </p>
      ) : (
        <>
          <SavedConnections
            profiles={profiles ?? []}
            onPick={doConnect}
            onChanged={() => void reload()}
            onError={setError}
            large
          />
          <AddConnection alwaysOpen={(profiles ?? []).length === 0} onSaved={() => void reload()} large />
        </>
      )}
    </main>
  );
}
