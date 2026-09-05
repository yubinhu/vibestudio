import { listDir, preferencesGet, type PickerContext } from "./api";

/** The current form wins, then the last confirmed selection, then Home. Each
 * candidate must actually be readable; stale saved paths never trap the picker. */
export async function loadPickerStart(context: PickerContext, initialPath: string | undefined, includeFiles: boolean) {
  const prefs = await preferencesGet().catch(() => null);
  const candidates = [...new Set([initialPath || undefined, prefs?.pickerLocations?.[context], ""].filter((p): p is string => p != null))];
  let usedFallback = false;
  for (const path of candidates) {
    try {
      return { listing: await listDir(path, includeFiles), usedFallback };
    } catch (error) {
      if (path === "") throw error;
      usedFallback = true;
    }
  }
  throw new Error("No folder is available.");
}
