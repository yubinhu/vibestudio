"use client";

import { useEffect, useState } from "react";
import type { ReactNode } from "react";
import { useNavigate } from "react-router-dom";
import NavBar from "@/components/NavBar";
import { Spinner } from "@/components/ui";
import PhoneModal from "@/components/PhoneModal";
import { RemoteDialog } from "@/components/RemoteMenu";
import NewSessionDialog from "@/components/NewSessionDialog";
import RecentStrip from "@/components/RecentStrip";
import SkillGallery from "@/pages/home/SkillGallery";
import * as api from "@/lib/api";
import type { ConnectionInfo, TermSession } from "@/lib/api";
import { useSessions, isUnread, refresh as refreshSessions, noteCreated, nativeNotifyState } from "@/lib/sessions";
import { useMining, refreshMining } from "@/lib/mining";
import { useSkills } from "@/lib/skills";
import MineDialog from "@/components/MineDialog";
import * as push from "@/lib/push";
import { useRemote } from "@/lib/remote";
import { sessionsPath, credentialsPath, miningPath } from "@/lib/routes";

// A live session carries only the bare agent family ("claude" | "codex" | …); the
// rail keys colors off human labels, so map family → label + brand color here.
const AGENT_META: Record<string, { label: string; color: string }> = {
  claude: { label: "Claude Code", color: "#d97757" },
  codex: { label: "Codex", color: "#10a37f" },
  gemini: { label: "Gemini CLI", color: "#4285f4" },
  cursor: { label: "Cursor", color: "#7c83ff" },
  opencode: { label: "opencode", color: "#f59e0b" },
  openclaw: { label: "OpenClaw", color: "#a855f7" },
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
const KeyIcon = () => <Icon><circle cx="7.5" cy="15.5" r="4.5" /><path d="m21 2-9.5 9.5" /><path d="m15.5 7.5 3 3" /></Icon>;
const LinkIcon = () => <Icon><path d="M9 17H7A5 5 0 0 1 7 7h2" /><path d="M15 7h2a5 5 0 1 1 0 10h-2" /><line x1="8" x2="16" y1="12" y2="12" /></Icon>;
const SkillIcon = () => <Icon><path d="M4 19.5A2.5 2.5 0 0 1 6.5 17H20" /><path d="M6.5 2H20v20H6.5A2.5 2.5 0 0 1 4 19.5v-15A2.5 2.5 0 0 1 6.5 2z" /></Icon>;
const ServerIcon = () => <Icon><rect width="20" height="8" x="2" y="2" rx="2" /><rect width="20" height="8" x="2" y="14" rx="2" /><path d="M6 6h.01M6 18h.01" /></Icon>;
const BellIcon = () => <Icon><path d="M10.3 21a1.9 1.9 0 0 0 3.4 0" /><path d="M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9" /></Icon>;

// ─── shared bits ───
const actionBase = "inline-flex items-center gap-2 rounded-lg px-3.5 py-2 text-sm font-medium transition-colors";
const infoTint =
  "border-[color-mix(in_srgb,var(--info)_45%,transparent)] bg-[color-mix(in_srgb,var(--info)_7%,var(--surface))] hover:border-[color-mix(in_srgb,var(--info)_60%,transparent)] hover:bg-[color-mix(in_srgb,var(--info)_12%,var(--surface))]";

function Heading({ children, count, action }: { children: ReactNode; count?: ReactNode; action?: ReactNode }) {
  return (
    <div className="mb-3 flex items-center gap-2.5">
      <h2 className="text-sm font-semibold tracking-wide text-fg">{children}</h2>
      {count != null && <span className="text-xs text-faint">{count}</span>}
      {action && <span className="ml-auto">{action}</span>}
    </div>
  );
}

function SessionCard({ s, waiting, onClick }: { s: TermSession; waiting: boolean; onClick: () => void }) {
  const meta = agentMeta(s.agent);
  return (
    <button
      type="button"
      onClick={onClick}
      className={`flex flex-col gap-2 rounded-xl border p-4 text-left transition-all hover:-translate-y-0.5 hover:shadow-[0_2px_8px_-2px_rgba(0,0,0,0.08)] ${
        waiting ? infoTint : "border-border bg-surface hover:border-border-strong hover:bg-panel"
      }`}
    >
      <div className="flex items-center gap-2">
        <span className="h-2.5 w-2.5 shrink-0 rounded-full" style={{ background: meta.color }} aria-hidden />
        <span className="min-w-0 flex-1 truncate text-sm font-semibold text-fg">{s.label}</span>
        {waiting && (
          <span className="shrink-0 rounded-full bg-[color-mix(in_srgb,var(--info)_16%,transparent)] px-1.5 py-0.5 text-[0.6rem] font-semibold uppercase tracking-wide text-info">
            Your turn
          </span>
        )}
      </div>
      {s.title ? (
        <span className="truncate text-[0.8rem] text-muted" title={s.cwd}>
          {s.title}
        </span>
      ) : (
        <span className="truncate font-mono text-[0.7rem] text-faint" title={s.cwd}>
          {s.cwd}
        </span>
      )}
      <span className="text-xs text-faint">
        {meta.label} · {waiting ? `finished ${ago(Number(s.bellAt))}` : `active ${ago(Number(s.activity))}`}
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

function ConnectionCard({ c, onClick }: { c: ConnectionInfo; onClick: () => void }) {
  const tone =
    c.status === "connected"
      ? { dot: "bg-ok", label: "Connected", cls: "text-ok bg-[color-mix(in_srgb,var(--ok)_16%,transparent)]" }
      : c.status === "needs_reauth"
        ? { dot: "bg-warn", label: "Needs sign-in", cls: "text-warn bg-[color-mix(in_srgb,var(--warning)_16%,transparent)]" }
        : { dot: "bg-danger", label: "Error", cls: "text-danger bg-[color-mix(in_srgb,var(--error)_16%,transparent)]" };
  return (
    <button
      type="button"
      onClick={onClick}
      className="flex flex-col gap-2 rounded-xl border border-border bg-surface p-4 text-left transition-all hover:-translate-y-0.5 hover:border-border-strong hover:bg-panel hover:shadow-[0_2px_8px_-2px_rgba(0,0,0,0.08)]"
    >
      <div className="flex items-center gap-2">
        <span className={`h-2.5 w-2.5 shrink-0 rounded-full ${tone.dot}`} aria-hidden />
        <span className="min-w-0 flex-1 truncate text-sm font-semibold text-fg">{c.label}</span>
        <span className={`shrink-0 rounded-full px-1.5 py-0.5 text-[0.6rem] font-semibold uppercase tracking-wide ${tone.cls}`}>{tone.label}</span>
      </div>
      <span className="truncate font-mono text-[0.7rem] text-faint" title={c.host}>
        {c.host}
      </span>
      <span className="truncate text-xs text-muted" title={c.agentsConfigured.join(", ")}>
        {c.agentsConfigured.length > 0 ? `Wired to ${c.agentsConfigured.join(", ")}` : "Not yet wired to agents"}
      </span>
    </button>
  );
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
  const [mineOpen, setMineOpen] = useState(false);

  const [connections, setConnections] = useState<ConnectionInfo[] | null>(null);
  const [secretNames, setSecretNames] = useState<string[] | null>(null);
  // Skills come from the shared cache store (same one the gallery below uses), so
  // the count is cached across visits and the page runs ONE discovery scan, not two.
  const skills = useSkills();

  // Keep session activity/attention fresh while the dashboard is the visible page
  // (the 5s poll backstop lives in the Sessions workspace, which isn't mounted here;
  // the module's SSE subscription still delivers bells, this just refills timestamps).
  useEffect(() => {
    void refreshSessions();
    const t = setInterval(() => {
      if (!document.hidden) void refreshSessions();
    }, 5000);
    return () => clearInterval(t);
  }, []);

  // One-shot overview fetches. Each tolerates a 404 (feature absent on this server)
  // by settling to an empty/zero value so its card just reads "0" rather than hanging.
  // (Skills are handled by the useSkills() cache store above, not fetched here.)
  useEffect(() => {
    let alive = true;
    api.connectionsList().then((c) => alive && setConnections(c)).catch(() => alive && setConnections([]));
    // secretsList (not just the count) so the Credentials section can show key names.
    api.secretsList().then((l) => alive && setSecretNames(l.map((e) => e.key))).catch(() => alive && setSecretNames([]));
    return () => {
      alive = false;
    };
  }, []);

  // The stat card: cached count shown instantly on revisit; `loading` is true only
  // on the very first scan (nothing cached yet), so it never blanks to a spinner
  // again. `dirtyRoots` excludes proposed drafts already, so its size is the count.
  const skillStats = skills.loading ? null : { total: skills.total, dirty: skills.dirtyRoots.size };

  const sessions = sessionStore.sessions;
  const waiting = sessions.filter((s) => isUnread(s, sessionStore.seen, null));
  const waitingIds = new Set(waiting.map((s) => s.id));
  const running = sessions.filter((s) => !waitingIds.has(s.id));

  const connected = connections?.filter((c) => c.status === "connected").length ?? 0;
  const needsReauth = connections ? connections.length - connected : 0;
  const remoteConnected = remote.status.state === "connected";
  const secretCount = secretNames ? secretNames.length : null;

  const openSession = (id: string) => navigate(sessionsPath(id));
  const openNewSession = () => setNewSessionOpen(true);
  const mineDays =
    mining?.startedUnix != null ? Math.floor((Date.now() / 1000 - mining.startedUnix) / 86400) : null;

  return (
    <div className="flex min-h-dvh flex-col">
      <NavBar />

      <main className="mx-auto w-full max-w-6xl flex-1 px-6 pb-24 pt-10">
        {/* Hero — greeting + positioning + the primary on-ramps. */}
        <section className="flex flex-col gap-5 sm:flex-row sm:items-end sm:justify-between">
          <div>
            <p className="text-[0.7rem] font-semibold uppercase tracking-[0.2em] text-accent">VibeStudio</p>
            <h1 className="mt-1 text-3xl font-semibold tracking-tight text-fg">{greeting()}.</h1>
            <p className="mt-1.5 text-sm text-muted">Run, teach, and connect all your coding agents — from any device, anywhere.</p>
          </div>
          <div className="flex flex-wrap gap-2">
            <button type="button" onClick={openNewSession} className={`${actionBase} bg-action text-action-fg hover:bg-action-hover`}>
              <TerminalIcon />
              New session
            </button>
            <button type="button" onClick={() => setMineOpen(true)} className={`${actionBase} border border-border text-fg hover:bg-panel`}>
              <PickaxeIcon />
              Mine
            </button>
            <button type="button" onClick={() => navigate(credentialsPath())} className={`${actionBase} border border-border text-fg hover:bg-panel`}>
              <LinkIcon />
              Connect
            </button>
          </div>
        </section>

        <RecentStrip />

        {/* Phone/browser only: the one gesture-gated moment to opt into pushes. */}
        <PushNudge />

        {/* At a glance — the overview strip, up top: quick counts + where you're
            running. Each card scrolls to (or opens) the fuller view below. */}
        <section className="mt-10">
          <Heading>At a glance</Heading>
          <div className="grid grid-cols-2 gap-3 lg:grid-cols-4">
            <StatCard
              icon={<SkillIcon />}
              label="Skills"
              value={skillStats ? skillStats.total : <Spinner className="h-5 w-5" />}
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
              icon={<KeyIcon />}
              label="Credentials"
              value={connections && secretNames ? connections.length + secretNames.length : <Spinner className="h-5 w-5" />}
              sub={
                needsReauth > 0
                  ? `${needsReauth} need sign-in`
                  : `${connections?.length ?? 0} connection${(connections?.length ?? 0) === 1 ? "" : "s"} · ${secretCount ?? 0} key${(secretCount ?? 0) === 1 ? "" : "s"}`
              }
              subTone={needsReauth > 0 ? "warn" : "muted"}
              onClick={() => document.getElementById("credentials")?.scrollIntoView({ behavior: "smooth", block: "start" })}
            />
            <StatCard
              icon={<ServerIcon />}
              label="Server"
              value={
                remoteConnected ? (
                  remote.status.host ? (
                    <span title={remote.status.host}>{hostLabel(remote.status.host)}</span>
                  ) : (
                    "Remote"
                  )
                ) : (
                  "Local"
                )
              }
              sub={remoteConnected ? "connected over SSH" : "running on this machine"}
              subTone={remoteConnected ? "ok" : "muted"}
              onClick={remote.available ? () => setRemoteOpen(true) : undefined}
            />
          </div>
        </section>

        {/* Sessions — one list; the ones that finished a turn and need you are tinted
            "Your turn" and sorted to the front (a highlight, not "the rest aren't live"). */}
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
          {sessions.length === 0 ? (
            <div className="flex flex-col items-start gap-3 rounded-xl border border-dashed border-border bg-surface p-6">
              <p className="text-sm text-muted">No agents running. Start a session to launch Claude Code, Codex, or a shell.</p>
              <button type="button" onClick={openNewSession} className={`${actionBase} bg-action text-action-fg hover:bg-action-hover`}>
                <TerminalIcon />
                New session
              </button>
            </div>
          ) : (
            <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-3">
              {[...waiting, ...running].map((s) => (
                <SessionCard key={s.id} s={s} waiting={waitingIds.has(s.id)} onClick={() => openSession(s.id)} />
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
          )}
        </section>

        {/* Credentials — the detail view: each connection with its status + which
            agents it's wired to, plus your API keys by name. */}
        <section id="credentials" className="mt-10">
          <Heading
            count={
              <>
                {connected} connected
                {needsReauth > 0 && <span className="text-warn"> · {needsReauth} need sign-in</span>}
                {secretCount != null && secretCount > 0 && ` · ${secretCount} key${secretCount === 1 ? "" : "s"}`}
              </>
            }
            action={
              <button type="button" onClick={() => navigate(credentialsPath())} className="text-xs font-medium text-accent hover:opacity-80">
                Manage →
              </button>
            }
          >
            Credentials
          </Heading>
          <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-3">
            {(connections ?? []).map((c) => (
              <ConnectionCard key={c.id} c={c} onClick={() => navigate(credentialsPath())} />
            ))}
            <button
              type="button"
              onClick={() => navigate(credentialsPath())}
              className="flex items-center gap-2 rounded-xl border border-dashed border-border p-4 text-left text-muted transition-colors hover:border-accent hover:text-accent"
            >
              <LinkIcon />
              <span className="text-sm font-medium">Connect a service</span>
            </button>
          </div>
          {secretNames && secretNames.length > 0 && (
            <div className="mt-4">
              <div className="mb-2 text-xs font-semibold uppercase tracking-wider text-muted">API keys</div>
              <div className="flex flex-wrap gap-2">
                {secretNames.map((k) => (
                  <button
                    key={k}
                    type="button"
                    onClick={() => navigate(credentialsPath())}
                    title="Manage in Credentials"
                    className="rounded-md border border-border bg-surface px-2.5 py-1 font-mono text-xs text-fg transition-colors hover:bg-panel"
                  >
                    {k}
                  </button>
                ))}
                <button
                  type="button"
                  onClick={() => navigate(credentialsPath())}
                  className="rounded-md border border-dashed border-border px-2.5 py-1 text-xs text-muted transition-colors hover:border-accent hover:text-accent"
                >
                  + Add key
                </button>
              </div>
            </div>
          )}
        </section>

        {/* The skill gallery, folded in below the cockpit — one home page, not two. */}
        <SkillGallery embedded />
      </main>

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
      {mineOpen && (
        <MineDialog
          onClose={() => setMineOpen(false)}
          onStarted={(terminalId) => {
            setMineOpen(false);
            void refreshMining();
            navigate(sessionsPath(terminalId)); // land in the run's conversation
          }}
        />
      )}
    </div>
  );
}
