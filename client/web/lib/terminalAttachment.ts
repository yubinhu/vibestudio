import { subscribeWorkspaceConnection, workspaceConnection, workspaceUnavailable } from "./workspaceConnection";

export type TerminalConnectionState = "connecting" | "ready" | "reconnecting" | "incompatible";
export interface TerminalHandle {
  write(data: string, claimGeometry?: boolean): void;
  resize(cols: number, rows: number): void;
  inputToken(): string | null;
  pasteImage(blob: Blob, mime: string): Promise<string>;
  detach(): void;
}

type Send = <T>(path: string, args: Record<string, unknown>) => Promise<T>;

/** Each ready token owns one viewer. Nothing typed during a gap is queued. */
export function createTerminalAttachment(
  id: string,
  opts: {
    cols: number; rows: number;
    onData: (bytes: Uint8Array) => void;
    onReady?: () => void | Promise<void>;
    onState?: (state: TerminalConnectionState) => void;
  },
  transport: {
    url: (cols: number, rows: number) => string;
    send: Send;
    encode: (bytes: Uint8Array) => string;
    decode: (data: string) => Uint8Array;
  },
): TerminalHandle {
  let closed = false;
  let stream: EventSource | null = null;
  let token: string | null = null;
  let renderingReady = false;
  let retry: ReturnType<typeof setTimeout> | null = null;
  let readyTimer: ReturnType<typeof setTimeout> | null = null;
  let everReady = false;
  let size = { cols: opts.cols, rows: opts.rows };
  let pendingInput = "";
  let pendingClaim = false;
  let sendingInput: string | null = null;
  let pendingResize = false;
  let sendingResize: string | null = null;

  const current = (value: string | null): value is string =>
    value !== null && value === token && renderingReady && !closed && workspaceConnection().available;
  const release = (attachmentId: string | null) => {
    if (attachmentId) void transport.send("terminal/detach", { id, attachmentId }).catch(() => {});
  };
  const clearReadyTimer = () => {
    if (readyTimer !== null) clearTimeout(readyTimer);
    readyTimer = null;
  };
  const invalidate = () => {
    const previous = token;
    token = null;
    renderingReady = false;
    pendingInput = "";
    pendingClaim = false;
    sendingInput = null;
    pendingResize = false;
    sendingResize = null;
    clearReadyTimer();
    release(previous);
    if (!closed) opts.onState?.(everReady ? "reconnecting" : "connecting");
  };
  const schedule = () => {
    if (closed || retry !== null || !workspaceConnection().available) return;
    retry = setTimeout(() => { retry = null; open(); }, 1200);
  };
  const lost = () => {
    stream?.close();
    stream = null;
    invalidate();
    schedule();
  };
  const pumpInput = async () => {
    const attachmentId = token;
    if (!current(attachmentId) || sendingInput === attachmentId) return;
    sendingInput = attachmentId;
    try {
      while (pendingInput && current(attachmentId)) {
        const data = transport.encode(new TextEncoder().encode(pendingInput));
        const claimGeometry = pendingClaim;
        pendingInput = "";
        pendingClaim = false;
        await transport.send("terminal/input", { id, attachmentId, data, claimGeometry });
      }
    } catch {
      if (current(attachmentId)) lost();
    } finally {
      if (sendingInput === attachmentId) sendingInput = null;
    }
  };
  const pumpResize = async () => {
    const attachmentId = token;
    if (!current(attachmentId) || sendingResize === attachmentId) return;
    sendingResize = attachmentId;
    try {
      while (pendingResize && current(attachmentId)) {
        pendingResize = false;
        await transport.send("terminal/resize", { id, attachmentId, ...size });
      }
    } catch {
      if (current(attachmentId)) lost();
    } finally {
      if (sendingResize === attachmentId) sendingResize = null;
    }
  };
  const open = () => {
    if (closed || !workspaceConnection().available) return;
    stream?.close();
    const es = new EventSource(transport.url(size.cols, size.rows));
    const awaitingRender: Uint8Array[] = [];
    let awaitingBytes = 0;
    stream = es;
    opts.onState?.(everReady ? "reconnecting" : "connecting");
    es.onopen = () => {
      if (stream !== es || closed) return;
      clearReadyTimer();
      readyTimer = setTimeout(() => {
        if (stream === es && !token) opts.onState?.("incompatible");
      }, 5000);
    };
    es.addEventListener("ready", (event) => {
      if (closed || stream !== es || !workspaceConnection().available) return;
      let attachmentId: unknown;
      try { attachmentId = JSON.parse((event as MessageEvent).data).attachmentId; } catch { return; }
      if (typeof attachmentId !== "string" || !attachmentId || token) return;
      clearReadyTimer();
      token = attachmentId;
      everReady = true;
      // Let xterm drain its old parser queue and reset before fresh output.
      void Promise.resolve().then(() => opts.onReady?.()).then(() => {
        if (closed || stream !== es || token !== attachmentId || !workspaceConnection().available) return;
        renderingReady = true;
        for (const bytes of awaitingRender) opts.onData(bytes);
        awaitingRender.length = 0;
        opts.onState?.("ready");
        pendingResize = true;
        void pumpResize();
      }).catch(() => { if (stream === es && !closed) lost(); });
    });
    es.onmessage = (event) => {
      if (stream !== es || !token || closed || !workspaceConnection().available || !event.data) return;
      try {
        const bytes = transport.decode(event.data);
        if (renderingReady) opts.onData(bytes);
        else {
          awaitingBytes += bytes.length;
          if (awaitingBytes > 4 * 1024 * 1024) { lost(); return; }
          awaitingRender.push(bytes);
        }
      } catch { lost(); }
    };
    es.onerror = () => { if (stream === es && !closed) lost(); };
  };
  const unsubscribe = subscribeWorkspaceConnection(() => {
    if (retry !== null) clearTimeout(retry);
    retry = null;
    stream?.close();
    stream = null;
    invalidate();
    if (workspaceConnection().available) open();
  });
  open();
  return {
    write(data, claimGeometry = false) {
      if (!current(token)) return;
      pendingInput += data;
      pendingClaim ||= claimGeometry;
      void pumpInput();
    },
    resize(cols, rows) {
      size = { cols, rows };
      if (!current(token)) return;
      pendingResize = true;
      void pumpResize();
    },
    inputToken: () => current(token) ? token : null,
    async pasteImage(blob, mime) {
      const attachmentId = token;
      if (!current(attachmentId)) throw workspaceUnavailable();
      const bytes = new Uint8Array(await blob.arrayBuffer());
      if (!current(attachmentId)) throw workspaceUnavailable();
      const result = await transport.send<{ path: string }>("terminal/paste-image", {
        id, attachmentId, data: transport.encode(bytes), mime,
      });
      if (!current(attachmentId)) throw workspaceUnavailable();
      return result.path;
    },
    detach() {
      closed = true;
      unsubscribe();
      if (retry !== null) clearTimeout(retry);
      stream?.close();
      stream = null;
      invalidate();
    },
  };
}
