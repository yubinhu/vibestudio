import logoUrl from "../../../design/vibestudio-logo.svg";

/** Full-color identity mark, always paired with a visible or accessible app name. */
export function VibeStudioMark({ className = "h-7 w-7 shrink-0" }: { className?: string }) {
  return <img src={logoUrl} alt="" aria-hidden draggable={false} className={`object-contain ${className}`} />;
}
