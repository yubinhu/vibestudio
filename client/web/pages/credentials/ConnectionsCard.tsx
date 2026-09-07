"use client";

import { useCallback, useEffect, useState } from "react";
import { Modal } from "@/components/Modal";
import { Spinner, btnGhost, btnPrimary } from "@/components/ui";
import * as api from "@/lib/api";
import type { ConnectionInfo } from "@/lib/api";

/** A few well-known OAuth-protected MCP servers offered as quick-add chips — a
 *  sample, NOT the supported set: any server that speaks the MCP OAuth flow
 *  works via the URL field. Rows also key into this by host for the capability
 *  line + access pill; any other host gets a generic line and no pill. All
 *  URLs were live-verified against VibeStudio's anonymous-DCR + PKCE flow. */
const CATALOG = [
  {
    label: "Linear",
    url: "https://mcp.linear.app/mcp",
    capability: "Can create and update issues, projects, and comments in your Linear workspace.",
    pill: "Write access",
  },
  {
    label: "Notion",
    url: "https://mcp.notion.com/mcp",
    capability: "Can search, read, and edit pages and databases in your Notion workspace.",
    pill: "Write access",
  },
  {
    label: "Sentry",
    url: "https://mcp.sentry.dev/mcp",
    capability: "Can query, triage, and resolve errors and releases in your Sentry account.",
    pill: "Write access",
  },
  {
    label: "Cloudflare",
    url: "https://observability.mcp.cloudflare.com/mcp",
    capability: "Can read your Cloudflare Workers logs, analytics, and observability data.",
    pill: "",
  },
  {
    label: "Stripe",
    url: "https://mcp.stripe.com/",
    capability: "Can create customers, payment links, invoices, and refunds in your Stripe account.",
    pill: "Payment access",
  },
  {
    label: "Canva",
    url: "https://mcp.canva.com/mcp",
    capability: "Can read and edit designs, folders, and brand assets in your Canva account.",
    pill: "Write access",
  },
  {
    label: "Robinhood Trading",
    url: "https://agent.robinhood.com/mcp/trading",
    capability: "Can view your portfolio and place trades in your Agentic account.",
    pill: "Trading access",
  },
  {
    label: "Robinhood Banking",
    url: "https://banking-agent.robinhood.com/mcp/banking",
    capability: "Can view and manage your Agentic credit card.",
    pill: "Card access",
  },
];

/** Prefer the server's human `message` (carried as `detail` by the http
 *  helper) over the machine `error` code the Error message holds. */
function errText(e: unknown, fallback: string): string {
  const detail = (e as { detail?: unknown } | null)?.detail;
  if (typeof detail === "string") return detail;
  return e instanceof Error ? e.message : fallback;
}

// Open in the system browser: the synthetic-anchor variant of the
// _blank + noopener links the GitHub device flow uses (wry hands
// _blank navigations to the OS; in a plain browser it's a new tab).
function openExternal(url: string) {
  const a = document.createElement("a");
  a.href = url;
  a.target = "_blank";
  a.rel = "noreferrer noopener";
  document.body.appendChild(a);
  a.click();
  a.remove();
}

/** The begin → open-browser → poll loop, shared by Add and Reconnect: add mode
 *  starts on the URL form; reconnect mode begins as soon as the dialog opens. */
export function ConnectDialog({
  reconnect,
  onClose,
  onDone,
}: {
  /** When set, redo OAuth for this connection instead of adding a new one. */
  reconnect?: ConnectionInfo;
  onClose: () => void;
  onDone: () => void;
}) {
  const [url, setUrl] = useState("");
  const [label, setLabel] = useState<string | undefined>(undefined);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  // A begun attempt: the browser holds the consent page; we poll its `state`.
  const [attempt, setAttempt] = useState<api.ConnectionBegin | null>(null);

  const start = useCallback(async (req: Promise<api.ConnectionBegin>) => {
    setBusy(true);
    setErr(null);
    try {
      const r = await req;
      openExternal(r.authorizeUrl);
      setAttempt(r);
    } catch (e) {
      setErr(errText(e, "Couldn’t start the sign-in"));
    } finally {
      setBusy(false);
    }
  }, []);

  useEffect(() => {
    if (reconnect) void start(api.connectionReconnect(reconnect.id, window.location.origin));
  }, [reconnect, start]);

  // Poll while the attempt is pending: done → hand off to the parent (which
  // closes + refreshes); denied/expired → drop the attempt, explain inline.
  useEffect(() => {
    if (!attempt) return;
    let stopped = false;
    const timer = setInterval(() => {
      api
        .connectionPending(attempt.state)
        .then((p) => {
          if (stopped || p.status === "waiting") return;
          if (p.status === "done") {
            onDone();
            return;
          }
          setAttempt(null);
          setErr(p.status === "denied" ? "You declined — nothing was saved." : "That sign-in attempt expired. Try again.");
        })
        .catch(() => {
          /* transient poll failure — keep trying */
        });
    }, 1000);
    return () => {
      stopped = true;
      clearInterval(timer);
    };
  }, [attempt, onDone]);

  return (
    <Modal title={reconnect ? `Reconnect ${reconnect.label}` : "Add connector"} onClose={onClose}>
      {attempt ? (
        <div className="space-y-3 px-5 py-4">
          <p className="text-sm text-muted">Approve the sign-in in your browser. If no tab opened, use the button below.</p>
          <p className="flex items-center gap-2 text-xs text-faint">
            <Spinner className="h-3 w-3" /> Waiting for approval…
          </p>
          <div className="flex justify-end gap-2 pt-1">
            <button type="button" onClick={onClose} className={btnGhost}>
              Cancel
            </button>
            <button type="button" onClick={() => openExternal(attempt.authorizeUrl)} className={btnPrimary}>
              Open sign-in page
            </button>
          </div>
        </div>
      ) : reconnect ? (
        <div className="space-y-3 px-5 py-4">
          {err ? (
            <>
              <p className="text-xs text-danger">{err}</p>
              <div className="flex justify-end gap-2 pt-1">
                <button type="button" onClick={onClose} className={btnGhost}>
                  Cancel
                </button>
                <button
                  type="button"
                  disabled={busy}
                  onClick={() => void start(api.connectionReconnect(reconnect.id, window.location.origin))}
                  className={btnPrimary}
                >
                  Try again
                </button>
              </div>
            </>
          ) : (
            <p className="flex items-center gap-2 text-sm text-muted">
              <Spinner className="h-3.5 w-3.5" /> Starting sign-in…
            </p>
          )}
        </div>
      ) : (
        <form
          onSubmit={(e) => {
            e.preventDefault();
            if (!url.trim() || busy) return;
            void start(api.connectionBegin(url.trim(), window.location.origin, label));
          }}
          className="space-y-4 px-5 py-4"
        >
          <div className="space-y-1.5">
            <label htmlFor="mcp-url" className="text-xs font-medium uppercase tracking-wide text-faint">
              MCP server URL
            </label>
            <input
              id="mcp-url"
              value={url}
              onChange={(e) => {
                // A hand-edited URL is no longer the chip's service — drop its label.
                setUrl(e.target.value);
                setLabel(undefined);
              }}
              placeholder="https://mcp.example.com/mcp"
              spellCheck={false}
              autoFocus
              className="w-full rounded-md border border-border bg-surface px-2.5 py-1.5 font-mono text-sm text-fg outline-none placeholder:font-sans focus:border-accent"
            />
            <p className="text-xs text-faint">
              Any OAuth-protected MCP server works — you’ll sign in through your browser.
            </p>
          </div>
          <div className="space-y-1.5">
            <p className="text-xs font-medium uppercase tracking-wide text-faint">Popular servers</p>
            <div className="flex flex-wrap gap-1.5">
              {CATALOG.map((c) => (
                <button
                  key={c.url}
                  type="button"
                  onClick={() => {
                    setUrl(c.url);
                    setLabel(c.label);
                    setErr(null);
                  }}
                  className={`rounded-full border px-2.5 py-1 text-xs transition-colors ${
                    url === c.url ? "border-accent text-accent" : "border-border text-muted hover:text-fg"
                  }`}
                >
                  {c.label}
                </button>
              ))}
            </div>
          </div>
          {err && <p className="text-xs text-danger">{err}</p>}
          <div className="flex justify-end gap-2 pt-1">
            <button type="button" onClick={onClose} className={btnGhost}>
              Cancel
            </button>
            <button type="submit" disabled={busy || !url.trim()} className={btnPrimary}>
              {busy ? "Connecting…" : "Connect"}
            </button>
          </div>
        </form>
      )}
    </Modal>
  );
}
