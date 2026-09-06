"use client";

import type { ReactNode } from "react";

/* Shared button classes — a lime primary with ink text and a bordered
   secondary/ghost. Exported as strings so the dialogs that
   build raw <button>s (which carry their own busy/disabled logic) stay consistent
   without each redefining the look. */
export const btnPrimary =
  "rounded-md bg-action px-3 py-1.5 text-sm font-medium text-action-fg transition-colors hover:bg-action-hover disabled:opacity-40";
export const btnGhost =
  "rounded-md border border-border px-3 py-1.5 text-sm text-fg transition-colors hover:bg-panel disabled:opacity-40";
/** Destructive confirm action — semantic red with a theme-aware foreground. */
export const btnDanger =
  "rounded-md bg-danger px-3 py-1.5 text-sm font-medium text-danger-fg transition-opacity hover:opacity-90 disabled:opacity-40";

export type Tone = "default" | "accent" | "ok" | "warn" | "danger" | "info" | "muted";

const TONE_CLASSES: Record<Tone, string> = {
  default: "bg-panel text-fg border-border",
  muted: "bg-panel text-muted border-border",
  accent: "border-transparent text-action-fg",
  ok: "bg-[color-mix(in_srgb,var(--ok)_14%,transparent)] text-ok border-[color-mix(in_srgb,var(--ok)_35%,transparent)]",
  warn: "bg-[color-mix(in_srgb,var(--warning)_14%,transparent)] text-warn border-[color-mix(in_srgb,var(--warning)_35%,transparent)]",
  danger: "bg-[color-mix(in_srgb,var(--error)_14%,transparent)] text-danger border-[color-mix(in_srgb,var(--error)_35%,transparent)]",
  info: "bg-[color-mix(in_srgb,var(--info)_14%,transparent)] text-info border-[color-mix(in_srgb,var(--info)_35%,transparent)]",
};

export function Badge({
  children,
  tone = "default",
  title,
  className = "",
}: {
  children: ReactNode;
  tone?: Tone;
  title?: string;
  className?: string;
}) {
  const accentStyle = tone === "accent" ? { background: "var(--action)" } : undefined;
  return (
    <span
      title={title}
      style={accentStyle}
      className={`inline-flex items-center gap-1 rounded-full border px-2.5 py-0.5 text-xs font-medium ${TONE_CLASSES[tone]} ${className}`}
    >
      {children}
    </span>
  );
}

export function Spinner({ className = "" }: { className?: string }) {
  return (
    <span
      role="status"
      className={`inline-block h-4 w-4 animate-spin rounded-full border-2 border-current border-t-transparent ${className}`}
      aria-label="Loading"
    />
  );
}

/** Small accent pill flagging a feature that's previewed but not yet functional
 *  (e.g. the studio Collaborate section, or a not-yet-connectable secret provider). */
export function PreviewBadge({ children = "Preview" }: { children?: ReactNode }) {
  return (
    <span className="rounded-full border border-accent/30 bg-accent-soft px-1.5 py-0.5 text-[0.6rem] font-medium uppercase tracking-wide text-accent">
      {children}
    </span>
  );
}

export function ThemeToggle({ onClick }: { onClick: () => void }) {
  return (
    <button
      type="button"
      onClick={onClick}
      title="Toggle theme"
      aria-label="Toggle theme"
      className="rounded-md p-1.5 text-muted hover:bg-panel hover:text-fg"
    >
      {/* Icon follows the .dark class via CSS, so it stays correct without JS state. */}
      <svg className="hidden dark:block" width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
        <circle cx="12" cy="12" r="4" />
        <path d="M12 2v2M12 20v2M2 12h2M20 12h2M5 5l1.5 1.5M17.5 17.5L19 19M19 5l-1.5 1.5M6.5 17.5L5 19" />
      </svg>
      <svg className="block dark:hidden" width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
        <path d="M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8z" />
      </svg>
    </button>
  );
}
