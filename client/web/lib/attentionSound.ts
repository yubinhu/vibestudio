import { useSyncExternalStore } from "react";
import { notifySound } from "./api";

type SoundKind = "request" | "done";
const KEY = "vibestudio-attention-sound";
let enabled = true;
try { enabled = localStorage.getItem(KEY) !== "off"; } catch { /* default on */ }
const listeners = new Set<() => void>();
let generation = 0;
let nativeAvailable: boolean | null = null;
let context: AudioContext | null = null;
const buffers = new Map<SoundKind, Promise<AudioBuffer>>();

/** Unlock in the actual user gesture. Never queue an alert for a later gesture. */
export function unlockAttentionSound(): void {
  if (!enabled || typeof AudioContext === "undefined") return;
  try {
    context ??= new AudioContext();
    void context.resume().catch(() => {});
  } catch { /* unavailable audio device; native playback can still work */ }
}

export function setAttentionSoundEnabled(value: boolean): void {
  enabled = value;
  generation++; // a muted alert must not play when an outstanding decode finishes
  try { localStorage.setItem(KEY, value ? "on" : "off"); } catch { /* best effort */ }
  if (value) unlockAttentionSound();
  for (const l of listeners) l();
}

export function useAttentionSound(): boolean {
  return useSyncExternalStore((cb) => {
    listeners.add(cb);
    return () => listeners.delete(cb);
  }, () => enabled, () => enabled);
}

export async function playAttentionSound(kind: SoundKind, current: () => boolean): Promise<void> {
  const started = generation;
  const valid = () => enabled && generation === started && current();
  if (!valid()) return;
  if (nativeAvailable !== false) {
    try {
      await notifySound(kind);
      nativeAvailable = true;
      return;
    } catch (e) {
      // A transport failure may have played already; do not double it in the browser.
      if ((e as { status?: number })?.status !== 404) return;
      nativeAvailable = false;
    }
  }
  const audio = context;
  if (!valid() || !audio || audio.state !== "running") return;
  try {
    let buffer = buffers.get(kind);
    if (!buffer) {
      buffer = fetch(`/sounds/${kind}.mp3`).then((r) => {
        if (!r.ok) throw new Error("Sound unavailable");
        return r.arrayBuffer();
      }).then((bytes) => audio.decodeAudioData(bytes));
      buffers.set(kind, buffer);
    }
    const decoded = await buffer;
    if (!valid() || audio.state !== "running") return;
    const source = audio.createBufferSource();
    source.buffer = decoded;
    source.connect(audio.destination);
    source.start();
  } catch {
    buffers.delete(kind); // a subsequent real transition may try again
  }
}

// Native sound works without an AudioContext; the browser fallback is gesture-bound.
window.addEventListener("pointerdown", unlockAttentionSound, { passive: true });
window.addEventListener("keydown", unlockAttentionSound, { passive: true });
