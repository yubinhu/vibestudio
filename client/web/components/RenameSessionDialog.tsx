"use client";

import { useEffect, useId, useRef, useState } from "react";
import { Modal } from "@/components/Modal";
import { btnGhost, btnPrimary } from "@/components/ui";
import * as api from "@/lib/api";
import * as sessions from "@/lib/sessions";
import { sessionTitle } from "@/lib/sessionTitle";

/** Keep the draft local to this dialog so session-list polls never replace it. */
export default function RenameSessionDialog({
  session,
  onClose,
}: {
  session: api.TermSession;
  onClose: () => void;
}) {
  const [title, setTitle] = useState(() => sessionTitle(session));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const alive = useRef(true);
  const dialogRef = useRef<HTMLDivElement>(null);
  const opener = useRef(document.activeElement as HTMLElement | null);
  const inputId = useId();
  const helpId = useId();
  const errorId = useId();
  const titleLength = Array.from(title.trim()).length;

  useEffect(() => {
    return () => {
      alive.current = false;
      // Return to the originating session row, Home card or compact picker.
      const trigger = opener.current;
      if (trigger?.isConnected) trigger.focus();
    };
  }, []);

  const save = async (nextTitle: string | null) => {
    if (busy) return;
    if (nextTitle !== null && (!nextTitle.trim() || Array.from(nextTitle.trim()).length > 200)) {
      setError("Enter a title between 1 and 200 characters.");
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await api.terminalRename(session.id, nextTitle?.trim() ?? null);
      await sessions.refresh();
      if (alive.current) onClose();
    } catch (e) {
      if (alive.current) {
        setError(e instanceof Error ? e.message : "Couldn't rename the session.");
        setBusy(false);
      }
    }
  };

  return (
    <div
      ref={dialogRef}
      role="dialog"
      aria-modal="true"
      aria-label="Rename session"
      onKeyDown={(e) => {
        if (e.key !== "Tab") return;
        const controls = dialogRef.current?.querySelectorAll<HTMLElement>("button:not(:disabled), input:not(:disabled)");
        if (!controls?.length) { e.preventDefault(); return; }
        const first = controls[0];
        const last = controls[controls.length - 1];
        if (e.shiftKey && document.activeElement === first) { e.preventDefault(); last.focus(); }
        if (!e.shiftKey && document.activeElement === last) { e.preventDefault(); first.focus(); }
      }}
    >
    <Modal title="Rename session" onClose={onClose} dismissDisabled={busy}>
      <form
        className="space-y-4 px-5 py-4"
        onSubmit={(e) => {
          e.preventDefault();
          void save(title);
        }}
      >
        <div>
          <label htmlFor={inputId} className="mb-1 block text-xs font-medium text-muted">Session title</label>
          <input
            id={inputId}
            value={title}
            onChange={(e) => { setTitle(e.target.value); setError(null); }}
            onFocus={(e) => e.currentTarget.select()}
            maxLength={400}
            disabled={busy}
            autoFocus
            aria-describedby={error ? `${helpId} ${errorId}` : helpId}
            aria-invalid={!!error}
            className="w-full rounded-md border border-border bg-surface px-2.5 py-2 text-sm text-fg outline-none focus:border-accent disabled:opacity-50"
          />
          <p id={helpId} className="mt-2 text-xs text-muted">Give this session a name you can recognize.</p>
        </div>
        {error && <p id={errorId} role="alert" className="text-xs text-danger">{error}</p>}
        {!error && titleLength > 200 && <p role="alert" className="text-xs text-danger">Use 200 characters or fewer.</p>}
        {session.customTitle?.trim() && (
          <button
            type="button"
            onClick={() => void save(null)}
            disabled={busy}
            className="rounded text-xs text-accent underline-offset-2 hover:underline focus-visible:outline focus-visible:outline-accent disabled:opacity-40"
          >
            Use automatic title
          </button>
        )}
        <div className="flex justify-end gap-2">
          <button type="button" onClick={onClose} disabled={busy} className={btnGhost}>Cancel</button>
          <button type="submit" disabled={busy || titleLength === 0 || titleLength > 200} className={btnPrimary}>
            {busy ? "Saving…" : "Save"}
          </button>
        </div>
      </form>
    </Modal>
    </div>
  );
}
