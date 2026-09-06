"use client";

import NavBar from "@/components/NavBar";
import LocalStoreCard from "./LocalStoreCard";
import ConnectionsCard from "./ConnectionsCard";
import ProviderGallery from "./ProviderGallery";

/**
 * The dedicated Connectors page (route: /connectors). A provider dashboard: the active
 * machine-local store up top, with future managed providers (1Password, Doppler,
 * a team cloud) queued in a quiet gallery below. Reachable from the Home nav bar
 * and the studio Manage drawer.
 */
export function Component() {
  return (
    <div className="flex min-h-dvh flex-col">
      <NavBar
        breadcrumb={
          <>
            <span className="text-faint" aria-hidden>
              /
            </span>
            <span className="truncate font-medium text-fg">Connectors</span>
          </>
        }
      />

      <main className="mx-auto w-full max-w-3xl flex-1 px-6 pb-24 pt-10">
        <h1 className="text-2xl font-semibold tracking-tight text-fg">Connectors</h1>
        <p className="mt-1.5 max-w-prose text-sm text-muted">
          Connect services and store API keys for your agents. This page shows connectors and keys managed by
          VibeStudio on the active server. Connectors configured in other apps aren’t discovered here yet.
        </p>

        <div className="mt-8">
          <LocalStoreCard />
        </div>

        <div className="mt-6">
          <ConnectionsCard />
        </div>

        <ProviderGallery />
      </main>
    </div>
  );
}
