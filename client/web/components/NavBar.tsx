"use client";

import type { ReactNode } from "react";
import { useLocation, useNavigate } from "react-router-dom";
import { VibeStudioMark } from "./VibeStudioMark";
import { ThemeToggle } from "./ui";
import RemoteMenu from "./RemoteMenu";
import { connectorsPath, studioPath } from "@/lib/routes";
import { useRecents } from "@/lib/recents";
import { useSessions } from "@/lib/sessions";
import { useUpdate } from "@/lib/updates";
import { toggleTheme } from "@/lib/theme";

function TerminalIcon() {
  return (
    <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
      <rect x="3" y="4" width="18" height="16" rx="2" />
      <path d="m7 9 3 3-3 3M13 15h4" />
    </svg>
  );
}
function KeyIcon() {
  return (
    <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
      <circle cx="7.5" cy="15.5" r="5.5" />
      <path d="m21 2-9.6 9.6" />
      <path d="m15.5 7.5 3 3L22 7l-3-3" />
    </svg>
  );
}
function StudioIcon() {
  return (
    <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
      <path d="M12 3H5a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-7" />
      <path d="M18.375 2.625a1 1 0 0 1 3 3l-9.013 9.014a2 2 0 0 1-.853.505l-2.873.84a.5.5 0 0 1-.62-.62l.84-2.873a2 2 0 0 1 .506-.852z" />
    </svg>
  );
}

/** A persistent app-nav link (Sessions, Connectors) shown on every page; the entry
 *  for the current page reads as active. `dot` is the same blue unread dot as the
 *  session rail's — "an agent finished a turn somewhere you aren't looking". */
function NavLink({
  icon,
  label,
  active,
  onClick,
  dot = false,
}: {
  icon: ReactNode;
  label: string;
  active: boolean;
  onClick: () => void;
  dot?: boolean;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      title={label}
      aria-current={active ? "page" : undefined}
      className={`relative flex items-center gap-1.5 rounded-md px-2 py-1 text-xs transition-colors ${
        active ? "bg-accent-soft text-accent" : "text-muted hover:bg-panel hover:text-fg"
      }`}
    >
      {icon}
      <span className="hidden text-xs sm:inline">{label}</span>
      {dot && (
        <span
          className="absolute right-0.5 top-0.5 h-1.5 w-1.5 rounded-full bg-info"
          title="An agent finished a turn"
          aria-label="An agent finished a turn"
        />
      )}
    </button>
  );
}

/**
 * The app's top chrome — constant height + identical layout across pages (no shift on
 * navigation). It deliberately carries four distinct IA categories (our mental model):
 *   1. Identity / location — the "VibeStudio" brand (links home, except on home) and
 *      the optional `breadcrumb` (page or skill name).
 *   2. Page chrome — the page's own `children` actions (e.g. Studio's Review/Manage/
 *      Export). Owned by the page; they sit in the bar only because there's room, and
 *      may move into the page body later.
 *   3. Destinations (pages) — Sessions, Connectors (Home = the brand). Always navigation;
 *      the current page reads active.
 *   4. Status / controls — Remote (connection status) and the theme toggle. Global.
 *
 * Known overlap kept for now: in Studio the Sessions link toggles the in-page
 * *projection* of the Sessions destination (the side panel) instead of navigating (see
 * `onSessions`). The clean future split = that toggle becomes Studio page chrome and
 * the link navigates everywhere.
 */
export default function NavBar({
  breadcrumb,
  children,
  onSessions,
  sessionsOpen,
}: {
  breadcrumb?: ReactNode;
  children?: ReactNode;
  /** Categories 2↔3 overlap (see header): a page that projects the Sessions
   *  destination inline (Studio's side panel) overrides the link to OPEN that
   *  projection; once it's open the link falls through to navigating to the full
   *  Sessions page. Future-clean: move the projection toggle to page chrome. */
  onSessions?: () => void;
  sessionsOpen?: boolean;
}) {
  const navigate = useNavigate();
  const { pathname } = useLocation();
  const atHome = pathname === "/";
  const recents = useRecents();
  const sessionsUnread = useSessions().unreadCount > 0;
  // This desktop's own version (the update ledger's `current` — always local, never the
  // connected remote's), so the running build is visible at a glance. "0.0.0" = an
  // unstamped dev build; release tags stamp the real version.
  const version = useUpdate()?.current;

  // "Studio" is a persistent destination with no singleton route (one repo per skill):
  // in a skill → its index (SKILL.md); else resume the last-opened skill; else Home.
  const studioSeg = pathname.startsWith("/skills/") ? pathname.split("/")[2] : null;
  const lastSkill = recents.find((r) => r.kind !== "markdown");
  // Skills now live on the home dashboard. Resume the current/last skill if there
  // is one; otherwise go home and scroll to the gallery section.
  const goSkills = () => {
    if (studioSeg) return navigate(`/skills/${studioSeg}`);
    if (lastSkill) return navigate(studioPath(lastSkill.root));
    if (pathname === "/") document.getElementById("skills")?.scrollIntoView({ behavior: "smooth", block: "start" });
    else navigate("/");
  };

  // Sessions is a first-class destination; the Studio side panel is just its inline
  // projection. So in Studio the link opens that projection, and once it's open a
  // second click navigates to the full Sessions page (closing the panel is the
  // panel's own control). Everywhere else the link just navigates.
  const onSessionsClick = onSessions && !sessionsOpen ? onSessions : () => navigate("/sessions");

  const brand = (
    <span className="flex items-center gap-2 text-brand">
      <VibeStudioMark className="h-7 w-7 shrink-0" />
      {/* Phones show just the mark (it's the home button and carries the brand);
          the wordmark + version appear at ≥sm, where there's room. Keeps the bar
          from crowding the destinations/status cluster off a narrow screen. */}
      <span className="hidden whitespace-nowrap text-[0.95rem] font-semibold tracking-tight sm:inline">VibeStudio</span>
      {version && (
        <span className="hidden text-[0.7rem] font-normal tabular-nums text-muted sm:inline" title={`Version ${version}`}>
          {version}
        </span>
      )}
    </span>
  );

  return (
    <header className="z-20 flex h-[calc(3rem+env(safe-area-inset-top))] shrink-0 items-center gap-1 overflow-hidden border-b border-border px-2 sm:gap-2 sm:px-3 pt-[env(safe-area-inset-top)] text-sm">
      {/* (1) identity + location. This left side is the flex-shrink sink: on a
          narrow (phone) screen the brand holds and the breadcrumb truncates, so the
          destinations/status cluster on the right is never pushed past the viewport
          edge — the old bug, where the whole non-shrinking row overflowed the screen.
          overflow-hidden on the header is the hard backstop against any protrusion. */}
      {atHome ? (
        <span className="shrink-0 px-1 sm:px-1.5">{brand}</span>
      ) : (
        <button
          type="button"
          onClick={() => navigate("/")}
          title="Back to home"
          className="flex shrink-0 items-center rounded-md px-1 py-1 hover:bg-panel sm:px-1.5"
        >
          {brand}
        </button>
      )}
      {/* Action-heavy pages already name the document in their content. On phones,
          give those controls the breadcrumb's space so every action stays visible. */}
      {breadcrumb && (
        <div className={`${children ? "hidden sm:flex" : "flex"} min-w-0 items-center gap-2 overflow-hidden whitespace-nowrap [&>span]:min-w-0`}>{breadcrumb}</div>
      )}
      {/* (2-4) destinations + status — pinned (shrink-0), always fully visible */}
      <div className="ml-auto flex shrink-0 items-center gap-0.5 sm:gap-1">
        {/* Three visible buckets, divided to match the IA categories (see header):
            (2) page chrome | (3) destinations | (4) status + controls. */}
        {/* (2) page chrome — owned by the page, here only for space */}
        {children}
        {children && <span className="mx-0.5 h-5 w-px bg-border sm:mx-1" aria-hidden />}
        {/* (3) destinations — the persistent "pages" cluster, identical on every page.
            Studio has no singleton route (per-skill), so it points at the current/last skill
            (else Home); in Studio the Sessions link opens its projection, then navigates to
            the full page once it's open (see onSessions). */}
        <NavLink
          icon={<StudioIcon />}
          label="Skills"
          active={pathname.startsWith("/skills")}
          onClick={goSkills}
        />
        <NavLink
          icon={<TerminalIcon />}
          label="Sessions"
          active={onSessions ? !!sessionsOpen : pathname === "/sessions"}
          onClick={onSessionsClick}
          dot={sessionsUnread}
        />
        <NavLink icon={<KeyIcon />} label="Connectors" active={pathname === "/connectors"} onClick={() => navigate(connectorsPath())} />
        <span className="mx-0.5 h-5 w-px bg-border sm:mx-1" aria-hidden />
        {/* (4) status + controls — Remote (connection status) + theme toggle, the global utility corner */}
        <RemoteMenu />
        <ThemeToggle onClick={toggleTheme} />
      </div>
    </header>
  );
}
