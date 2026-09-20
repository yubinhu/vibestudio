import { useState } from "react";
import { btnPrimary, btnGhost, Spinner } from "@/components/ui";
import { useUpdate, applyUpdate } from "@/lib/updates";

const DISMISS_KEY = "vibestudio-update-dismissed";

/** Floating bottom-right update prompt. "Later" hides it for that version only —
 *  a newer release re-prompts. */
export default function UpdateBanner() {
  const status = useUpdate();
  const [dismissed, setDismissed] = useState<string | null>(() => localStorage.getItem(DISMISS_KEY));
  if (!status?.available) return null;
  const version = status.available.version;
  if (dismissed === version) return null;

  const dismiss = () => {
    localStorage.setItem(DISMISS_KEY, version);
    setDismissed(version);
  };

  return (
    <div className="fixed bottom-4 right-4 z-50 max-w-sm rounded-lg border border-border bg-panel p-3 text-sm shadow-lg">
      {status.phase === "saving" ? (
        <div className="flex items-center gap-2 text-fg">
          <Spinner className="h-3.5 w-3.5" />
          <span>Saving changes…</span>
        </div>
      ) : status.phase === "downloading" ? (
        <div className="flex items-center gap-2 text-fg">
          <Spinner className="h-3.5 w-3.5" />
          <span>Downloading…{status.progress != null ? ` ${status.progress}%` : ""}</span>
        </div>
      ) : status.phase === "ready" ? (
        <div className="flex items-center gap-2 text-fg">
          <Spinner className="h-3.5 w-3.5" />
          <span>Installing and restarting…</span>
        </div>
      ) : status.phase === "error" ? (
        <div className="flex flex-col items-start gap-2">
          <div className="flex items-start gap-2">
            <span className="text-danger">{status.error || "The update failed."}</span>
            <button
              type="button"
              onClick={dismiss}
              aria-label="Dismiss"
              className="rounded-md px-1 text-muted hover:text-fg"
            >
              ✕
            </button>
          </div>
          {status.canAuto && (
            <button type="button" className={btnPrimary} onClick={() => void applyUpdate()}>
              Try again
            </button>
          )}
          <a
            href={status.releaseUrl}
            target="_blank"
            rel="noreferrer noopener"
            className={`${btnGhost} inline-block`}
          >
            Download manually ↗
          </a>
        </div>
      ) : (
        <div className="flex flex-col gap-2">
          <div>
            <div className="font-medium text-fg">Update available</div>
            <div className="text-muted">VibeStudio {version}</div>
          </div>
          <div className="flex items-center gap-2">
            {status.canAuto ? (
              <button type="button" className={btnPrimary} onClick={() => void applyUpdate()}>
                Restart to update
              </button>
            ) : (
              <a
                href={status.releaseUrl}
                target="_blank"
                rel="noreferrer noopener"
                className={`${btnPrimary} inline-block`}
              >
                Download update ↗
              </a>
            )}
            <button type="button" className={btnGhost} onClick={dismiss}>
              Later
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
