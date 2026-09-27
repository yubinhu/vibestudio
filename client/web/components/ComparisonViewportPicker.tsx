import type { ComparisonViewport } from "@/lib/api";
import { comparisonViewportFromPreset, comparisonViewportOptionLabel, comparisonViewportPresetId, comparisonViewportPresets } from "@/lib/comparisonViewports";
import { syncComparisonDevices, useComparisonDevices } from "@/lib/comparisonDevices";

export default function ComparisonViewportPicker({ viewport, disabled = false, onChange, className }: {
  viewport: ComparisonViewport;
  disabled?: boolean;
  onChange: (viewport: ComparisonViewport) => void;
  className?: string;
}) {
  const catalog = useComparisonDevices();
  const presets = comparisonViewportPresets(catalog.devices);
  const groups = (["Phones", "Tablets", "Computers"] as const).flatMap((group) => {
    const devices = presets.filter((preset) => preset.group === group)
      .sort((a, b) => a.order - b.order || a.label.localeCompare(b.label));
    return [
      { label: group, devices: devices.filter((device) => device.showByDefault) },
      { label: `More ${group.toLowerCase()}`, devices: devices.filter((device) => !device.showByDefault) },
    ];
  });
  // Keep Chrome's current default choices ahead of its complete compatibility list.
  groups.sort((a, b) => Number(a.label.startsWith("More ")) - Number(b.label.startsWith("More ")));
  return (
    <select
      aria-label="Device preset"
      title={catalog.error || "Chrome DevTools viewport sizes. Choose ‘Sync from Chrome…’ to update the list. Pixel density, user agent, touch input, and hinges are not emulated."}
      value={comparisonViewportPresetId(viewport, presets)}
      disabled={disabled}
      className={className}
      onChange={(event) => {
        if (event.target.value === "__sync_chrome_devices__") {
          event.currentTarget.value = comparisonViewportPresetId(viewport, presets);
          void syncComparisonDevices();
        } else onChange(comparisonViewportFromPreset(event.target.value, presets) ?? { ...viewport, preset: "custom" });
      }}
    >
      <option value="custom">Responsive · custom size</option>
      {groups.filter((group) => group.devices.length).map((group) => (
        <optgroup key={group.label} label={group.label}>
          {group.devices.map((preset) => (
            <option key={preset.id} value={preset.id}>{comparisonViewportOptionLabel(preset, viewport)}</option>
          ))}
        </optgroup>
      ))}
      <optgroup label="Device list">
        <option value="__sync_chrome_devices__" disabled={catalog.refreshing}>
          {catalog.refreshing ? "Syncing from Chrome…" : catalog.error ? "Sync failed — retry from Chrome…" : "Sync from Chrome…"}
        </option>
      </optgroup>
    </select>
  );
}
