import { useEffect, useRef, useState } from "react";
import { VibeStudioMark } from "@/components/VibeStudioMark";
import "./comparison.css";

type View = "compare" | "current" | "proposed";
type Theme = "light" | "dark";
type ViewportName = "desktop" | "tablet" | "phone";
type Variant = Exclude<View, "compare">;

const VIEWPORTS = {
  desktop: { label: "Desktop", width: 1280, height: 900 },
  tablet: { label: "Tablet", width: 768, height: 1024 },
  phone: { label: "Phone", width: 390, height: 844 },
} as const;

const VIEW_OPTIONS: { value: View; label: string }[] = [
  { value: "compare", label: "Compare" },
  { value: "current", label: "Current" },
  { value: "proposed", label: "Proposed" },
];

const VIEWPORT_OPTIONS: { value: ViewportName; label: string }[] = [
  { value: "desktop", label: "Desktop 1280" },
  { value: "tablet", label: "Tablet 768" },
  { value: "phone", label: "Phone 390" },
];

const THEME_OPTIONS: { value: Theme; label: string }[] = [
  { value: "light", label: "Light" },
  { value: "dark", label: "Dark" },
];

function SegmentedControl<T extends string>({
  label,
  options,
  value,
  onChange,
}: {
  label: string;
  options: { value: T; label: string }[];
  value: T;
  onChange: (value: T) => void;
}) {
  return (
    <div className="home-lab-control">
      <span className="home-lab-control-label">{label}</span>
      <div className="home-lab-segments" role="group" aria-label={label}>
        {options.map((option) => (
          <button
            key={option.value}
            type="button"
            aria-pressed={value === option.value}
            onClick={() => onChange(option.value)}
          >
            {option.label}
          </button>
        ))}
      </div>
    </div>
  );
}

function Preview({
  variant,
  viewport,
  theme,
}: {
  variant: Variant;
  viewport: (typeof VIEWPORTS)[ViewportName];
  theme: Theme;
}) {
  const stageRef = useRef<HTMLDivElement>(null);
  const [availableWidth, setAvailableWidth] = useState(0);

  useEffect(() => {
    const stage = stageRef.current;
    if (!stage) return;
    const observer = new ResizeObserver(([entry]) => {
      if (entry) setAvailableWidth(entry.contentRect.width);
    });
    observer.observe(stage);
    return () => observer.disconnect();
  }, []);

  // Keep the iframe's CSS viewport fixed. Only its presentation size changes,
  // so both versions use the same breakpoints and can be compared fairly.
  const scale = Math.min(1, availableWidth / viewport.width);
  const scaleLabel = `${Math.round(scale * 1000) / 10}% scale`;
  const label = variant === "current" ? "Current" : "Proposed";
  const frameSrc = `/home-lab.html?view=${variant}&theme=${theme}`;

  return (
    <section className="home-lab-preview" aria-label={`${label} home preview`}>
      <header className="home-lab-preview-heading">
        <div className="home-lab-preview-title">
          <span className={`home-lab-version-dot home-lab-version-dot--${variant}`} aria-hidden="true" />
          <h2>{label}</h2>
          <span className="home-lab-version-note">
            {variant === "current" ? "Production UI" : "Experiment UI"}
          </span>
        </div>
        <div className="home-lab-preview-tools">
          <span className="home-lab-scale">
            {viewport.width} × {viewport.height} · {availableWidth > 0 ? scaleLabel : "Sizing preview…"}
          </span>
          <a href={frameSrc} target="_blank" rel="noreferrer" aria-label={`Open ${label.toLowerCase()} home at full size in a new tab`}>
            Full size ↗
          </a>
        </div>
      </header>
      <div className="home-lab-preview-canvas">
        <div ref={stageRef} className="home-lab-stage">
          <div
            className="home-lab-frame-window"
            style={{ width: viewport.width * scale, height: viewport.height * scale }}
          >
            <iframe
              className="home-lab-frame"
              src={frameSrc}
              title={`${label} home, ${viewport.label.toLowerCase()} viewport`}
              width={viewport.width}
              height={viewport.height}
              style={{ width: viewport.width, height: viewport.height, transform: `scale(${scale})` }}
            />
          </div>
        </div>
      </div>
    </section>
  );
}

/** An isolated comparison surface; preferences here never write app settings. */
export default function Comparison() {
  const [view, setView] = useState<View>("compare");
  const [viewportName, setViewportName] = useState<ViewportName>("desktop");
  const [theme, setTheme] = useState<Theme>(() =>
    new URLSearchParams(window.location.search).get("theme") === "dark" ? "dark" : "light",
  );
  const variants: Variant[] = view === "compare" ? ["current", "proposed"] : [view];

  return (
    <main className="home-lab" data-theme={theme}>
      <header className="home-lab-toolbar">
        <div className="home-lab-intro">
          <div className="home-lab-title-row">
            <VibeStudioMark className="home-lab-mark" />
            <h1>VibeStudio UI lab</h1>
            <span className="home-lab-experiment-tag">Experiment</span>
          </div>
          <p>Compare the home page’s type, spacing, and hierarchy at the same viewport size.</p>
        </div>
        <div className="home-lab-controls">
          <SegmentedControl label="View" options={VIEW_OPTIONS} value={view} onChange={setView} />
          <SegmentedControl label="Viewport" options={VIEWPORT_OPTIONS} value={viewportName} onChange={setViewportName} />
          <SegmentedControl label="Theme" options={THEME_OPTIONS} value={theme} onChange={setTheme} />
        </div>
      </header>
      <div className="home-lab-workspace">
        <div className="home-lab-comparison-note">
          <span>Previews fit the available space. Choose Current or Proposed for a larger view.</span>
          <span>{VIEWPORTS[viewportName].label} · {theme === "light" ? "Light" : "Dark"}</span>
        </div>
        <div className={`home-lab-previews${view === "compare" ? " home-lab-previews--compare" : ""}`}>
          {variants.map((variant) => (
            <Preview key={variant} variant={variant} viewport={VIEWPORTS[viewportName]} theme={theme} />
          ))}
        </div>
      </div>
    </main>
  );
}
