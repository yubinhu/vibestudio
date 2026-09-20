export interface TouchPoint {
  clientX: number;
  clientY: number;
}

interface TerminalTouchActions {
  active(): boolean;
  tap(point: TouchPoint): void;
  scroll(direction: -1 | 1, point: TouchPoint): void;
  startSelection(point: TouchPoint): void;
  moveSelection(point: TouchPoint): void;
  endSelection(point: TouchPoint): void;
  showMenu(point: TouchPoint): void;
  dismissSelection(): void;
}

const HOLD_MS = 500;
const AXIS_LOCK_PX = 6;
const SCROLL_PX = 25;
const FLING_MIN_VELOCITY = 0.2;
const FLING_FRICTION = 0.95;
const FLING_STOP_VELOCITY = 0.02;

/** xterm has mouse selection but no touch gestures. Keep the gesture lifecycle
 * separate from rendering: holds select, pans scroll, and only short taps click.
 * Every selection start must have an end, even on cancellation or disposal. */
export function attachTerminalTouch(host: HTMLElement, actions: TerminalTouchActions) {
  let hold: ReturnType<typeof setTimeout> | undefined;
  let pan: (TouchPoint & { acc: number; axis: "v" | "h" | null; v: number; t: number; dismissOnly: boolean }) | null = null;
  let selecting = false;
  let selectionActive = false;
  let ignoreRelease = false;
  let point: TouchPoint = { clientX: 0, clientY: 0 };
  let flingRaf = 0;

  const cancelHold = () => { clearTimeout(hold); hold = undefined; };
  const stopFling = () => { cancelAnimationFrame(flingRaf); flingRaf = 0; };
  const endSelection = () => {
    if (!selecting) return;
    selecting = false;
    actions.endSelection(point);
  };
  const reset = () => {
    cancelHold();
    stopFling();
    ignoreRelease ||= selecting || pan !== null;
    endSelection();
    pan = null;
    if (selectionActive) actions.dismissSelection();
    selectionActive = false;
  };
  const scroll = (travel: number, at: TouchPoint) => {
    while (Math.abs(travel) >= SCROLL_PX) {
      const direction = travel > 0 ? -1 : 1;
      travel += direction * SCROLL_PX;
      actions.scroll(direction, at);
    }
    return travel;
  };
  const startFling = (velocity: number, at: TouchPoint) => {
    let last = performance.now();
    let acc = 0;
    const tick = (now: number) => {
      if (!actions.active()) return stopFling();
      const dt = Math.min(now - last, 32);
      last = now;
      acc = scroll(acc + velocity * dt, at);
      velocity *= Math.pow(FLING_FRICTION, dt / 16);
      if (Math.abs(velocity) < FLING_STOP_VELOCITY) return stopFling();
      flingRaf = requestAnimationFrame(tick);
    };
    flingRaf = requestAnimationFrame(tick);
  };
  const onStart = (event: TouchEvent) => {
    cancelHold();
    stopFling();
    if (event.touches.length !== 1 || !actions.active()) { reset(); return; }
    const touch = event.touches[0];
    point = { clientX: touch.clientX, clientY: touch.clientY };
    const dismissOnly = selectionActive;
    if (selectionActive) reset();
    ignoreRelease = false;
    pan = { ...point, acc: 0, axis: null, v: 0, t: performance.now(), dismissOnly };
    hold = setTimeout(() => {
      hold = undefined;
      if (!pan || pan.axis || !actions.active()) return;
      pan = null;
      selectionActive = selecting = true;
      actions.startSelection(point);
    }, HOLD_MS);
  };
  const onMove = (event: TouchEvent) => {
    if (event.touches.length !== 1 || !actions.active()) { reset(); return; }
    const touch = event.touches[0];
    point = { clientX: touch.clientX, clientY: touch.clientY };
    if (selecting) {
      event.preventDefault();
      actions.moveSelection(point);
      return;
    }
    if (!pan) return;
    const dx = point.clientX - pan.clientX;
    const dy = point.clientY - pan.clientY;
    if (pan.axis === null) {
      if (Math.max(Math.abs(dx), Math.abs(dy)) < AXIS_LOCK_PX) return;
      cancelHold();
      pan.axis = Math.abs(dy) >= Math.abs(dx) ? "v" : "h";
    }
    if (pan.axis === "h") return;
    event.preventDefault();
    const now = performance.now();
    const dt = now - pan.t;
    if (dt > 0) pan.v = (dy / dt) * 0.6 + pan.v * 0.4;
    pan.t = now;
    pan.acc = scroll(pan.acc + dy, point);
    pan.clientX = point.clientX;
    pan.clientY = point.clientY;
  };
  const onEnd = (event: TouchEvent) => {
    cancelHold();
    if (event.type === "touchcancel" || !actions.active()) { reset(); event.preventDefault(); return; }
    if (ignoreRelease) event.preventDefault();
    if (event.touches.length === 0) ignoreRelease = false;
    const touch = event.changedTouches[0];
    if (touch) point = { clientX: touch.clientX, clientY: touch.clientY };
    if (selecting) {
      event.preventDefault(); // no compatibility click or link activation after a hold
      endSelection();
      actions.showMenu(point);
    } else if (pan) {
      event.preventDefault(); // we send at most one click ourselves
      if (pan.axis === null && !pan.dismissOnly) actions.tap(point);
      if (pan.axis === "v" && performance.now() - pan.t < 60 && Math.abs(pan.v) >= FLING_MIN_VELOCITY) {
        startFling(pan.v, point);
      }
    }
    pan = null;
  };
  const onContextMenu = (event: MouseEvent) => {
    if (!pan && !selectionActive && !window.matchMedia("(pointer: coarse)").matches) return;
    // Let our hold finish without xterm moving/focusing its hidden textarea or
    // the browser offering an unrelated page context menu. Desktop keeps its menu.
    event.preventDefault();
    event.stopImmediatePropagation();
  };
  const onVisibility = () => { if (document.hidden) reset(); };
  host.addEventListener("touchstart", onStart, { passive: true });
  host.addEventListener("touchmove", onMove, { passive: false });
  host.addEventListener("touchend", onEnd, { passive: false });
  host.addEventListener("touchcancel", onEnd);
  host.addEventListener("contextmenu", onContextMenu, true);
  window.addEventListener("blur", reset);
  document.addEventListener("visibilitychange", onVisibility);

  return {
    reset,
    dispose() {
      reset();
      host.removeEventListener("touchstart", onStart);
      host.removeEventListener("touchmove", onMove);
      host.removeEventListener("touchend", onEnd);
      host.removeEventListener("touchcancel", onEnd);
      host.removeEventListener("contextmenu", onContextMenu, true);
      window.removeEventListener("blur", reset);
      document.removeEventListener("visibilitychange", onVisibility);
    },
  };
}
