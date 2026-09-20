"use client";

import { useEffect, useState } from "react";
import type { ReactNode } from "react";
import { useNavigate } from "react-router-dom";
import NavBar from "@/components/NavBar";
import { Spinner } from "@/components/ui";
import PhoneModal from "@/components/PhoneModal";
import { RemoteDialog } from "@/components/RemoteMenu";
import NewSessionDialog from "@/components/NewSessionDialog";
import RenameSessionDialog from "@/components/RenameSessionDialog";
import { useSessionTitleMenu } from "@/components/useSessionTitleMenu";
import RecentStrip from "@/components/RecentStrip";
import SkillGallery from "@/pages/home/SkillGallery";
import * as api from "@/lib/api";
import type { TermSession } from "@/lib/api";
import { sessionTitle } from "@/lib/sessionTitle";
import { attentionLabel } from "@/lib/sessionAttention";
import { useSessions, isUnread, refresh as refreshSessions, noteCreated, nativeNotifyState } from "@/lib/sessions";
import { useMining } from "@/lib/mining";
import { useSkills } from "@/lib/skills";
import { useConnectors } from "@/lib/connectors";
import OpenSkillDialog from "@/components/OpenSkillDialog";
import * as push from "@/lib/push";
import { useRemote } from "@/lib/remote";
import { sessionsPath, connectorsPath, miningPath } from "@/lib/routes";

// A live session carries only the bare agent family ("claude" | "codex" | …); the
// rail keys colors off human labels, so map family → label + brand color here.
const AGENT_META: Record<string, { label: string; color: string }> = {
  claude: { label: "Claude Code", color: "#d97757" },
  codex: { label: "Codex", color: "#10a37f" },
  gemini: { label: "Gemini CLI", color: "#4285f4" },
  cursor: { label: "Cursor", color: "#7c83ff" },
  opencode: { label: "opencode", color: "#f59e0b" },
  openclaw: { label: "OpenClaw", color: "#a855f7" },
  hermes: { label: "Hermes", color: "#c17d3f" },
  pi: { label: "Pi", color: "#ab70d6" },
  shell: { label: "Shell", color: "var(--muted)" },
};
const agentMeta = (a: string) => AGENT_META[a] ?? { label: a || "Shell", color: "var(--muted)" };

const nowSecs = () => Math.floor(Date.now() / 1000);
function ago(unixSecs: number): string {
  const s = Math.max(0, nowSecs() - unixSecs);
  if (s < 45) return "just now";
  const m = Math.floor(s / 60);
  if (m < 1) return "just now";
  if (m < 60) return `${m}m ago`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h ago`;
  return `${Math.floor(h / 24)}d ago`;
}

// Read/unread marks describe what the user has seen, not whether the agent is
// working. Terminal activity is also unsuitable: idle TUIs keep repainting.
function sessionStatus(s: TermSession): string {
  if (s.attention) {
    const label = attentionLabel(s) ?? "Status unavailable";
    const { state, kind, changedAt } = s.attention;
    if (state === "idle" && kind === "done" && Number.isFinite(changedAt) && changedAt > 0) {
      return `${label} ${ago(changedAt / 1000)}`;
    }
    return label;
  }
  // Older servers only report bells. That is a historical completion signal,
  // not evidence that the agent has or hasn't started another turn since then.
  const bellAt = Number(s.bellAt);
  if (Number.isFinite(bellAt) && bellAt > 0) return `Last finished ${ago(bellAt)}`;
  return s.agent === "shell" ? "Terminal open" : "Status unavailable";
}

function greeting(): string {
  const h = new Date().getHours();
  if (h >= 5 && h < 12) return "Good morning";
  if (h >= 12 && h < 18) return "Good afternoon";
  return "Good evening";
}

// ─── icons ───
function Icon({ children, size = 16 }: { children: ReactNode; size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
      {children}
    </svg>
  );
}
const PlusIcon = () => <Icon><path d="M12 5v14M5 12h14" /></Icon>;
const TerminalIcon = () => <Icon><path d="m4 17 6-6-6-6" /><path d="M12 19h8" /></Icon>;
const PickaxeIcon = () => (
  <Icon>
    <path d="M14.5 12.5 6.6 20.4a1 1 0 1 1-3-3l7.9-7.9" />
    <path d="M15.7 4.3A12.5 12.5 0 0 0 5.5 3a1 1 0 0 0 .1 1.8 22 22 0 0 1 6.3 3.4" />
    <path d="M17.7 3.7a1 1 0 0 0-1.4 0l-4.6 4.6a1 1 0 0 0 0 1.4l2.6 2.6a1 1 0 0 0 1.4 0l4.6-4.6a1 1 0 0 0 0-1.4z" />
    <path d="M19.7 8.3a12.5 12.5 0 0 1 1.3 10.2 1 1 0 0 1-1.7-.1 22 22 0 0 0-3.4-6.3" />
  </Icon>
);
const LinkIcon = () => <Icon><path d="M9 17H7A5 5 0 0 1 7 7h2" /><path d="M15 7h2a5 5 0 1 1 0 10h-2" /><line x1="8" x2="16" y1="12" y2="12" /></Icon>;
const SkillIcon = () => <Icon><path d="M4 19.5A2.5 2.5 0 0 1 6.5 17H20" /><path d="M6.5 2H20v20H6.5A2.5 2.5 0 0 1 4 19.5v-15A2.5 2.5 0 0 1 6.5 2z" /></Icon>;
const ServerIcon = () => <Icon><rect width="20" height="8" x="2" y="2" rx="2" /><rect width="20" height="8" x="2" y="14" rx="2" /><path d="M6 6h.01M6 18h.01" /></Icon>;
const BellIcon = () => <Icon><path d="M10.3 21a1.9 1.9 0 0 0 3.4 0" /><path d="M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9" /></Icon>;

// ─── shared bits ───
const actionBase = "inline-flex items-center gap-2 rounded-lg px-3.5 py-2 text-sm font-medium transition-colors";
const infoTint =
  "border-[color-mix(in_srgb,var(--info)_45%,transparent)] bg-[color-mix(in_srgb,var(--info)_7%,var(--surface))] hover:border-[color-mix(in_srgb,var(--info)_60%,transparent)] hover:bg-[color-mix(in_srgb,var(--info)_12%,var(--surface))]";

function Heading({ children, count, action, level = 2 }: { children: ReactNode; count?: ReactNode; action?: ReactNode; level?: 2 | 3 }) {
  const Title = level === 3 ? "h3" : "h2";
  return (
    <div className="mb-4 flex flex-wrap items-center gap-x-2 gap-y-2">
      <Title className="text-2xl font-semibold tracking-tight text-fg">{children}</Title>
      {count != null && <span className="text-xs text-faint">{count}</span>}
      {action && <span className="ml-auto">{action}</span>}
    </div>
  );
}

function SessionCard({ s, waiting, onClick, titleMenu }: {
  s: TermSession;
  waiting: boolean;
  onClick: () => void;
  titleMenu: ReturnType<typeof useSessionTitleMenu>;
}) {
  const meta = agentMeta(s.agent);
  return (
    <button
      type="button"
      onClick={onClick}
      onKeyDown={(e) => titleMenu.onTitleKeyDown(s, e)}
      aria-keyshortcuts="Shift+F10 F2"
      className={`flex flex-col gap-2 rounded-xl border p-4 text-left transition-all hover:-translate-y-0.5 hover:shadow-[0_2px_8px_-2px_rgba(0,0,0,0.08)] ${
        waiting ? infoTint : "border-border bg-surface hover:border-border-strong hover:bg-panel"
      }`}
    >
      <div className="flex items-center gap-2">
        <span className="h-2.5 w-2.5 shrink-0 rounded-full" style={{ background: meta.color }} aria-hidden />
        <span {...titleMenu.titleProps(s)} className="min-w-0 flex-1 truncate text-sm font-semibold text-fg">{sessionTitle(s)}</span>
        {waiting && (
          <span className="shrink-0 rounded-full bg-[color-mix(in_srgb,var(--info)_16%,transparent)] px-1.5 py-0.5 text-[0.6rem] font-semibold uppercase tracking-wide text-info">
            Your turn
          </span>
        )}
      </div>
      <span className="truncate font-mono text-[0.7rem] text-faint" title={s.cwd}>
        {s.cwd}
      </span>
      <span className="text-xs text-faint">
        {meta.label} · {sessionStatus(s)}
      </span>
    </button>
  );
}

function StatCard({
  icon,
  label,
  value,
  sub,
  subTone = "muted",
  onClick,
}: {
  icon: ReactNode;
  label: string;
  value: ReactNode;
  sub?: ReactNode;
  subTone?: "muted" | "warn" | "ok";
  onClick?: () => void;
}) {
  const body = (
    <>
      <div className="flex items-center gap-2 text-muted">
        {icon}
        <span className="text-[0.68rem] font-semibold uppercase tracking-wider">{label}</span>
      </div>
      <div className="mt-2 wrap-break-word text-2xl font-semibold tracking-tight text-fg">{value}</div>
      {sub != null && (
        <span className={`mt-0.5 text-xs ${subTone === "warn" ? "text-warn" : subTone === "ok" ? "text-ok" : "text-muted"}`}>
          {sub}
        </span>
      )}
    </>
  );
  // min-w-0: a grid child otherwise refuses to shrink below its content, so one
  // long unbreakable value (an SSH host id) would widen the whole page.
  const cls = "flex min-w-0 flex-col rounded-xl border border-border bg-surface p-4 text-left";
  return onClick ? (
    <button type="button" onClick={onClick} className={`${cls} transition-all hover:-translate-y-0.5 hover:border-border-strong hover:bg-panel`}>
      {body}
    </button>
  ) : (
    <div className={cls}>{body}</div>
  );
}

// The switchboard reports the full connection id ("user@host:port"); the stat
// card is a glance, so show just the machine's name — the dialog behind the tap
// has the rest. IPs stay whole (their first label alone would say nothing).
function hostLabel(id: string): string {
  const host = id.replace(/^[^@]*@/, "").replace(/:\d+$/, "");
  if (/^[\d.]+$/.test(host)) return host;
  return host.split(".")[0] || host;
}

// A gesture-driven "enable notifications" nudge for phone/browser clients — the
// one place WebKit will let us ask (permission requires a real tap, no button =
// no way to opt in). Shows only where there's NO desktop toast surface
// (nativeNotifyState() === false, set once notify/status 404s a phone) and the
// permission is still undecided; grant OR deny flips canOfferPush() false and it
// self-clears. Desktop shells never see it (they get real OS toasts already).
function PushNudge() {
  const [show, setShow] = useState(false);
  const [dismissed, setDismissed] = useState(() => {
    try {
      return sessionStorage.getItem("skillviewer-push-nudge") === "off";
    } catch {
      return false;
    }
  });
  useEffect(() => {
    if (dismissed) return;
    const check = () => setShow(push.canOfferPush() && nativeNotifyState() === false);
    check();
    // The native-surface probe (notify/status) resolves async right after boot;
    // poll briefly so the nudge appears the moment we KNOW this is a phone.
    const t = setInterval(check, 1000);
    const stop = setTimeout(() => clearInterval(t), 6000);
    return () => {
      clearInterval(t);
      clearTimeout(stop);
    };
  }, [dismissed]);
  if (dismissed || !show) return null;

  // enablePushInGesture() calls Notification.requestPermission() synchronously —
  // must run straight off the click, no awaits before it, or iOS refuses.
  const enable = () => void push.enablePushInGesture().then(() => setShow(push.canOfferPush()));
  const dismiss = () => {
    try {
      sessionStorage.setItem("skillviewer-push-nudge", "off");
    } catch {
      /* private mode — the nudge just re-appears next load */
    }
    setDismissed(true);
  };
  return (
    <div className={`mt-6 flex items-center gap-3 rounded-xl border p-4 ${infoTint}`}>
      <span className="grid h-9 w-9 shrink-0 place-items-center rounded-lg bg-[color-mix(in_srgb,var(--info)_16%,transparent)] text-info">
        <BellIcon />
      </span>
      <div className="min-w-0 flex-1">
        <p className="text-sm font-semibold text-fg">Turn notifications</p>
        <p className="text-xs text-muted">Get a push the moment an agent finishes — even with VibeStudio closed.</p>
      </div>
      <button type="button" onClick={enable} className={`${actionBase} shrink-0 bg-action text-action-fg hover:bg-action-hover`}>
        Enable
      </button>
      <button type="button" onClick={dismiss} aria-label="Not now" className="shrink-0 rounded-md p-1 text-muted transition-colors hover:bg-panel hover:text-fg">
        <Icon size={16}><path d="M18 6 6 18M6 6l12 12" /></Icon>
      </button>
    </div>
  );
}

export function Component() {
  const navigate = useNavigate();
  const sessionStore = useSessions();
  const remote = useRemote();
  const mining = useMining();
  const [remoteOpen, setRemoteOpen] = useState(false);
  const [phoneOpen, setPhoneOpen] = useState(false);
  const [newSessionOpen, setNewSessionOpen] = useState(false);
  const [openDialogOpen, setOpenDialogOpen] = useState(false);
  const [renaming, setRenaming] = useState<TermSession | null>(null);
  const titleMenu = useSessionTitleMenu(setRenaming);

  const connectorStore = useConnectors();
  const connectors = connectorStore.inventory?.connectors;
  const [secretCount, setSecretCount] = useState<number | null>(null);
  const [secretError, setSecretError] = useState<string | null>(null);
  // Skills come from the shared cache store (same one the gallery below uses), so
  // the count is cached across visits and the page runs ONE discovery scan, not two.
  const skills = useSkills();

  // Refresh detector state and relative completion times while Home is visible.
  // SSE delivers transitions; polling is the backstop for missed events.
  useEffect(() => {
    void refreshSessions();
    const t = setInterval(() => {
      if (!document.hidden) void refreshSessions();
    }, 5000);
    return () => clearInterval(t);
  }, []);

  // The footer only needs the count, not secret names or values. A failed read
  // stays unknown, so an unavailable store cannot be mistaken for an empty one.
  useEffect(() => {
    let alive = true;
    api.secretsStatus().then((status) => {
      if (!alive) return;
      setSecretCount(status.count);
      setSecretError(null);
    }).catch(() => {
      if (alive) setSecretError("Couldn’t load API keys and secrets from this server.");
    });
    return () => {
      alive = false;
    };
  }, []);

  // Keep the count visible during scanning; wait for the first inventory
  // before showing its change summary. `dirtyRoots` already excludes proposals.
  const skillStats = skills.loading ? null : { total: skills.total, dirty: skills.dirtyRoots.size };

  const sessions = sessionStore.sessions;
  const waiting = sessions.filter((s) => isUnread(s, sessionStore.seen, null));
  const waitingIds = new Set(waiting.map((s) => s.id));
  const otherSessions = sessions.filter((s) => !waitingIds.has(s.id));

  const needsReauth = connectors?.filter((c) => c.availability.some((a) => a.state === "needs_auth")).length ?? 0;
  const remoteConnected = remote.status.state === "connected";
  const workspaceHost = remote.workspaceHost ?? (remoteConnected ? remote.status.host : null);
  const connectorTotal = connectors && secretCount != null ? connectors.length + secretCount : null;
  const connectorLoadError = !!connectorStore.error || !!secretError;
  const serviceSummary = connectors ? `${connectors.length} ${connectors.length === 1 ? "service" : "services"}${connectorStore.error ? " (last result)" : ""}` : connectorStore.error ? "Services unavailable" : "Services loading…";
  const secretSummary = secretCount != null ? `${secretCount} ${secretCount === 1 ? "API key or secret" : "API keys & secrets"}` : secretError ? "Secrets unavailable" : "Secrets loading…";
  const serviceAttention = connectors?.filter((c) => c.availability.some((a) => a.state === "needs_auth" || a.state === "error")).length ?? 0;


  const openSession = (id: string) => navigate(sessionsPath(id));
  const openNewSession = () => setNewSessionOpen(true);
  const mineDays =
    mining?.startedUnix != null ? Math.floor((Date.now() / 1000 - mining.startedUnix) / 86400) : null;

  return (
    <div className="flex min-h-dvh flex-col">
      <NavBar />

      <main className="mx-auto w-full max-w-6xl flex-1 px-6 pb-10 pt-10">
        {/* Hero — greeting + positioning + the primary on-ramps. */}
        <section className="flex flex-col gap-5 sm:flex-row sm:items-center sm:justify-between">
          <div>
            <h1 className="text-3xl font-semibold tracking-tight text-fg">{greeting()}.</h1>
            <p className="mt-1.5 text-sm text-muted">Run, teach, and connect all your coding agents — from any device, anywhere.</p>
          </div>
          <div className="flex shrink-0 flex-wrap gap-2">
            <button type="button" onClick={openNewSession} className={`${actionBase} bg-action text-action-fg hover:bg-action-hover`}>
              <TerminalIcon />
              New session
            </button>
          </div>
        </section>

        {/* Phone/browser only: the one gesture-gated moment to opt into pushes. */}
        <PushNudge />

        {/* At a glance — the overview strip, up top: quick counts + where you're
            running. Each card scrolls to (or opens) the fuller view below. */}
        <section className="mt-8">
          <Heading>At a glance</Heading>
          <div className="grid grid-cols-2 gap-3 lg:grid-cols-4">
            <StatCard
              icon={<SkillIcon />}
              label="Skills"
              value={
                <span
                  className={`inline-block tabular-nums${skills.loading || skills.scanning ? " skill-count-loading" : ""}`}
                  aria-busy={skills.loading || skills.scanning}
                >
                  <span className="sr-only">{skills.total}</span>
                  {String(skills.total).split("").map((digit, index) => (
                    <span
                      key={index}
                      className="skill-count-digit inline-block"
                      style={{ animationDelay: `${index * 160}ms` }}
                      aria-hidden="true"
                    >
                      {digit}
                    </span>
                  ))}
                </span>
              }
              sub={skillStats ? (skillStats.dirty > 0 ? `${skillStats.dirty} with changes` : "all committed") : undefined}
              subTone={skillStats && skillStats.dirty > 0 ? "warn" : "muted"}
              onClick={() => document.getElementById("skills")?.scrollIntoView({ behavior: "smooth", block: "start" })}
            />
            <StatCard
              icon={<PickaxeIcon />}
              label="Mining"
              value={
                mining == null ? (
                  <Spinner className="h-5 w-5" />
                ) : mining.status === "running" ? (
                  "Running"
                ) : mineDays != null ? (
                  mineDays
                ) : (
                  "—"
                )
              }
              sub={
                mining == null
                  ? undefined
                  : mining.status === "running"
                    ? "in progress"
                    : mineDays != null
                      ? mineDays === 1
                        ? "day since last mine"
                        : "days since last mine"
                      : "not mined yet"
              }
              subTone={mining?.status === "running" ? "ok" : "muted"}
              onClick={() => navigate(miningPath())}
            />
            <StatCard
              icon={<LinkIcon />}
              label="Connectors"
              value={connectorTotal ?? (connectorLoadError ? "—" : <Spinner className="h-5 w-5" />)}
              sub={`${serviceSummary} · ${secretSummary}`}
              subTone={connectorLoadError || needsReauth > 0 ? "warn" : "muted"}
              onClick={() => navigate(connectorsPath())}
            />
            <StatCard
              icon={<ServerIcon />}
              label="Server"
              value={
                workspaceHost ? (
                  <span title={workspaceHost}>{hostLabel(workspaceHost)}</span>
                ) : (
                  "Local"
                )
              }
              sub={remote.interrupted ? "connection interrupted" : remoteConnected ? "connected over SSH" : "running on this machine"}
              subTone={remote.interrupted ? "warn" : remoteConnected ? "ok" : "muted"}
              onClick={remote.available ? () => setRemoteOpen(true) : undefined}
            />
          </div>
        </section>

        <RecentStrip />

        {/* Sessions — one list; the ones that finished a turn and need you are tinted
            "Your turn" and sorted to the front (a highlight, not "the rest aren't live"). */}
        {sessions.length > 0 && (
          <section className="mt-10">
            <Heading
              count={
                <>
                  {sessions.length}
                  {waiting.length > 0 && <span className="text-info"> · {waiting.length} waiting for you</span>}
                </>
              }
              action={
                <button type="button" onClick={() => navigate(sessionsPath())} className="text-xs font-medium text-accent hover:opacity-80">
                  Open Sessions →
                </button>
              }
            >
              Sessions
            </Heading>
            <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-3">
              {[...waiting, ...otherSessions].map((s) => (
                <SessionCard key={s.id} s={s} waiting={waitingIds.has(s.id)} onClick={() => openSession(s.id)} titleMenu={titleMenu} />
              ))}
              <button
                type="button"
                onClick={openNewSession}
                className="flex items-center gap-2 rounded-xl border border-dashed border-border p-4 text-left text-muted transition-colors hover:border-accent hover:text-accent"
              >
                <PlusIcon />
                <span className="text-sm font-medium">New session</span>
              </button>
            </div>
          </section>
        )}

        <SkillGallery onBrowse={() => setOpenDialogOpen(true)} />
      </main>

      <footer id="connectors" aria-label="Connectors" className="mx-auto w-full max-w-6xl px-6 pb-6">
        <div className="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-x-5 gap-y-2 border-t border-border py-4 text-xs text-muted sm:flex sm:flex-wrap">
          <span className="inline-flex items-center gap-2 font-medium"><LinkIcon />Connectors</span>
          <div className="col-span-2 row-start-2 flex flex-wrap items-center gap-x-3 gap-y-2">
            <button type="button" onClick={() => navigate(connectorsPath())} className={`${connectorStore.error ? "text-danger" : "text-muted"} transition-colors hover:text-fg`}>{serviceSummary}</button>
            <span aria-hidden className="text-faint">·</span>
            <button type="button" onClick={() => navigate(connectorsPath("secrets"))} className={`${secretError ? "text-danger" : "text-muted"} transition-colors hover:text-fg`}>{secretSummary}</button>
            {serviceAttention > 0 && <span className="text-warn">{serviceAttention} need attention</span>}
          </div>
          <button type="button" onClick={() => navigate(connectorsPath())} className="col-start-2 row-start-1 ml-auto font-medium text-muted transition-colors hover:text-fg">Manage →</button>
        </div>
      </footer>

      {remoteOpen && (
        <RemoteDialog
          onClose={() => setRemoteOpen(false)}
          onOpenPhone={() => {
            setRemoteOpen(false);
            setPhoneOpen(true);
          }}
        />
      )}
      {phoneOpen && <PhoneModal onClose={() => setPhoneOpen(false)} />}
      {newSessionOpen && (
        <NewSessionDialog
          onClose={() => setNewSessionOpen(false)}
          onCreated={(s) => {
            setNewSessionOpen(false);
            noteCreated(s); // optimistic insert so the rail/list shows it immediately
            navigate(sessionsPath(s.id)); // land in the new session
          }}
        />
      )}
      {openDialogOpen && <OpenSkillDialog onClose={() => setOpenDialogOpen(false)} />}
      {renaming && <RenameSessionDialog session={renaming} onClose={() => setRenaming(null)} />}
      {titleMenu.menu}
    </div>
  );
}
