"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import * as api from "@/lib/api";
import { log } from "@/lib/log";

/** Reject pasted images larger than this client-side, matching the server cap in
 *  `save_pasted_image`, so an over-limit paste fails instantly instead of after a
 *  full encode-and-upload through the tunnel. */
const MAX_PASTE_BYTES = 32 * 1024 * 1024;

/** A tmux copy-mode copy (OSC 52) replayed via the copy chord stays valid this
 *  long — fresh enough to mean "the thing I just selected", stale after that. */
const COPY_STASH_MS = 60_000;

const IS_MAC = /Mac|iPhone|iPad/.test(navigator.userAgent);

/** Touch panning: xterm 6 has no touch support, so vertical pans are converted
 *  into synthetic wheel ticks — one per this many pixels of pan. */
const TOUCH_SCROLL_PX = 25;
/** A pan must move this far before committing to an axis (vertical = scroll,
 *  horizontal = ignored). */
const TOUCH_AXIS_LOCK_PX = 6;
/** Within this many px of the terminal's top/bottom edge, a select drag nudges
 *  xterm's own drag-scroll so the selection can reach into off-screen scrollback —
 *  a finger is already selecting, so there's no second gesture free to scroll. */
const SELECT_EDGE_SCROLL_PX = 40;
/** Flick-to-coast (iOS-style momentum): after the finger lifts, keep scrolling at
 *  the release velocity and decay it each frame. All three are feel knobs — tune
 *  on-device. Velocities are px/ms (signed like the pan delta). */
const TOUCH_FLING_MIN_VELOCITY = 0.2; // below this at release, don't coast at all
const TOUCH_FLING_FRICTION = 0.95; // fraction of velocity kept per 16ms frame
const TOUCH_FLING_STOP_VELOCITY = 0.02; // coast ends once velocity decays past this

/** No real pane is ever this small — below it is a transient layout artifact.
 *  skill-term enforces the same floor as a backstop. */
const MIN_COLS = 20;
const MIN_ROWS = 5;

/** Write to the system clipboard from inside a user gesture (the copy chord). The
 *  synchronous hidden-textarea execCommand copy goes FIRST: it lands within the
 *  gesture on WKWebView and on insecure-context LAN origins (which have no
 *  navigator.clipboard at all), where the async Clipboard API is gesture-gated and
 *  rejects a tick too late for a fallback to recover. Only if the legacy command is
 *  unavailable (some browsers are retiring it) do we reach for the async API. */
function copyText(text: string, refocus: () => void): void {
  let copied = false;
  const ta = document.createElement("textarea");
  ta.value = text;
  ta.style.position = "fixed";
  ta.style.opacity = "0";
  document.body.appendChild(ta);
  ta.select();
  try {
    copied = document.execCommand("copy");
  } catch {
    copied = false;
  }
  ta.remove();
  refocus();
  if (!copied) navigator.clipboard?.writeText(text).catch(() => {});
}

// Pull terminal colors from the app's CSS variables so it tracks the theme.
function themeFromCss(): Record<string, string | undefined> {
  const css = getComputedStyle(document.documentElement);
  const v = (n: string) => {
    const s = css.getPropertyValue(n).trim();
    return s || undefined;
  };
  return {
    background: v("--surface") ?? v("--bg"),
    foreground: v("--fg"),
    cursor: v("--accent"),
    cursorAccent: v("--surface"),
    selectionBackground: v("--sel"),
  };
}

/**
 * A live terminal view bound to one tmux-backed session. Mounting attaches;
 * unmounting detaches (the session keeps running). Remount with a new `id`
 * (via `key`) to switch sessions.
 */
export default function TerminalPane({ id, visible = true }: { id: string; visible?: boolean }) {
  const hostRef = useRef<HTMLDivElement>(null);
  // Refreshed on each (re)attach; invoked when the pane becomes visible again.
  const refitRef = useRef<() => void>(() => {});
  const termRef = useRef<Terminal | null>(null);

  // Select mode: while on, a plain drag makes a NATIVE xterm selection instead of
  // being forwarded to the agent's own mouse handling — the only way to drag-copy a
  // region out of a full-screen, mouse-grabbing TUI (opencode) without tmux. The ref
  // is what the (effect-scoped) shouldForceSelection override reads on each
  // mousedown; the state drives the toggle's appearance. Toggling it never re-runs
  // the attach effect (deps are stable), so the terminal is not torn down.
  const selectModeRef = useRef(false);
  const [selectMode, setSelectMode] = useState(false);
  const applySelectMode = useCallback((on: boolean) => {
    selectModeRef.current = on;
    setSelectMode(on);
    const term = termRef.current;
    const ta = term?.textarea;
    if (!term || !ta) return;
    // On a phone the soft keyboard would just bury the text you're selecting, and
    // xterm re-focuses its input on every mousedown — so a select drag would keep
    // re-summoning it. inputmode=none lets xterm hold focus without iOS raising the
    // keyboard; blur closes it if it was already up. Fine pointers keep real focus
    // so the copy chord and Esc-to-exit still work (they have no soft keyboard).
    if (window.matchMedia("(pointer: coarse)").matches) {
      ta.inputMode = on ? "none" : "";
      ta.blur();
    } else {
      term.focus();
    }
  }, []);

  // Coarse-pointer tap affordances: chords and native clipboard events don't
  // exist under a finger. The attach effect publishes the pieces they need.
  const [canCopy, setCanCopy] = useState(false);
  const tapOpsRef = useRef<{
    copySource: () => string;
    pasteImage: (blob: Blob, mime: string) => Promise<void>;
    note: (msg: string) => void;
  } | null>(null);

  /** Async-clipboard first — the inverse of the chord's copyText — because iOS
   *  Safari ignores textarea.select(), letting execCommand "succeed" while
   *  copying nothing. Inside a tap gesture writeText is allowed on secure
   *  origins; copyText stays the fallback where navigator.clipboard is absent. */
  const tapCopy = useCallback(() => {
    const term = termRef.current;
    const text = tapOpsRef.current?.copySource() ?? "";
    if (!term || !text) return;
    if (navigator.clipboard?.writeText) {
      navigator.clipboard.writeText(text).catch(() => copyText(text, () => term.focus()));
    } else {
      copyText(text, () => term.focus());
    }
    term.clearSelection();
    term.focus();
  }, []);

  /** The tap is the user gesture clipboard.read() demands. Images ride the same
   *  backend upload as the paste-event path; text goes straight into the pty. */
  const tapPaste = useCallback(() => {
    const term = termRef.current;
    const ops = tapOpsRef.current;
    if (!term || !ops) return;
    void (async () => {
      try {
        if (navigator.clipboard?.read) {
          const [item] = await navigator.clipboard.read();
          const imgType = item?.types.find((t) => t.startsWith("image/"));
          // Text wins when both are present, matching the paste-event path.
          if (item?.types.includes("text/plain")) {
            const text = await (await item.getType("text/plain")).text();
            if (text) term.paste(text);
          } else if (item && imgType) {
            await ops.pasteImage(await item.getType(imgType), imgType);
          }
          term.focus();
          return;
        }
      } catch {
        // read() denied or exotic types — fall through to the text-only API.
      }
      try {
        const text = await navigator.clipboard.readText();
        if (text) term.paste(text);
        term.focus();
      } catch {
        ops.note("clipboard read blocked — allow clipboard access for this site and retry");
      }
    })();
  }, []);

  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;

    const term = new Terminal({
      cursorBlink: true,
      fontSize: 13,
      fontFamily: "'JetBrains Mono', ui-monospace, SFMono-Regular, Menlo, Consolas, monospace",
      scrollback: 8000,
      // `mouse on` (server-side) lets the wheel scroll tmux scrollback, but means a
      // plain drag is sent to the agent rather than selected. The Select toggle and
      // Shift/Option+drag both force a native xterm selection instead — see the
      // shouldForceSelection override after open().
      macOptionClickForcesSelection: true,
      theme: themeFromCss(),
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    // tmux owns the scrollback (wheel → copy-mode); copying there arrives as an
    // OSC 52 write — honor it so copy-mode copies land on the system clipboard.
    // Releasing a mouse-drag IS the copy in tmux: there is no Ctrl+C step.
    let lastCopy = { text: "", at: 0 };
    // canCopy shows the coarse-pointer Copy button: a native selection, or a
    // stash that hasn't aged out yet — re-checked when it does.
    let staleTimer: ReturnType<typeof setTimeout> | undefined;
    const updateCanCopy = () => {
      const fresh = Date.now() - lastCopy.at < COPY_STASH_MS;
      setCanCopy(term.hasSelection() || fresh);
      clearTimeout(staleTimer);
      if (fresh) staleTimer = setTimeout(updateCanCopy, lastCopy.at + COPY_STASH_MS - Date.now());
    };
    const selSub = term.onSelectionChange(updateCanCopy);
    term.parser.registerOscHandler(52, (data) => {
      const semi = data.indexOf(";");
      const payload = semi < 0 ? "" : data.slice(semi + 1);
      if (!payload || payload === "?") return true; // never answer clipboard *reads*
      let text: string;
      try {
        text = new TextDecoder().decode(Uint8Array.from(atob(payload), (c) => c.charCodeAt(0)));
      } catch {
        return true; // malformed base64
      }
      // Stash first: this write fires from the SSE stream (no user gesture), and
      // WKWebView/insecure origins deny gestureless clipboard writes — the copy
      // chord below replays the stash from inside a real keystroke when that
      // happens. The rejection is async, so a try/catch could never see it.
      lastCopy = { text, at: Date.now() };
      updateCanCopy();
      navigator.clipboard?.writeText(text).catch((e) => log.debug("term", "OSC52 clipboard write denied", e));
      return true;
    });
    // Copy chord — Cmd+C on macOS, Ctrl+Shift+C elsewhere (plain Ctrl+C must stay
    // SIGINT; it only copies when a native Shift/Option+drag selection exists, VS
    // Code-style). Native selection wins; otherwise replay the last tmux copy —
    // from inside this keystroke the clipboard write is allowed everywhere.
    term.attachCustomKeyEventHandler((e) => {
      if (e.type !== "keydown") return true;
      // Esc leaves select mode (mirrors tmux copy-mode's q/Esc) without reaching the
      // agent; outside select mode it passes through untouched.
      if (e.key === "Escape" && selectModeRef.current) {
        applySelectMode(false);
        return false;
      }
      const key = e.key.toLowerCase();
      // Paste chord (non-Mac; Cmd+V is already native on Mac): skip xterm so the
      // browser's default paste fires and rides the normal text/image path.
      // Otherwise Ctrl+V becomes a literal ^V in the pty — which Claude/Codex
      // bind to "read MY clipboard": a dead end when the agent runs on a remote
      // headless backend that can't see this machine's clipboard.
      if (!IS_MAC && key === "v" && e.ctrlKey && !e.altKey && !e.metaKey) return false;
      if (key !== "c" || e.altKey) return true;
      const wantsCopy = IS_MAC
        ? e.metaKey && !e.ctrlKey
        : e.ctrlKey && !e.metaKey && (e.shiftKey || term.hasSelection());
      if (!wantsCopy) return true;
      const stashed = Date.now() - lastCopy.at < COPY_STASH_MS ? lastCopy.text : "";
      const text = term.hasSelection() ? term.getSelection() : stashed;
      if (!text) return true;
      copyText(text, () => term.focus());
      term.clearSelection();
      return false;
    });
    term.open(host);
    termRef.current = term;

    // Force a NATIVE xterm selection (instead of forwarding the drag to the agent)
    // when in select mode, or on Shift/Option-drag — the only way to drag-copy a
    // region out of a full-screen mouse-grabbing TUI (opencode) without tmux.
    // Reaches a private service like the fit addon reaches `_core`; guarded so a
    // future xterm that drops it degrades instead of throwing.
    const sel = (
      term as unknown as {
        _core?: { _selectionService?: { shouldForceSelection?: (e: MouseEvent) => boolean } };
      }
    )._core?._selectionService;
    if (sel && typeof sel.shouldForceSelection === "function") {
      sel.shouldForceSelection = (ev: MouseEvent) =>
        selectModeRef.current || ev.shiftKey || (IS_MAC && ev.altKey);
    } else {
      log.warn("term", "xterm selection service unavailable — select mode inert");
    }

    // xterm captures its colors at construction and renders to a canvas, so it
    // can't follow CSS the way the rest of the UI does — without this it keeps
    // stale colors until it's remounted, lagging the rest of the app on a theme
    // toggle. Re-read the CSS vars whenever the `.dark` class on <html> flips
    // (the theme's source of truth), regardless of what triggered the change.
    const themeObs = new MutationObserver(() => {
      term.options.theme = themeFromCss();
    });
    themeObs.observe(document.documentElement, { attributes: true, attributeFilter: ["class"] });

    // The single gate for every size reported to the backend (attach + resize).
    // tmux sizes the whole window to our pty (`window-size latest`) and a TUI
    // repaints on SIGWINCH, so one transient postage-stamp measurement (a side
    // panel mid-layout, a display:none flip) gets baked into the scrollback for
    // every viewer. Refuse implausible sizes; log every attempt (refusals at
    // warn → forwarded to the server log).
    let handle: api.TerminalHandle | null = null;
    let dataSub: { dispose: () => void } | null = null;
    let sent = { cols: 0, rows: 0 };
    // Ctrl+W is readline delete-word but also the browser's tab-close chord,
    // which no key handler can intercept — make the close ask first. Safe to
    // register unconditionally: the Tauri desktop webview ignores beforeunload,
    // and tmux keeps the session alive either way (this guards accidents only).
    const guardUnload = (e: BeforeUnloadEvent) => {
      e.preventDefault();
      e.returnValue = "";
    };
    const syncSize = (why: string) => {
      const w = host.clientWidth;
      const h = host.clientHeight;
      if (w < 120 || h < 80) {
        log.debug("term-size", `skip (${why}): host ${w}×${h}px, id=${id}`);
        return;
      }
      try {
        fit.fit();
      } catch {
        return; // not laid out yet
      }
      if (term.cols < MIN_COLS || term.rows < MIN_ROWS) {
        log.warn("term-size", `refused ${term.cols}×${term.rows} (${why}): host ${w}×${h}px, id=${id}`);
        return;
      }
      if (!handle) {
        handle = api.attachTerminal(id, {
          cols: term.cols,
          rows: term.rows,
          onData: (bytes) => term.write(bytes),
          onClose: () => term.write("\r\n\x1b[2m[disconnected — the session may have ended]\x1b[0m\r\n"),
        });
        dataSub = term.onData((d) => handle!.write(d));
        window.addEventListener("beforeunload", guardUnload);
        log.debug("term-size", `attach ${term.cols}×${term.rows} (${why}), id=${id}`);
      } else if (term.cols !== sent.cols || term.rows !== sent.rows) {
        handle.resize(term.cols, term.rows);
        log.debug("term-size", `resize ${term.cols}×${term.rows} (${why}), id=${id}`);
      } else {
        return; // unchanged — nothing to report
      }
      sent = { cols: term.cols, rows: term.rows };
    };
    syncSize("mount");

    // Images can't ride the text paste path: ship the bytes to the backend
    // (where the agent runs — possibly a remote host with no access to this
    // machine's clipboard) and paste the returned file path, the same shape
    // drag-and-drop produces in a native terminal. Shared by the paste event
    // below and the coarse-pointer Paste button.
    let gone = false;
    const note = (msg: string) => {
      if (!gone) term.write(`\r\n\x1b[2m[${msg}]\x1b[0m\r\n`);
    };
    const pasteImage = async (blob: Blob, mime: string) => {
      // Reject oversized images here, before encoding ~1.33× their bytes into a
      // JSON body and shipping it through the SSH tunnel only for the server's
      // identical cap to 400 it. Keep the limit in sync with save_pasted_image.
      if (blob.size > MAX_PASTE_BYTES) {
        note("image too large (max 32 MB)");
        return;
      }
      try {
        const bytes = new Uint8Array(await blob.arrayBuffer());
        const { path } = await api.terminalPasteImage(bytes, mime);
        if (!gone) term.paste(path);
      } catch (err) {
        note(`couldn't paste image: ${err instanceof Error ? err.message : String(err)}`);
      }
    };
    // When the clipboard carries both text and an image (e.g. spreadsheet
    // cells), text wins and xterm's own paste handler takes it.
    const onPaste = (e: ClipboardEvent) => {
      const dt = e.clipboardData;
      const img = dt && Array.from(dt.items).find((it) => it.kind === "file" && it.type.startsWith("image/"));
      if (!img || dt.getData("text/plain")) return;
      e.preventDefault();
      e.stopPropagation();
      const file = img.getAsFile();
      if (!file) return;
      // Capture the mime NOW: the File snapshot keeps its type, but the
      // DataTransferItem is neutered once this handler returns, so `img.type`
      // reads "" after the first await — which the server's allowlist rejects.
      const mime = file.type || img.type;
      void pasteImage(file, mime);
    };
    host.addEventListener("paste", onPaste, true);

    // macOS ⌘C — and the native Edit ▸ Copy menu / right-click Copy — fire a DOM
    // `copy` event, but find no selection to copy: xterm renders to a canvas, so
    // there is no DOM selection. On the desktop app the OS menu's ⌘C accelerator
    // also swallows the keystroke before it reaches the keydown chord above, so ⌘C
    // never copied at all. Catch the copy event directly and serve the terminal's
    // selection (or the last tmux copy) — ⌘C now copies like a native terminal,
    // with no reliance on the keystroke reaching xterm. Gated on the terminal being
    // focused, so copies elsewhere (and copyText's own hidden-textarea copy, which
    // focuses a node outside `host`) pass through untouched.
    const onCopy = (e: ClipboardEvent) => {
      if (!host.contains(document.activeElement)) return;
      const stashed = Date.now() - lastCopy.at < COPY_STASH_MS ? lastCopy.text : "";
      const text = term.hasSelection() ? term.getSelection() : stashed;
      if (!text) return;
      e.clipboardData?.setData("text/plain", text);
      e.preventDefault();
    };
    document.addEventListener("copy", onCopy);

    // xterm 6 has no touch handling, so we translate touch gestures ourselves.
    // The Select toggle picks the mode at gesture start:
    //  • normal — vertical pans become synthetic wheel ticks (one per
    //    TOUCH_SCROLL_PX) on the screen element, riding the wheel → tmux copy-mode
    //    scroll path. DOM_DELTA_LINE dodges the trackpad damping in xterm's
    //    consumeWheelEvent; pan down ⇒ deltaY < 0 ⇒ back into history.
    //  • select — the drag becomes synthetic mouse events so xterm builds a NATIVE
    //    selection (shouldForceSelection is true in select mode, so mousedown wins
    //    over tmux's mouse mode). xterm's selection is mouse-only, so without this
    //    it's unreachable by finger — a phone can't select at all.
    const asMouse = (
      type: "mousedown" | "mousemove" | "mouseup",
      p: { clientX: number; clientY: number },
      buttons: number,
    ) =>
      new MouseEvent(type, {
        bubbles: true,
        cancelable: true,
        view: window,
        button: 0,
        buttons,
        detail: type === "mousedown" ? 1 : 0,
        clientX: p.clientX,
        clientY: p.clientY,
      });
    // v/t are the smoothed release velocity (px/ms) and its timestamp, read at
    // touchend to decide whether the flick coasts.
    let pan: { x: number; y: number; acc: number; axis: "v" | "h" | null; v: number; t: number } | null = null;
    let selecting = false;
    // A synthetic mousedown makes xterm attach its own mousemove/mouseup listeners
    // on document — we MUST emit a matching mouseup or they leak and the next real
    // drag keeps extending the selection.
    const endSelect = (p: { clientX: number; clientY: number }) => {
      if (!selecting) return;
      selecting = false;
      document.dispatchEvent(asMouse("mouseup", p, 0));
    };
    // Drain travel into wheel ticks (one per TOUCH_SCROLL_PX) forwarded to xterm →
    // tmux copy-mode — tmux owns the scrollback, so term.scrollLines() is wrong
    // here. Returns the leftover sub-tick travel so nothing is dropped between calls.
    const emitScrollTicks = (acc: number, clientX: number, clientY: number): number => {
      const screen = host.querySelector(".xterm-screen");
      if (!screen) return 0;
      while (Math.abs(acc) >= TOUCH_SCROLL_PX) {
        const back = acc > 0;
        acc -= back ? TOUCH_SCROLL_PX : -TOUCH_SCROLL_PX;
        screen.dispatchEvent(
          new WheelEvent("wheel", {
            bubbles: true,
            cancelable: true,
            clientX,
            clientY,
            deltaY: back ? -1 : 1,
            deltaMode: WheelEvent.DOM_DELTA_LINE,
          }),
        );
      }
      return acc;
    };
    // Momentum after release: keep feeding ticks at the release velocity, decaying
    // it per frame (normalized to 16ms so it's frame-rate independent) until it
    // drops below the stop threshold. A new touch cancels it (stopFling below).
    let flingRaf = 0;
    let fling: { v: number; acc: number; x: number; y: number; t: number } | null = null;
    const stopFling = () => {
      if (flingRaf) cancelAnimationFrame(flingRaf);
      flingRaf = 0;
      fling = null;
    };
    const startFling = (v: number, clientX: number, clientY: number) => {
      fling = { v, acc: 0, x: clientX, y: clientY, t: performance.now() };
      const tick = (now: number) => {
        if (!fling) return;
        const dt = Math.min(now - fling.t, 32); // clamp a big gap (backgrounded tab)
        fling.t = now;
        fling.acc = emitScrollTicks(fling.acc + fling.v * dt, fling.x, fling.y);
        fling.v *= Math.pow(TOUCH_FLING_FRICTION, dt / 16);
        if (Math.abs(fling.v) < TOUCH_FLING_STOP_VELOCITY) return stopFling();
        flingRaf = requestAnimationFrame(tick);
      };
      flingRaf = requestAnimationFrame(tick);
    };
    const onTouchStart = (e: TouchEvent) => {
      stopFling(); // any new touch halts a coast already in flight
      // A second finger ends any in-progress selection and is never a scroll.
      if (e.touches.length !== 1) {
        endSelect(e.touches[0] ?? { clientX: 0, clientY: 0 });
        pan = null;
        return;
      }
      const t = e.touches[0];
      if (selectModeRef.current) {
        pan = null;
        selecting = true;
        host.querySelector(".xterm-screen")?.dispatchEvent(asMouse("mousedown", t, 1));
        return;
      }
      pan = { x: t.clientX, y: t.clientY, acc: 0, axis: null, v: 0, t: performance.now() };
    };
    const onTouchMove = (e: TouchEvent) => {
      if (e.touches.length !== 1) return;
      const t = e.touches[0];
      if (selecting) {
        e.preventDefault(); // hold the page still while dragging out a selection
        // In the edge margin, shift the synthetic point past the edge so xterm's
        // drag-scroll engages and the selection extends into off-screen scrollback.
        // The shift grows from 0 at the margin's inner boundary to the full margin
        // at the terminal edge (and beyond, if the finger leaves the terminal), so
        // the scroll speed tracks how far into the margin the finger is.
        const rect = host.getBoundingClientRect();
        let y = t.clientY;
        if (y < rect.top + SELECT_EDGE_SCROLL_PX) y -= SELECT_EDGE_SCROLL_PX;
        else if (y > rect.bottom - SELECT_EDGE_SCROLL_PX) y += SELECT_EDGE_SCROLL_PX;
        document.dispatchEvent(asMouse("mousemove", { clientX: t.clientX, clientY: y }, 1));
        return;
      }
      if (!pan || !handle) return;
      const dx = t.clientX - pan.x;
      const dy = t.clientY - pan.y;
      if (pan.axis === null) {
        if (Math.max(Math.abs(dx), Math.abs(dy)) < TOUCH_AXIS_LOCK_PX) return;
        pan.axis = Math.abs(dy) >= Math.abs(dx) ? "v" : "h";
        // Starting a vertical scroll dismisses the soft keyboard (mobile
        // convention): blur xterm's input so iOS closes the keyboard, releasing
        // the visual-viewport clamp so the terminal grows back as history scrolls.
        if (pan.axis === "v") term.textarea?.blur();
      }
      if (pan.axis === "h") return; // horizontal pans are not ours
      e.preventDefault(); // ours: keep the page from scrolling / pull-to-refresh
      // Smooth the release velocity from each move so a flick can coast; a pause
      // before lift pulls it toward zero (endPan then declines to coast).
      const now = performance.now();
      const dt = now - pan.t;
      if (dt > 0) pan.v = (dy / dt) * 0.6 + pan.v * 0.4;
      pan.t = now;
      pan.acc = emitScrollTicks(pan.acc + dy, t.clientX, t.clientY);
      pan.x = t.clientX;
      pan.y = t.clientY;
    };
    const endPan = (e: TouchEvent) => {
      endSelect(e.changedTouches[0] ?? { clientX: 0, clientY: 0 });
      // A tap (a one-finger touch that lifts without committing to a scroll axis;
      // select mode leaves `pan` null, so it's skipped) forwards a click to the
      // pty — the only way a phone with no arrow keys can pick an option out of a
      // mouse-mode TUI (Claude Code's menus). We own the gesture (touch-pinch-zoom),
      // so iOS won't synthesize a dependable compat click; emit the press/release
      // ourselves (xterm reports it to tmux like a trackpad click) and preventDefault
      // drops any compat click the browser does fire, so the option isn't picked twice.
      if (e.type === "touchend" && pan && pan.axis === null && handle) {
        const p = e.changedTouches[0] ?? { clientX: pan.x, clientY: pan.y };
        e.preventDefault();
        host.querySelector(".xterm-screen")?.dispatchEvent(asMouse("mousedown", p, 1));
        document.dispatchEvent(asMouse("mouseup", p, 0));
      }
      // A vertical flick coasts; a slow drag or a hold-then-lift (stale velocity,
      // >60ms since the last move) just stops with the finger.
      if (pan && pan.axis === "v" && performance.now() - pan.t < 60 && Math.abs(pan.v) >= TOUCH_FLING_MIN_VELOCITY) {
        const lift = e.changedTouches[0];
        startFling(pan.v, lift?.clientX ?? pan.x, lift?.clientY ?? pan.y);
      }
      pan = null;
    };
    host.addEventListener("touchstart", onTouchStart, { passive: true });
    // passive:false — the preventDefault above must be honored mid-gesture.
    host.addEventListener("touchmove", onTouchMove, { passive: false });
    host.addEventListener("touchend", endPan);
    host.addEventListener("touchcancel", endPan);

    let raf = 0;
    const ro = new ResizeObserver(() => {
      cancelAnimationFrame(raf);
      raf = requestAnimationFrame(() => syncSize("resize"));
    });
    ro.observe(host);
    term.focus();

    refitRef.current = () => syncSize("shown");
    tapOpsRef.current = {
      copySource: () => {
        if (term.hasSelection()) return term.getSelection();
        return Date.now() - lastCopy.at < COPY_STASH_MS ? lastCopy.text : "";
      },
      pasteImage,
      note,
    };

    return () => {
      gone = true;
      host.removeEventListener("paste", onPaste, true);
      document.removeEventListener("copy", onCopy);
      host.removeEventListener("touchstart", onTouchStart);
      host.removeEventListener("touchmove", onTouchMove);
      host.removeEventListener("touchend", endPan);
      host.removeEventListener("touchcancel", endPan);
      window.removeEventListener("beforeunload", guardUnload);
      clearTimeout(staleTimer);
      selSub.dispose();
      cancelAnimationFrame(raf);
      stopFling();
      themeObs.disconnect();
      ro.disconnect();
      dataSub?.dispose();
      handle?.detach();
      term.dispose();
      termRef.current = null;
      refitRef.current = () => {};
      tapOpsRef.current = null;
      setCanCopy(false);
    };
  }, [id, applySelectMode]);

  // Kept alive across navigation via display:none, where the host has zero size
  // and fit() can't measure; re-fit (and resize the pty) once shown again.
  useEffect(() => {
    if (visible) requestAnimationFrame(() => refitRef.current());
  }, [visible]);

  return (
    <div className="group relative h-full w-full">
      {/* pointer-events-none on the row (the property inherits) so the overlay
          never blocks the terminal; each button opts back in. Copy/Paste are
          coarse-pointer only — mouse-and-keyboard users have the chords. */}
      <div className="pointer-events-none absolute right-2 top-2 z-10 flex gap-1.5">
        {canCopy && (
          <button
            type="button"
            onMouseDown={(e) => e.preventDefault()}
            onClick={tapCopy}
            title="Copy the selection (or the last tmux copy)"
            className="pointer-events-auto hidden rounded-md border border-border bg-surface/85 px-2 py-0.5 text-xs font-medium text-muted shadow-sm backdrop-blur pointer-coarse:block"
          >
            Copy
          </button>
        )}
        <button
          type="button"
          onMouseDown={(e) => e.preventDefault()}
          onClick={tapPaste}
          title="Paste from the clipboard"
          className="pointer-events-auto hidden rounded-md border border-border bg-surface/85 px-2 py-0.5 text-xs font-medium text-muted shadow-sm backdrop-blur pointer-coarse:block"
        >
          Paste
        </button>
        <button
          type="button"
          // preventDefault keeps the click from stealing focus / starting a drag in
          // the terminal underneath; applySelectMode hands focus back to the term.
          onMouseDown={(e) => e.preventDefault()}
          onClick={() => applySelectMode(!selectMode)}
          title={
            selectMode
              ? "Select mode on — drag to highlight, ⌘/Ctrl-C to copy, Esc to exit"
              : "Select text — drag to highlight a region, then ⌘/Ctrl-C"
          }
          className={`rounded-md px-2 py-0.5 text-xs font-medium shadow-sm transition ${
            selectMode
              ? "pointer-events-auto bg-action text-action-fg"
              : "pointer-events-none border border-border bg-surface/85 text-muted opacity-0 backdrop-blur hover:bg-panel hover:text-fg focus-visible:opacity-100 group-hover:pointer-events-auto group-hover:opacity-100 pointer-coarse:pointer-events-auto pointer-coarse:opacity-100"
          }`}
        >
          {selectMode ? "Selecting…" : "Select"}
        </button>
      </div>
      {/* Padding lives on the wrapper, NOT the fit-measured host: the fit addon
          reads the host's computed height, which under border-box includes its
          own padding — counting it as usable space adds a clipped extra row. */}
      <div className="h-full w-full overflow-hidden bg-surface p-1.5">
        {/* touch-pinch-zoom disables single-finger native panning so iOS can't
            commit a slow drag to a rubber-band / URL-bar scroll that fights the
            synthetic wheel→tmux scroll below (the mid-scroll "hop") — the gesture
            handler owns every one-finger pan; two-finger zoom still works. */}
        <div ref={hostRef} className={`h-full w-full touch-pinch-zoom ${selectMode ? "[&_.xterm-screen]:!cursor-text" : ""}`} />
      </div>
    </div>
  );
}
