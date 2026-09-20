import { useCallback, useEffect, useLayoutEffect, useRef, useState, type HTMLAttributes, type KeyboardEvent } from "react";
import { createPortal } from "react-dom";
import type { TermSession } from "@/lib/api";
import { sessionTitle } from "@/lib/sessionTitle";

type OpenMenu = { session: TermSession; x: number; y: number; trigger: HTMLElement };

/** Title-only context actions, shared by session rows, Home cards and the picker.
 * A normal click keeps its existing selection/navigation behavior. */
export function useSessionTitleMenu(onRename: (session: TermSession) => void, enabled = true) {
  const [open, setOpen] = useState<OpenMenu | null>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const hold = useRef<{ timer: ReturnType<typeof setTimeout>; x: number; y: number } | null>(null);
  const suppressClick = useRef(false);

  const cancelHold = useCallback(() => {
    if (hold.current) clearTimeout(hold.current.timer);
    hold.current = null;
  }, []);
  const close = useCallback((restoreFocus = false) => {
    if (restoreFocus) open?.trigger.focus();
    setOpen(null);
  }, [open]);

  useEffect(() => cancelHold, [cancelHold]);
  useEffect(() => {
    if (!enabled) { cancelHold(); setOpen(null); }
  }, [enabled, cancelHold]);
  useEffect(() => {
    if (!open) return;
    const outside = (e: PointerEvent) => {
      if (!menuRef.current?.contains(e.target as Node)) close();
    };
    const key = (e: globalThis.KeyboardEvent) => {
      if (e.key === "Escape" || e.key === "Tab") {
        e.preventDefault();
        close(true);
      }
    };
    const dismiss = () => close();
    document.addEventListener("pointerdown", outside);
    window.addEventListener("keydown", key);
    window.addEventListener("resize", dismiss);
    window.addEventListener("scroll", dismiss, true);
    return () => {
      document.removeEventListener("pointerdown", outside);
      window.removeEventListener("keydown", key);
      window.removeEventListener("resize", dismiss);
      window.removeEventListener("scroll", dismiss, true);
    };
  }, [open, close]);
  useLayoutEffect(() => {
    const menu = menuRef.current;
    if (!menu || !open) return;
    const bounds = menu.getBoundingClientRect();
    menu.style.left = `${Math.max(8, Math.min(open.x, window.innerWidth - bounds.width - 8))}px`;
    menu.style.top = `${Math.max(8, Math.min(open.y, window.innerHeight - bounds.height - 8))}px`;
    menu.querySelector("button")?.focus();
  }, [open]);

  const show = (session: TermSession, target: HTMLElement, x?: number, y?: number) => {
    if (!enabled) return;
    cancelHold();
    const title = target.querySelector<HTMLElement>("[data-session-title]") ?? target;
    const bounds = title.getBoundingClientRect();
    const trigger = target.closest<HTMLElement>("button, select") ?? target;
    trigger.focus();
    setOpen({ session, x: x || bounds.left, y: y || bounds.bottom, trigger });
  };

  const titleProps = (session: TermSession): HTMLAttributes<HTMLElement> & { "data-session-title": boolean } => ({
    "data-session-title": true,
    title: `${sessionTitle(session)} — right-click or hold to rename`,
    style: { WebkitTouchCallout: "none" },
    onContextMenu: (e) => {
      e.preventDefault();
      e.stopPropagation();
      if (hold.current) suppressClick.current = true;
      show(session, e.currentTarget, e.clientX, e.clientY);
    },
    onPointerDown: (e) => {
      suppressClick.current = false;
      if (e.pointerType !== "touch" || !e.isPrimary) return;
      // Let scrolling cancel the hold; do not start the row's drag gesture.
      e.stopPropagation();
      cancelHold();
      const target = e.currentTarget;
      hold.current = {
        x: e.clientX, y: e.clientY,
        timer: setTimeout(() => {
          suppressClick.current = true;
          show(session, target);
        }, 600),
      };
    },
    onPointerMove: (e) => {
      if (hold.current && Math.hypot(e.clientX - hold.current.x, e.clientY - hold.current.y) > 8) cancelHold();
    },
    onPointerUp: cancelHold,
    onPointerCancel: cancelHold,
    onPointerLeave: cancelHold,
    onClickCapture: (e) => {
      if (suppressClick.current) {
        suppressClick.current = false;
        e.preventDefault();
        e.stopPropagation();
      }
    },
  });

  const onTitleKeyDown = (session: TermSession, e: KeyboardEvent<HTMLElement>) => {
    if (e.key === "ContextMenu" || (e.shiftKey && e.key === "F10")) {
      e.preventDefault();
      show(session, e.currentTarget);
    } else if (e.key === "F2") {
      e.preventDefault();
      onRename(session);
    }
  };

  const menu = enabled && open && createPortal(
    <div
      ref={menuRef}
      role="menu"
      aria-label={`Actions for ${sessionTitle(open.session)}`}
      className="fixed z-50 min-w-36 rounded-lg border border-border bg-surface p-1 shadow-lg"
      style={{ left: open.x, top: open.y }}
      onContextMenu={(e) => e.preventDefault()}
      onClick={(e) => e.stopPropagation()}
    >
      <button
        type="button"
        role="menuitem"
        className="w-full rounded-md px-3 py-2 text-left text-sm text-fg hover:bg-panel focus:bg-panel focus:outline-none"
        onClick={() => { close(true); onRename(open.session); }}
      >
        Rename…
      </button>
    </div>,
    document.body,
  );

  return { titleProps, onTitleKeyDown, menu };
}
