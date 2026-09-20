import { isUpdateInProgress } from "./updates";

/** Protect browser Ctrl+W accidents without blocking an intentional app update.
 * tmux sessions survive either close; unsaved editors own their separate guard. */
export function guardTerminalUnload(event: BeforeUnloadEvent): void {
  if (isUpdateInProgress()) return;
  event.preventDefault();
  event.returnValue = "";
}
