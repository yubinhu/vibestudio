/** Transport availability changes without replacing the mounted workspace. */
export const WORKSPACE_RESTORED = "vibestudio:workspace-restored";
let available = true;
let epoch = 0;
const listeners = new Set<() => void>();

export const workspaceConnection = () => ({ available, epoch });
export function subscribeWorkspaceConnection(listener: () => void): () => void {
  listeners.add(listener);
  return () => { listeners.delete(listener); };
}
export function setWorkspaceAvailable(next: boolean): void {
  if (next === available) return;
  available = next;
  epoch++;
  for (const listener of listeners) listener();
  if (available) window.dispatchEvent(new Event(WORKSPACE_RESTORED));
}

export function workspaceUnavailable(): Error & { status: number } {
  return Object.assign(new Error("The remote connection is unavailable. Your workspace is preserved while it reconnects."), { status: 503 });
}
