import type { ComparisonDevice, ComparisonViewport } from "./api";
import chromeDevices from "./comparisonDevices.generated.json";

export type ComparisonViewportPreset = ComparisonDevice;

// Chrome owns named device data. This generated snapshot is the offline fallback;
// comparisonDevices.ts supplies the desktop's refreshed catalog while connected.
// Generic computers describe a responsive layout size without claiming a model.
const COMPUTER_PRESETS: readonly ComparisonViewportPreset[] = [
  { id: "laptop", label: "Laptop (generic)", group: "Computers", width: 1280, height: 800, showByDefault: true, order: 0 },
  { id: "desktop", label: "Desktop (generic)", group: "Computers", width: 1440, height: 900, showByDefault: true, order: 1 },
];

export function comparisonViewportPresets(devices: readonly ComparisonViewportPreset[]): ComparisonViewportPreset[] {
  const ids = new Set(devices.map((device) => device.id));
  const offline = (chromeDevices.devices as ComparisonViewportPreset[])
    .filter((device) => !ids.has(device.id)).map((device) => ({ ...device, showByDefault: false }));
  return [...devices.filter((device) => device.group !== "Computers"), ...offline, ...COMPUTER_PRESETS];
}

export const COMPARISON_VIEWPORT_PRESETS = comparisonViewportPresets(chromeDevices.devices as ComparisonViewportPreset[]);

/** Generic phone/tablet presets stay responsive; only an explicit matching device ID names hardware. */
export function comparisonViewportPresetId(viewport: ComparisonViewport, presets: readonly ComparisonViewportPreset[] = COMPARISON_VIEWPORT_PRESETS): string {
  const id = viewport.preset;
  const preset = presets.find((candidate) => candidate.id === id);
  if (!preset) return "custom";
  return (viewport.width === preset.width && viewport.height === preset.height)
    || (viewport.width === preset.height && viewport.height === preset.width) ? preset.id : "custom";
}

export function comparisonViewportFromPreset(id: string, presets: readonly ComparisonViewportPreset[] = COMPARISON_VIEWPORT_PRESETS): ComparisonViewport | null {
  const preset = presets.find((candidate) => candidate.id === id);
  return preset ? {
    preset: preset.id, width: preset.width, height: preset.height,
    orientation: preset.width > preset.height ? "landscape" : "portrait",
  } : null;
}

export function comparisonViewportOptionLabel(preset: ComparisonViewportPreset, current: ComparisonViewport): string {
  const selected = comparisonViewportPresetId(current, [preset]) === preset.id;
  return `${preset.label} · ${selected ? current.width : preset.width} × ${selected ? current.height : preset.height} CSS px`;
}

export function rotateComparisonViewport(viewport: ComparisonViewport): ComparisonViewport {
  return {
    ...viewport, width: viewport.height, height: viewport.width,
    orientation: viewport.orientation === "portrait" ? "landscape" : "portrait",
  };
}

export function validComparisonViewportSize(width: number, height: number): boolean {
  return Number.isInteger(width) && Number.isInteger(height) && width >= 240 && width <= 3840 && height >= 240 && height <= 3840;
}
