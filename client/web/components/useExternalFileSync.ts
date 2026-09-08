"use client";

import { useEffect, useRef } from "react";
import * as api from "@/lib/api";
import type { FileData } from "@/lib/types";

/** How often (ms) an open file's disk stat is polled for external writes. A stat is
 *  metadata-only (cheap, tiny over the remote tunnel); a full re-read happens only
 *  when mtime/size actually moved. */
const POLL_MS = 1500;

/**
 * Keep an open editor aware of writes made to its file by ANOTHER process — an agent
 * in a Studio terminal, `git`, a formatter, vim. Wordless autosave only ever writes,
 * so it can't notice these on its own; the window-focus trigger alone misses the
 * common case (a same-window agent edit never blurs the window). So we watch the
 * file: a cheap mtime/size poll plus a focus re-read. When the disk version differs
 * from the tag the host last loaded (`knownEtag`), `onChanged(fresh)` fires with the
 * latest file; the host decides whether to swap it in (clean buffer) or surface a
 * conflict (dirty buffer).
 *
 * No filesystem watcher — a poll rides the HTTP-only transport unchanged whether the
 * server is local or across an SSH tunnel, and the write-time compare-and-swap (not
 * this poll) is what actually guarantees no clobber; this only makes "show the
 * latest" happen on its own.
 */
export function useExternalFileSync(
  root: string,
  rel: string,
  enabled: boolean,
  knownEtag: () => string | undefined,
  onChanged: (fresh: FileData) => void,
) {
  // Live refs so the long-lived interval/listeners always see the current closures
  // without tearing down on every keystroke.
  const knownRef = useRef(knownEtag);
  const onChangedRef = useRef(onChanged);
  useEffect(() => {
    knownRef.current = knownEtag;
    onChangedRef.current = onChanged;
  });

  useEffect(() => {
    if (!enabled) return;
    let stopped = false;
    let last: { mtimeMs: number; size: number } | null = null;

    const reconcile = async () => {
      try {
        const fresh = await api.readFile(root, rel);
        const known = knownRef.current();
        // Only fire once the host has a baseline to compare against (avoids a
        // spurious reload before the initial load settles).
        if (fresh.etag && known !== undefined && fresh.etag !== known) onChangedRef.current(fresh);
      } catch {
        /* file briefly unreadable (mid-write / vanished) — retry next tick */
      }
    };
    const poll = async () => {
      try {
        const st = await api.statFile(root, rel);
        const prev = last;
        last = st;
        // The first tick (prev == null) reconciles unconditionally: a change that
        // landed between mount and now predates any stat baseline, so stat-gating
        // alone would miss it. Later ticks only re-read when the stat actually moved.
        if (prev == null || prev.mtimeMs !== st.mtimeMs || prev.size !== st.size) await reconcile();
      } catch {
        /* ignore — file may be momentarily absent during an external write */
      }
    };
    // Focus / tab-visible: re-read directly (no stat gate) so even a same-stat edit
    // is caught when the user returns to the window.
    const onFocus = () => {
      if (document.visibilityState !== "hidden") void reconcile();
    };

    const timer = setInterval(() => {
      if (!stopped) void poll();
    }, POLL_MS);
    window.addEventListener("focus", onFocus);
    window.addEventListener("vibestudio:workspace-restored", onFocus);
    document.addEventListener("visibilitychange", onFocus);
    return () => {
      stopped = true;
      clearInterval(timer);
      window.removeEventListener("focus", onFocus);
      window.removeEventListener("vibestudio:workspace-restored", onFocus);
      document.removeEventListener("visibilitychange", onFocus);
    };
  }, [enabled, root, rel]);
}
