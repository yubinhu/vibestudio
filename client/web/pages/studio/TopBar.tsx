"use client";

import NavBar from "@/components/NavBar";
import { requestSave, useEditorStatus } from "@/lib/editorState";

/** Wordless autosave: nothing to see while it's working. Surfaces ONLY a failure,
 *  so a dropped write is never silent — clicking retries it. (Saving a *version*
 *  lives in the Versions sidebar panel, not here.) */
function AutosaveIndicator() {
  const { present, error } = useEditorStatus();
  if (!present || !error) return null;
  return (
    <button
      type="button"
      onClick={requestSave}
      title={`${error} — click to retry`}
      className="flex items-center gap-1.5 rounded-md px-2 py-1 text-xs font-medium text-danger transition-colors hover:bg-panel"
    >
      <span aria-hidden className="h-1.5 w-1.5 animate-pulse rounded-full bg-danger" />
      Couldn’t save — retry
    </button>
  );
}

export default function TopBar({
  skillName,
  selected,
  reviewMode,
  showReview,
  previewing,
  sessionsOpen,
  narrow,
  sidebarOpen,
  onToggleReview,
  onSessions,
  onToggleSidebar,
  onManage,
  onExport,
}: {
  skillName: string;
  selected: string | null;
  /** The diff overlay is currently on for the open file. */
  reviewMode: boolean;
  /** The open file can be reviewed (tracked + has changes) — show the toggle. */
  showReview: boolean;
  /** Viewing a past version: review shows what THAT version changed, not working-tree edits. */
  previewing: boolean;
  /** The embedded sessions side panel (Studio's projection of the Sessions
   *  destination) is open — the nav's Sessions link toggles it here instead of
   *  leaving the skill; the full page is reachable from the panel's expand button. */
  sessionsOpen: boolean;
  /** Container is phone-narrow: the sidebar column is an overlay drawer, toggled here. */
  narrow: boolean;
  sidebarOpen: boolean;
  onToggleReview: () => void;
  onSessions: () => void;
  onToggleSidebar: () => void;
  onManage: () => void;
  onExport: () => void;
}) {
  return (
    <NavBar
      onSessions={onSessions}
      sessionsOpen={sessionsOpen}
      breadcrumb={
        <>
          <span className="text-faint" aria-hidden>
            /
          </span>
          <span className="truncate font-medium text-fg">{skillName}</span>
          {selected && selected !== "SKILL.md" && (
            <>
              <span className="text-faint" aria-hidden>
                /
              </span>
              <span className="truncate font-mono text-xs text-muted">{selected}</span>
            </>
          )}
        </>
      }
    >
      {/* Studio page chrome (NavBar category 2): rendered into the bar for now;
          conceptually belongs to the page and could move into the page body later. */}
      {narrow && (
        <button
          type="button"
          onClick={onToggleSidebar}
          aria-pressed={sidebarOpen}
          title="Files & versions"
          className={`flex items-center gap-1.5 rounded-md px-2 py-1 text-xs transition-colors ${
            sidebarOpen ? "bg-action text-action-fg hover:opacity-90" : "text-muted hover:bg-panel hover:text-fg"
          }`}
        >
          <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
            <rect x="3" y="4" width="18" height="16" rx="2" />
            <path d="M9 4v16" />
          </svg>
        </button>
      )}
      <AutosaveIndicator />
      {showReview && (
        <button
          type="button"
          onClick={onToggleReview}
          aria-pressed={reviewMode}
          title={
            previewing
              ? "Review change mode — show what changed in this version"
              : "Review change mode — show changes since the last saved version"
          }
          className={`flex items-center gap-1.5 rounded-md px-2 py-1 text-xs transition-colors ${
            reviewMode ? "bg-action text-action-fg hover:opacity-90" : "text-muted hover:bg-panel hover:text-fg"
          }`}
        >
          <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
            <path d="M9 3v12M9 15a3 3 0 1 0 0 6 3 3 0 0 0 0-6zM9 3a3 3 0 1 0 0 0M18 9v6M18 9a3 3 0 1 0 0-6 3 3 0 0 0 0 6zm0 6c0 3-3 3-9 3" />
          </svg>
          <span className="hidden sm:inline">Review changes</span>
        </button>
      )}
      <button
        type="button"
        onClick={onManage}
        title="Secrets, sync & collaborate"
        className="flex items-center gap-1.5 rounded-md px-2 py-1 text-xs text-muted hover:bg-panel hover:text-fg"
      >
        <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
          <circle cx="6" cy="6" r="2.5" />
          <circle cx="6" cy="18" r="2.5" />
          <circle cx="18" cy="9" r="2.5" />
          <path d="M6 8.5v7M8.4 6.6c5 .3 7.5 1 7.5 4.4M18 11.5c0 3-2.5 4-6 4.2" />
        </svg>
        <span className="hidden sm:inline">Manage</span>
      </button>
      <button
        type="button"
        onClick={onExport}
        title="Package skill as a .skill file"
        className="flex items-center gap-1.5 rounded-md px-2 py-1 text-xs text-muted hover:bg-panel hover:text-fg"
      >
        <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
          <path d="M12 3v12m0 0l-4-4m4 4l4-4M5 21h14" />
        </svg>
        <span className="hidden sm:inline">Export .skill</span>
      </button>
    </NavBar>
  );
}
