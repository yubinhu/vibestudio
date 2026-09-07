"use client";

import { useCallback, useEffect, useMemo, useState } from "react";
import { useLocation } from "react-router-dom";
import NavBar from "@/components/NavBar";
import FolderPicker from "@/components/FolderPicker";
import { ConnectorAgents, ConnectorStatus } from "@/components/ConnectorAgents";
import { Spinner, btnGhost, btnPrimary } from "@/components/ui";
import { useConfirm } from "@/components/useConfirm";
import { agentColor } from "@/lib/agents";
import { useConnectors, refreshConnectors, checkConnectors, invalidateConnectors } from "@/lib/connectors";
import * as api from "@/lib/api";
import LocalStoreCard from "./LocalStoreCard";
import { ConnectDialog } from "./ConnectionsCard";

const KIND_LABEL: Record<api.ConnectorInfo["kind"], string> = { remote: "Remote MCP", local: "Local MCP", app: "Account app" };
const SCOPE_LABEL: Record<api.ConnectorAvailability["scope"], string> = {
  user: "User configuration", project: "Project", plugin: "Plugin", account: "Account", managed: "VibeStudio",
};

function Chevron({ className = "" }: { className?: string }) {
  return <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden className={className}><path d="m9 5 7 7-7 7" /></svg>;
}

function ConnectorIcon({ kind }: { kind: api.ConnectorInfo["kind"] }) {
  return (
    <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.75" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
      {kind === "remote" ? <><circle cx="12" cy="12" r="9" /><ellipse cx="12" cy="12" rx="4" ry="9" /><path d="M3 12h18" /></>
        : kind === "local" ? <><rect x="3" y="4" width="18" height="16" rx="2" /><path d="m7 9 3 3-3 3m6 0h4" /></>
          : <><rect x="3" y="3" width="7" height="7" rx="1.5" /><rect x="14" y="3" width="7" height="7" rx="1.5" /><rect x="3" y="14" width="7" height="7" rx="1.5" /><rect x="14" y="14" width="7" height="7" rx="1.5" /></>}
    </svg>
  );
}

function ConnectorRow({ connector, agents, busy, onReconnect, onDisconnect }: {
  connector: api.ConnectorInfo;
  agents: api.ConnectorAgent[];
  busy: boolean;
  onReconnect: (id: string) => void;
  onDisconnect: (id: string, name: string) => void;
}) {
  const managedIds = [...new Set([...(connector.managedConnectionIds ?? []), ...connector.availability.flatMap((a) => a.managedConnectionId ? [a.managedConnectionId] : [])])];
  return (
    <li className="border-t border-border first:border-t-0">
      <details className="group/connector">
        <summary className="grid cursor-pointer list-none grid-cols-[1fr_auto] items-center gap-x-5 gap-y-3 px-4 py-4 outline-none hover:bg-panel/50 focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-accent sm:px-5 md:grid-cols-[minmax(180px,0.75fr)_minmax(0,1.3fr)_auto] [&::-webkit-details-marker]:hidden">
          <div className="flex min-w-0 items-center gap-3">
            <span className="grid h-9 w-9 shrink-0 place-items-center rounded-lg border border-border bg-surface text-muted"><ConnectorIcon kind={connector.kind} /></span>
            <div className="min-w-0">
              <span className="block truncate text-sm font-semibold text-fg" title={connector.name}>{connector.name}</span>
              <span className="mt-0.5 block truncate text-xs text-muted" title={connector.host}>{connector.host ?? KIND_LABEL[connector.kind]}</span>
            </div>
          </div>
          <div className="col-span-2 row-start-2 min-w-0 md:col-span-1 md:row-start-auto"><ConnectorAgents connector={connector} agents={agents} /></div>
          <Chevron className="col-start-2 row-start-1 shrink-0 text-faint transition-transform group-open/connector:rotate-90 md:col-start-3" />
        </summary>
        <div className="border-t border-border bg-panel/35 px-4 py-4 sm:px-5">
          <div className="mb-3 flex flex-wrap items-center gap-2 text-xs text-muted">
            <span>{KIND_LABEL[connector.kind]}</span><span aria-hidden>·</span>
            <span>{managedIds.length ? "Includes access managed by VibeStudio" : "Managed in the source app"}</span>
          </div>
          <ul className="divide-y divide-border">
            {connector.availability.map((access, index) => {
              const agent = agents.find((a) => a.id === access.agentId);
              return (
                <li key={`${access.sourceId}:${access.agentId}:${index}`} className="grid gap-2 py-3 first:pt-0 last:pb-0 sm:grid-cols-[minmax(140px,0.65fr)_minmax(0,1.4fr)_auto]">
                  <div className="min-w-0">
                    <span className="text-xs font-medium text-fg">{agent?.label ?? access.agentId}</span>
                    {!!agent?.clients.length && <p className="mt-1 text-xs leading-relaxed text-muted">{agent.clients.join(" · ")}</p>}
                  </div>
                  <div className="min-w-0 text-xs">
                    <p className="text-fg">{access.sourceLabel} <span className="text-faint">· {SCOPE_LABEL[access.scope]}</span></p>
                    {access.projectPath && <p className="mt-1 break-all font-mono text-[11px] text-muted">Project: {access.projectPath}</p>}
                    {access.sourcePath && <p className="mt-1 break-all font-mono text-[11px] text-muted">{access.sourcePath}</p>}
                  </div>
                  <div className="sm:text-right"><ConnectorStatus state={access.state} /></div>
                </li>
              );
            })}
          </ul>
          {managedIds.length > 0 ? (
            <div className="mt-4 space-y-2 border-t border-border pt-3">
              {managedIds.map((id) => (
                <div key={id} className="flex flex-wrap items-center gap-2">
                  <span className="mr-auto text-xs text-muted">Managed by VibeStudio{managedIds.length > 1 ? ` · ${id}` : ""}</span>
                  <button type="button" disabled={busy} onClick={() => onReconnect(id)} className={btnGhost}>Reconnect</button>
                  <button type="button" disabled={busy} onClick={() => onDisconnect(id, connector.name)} className={`${btnGhost} text-danger`}>Disconnect</button>
                </div>
              ))}
            </div>
          ) : <p className="mt-4 border-t border-border pt-3 text-xs text-muted">Manage sign-in and settings in the source app. Discovery keeps its authentication there.</p>}
        </div>
      </details>
    </li>
  );
}

function DiscoverySources({ sources, agents }: { sources: api.ConnectorSource[]; agents: api.ConnectorAgent[] }) {
  const checked = sources.filter((s) => s.state === "scanned").length;
  const unavailable = sources.filter((s) => s.state === "unavailable").length;
  const errors = sources.filter((s) => s.state === "error").length;
  return (
    <details className="group/sources mt-5 rounded-lg border border-border">
      <summary className="flex cursor-pointer list-none flex-wrap items-center gap-2 px-4 py-3 text-xs outline-none focus-visible:ring-2 focus-visible:ring-accent [&::-webkit-details-marker]:hidden">
        <Chevron className="text-faint transition-transform group-open/sources:rotate-90" />
        <span className="font-medium text-fg">Discovery sources</span>
        <span className="text-muted">{checked} scanned{unavailable > 0 ? ` · ${unavailable} unavailable` : ""}</span>
        {errors > 0 && <span className="text-danger">· {errors} {errors === 1 ? "error" : "errors"}</span>}
      </summary>
      <div className="border-t border-border px-4 py-3">
        <p className="mb-3 text-xs leading-relaxed text-muted">Coverage depends on the apps, accounts, and project on this server. An unavailable source can still have connectors that VibeStudio cannot see.</p>
        <ul className="divide-y divide-border">
          {sources.map((source) => {
            const agentLabel = agents.find((agent) => agent.id === source.agentId)?.label ?? source.agentId;
            const showAgent = agentLabel && !source.label.toLowerCase().includes(agentLabel.toLowerCase());
            return (
              <li key={source.id} className="py-2 first:pt-0 last:pb-0">
                <div className="flex flex-wrap items-center gap-2 text-xs">
                  <span className="font-medium text-fg">{source.label}</span>
                  {showAgent && <span className="inline-flex items-center gap-1.5 rounded-full border border-border px-2 py-0.5 text-muted"><span className="h-1.5 w-1.5 rounded-full" style={{ background: agentColor(agentLabel) }} aria-hidden />{agentLabel}</span>}
                  <span className={source.state === "error" ? "text-danger" : "text-muted"}>{source.state === "scanned" ? "Scanned" : source.state === "unavailable" ? "Unavailable" : "Error"}</span>
                </div>
                {source.message && <p className="mt-1 break-words text-xs leading-relaxed text-muted">{source.message}</p>}
              </li>
            );
          })}
        </ul>
      </div>
    </details>
  );
}

/** Agent availability is the primary dimension; transport and ownership are
 * details. CLI/IDE variants share a family instead of duplicating connectors. */
export function Component() {
  const { hash } = useLocation();
  const [secretCount, setSecretCount] = useState<number | null>(null);
  const [project, setProject] = useState<string>();
  const [picker, setPicker] = useState(false);
  const [agentFilter, setAgentFilter] = useState("");
  const [query, setQuery] = useState("");
  const [dialog, setDialog] = useState<{ reconnect?: api.ConnectionInfo } | null>(null);
  const [busy, setBusy] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const { inventory, loading, refreshing, checking, error, checkedAt } = useConnectors(project);
  const confirm = useConfirm();

  useEffect(() => {
    if (hash === "#secrets") document.getElementById("secrets")?.scrollIntoView({ block: "start" });
  }, [hash]);

  const connectors = useMemo(() => {
    const search = query.trim().toLowerCase();
    return (inventory?.connectors ?? []).filter((c) =>
      (!agentFilter || c.availability.some((a) => a.agentId === agentFilter)) &&
      (!search || `${c.name} ${c.host ?? ""} ${KIND_LABEL[c.kind]}`.toLowerCase().includes(search)),
    ).sort((a, b) => a.name.localeCompare(b.name));
  }, [inventory, agentFilter, query]);

  const closeAndRefresh = useCallback(() => {
    setDialog(null);
    invalidateConnectors();
    void refreshConnectors(project);
    if (project) void refreshConnectors();
  }, [project]);

  const reconnect = async (id: string) => {
    setBusy(true);
    setActionError(null);
    try {
      const connection = (await api.connectionsList()).find((c) => c.id === id);
      if (!connection) throw new Error("This managed connector no longer exists. Refresh to see the latest connectors.");
      setDialog({ reconnect: connection });
    } catch (e) {
      setActionError(e instanceof Error ? e.message : "Couldn’t load the connector.");
    } finally { setBusy(false); }
  };

  const disconnect = async (id: string, name: string) => {
    if (!(await confirm({ title: `Disconnect ${name}?`, body: "Agents using this VibeStudio connection lose access immediately. Connections managed in other apps stay in those apps.", confirmLabel: "Disconnect", danger: true }))) return;
    setBusy(true);
    setActionError(null);
    try {
      await api.connectionDelete(id);
      invalidateConnectors();
      await refreshConnectors(project);
      if (project) void refreshConnectors();
    } catch (e) {
      setActionError(e instanceof Error ? e.message : "Couldn’t disconnect.");
    } finally { setBusy(false); }
  };

  return (
    <div className="flex min-h-dvh flex-col">
      <NavBar
        breadcrumb={
          <>
            <span className="text-faint" aria-hidden>
              /
            </span>
            <span className="truncate font-medium text-fg">Connectors</span>
          </>
        }
      />

      <main className="mx-auto w-full max-w-6xl flex-1 px-4 pb-24 pt-8 sm:px-6 sm:pt-10">
        <h1 className="text-2xl font-semibold tracking-tight text-fg">Connectors</h1>
        <p className="mt-1.5 max-w-2xl text-sm leading-relaxed text-muted">Connect your agents to services with integrations, API keys, and secrets.</p>

        <div className="mt-5 flex flex-wrap items-center gap-x-4 gap-y-2 text-sm">
          <span className="font-semibold text-fg">{inventory && secretCount !== null ? `${inventory.connectors.length + secretCount} connectors` : "Connector inventory"}</span>
          <span className="text-muted">{inventory?.connectors.length ?? "—"} services</span>
          <span className="text-muted">{secretCount ?? "—"} API keys & secrets</span>
        </div>

        <div className="mt-8 grid grid-cols-1 items-start gap-8 xl:grid-cols-[minmax(0,1.2fr)_minmax(0,1fr)]">
          <div id="secrets" className="min-w-0 scroll-mt-6 xl:col-start-2 xl:row-start-1">
            <LocalStoreCard onCountChange={setSecretCount} />
          </div>
          <div id="services" className="min-w-0 xl:col-start-1 xl:row-start-1">
            <div className="flex flex-wrap items-start justify-between gap-5">
              <div>
                <h2 className="text-lg font-semibold text-fg">Services</h2>
                <p className="mt-1.5 max-w-xl text-sm leading-relaxed text-muted">See which services your agents can use, and where their access is configured.</p>
              </div>
              <div className="flex flex-wrap items-center gap-2">
                <button type="button" disabled={refreshing || busy} onClick={() => void refreshConnectors(project)} className={`inline-flex items-center gap-2 ${btnGhost}`}>
                  {refreshing && !checking ? <Spinner className="h-3.5 w-3.5" /> : <span aria-hidden>↻</span>} Refresh
                </button>
                <button type="button" disabled={refreshing || busy} onClick={() => void checkConnectors(project)} title="Ask installed agents for account connectors and current access. This may start agent processes." className={`inline-flex items-center gap-2 ${btnGhost}`}>
                  {checking && <Spinner className="h-3.5 w-3.5" />}{checking ? "Checking agents…" : "Check agents"}
                </button>
                <button type="button" disabled={busy} onClick={() => setDialog({})} className={btnPrimary}>Add connector</button>
              </div>
            </div>

            <div className="mt-7 flex flex-wrap items-center gap-x-3 gap-y-2 border-y border-border py-3 text-xs">
              <span className="font-medium text-muted">Active server</span>
              <span className="text-faint">Known configurations on this server{project ? " + project" : ""}</span>
              {project ? <>
                <span className="min-w-0 max-w-full break-all font-mono text-fg" title={project}>{project}</span>
                <button type="button" onClick={() => setProject(undefined)} className="rounded px-1.5 py-1 text-muted hover:bg-panel hover:text-fg" aria-label="Remove project scope">✕</button>
                <button type="button" onClick={() => setPicker(true)} className="ml-auto text-accent hover:underline">Change project</button>
              </> : <button type="button" onClick={() => setPicker(true)} className="ml-auto text-accent hover:underline">Include a project…</button>}
            </div>

            {inventory && <div className="mt-5 flex flex-wrap items-center gap-2" role="group" aria-label="Filter connectors by agent">
              {[{ id: "", label: "All agents", clients: [] }, ...inventory.agents].map((agent) => {
                const count = agent.id ? inventory.connectors.filter((c) => c.availability.some((a) => a.agentId === agent.id)).length : inventory.connectors.length;
                const selected = agentFilter === agent.id;
                return <button key={agent.id} type="button" aria-pressed={selected} onClick={() => setAgentFilter(agent.id)} title={agent.clients.join(" · ")} className={`inline-flex items-center gap-2 rounded-full border px-3 py-1.5 text-xs transition-colors ${selected ? "border-accent/40 bg-accent-soft text-accent" : "border-border text-muted hover:bg-panel hover:text-fg"}`}>
                  {agent.id && <span className="h-1.5 w-1.5 rounded-full" style={{ background: agentColor(agent.label) }} aria-hidden />}{agent.label}<span className={selected ? "text-accent" : "text-faint"}>{count}</span>
                </button>;
              })}
            </div>}

            <div className="mb-3 mt-6 flex flex-wrap items-center justify-between gap-3">
              <h3 className="text-sm font-semibold text-fg">{inventory ? `${connectors.length} ${connectors.length === 1 ? "service" : "services"}` : "Discovered services"}</h3>
              <input type="search" aria-label="Search connectors" value={query} onChange={(e) => setQuery(e.target.value)} placeholder="Search connectors…" className="w-56 max-w-full rounded-md border border-border bg-surface px-3 py-1.5 text-sm text-fg outline-none placeholder:text-faint focus:border-accent" />
            </div>

            {(error || actionError) && <p role="alert" className="mb-4 rounded-lg border border-danger/30 bg-danger/5 px-4 py-3 text-sm text-danger">{actionError ?? error}{error && inventory ? " Showing the last successful discovery." : ""}</p>}

            <section aria-label="Connector inventory" aria-busy={refreshing} className="overflow-hidden rounded-xl border border-border bg-surface">
              {loading ? <p className="flex items-center gap-2 px-5 py-10 text-sm text-muted"><Spinner className="h-3.5 w-3.5" /> Discovering connectors on this server…</p>
                : !inventory ? <div className="space-y-2 px-5 py-10 text-center"><p className="text-sm font-medium text-fg">Connector discovery is unavailable</p><p className="text-sm text-muted">Refresh when the server is available to see your connectors.</p></div>
                  : connectors.length === 0 ? <div className="space-y-2 px-5 py-10 text-center">
                    <p className="text-sm font-medium text-fg">{query || agentFilter ? "No connectors match this filter" : "No connectors found in the scanned sources"}</p>
                    <p className="mx-auto max-w-md text-sm leading-relaxed text-muted">{query || agentFilter ? "Try another agent or clear your search." : "Include a project to discover its configuration, or check agents for account connectors. Discovery sources below show what could be checked."}</p>
                    {(query || agentFilter) && <button type="button" onClick={() => { setQuery(""); setAgentFilter(""); }} className="pt-1 text-sm text-accent hover:underline">Clear filters</button>}
                  </div> : <>
                    <div aria-hidden className="hidden grid-cols-[minmax(180px,0.75fr)_minmax(0,1.3fr)_14px] gap-5 border-b border-border bg-panel/40 px-5 py-2 text-[11px] font-medium uppercase tracking-wide text-faint md:grid"><span>Service</span><span>Agents & access</span><span /></div>
                    <ul>{connectors.map((connector) => <ConnectorRow key={connector.id} connector={connector} agents={inventory.agents} busy={busy} onReconnect={(id) => void reconnect(id)} onDisconnect={(id, name) => void disconnect(id, name)} />)}</ul>
                  </>}
            </section>

            <div className="mt-3 flex flex-wrap items-start justify-between gap-x-5 gap-y-2 text-xs leading-relaxed text-muted">
              <p className="max-w-2xl"><span className="font-medium text-fg">Configured</span> means found in settings or plugin files, or enabled for this account; the service connection is unverified. <span className="font-medium text-fg">Connected</span> means reported by an agent at the last check. Access may vary by project, account, and client version.</p>
              {inventory && <span className="shrink-0 text-faint">Settings scanned {new Date(inventory.scannedAt < 1e12 ? inventory.scannedAt * 1000 : inventory.scannedAt).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })}</span>}
            </div>
            <p className="mt-2 text-xs leading-relaxed text-muted">{checkedAt ? `Agent results from ${new Date(checkedAt).toLocaleString([], { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" })}. Refresh reads settings; check agents again to update access.` : "Check agents asks installed runtimes for account connectors and current access. It may start agent processes."}</p>
            {inventory && <DiscoverySources sources={inventory.sources} agents={inventory.agents} />}
          </div>
        </div>
      </main>
      {picker && <FolderPicker initialPath={project} title="Include project connectors" selectLabel="Use this project" onClose={() => setPicker(false)} onSelect={(path) => { setProject(path); setPicker(false); }} />}
      {dialog && <ConnectDialog reconnect={dialog.reconnect} onClose={() => setDialog(null)} onDone={closeAndRefresh} />}
    </div>
  );
}
