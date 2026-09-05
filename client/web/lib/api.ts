// Backend bridge: the frontend reaches every capability over HTTP/JSON (+ SSE for
// streaming) at `/api/*`, served by skill-server on the same origin as the SPA —
// a loopback server locally, an SSH-tunnelled one remotely. There is no second
// transport. YAML parse/validate stays here in TS (lib/skill).
import {
  parseSkillMd,
  serializeSkillMd,
  validateSkill,
  estimateTokens,
  countLines,
  type SkillFrontmatter,
} from "@/lib/skill";
import type { SkillData, FileData, TreeNode } from "@/lib/types";
import { isBootstrapSkill } from "@/lib/agents";
import { log } from "@/lib/log";

// Same-origin by default (server serves the UI + /api). Override for dev with
// VITE_API_BASE (e.g. point a Vite dev server at a remote skill-server).
const API_BASE = (import.meta.env.VITE_API_BASE as string | undefined) ?? "";

// Switchboard routes are pinned to the local server and safe to repeat: profile
// save/delete overwrite by id, connect/disconnect are guarded server-side, and
// keygen just mints an unused keypair. Through-tunnel POSTs (sessions, git…) are
// NOT in this set — a blind repeat could double-apply.
const RETRYABLE_POST = /^(remote|ssh)\//;

async function http<T>(method: "GET" | "POST", path: string, args?: Record<string, unknown>, retried = false): Promise<T> {
  let res: Response;
  try {
    res = await fetch(`${API_BASE}/api/${path}`, {
      method,
      headers: method === "POST" ? { "Content-Type": "application/json" } : undefined,
      body: method === "POST" ? JSON.stringify(args ?? {}) : undefined,
    });
  } catch (e) {
    // Transport failure (server unreachable / dropped tunnel). On iOS this is the
    // signature of returning to foreground: the webview's pooled keep-alive
    // connection was reset during the suspension (WebKit's one-shot "Load failed",
    // never auto-retried for POSTs), or the shell is respawning a reclaimed
    // listener. One delayed retry cures both — but only where a repeat is safe.
    if (!retried && (method === "GET" || RETRYABLE_POST.test(path))) {
      await new Promise((r) => setTimeout(r, 400));
      return http(method, path, args, true);
    }
    // Console-only in dev; not forwarded — the log endpoint lives on the very
    // server that's unreachable.
    log.debug("api", `${method} ${path} — network error`, e instanceof Error ? e.message : String(e));
    throw e;
  }
  const json = await res.json().catch(() => ({}));
  if (!res.ok) {
    // Console-only (debug): the server already logs its own 4xx/5xx, so forwarding
    // would be redundant networking.
    log.debug("api", `${method} ${path} -> ${res.status}`, (json && json.error) || "");
    // Carry the HTTP status so callers can tell an HTTP error (e.g. a 404 — route
    // absent) from a transport failure (the rethrow above has no `status`). Used by
    // the remote store to distinguish "no remoting on this server" from "the local
    // server is unreachable".
    const err = new Error((json && json.error) || `Request failed (${res.status})`) as Error & {
      status?: number;
      detail?: string;
    };
    err.status = res.status;
    // Some routes pair the machine `error` code with a human `message` (e.g.
    // connection-begin 400s); carry it so callers can surface the readable one.
    if (json && typeof json.message === "string") err.detail = json.message;
    throw err;
  }
  return json as T;
}

interface RawSkill {
  root: string;
  dirName: string;
  raw: string;
  tree: TreeNode[];
  files: string[];
  fileCount: number;
  dirCount: number;
  totalBytes: number;
}

// --- HTTP endpoints (skill-server /api/*) ---
const readSkillRaw = (path: string) => http<RawSkill>("POST", "skills/read", { path });

export const readFile = (root: string, rel: string) => http<FileData>("POST", "fs/read", { root, rel });

/** Cheap metadata stat (mtime + size) for the show-latest poll — no read/hash. */
export interface FileStat {
  mtimeMs: number;
  size: number;
}
export const statFile = (root: string, rel: string) => http<FileStat>("POST", "fs/stat", { root, rel });

/** Outcome of a write. `written` carries the new baseline tag; `stale` means disk
 *  advanced past `expectedEtag` (an external process wrote the file) — the write was
 *  REFUSED and the current disk bytes come back for the caller to reconcile. */
export type WriteOutcome =
  | { status: "written"; etag: string }
  | { status: "stale"; diskEtag: string; diskContent: string };

/** Write a file. Pass `expectedEtag` (the tag from the last read/write) to make it a
 *  compare-and-swap that never clobbers a newer disk version; omit it for an
 *  unconditional overwrite. */
export const writeFile = (root: string, rel: string, content: string, expectedEtag?: string) =>
  http<WriteOutcome>("POST", "fs/write", { root, rel, content, expectedEtag });

/** Delete a file or folder inside the skill (folders recurse). SKILL.md and the
 *  skill root are protected server-side. Destructive — confirm before calling. */
export const deleteFile = (root: string, rel: string) =>
  http<{ ok: boolean }>("POST", "fs/delete", { root, rel }).then(() => {});

const readImage = (root: string, rel: string) =>
  http<{ mime: string; base64: string }>("POST", "fs/read-image", { root, rel });

/** Write a pasted/dropped media file into the skill folder. The bytes ride as
 *  base64 over JSON (decoded server-side, the machine the skill lives on — local
 *  or remote). The server places it under `dir` with a non-clobbering name
 *  derived from `name` and returns the path it wrote, relative to `root` — ready
 *  to drop into a `![](…)` link. */
export const writeSkillAsset = (root: string, dir: string, name: string, bytes: Uint8Array) =>
  http<{ rel: string }>("POST", "fs/write-asset", { root, dir, name, data: bytesToB64(bytes) });

export async function discoverSkills(): Promise<AgentSkills[]> {
  const groups = await http<AgentSkills[]>("GET", "skills/discover");
  // The bundled built-in skills (load-secrets, skill-miner) ship with the app
  // and install into personal dirs, so discovery tags them "personal". Re-tag
  // them "studio" so they keep their folder names but tuck into the bundled
  // dropdown (with a "VibeStudio" tag) rather than showing as your own skills.
  return groups.map((g) => ({
    ...g,
    skills: g.skills.map((s) => (isBootstrapSkill(s.root) ? { ...s, kind: "studio" } : s)),
  }));
}

// --- Remote-SSH connection manager (desktop only) ---
// These reach the LOCAL server's switchboard. While connected, EVERY other endpoint
// above transparently operates on the remote (the local server proxies it) — skills,
// files, git, secrets, terminals, the lot — so the whole window is remote-backed and
// this file is unchanged by remoting. On a server without remoting (browser dev, or
// the remote binary itself) these 404 — the caller treats that as "unavailable".
export interface RemoteHost {
  name: string;
  detail?: string | null;
}
export type RemoteState =
  | "idle"
  | "detecting"
  | "installing"
  | "launching"
  | "forwarding"
  | "connected"
  | "error";
export interface RemoteStatus {
  state: RemoteState;
  host?: string | null;
  message?: string | null;
}
export const remoteList = () => http<RemoteHost[]>("GET", "remote/list");
export const remoteStatus = () => http<RemoteStatus>("GET", "remote/status");
/** The host to auto-reconnect to on launch (the last one connected, VS Code-style),
 *  or null to start Local. THIS machine's connection memory — never proxied. */
export const remoteLast = () => http<{ host: string | null }>("GET", "remote/last");
export const remoteConnect = (host: string) => http<{ ok: boolean }>("POST", "remote/connect", { host });
export const remoteDisconnect = () => http<{ ok: boolean }>("POST", "remote/disconnect");

// --- Saved SSH connections (mobile only) ---
// The mobile app has no `~/.ssh` — connections are saved profiles whose private
// key lives in the OS keystore (iOS Keychain), managed over these pinned-local
// routes. `id` is `user@host:port`, the exact string `remoteConnect` takes. On
// servers without a credential store (desktop, standalone) these 404 — the UI
// treats that as "not the mobile app" and hides the credential form.
export interface SshProfile {
  id: string;
  host: string;
  port: number;
  user: string;
}
/** Resolves null when this server has no credential store (`/api/remote/profiles` 404s). */
export const sshProfiles = (): Promise<SshProfile[] | null> =>
  http<SshProfile[]>("GET", "remote/profiles").catch((e: unknown) => {
    if ((e as { status?: number } | null)?.status === 404) return null;
    throw e;
  });
/** Save a connection: profile to disk, `privateKey` into the OS keystore. The key
 *  travels in exactly once, here — no route ever returns it. */
export const sshProfileSave = (p: { host: string; port: number; user: string; privateKey: string }) =>
  http<{ ok: boolean; id: string }>("POST", "remote/profiles/save", p);
export const sshProfileDelete = (id: string) => http<{ ok: boolean }>("POST", "remote/profiles/delete", { id });
/** Generate an ed25519 keypair ON THIS DEVICE (pinned local, never proxied). The
 *  private half goes straight back out via sshProfileSave; the public half is what
 *  the user pastes into the server's authorized_keys. */
export interface GeneratedSshKey {
  privateKey: string;
  publicKey: string;
  fingerprint: string;
}
export const sshKeygen = (comment?: string) => http<GeneratedSshKey>("POST", "ssh/keygen", { comment });

// --- "Open on your phone" (desktop only): serve the app over the user's Tailscale
//     network and show a QR code a phone can scan. Absent (404) on standalone
//     remote servers — the caller treats that as "unavailable". ---
export interface PhoneStatus {
  tailscale: "ok" | "missing" | "stopped" | "needs_login";
  serving: boolean;
  /** The app's own in-process server — always present, it IS the responding process. */
  server: { version: string | null; port: number };
  /** Null unless `serving` and tailscale is "ok". */
  url: string | null;
  /** The URL as an inline SVG QR code; null on the same terms as `url`. */
  qrSvg: string | null;
}
export type PhoneEnableResult =
  | ({ ok: true } & PhoneStatus)
  | {
      ok: false;
      stage: "tailscale" | "operator" | "consent" | "serve";
      message: string;
      /** stage="consent": approve serving in the Tailscale admin console here. */
      consentUrl?: string;
      /** stage="operator": one-time shell command that grants tailscale access. */
      command?: string;
    };
/** Resolves null when this server has no phone feature (`/api/phone/*` 404s). */
export const phoneStatus = (): Promise<PhoneStatus | null> =>
  http<PhoneStatus>("GET", "phone/status").catch((e: unknown) => {
    if ((e as { status?: number } | null)?.status === 404) return null;
    throw e;
  });
export const phoneEnable = () => http<PhoneEnableResult>("POST", "phone/enable");
export const phoneDisable = () => http<{ ok: boolean; message?: string }>("POST", "phone/disable");
/** Sign in to Tailscale (or bring it up) without leaving the app. */
export interface PhoneLoginResult {
  ok: boolean;
  stage?: "login" | "started";
  /** stage="login": open this to finish signing in to Tailscale. */
  loginUrl?: string;
  message?: string;
}
export const phoneLogin = () => http<PhoneLoginResult>("POST", "phone/login");

// --- recents (server-side; a NORMAL /api/* route, so it follows the active server —
//     each machine has its own list, the same whether reached locally or over SSH) ---
export interface Recent {
  /** The opened skill folder, or a loose markdown file's absolute path. Dedup key. */
  root: string;
  name: string;
  /** "skill" (default when absent) routes via studioPath; "markdown" via markdownPath. */
  kind?: "skill" | "markdown";
  /** Unix seconds; absent on entries recorded by older versions. */
  openedAt?: number;
}
export const recentsList = () => http<Recent[]>("GET", "recents/list");
export const recentsAdd = (r: Recent) =>
  http<Recent[]>("POST", "recents/add", { root: r.root, name: r.name, kind: r.kind });
export const recentsRemove = (root: string) => http<Recent[]>("POST", "recents/remove", { root });

// Filesystem and session defaults live on the active server. Visual preferences
// (theme, panel sizes) stay in browser storage.
export type PickerContext = "open" | "session" | "import";
export interface TerminalPrefs {
  cwd: string;
  ide: boolean;
  skip: boolean;
  auto: boolean;
  extra: string;
}
export interface WorkspacePreferences {
  pickerLocations: Partial<Record<PickerContext, string>>;
  terminal: Record<string, TerminalPrefs>;
  lastAgent: string | null;
}
export const preferencesGet = () => http<WorkspacePreferences>("GET", "preferences");
export const rememberPicker = (context: PickerContext, path: string) =>
  http<WorkspacePreferences>("POST", "preferences/picker", { context, path });
export const rememberTerminal = (agentId: string, prefs: TerminalPrefs) =>
  http<WorkspacePreferences>("POST", "preferences/terminal", { agentId, prefs });

// --- app auto-update (the server checks GitHub releases; the desktop shell installs) ---
export interface UpdateAvailable {
  version: string;
  notes: string | null;
  date: string | null;
}
export interface UpdateStatus {
  /** The server's own version. */
  current: string;
  /** The strictly-newer release on offer, or null when up to date. */
  available: UpdateAvailable | null;
  /** This server can install the update itself (the desktop shell); when false
   *  the user downloads from `releaseUrl` manually. */
  canAuto: boolean;
  phase: "idle" | "downloading" | "ready" | "error";
  /** Download percentage while `phase` is "downloading", when known. */
  progress: number | null;
  error: string | null;
  releaseUrl: string;
}
export const updateStatus = () => http<UpdateStatus>("GET", "update/status");
export const updateApply = () => http<{ ok: boolean }>("POST", "update/apply");

// --- composed helpers ---
export async function loadSkill(path: string): Promise<SkillData> {
  const r = await readSkillRaw(path);
  const parsed = parseSkillMd(r.raw);
  const validation = validateSkill({
    frontmatter: parsed.frontmatter,
    body: parsed.body,
    hasFrontmatter: parsed.hasFrontmatter,
    parseError: parsed.parseError,
    dirName: r.dirName,
    files: r.files,
  });
  return {
    root: r.root,
    dirName: r.dirName,
    raw: r.raw,
    frontmatter: parsed.frontmatter,
    frontmatterRaw: parsed.frontmatterRaw,
    body: parsed.body,
    hasFrontmatter: parsed.hasFrontmatter,
    parseError: parsed.parseError,
    tree: r.tree,
    files: r.files,
    validation,
    stats: {
      bodyLines: countLines(parsed.body),
      bodyTokens: estimateTokens(parsed.body),
      fileCount: r.fileCount,
      dirCount: r.dirCount,
      totalBytes: r.totalBytes,
    },
  };
}

export async function saveSkillMd(root: string, frontmatter: SkillFrontmatter, body: string): Promise<void> {
  await writeFile(root, "SKILL.md", serializeSkillMd(frontmatter, body));
}

export async function imageDataUrl(root: string, rel: string): Promise<string> {
  const { mime, base64 } = await readImage(root, rel);
  return `data:${mime};base64,${base64}`;
}

/**
 * Package the skill into a `.skill` (a deflate zip) and save it, reporting where
 * it landed. `vars` (declared env names present in the store) bundles their
 * values as a `.env` so the recipient can run it immediately; empty =
 * declaration-only.
 *
 * On the desktop app (this machine, no remote connected) the server writes the
 * file into the Downloads folder and returns the real `path`, so the UI can name
 * it and reveal it — the webview's own blob download is silent and path-less. A
 * browser/phone client, or one with a remote connected, 404s that route and
 * falls back to the blob download, where the browser shows its own download UI
 * (`path` is then null). Packaging errors (bad frontmatter, size cap) propagate
 * for the caller to show.
 */
export async function exportSkill(root: string, vars: string[] = []): Promise<{ path: string | null }> {
  try {
    const { path } = await http<{ path: string }>("POST", "download/skill/save", { root, vars });
    return { path };
  } catch (e) {
    // 404 = save-to-disk unavailable here (browser/phone, or a remote is
    // connected) → the browser blob download below. Anything else is a real
    // packaging error; let it propagate.
    if ((e as { status?: number } | null)?.status !== 404) throw e;
  }
  await downloadSkillBlob(root, vars);
  return { path: null };
}

/** Reveal a saved file in the OS file manager (Finder/Explorer). Pinned-local —
 *  only reachable from the desktop app on this machine. */
export const revealPath = (path: string) => http<{ ok: boolean }>("POST", "reveal", { path });

/**
 * The browser blob-download fallback: fetch the `.skill` and save it via a
 * synthetic `<a download>`. Fetched as a blob rather than a bare link so the
 * server's validate gate — and the size cap — surface as a thrown error the
 * caller can show, instead of the browser silently saving a JSON error body as
 * the "file".
 */
async function downloadSkillBlob(root: string, vars: string[]): Promise<void> {
  const q = new URLSearchParams({ root });
  if (vars.length) q.set("vars", vars.join(","));
  const res = await fetch(`${API_BASE}/api/download/skill?${q.toString()}`);
  if (!res.ok) {
    let msg = `Couldn't package the skill (HTTP ${res.status}).`;
    try {
      const j = await res.json();
      if (j?.error) msg = j.error;
    } catch {
      /* non-JSON body — keep the generic message */
    }
    throw new Error(msg);
  }
  const blob = await res.blob();
  const cd = res.headers.get("Content-Disposition") ?? "";
  const filename = /filename="?([^";]+)"?/.exec(cd)?.[1] ?? "skill.skill";
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  a.remove();
  URL.revokeObjectURL(url);
}

/** Scan the skill's files for which managed secrets it references (auto-detect). */
export const detectRequiredEnv = (root: string) => http<string[]>("POST", "skills/detect-env", { root });

/**
 * Download the skill's secrets as a plain-text `.env` file (browser download).
 * `vars` = the skill's declared env names; only those present in the store are
 * included. Deliberately plain text: the values never travel in the repo, so
 * this is the explicit hand-off the user shares over a channel they trust.
 */
export function downloadEnv(root: string, vars: string[]): void {
  const q = new URLSearchParams({ root, vars: vars.join(",") });
  const a = document.createElement("a");
  a.href = `${API_BASE}/api/download/env?${q.toString()}`;
  a.rel = "noopener";
  document.body.appendChild(a);
  a.click();
  a.remove();
}

// --- folder browsing (server-side; backs the in-app FolderPicker) ---
export interface DirEntry {
  name: string;
  isDir: boolean;
  isSkill: boolean;
  /** A markdown-family file (only present when `listDir` is called with
   *  `includeFiles`); false for dirs and non-markdown files. */
  isMarkdown: boolean;
}
export interface DirListing {
  path: string;
  parent: string | null;
  entries: DirEntry[];
}
/** Browse a directory server-side. By default lists subdirectories only (the
 *  skill-folder picker); pass `includeFiles` to also list regular files (the
 *  loose-markdown picker), each flagged `isMarkdown`. */
export const listDir = (path: string, includeFiles = false) =>
  http<DirListing>("POST", "fs/list-dir", { path, includeFiles });

export interface DiscoveredSkill {
  name?: string;
  description?: string;
  root: string;
  /** Backend-computed provenance: "personal" | "official" | "plugin". */
  kind: string;
  /** Repo/folder name when the skill is project-scoped (`<repo>/.claude/skills/…`). */
  project?: string;
  /** A machine-generated draft staged under `generated-skills/` (e.g. by the
   *  skill-miner) — surfaced as a proposal to accept (promote) or discard. The
   *  backend always sends it (defaults to false), so it's required here. */
  proposed: boolean;
}
export interface AgentSkills {
  agent: string;
  skills: DiscoveredSkill[];
}

// --- sync a skill into a shared/global skills dir other agents read ---
export interface SyncTarget {
  /** Stable id passed back to syncSkill ("universal" | "claude-code"). */
  id: string;
  label: string;
  /** Canonical dir a new copy/link lands in. */
  dir: string;
  /** Agent display names this destination serves. */
  reaches: string[];
  /** Already reachable from this destination. */
  present: boolean;
  /** The skill natively lives here. */
  isSource: boolean;
  /** Present via a symlink (a shared copy that tracks the source). */
  linked: boolean;
  /** When present via a legacy alias dir, its basename (e.g. ".codex/skills"). */
  reachedVia?: string;
}
export interface SyncResult {
  dest: string;
  linked: boolean;
}
export const syncTargets = (root: string) => http<SyncTarget[]>("POST", "sync/targets", { root });
export const syncSkill = (root: string, target: string, overwrite: boolean, link: boolean) =>
  http<SyncResult>("POST", "sync/skill", { root, target, overwrite, link });

// --- create a brand-new skill ---
export interface SkillHome {
  /** Stable id passed back to createSkill ("universal" | "claude-code"). */
  id: string;
  label: string;
  /** Absolute path of the dir a new skill lands in. */
  dir: string;
  /** Agent display names this location serves. */
  reaches: string[];
}
export const skillHomes = () => http<SkillHome[]>("GET", "skills/homes");

/** A starter SKILL.md body: heading + the canonical Overview/When-to-use/Instructions skeleton. */
function starterBody(name: string): string {
  const title = name.replace(/-/g, " ").replace(/\b\w/g, (c) => c.toUpperCase());
  return [
    `# ${title}`,
    "",
    "One or two sentences describing what this skill does.",
    "",
    "## When to use",
    "",
    "Use this skill when … — describe the trigger conditions so the agent activates it reliably.",
    "",
    "## Instructions",
    "",
    "1. First step.",
    "2. Second step.",
    "",
    "## Resources",
    "",
    "Add supporting files in this folder and link to them here.",
  ].join("\n");
}

/**
 * Create a new skill folder in the chosen location with a scaffolded SKILL.md
 * (frontmatter + starter body). Returns the new skill's root path; load it next.
 */
export async function createSkill(target: string, name: string, description: string): Promise<string> {
  const content = serializeSkillMd({ name, description }, starterBody(name));
  return http<string>("POST", "skills/create", { target, name, content });
}

// --- import an existing skill (folder or .zip) into a chosen skill home ---
/** A `.env` pair carried by an imported skill (kept out of the copied folder). */
export interface ImportedSecret {
  key: string;
  value: string;
  /** A secret with this key already exists in the store (loading overwrites it). */
  exists: boolean;
}
export interface ImportResult {
  /** Canonical root of the imported skill — open it next. */
  root: string;
  /** The skill/folder name it was imported as. */
  name: string;
  /** The home directory it landed in. */
  dir: string;
  /** An existing skill of the same name was replaced. */
  overwrote: boolean;
  /** `.env` pairs found in the source, for optional loading into the secret store. */
  env: ImportedSecret[];
}

/** Copy an existing skill folder (by absolute path) into the chosen home. */
export const importSkillFolder = (source: string, target: string, overwrite: boolean) =>
  http<ImportResult>("POST", "import/folder", { source, target, overwrite });

/** Import from an uploaded `.zip` (base64 bytes, decoded server-side). */
export const importSkillZipUpload = (data: string, target: string, overwrite: boolean) =>
  http<ImportResult>("POST", "import/zip", { data, target, overwrite });

/** Import by cloning a skill repository (GitHub/GitLab/any git clone URL). The
 *  clone keeps its origin, so the skill arrives already connected for sync. */
export const importSkillFromRemote = (url: string, target: string, overwrite: boolean) =>
  http<ImportResult>("POST", "import/remote", { url, target, overwrite });

// --- delete a skill (guarded; unlinks a synced copy, else removes the folder) ---
export interface DeleteResult {
  removed: string;
  wasLink: boolean;
}
export const deleteSkill = (root: string) => http<DeleteResult>("POST", "skills/delete", { root });

// --- accept a proposed (generated-skills) skill: promote it into the real home ---
export interface PromoteResult {
  /** New canonical root after the skill is moved out of generated-skills/. */
  root: string;
}
/** Accept a proposed skill — move it out of its `generated-skills/` staging folder
 *  up into the real skills home, turning it into an ordinary skill. */
export const promoteSkill = (root: string) => http<PromoteResult>("POST", "skills/promote", { root });

// --- per-skill git version control ---
export interface GitInfo {
  available: boolean;
  isRepo: boolean;
  inParentRepo: boolean;
  toplevel?: string;
  branch?: string;
  dirty: boolean;
  hasRemote: boolean;
  hasIdentity: boolean;
}
export interface GitCommit {
  /** Full SHA — the handle for fetching this commit's diff. */
  sha: string;
  short: string;
  message: string;
  author: string;
  /** ISO-8601 author date (for an absolute-date tooltip). */
  isoDate: string;
  relativeDate: string;
  /** 1-based version number (position in linear history; first commit = 1). */
  number: number;
}
/** One uncommitted change in the working tree (a `git status` entry). */
export interface GitFileChange {
  /** Path relative to the skill root (new path for a rename). */
  path: string;
  /** Previous path for a rename/copy. */
  origPath?: string;
  /** added | modified | deleted | renamed | copied | untracked | typechange | unmerged */
  kind: string;
  /** Recorded in the index. */
  staged: boolean;
  /** Differs in the working tree beyond what's staged. */
  unstaged: boolean;
}
/** The working tree's uncommitted state: per-file summary + one unified diff. */
export interface GitWorktreeDiff {
  files: GitFileChange[];
  /** Unified diff text covering every change (empty when clean). */
  diff: string;
  /** The diff hit the size cap and was cut short. */
  truncated: boolean;
}
/** A single commit's metadata plus its full unified diff. */
export interface GitCommitDetail {
  sha: string;
  short: string;
  subject: string;
  body: string;
  author: string;
  email: string;
  isoDate: string;
  relativeDate: string;
  diff: string;
  truncated: boolean;
  /** 1-based version number (this commit's position in linear history). */
  number: number;
}
export const gitInfo = (root: string) => http<GitInfo>("POST", "git/info", { root });
/** One skill root's uncommitted-changes flag (from the batch [`gitDirtyMany`]). */
export interface DirtyState {
  root: string;
  dirty: boolean;
}
/** Batch "has uncommitted changes?" for the home page — one cheap status check per
 *  skill root, scoped to its own folder. Roots not under git report `dirty: false`. */
export const gitDirtyMany = (roots: string[]) => http<DirtyState[]>("POST", "git/dirty-many", { roots });
/** Begin (or resume) tracking a skill: init its repo with a baseline commit,
 *  clearing any prior opt-out. Personal skills are auto-tracked on discovery, so
 *  this is mainly the re-track path for a skill that was opted out. */
export const gitTrack = (root: string) => http<GitInfo>("POST", "git/track", { root });
/** Opt a skill out of version tracking: delete its local .git and remember the
 *  choice so discovery won't re-create it. Destructive — confirm before calling. */
export const gitUntrack = (root: string) =>
  http<{ ok: boolean }>("POST", "git/untrack", { root }).then(() => {});
export const gitCommit = (root: string, message: string) =>
  http<{ sha: string; summary: string }>("POST", "git/commit", { root, message });
export const gitLog = (root: string, limit = 20) => http<GitCommit[]>("POST", "git/log", { root, limit });

// --- commit-message generation (a logged-in coding-agent CLI by default;
//     the on-device llama.cpp engine when opted in) ---
/** Which backend will draft messages and whether it's ready, so the Save dialog
 *  can show "using your Claude login", a log-in hint, or the one-time on-device
 *  model download note. */
export interface CommitModelStatus {
  /** Active backend: "claude" | "codex" | "gemini" | "opencode" | "llama" | "none". */
  backend: "claude" | "codex" | "gemini" | "opencode" | "llama" | "none";
  /** A draft can be produced right now (logged-in CLI, or downloaded model). */
  ready: boolean;
  /** A supported CLI is installed but not logged in — hint the user to log in. */
  needsLogin: boolean;
  /** One-line human hint for the dialog. */
  detail: string;
  /** Model id (CLI model, or the llama GGUF id). Empty for "none". */
  model: string;
  /** llama backend: the GGUF is present on disk. Mirrors `ready` for a cloud CLI. */
  downloaded: boolean;
  /** llama backend: on-disk model size in MB, when present. */
  sizeMb?: number;
  /** llama backend: where the GGUF lives / will be cached. */
  path: string;
}
/** True when the active backend runs on-device (no cloud call / no metered
 *  credit / diff never leaves the machine) — so the eager background draft is
 *  safe to fire automatically. Cloud backends require an explicit click. */
export const isLocalCommitBackend = (s: CommitModelStatus | null) => s?.backend === "llama";
/** Draft a one-line message from the skill's uncommitted diff. The default
 *  backend shells out to a coding-agent CLI you're already logged into (keyless);
 *  the on-device model is used only when opted in. */
export const generateCommitMessage = (root: string) => http<string>("POST", "commit-message/generate", { root });
/** Force a fresh draft (the manual ✨ Generate button): ignores the cache and
 *  varies the seed, so each click offers a different phrasing. */
export const regenerateCommitMessage = (root: string) => http<string>("POST", "commit-message/regenerate", { root });
export const commitModelStatus = () => http<CommitModelStatus>("GET", "commit-message/model-status");
/** A draft already prepared in the background for `root`'s current diff, or null
 *  when none is ready. Instant — never runs the model. Used to pre-fill the Save
 *  dialog from the eagerly-generated message. */
export const peekCommitMessage = (root: string) => http<string | null>("POST", "commit-message/peek", { root });
export const gitStatus = (root: string) => http<GitFileChange[]>("POST", "git/status", { root });
export const gitWorktreeDiff = (root: string) => http<GitWorktreeDiff>("POST", "git/worktree-diff", { root });
export const gitCommitDiff = (root: string, sha: string) =>
  http<GitCommitDetail>("POST", "git/commit-diff", { root, sha });
/** The file's content at a revision ("HEAD" or a SHA) — the "original" the
 *  in-editor diff overlay compares against. Empty string when absent at that rev. */
export const gitFileAt = (root: string, rev: string, path: string) =>
  http<string>("POST", "git/file-at", { root, rev, path });
/** The tracked file paths at a revision (a SHA or "HEAD") — for browsing a past
 *  version's files. */
export const gitFilesAt = (root: string, rev: string) => http<string[]>("POST", "git/files-at", { root, rev });
/** Discard one path's working-tree changes back to HEAD (tracked → restore,
 *  untracked → delete). Destructive — confirm before calling. */
export const gitDiscard = (root: string, path: string) =>
  http<{ ok: boolean }>("POST", "git/discard", { root, path }).then(() => {});
/** Discard ALL uncommitted changes back to HEAD. Destructive — confirm first. */
export const gitDiscardAll = (root: string) =>
  http<{ ok: boolean }>("POST", "git/discard-all", { root }).then(() => {});

// --- version preview (view/edit a past version through the full editor) ---
/** Returned when entering version preview. */
export interface PreviewState {
  /** Uncommitted work was set aside (stashed) to show this version cleanly. */
  stashed: boolean;
  /** The branch we'll return to on exit. */
  branch?: string;
}
/** Enter "version preview": stash any uncommitted work and check `sha` into the
 *  working tree (detached) so the full editor renders that version as if current.
 *  Editing then autosaves onto it; saving makes a new linear version. Own-repo
 *  personal skills only. */
export const gitEnterVersion = (root: string, sha: string) =>
  http<PreviewState>("POST", "git/enter-version", { root, sha });
/** Leave version preview: reattach to the branch and restore the set-aside work
 *  (discarding any unsaved preview edits). Returns fresh GitInfo. */
export const gitExitVersion = (root: string) => http<GitInfo>("POST", "git/exit-version", { root });
/** Save the previewed/edited version as a NEW version on the branch tip (linear
 *  history); the set-aside work is discarded. */
export const gitKeepVersion = (root: string, message: string) =>
  http<{ sha: string; summary: string }>("POST", "git/keep-version", { root, message });

// --- publish a skill to GitHub (its own repo; the remote is the source of truth) ---
/** The GitHub sign-in the server found on its machine (where skill-server runs). */
export interface GhAuthInfo {
  /** Where it came from: "studio" (connected here) | "env" | "gh-cli" | "git-credential". */
  source: string;
  login: string;
  /** Classic-token scopes; fine-grained PATs don't report any. */
  scopes?: string;
}
/** The skill's remote, derived from its `origin` — having an origin IS being
 *  linked (a teammate's clone is linked automatically). Any git host works;
 *  github.com remotes additionally get token auth + repo-creation sugar. */
export interface GhLink {
  /** "github" (provider sugar applies) | "git" (GitLab, Bitbucket, self-hosted, …). */
  provider: "github" | "git";
  /** Short display label — "owner/repo" on GitHub, host/path elsewhere. */
  label: string;
  /** Browser URL, when derivable. */
  htmlUrl?: string;
  url: string;
}
/** Everything the "Publish to GitHub" panel needs, in one call. */
export interface GhStatus {
  auth?: GhAuthInfo;
  /** The gh CLI is installed on the server machine (for the sign-in hint). */
  ghCli: boolean;
  /** The OAuth device flow is available (a client id is configured). */
  deviceFlow: boolean;
  /** The skill is its own git repository (publishing requires it). */
  tracked: boolean;
  /** At least one version has been saved. */
  hasVersion: boolean;
  branch?: string;
  link?: GhLink;
  /** Uncommitted changes exist (they sync only once saved as a version). */
  dirty: boolean;
  /** Versions to push / pull — present when `checkRemote` and the fetch worked. */
  ahead?: number;
  behind?: number;
  /** The remote couldn't be reached (offline, auth) — shown, not fatal. */
  remoteError?: string;
}
/** A place the skill's repo can live: the user's account or one of their orgs. */
export interface GhOwner {
  login: string;
  kind: "user" | "org";
  /** The org's policy lets this user create repositories there. */
  canCreate: boolean;
}
export interface GhDeviceStart {
  userCode: string;
  verificationUri: string;
  /** Seconds between device-poll calls. */
  interval: number;
  expiresIn: number;
}
export interface GhDevicePoll {
  status: "pending" | "ok";
  login?: string;
  /** Current poll interval (grows when GitHub asks to slow down). */
  interval: number;
}
export interface GhPublishResult {
  htmlUrl: string;
  branch: string;
  /** Versions pushed by the initial publish. */
  pushed: number;
  login: string;
}
export interface GhSyncResult {
  /** What the sync did. */
  action: "upToDate" | "pushed" | "pulled" | "rebased";
  /** Versions pulled down / pushed up. */
  pulled: number;
  pushed: number;
  /** Both sides changed the same lines; the remote won those hunks (local
   *  versions were kept, rebased on top). */
  conflictResolved: boolean;
}
/** `checkRemote` also fetches and reports ahead/behind (one network round-trip). */
export const githubStatus = (root: string, checkRemote = false) =>
  http<GhStatus>("POST", "github/status", { root, checkRemote });
export const githubOwners = () => http<GhOwner[]>("POST", "github/owners");
/** Validate + store a pasted personal access token (the manual fallback). */
export const githubConnectToken = (token: string) =>
  http<GhAuthInfo>("POST", "github/connect-token", { token });
/** Forget the Studio-stored token (ambient gh/env/git sign-ins are untouched). */
export const githubDisconnect = () =>
  http<{ ok: boolean }>("POST", "github/disconnect").then(() => {});
export const githubDeviceStart = () => http<GhDeviceStart>("POST", "github/device-start");
export const githubDevicePoll = () => http<GhDevicePoll>("POST", "github/device-poll");
/** Create owner/repo on GitHub (empty, private by default), set it as the
 *  skill repo's origin, and push the local version history. */
export const githubPublish = (root: string, owner: string, repo: string, isPrivate: boolean) =>
  http<GhPublishResult>("POST", "github/publish", { root, owner, repo, private: isPrivate });
/** Connect ANY existing git remote by URL (GitLab, Bitbucket, self-hosted, …):
 *  sets origin and runs a first sync; on failure nothing is left connected.
 *  Uses the machine's own git credentials — no GitHub sign-in needed. */
export const githubConnectRemote = (root: string, url: string) =>
  http<GhSyncResult>("POST", "github/connect-remote", { root, url });
/** Reconcile with the remote (remote-first): ff-pull / push / rebase-and-push.
 *  After a pull/rebase the working tree changed — reload the skill. */
export const githubSyncNow = (root: string) => http<GhSyncResult>("POST", "github/sync", { root });
/** Quiet background fast-forward pull (remote is the source of truth); never
 *  pushes, rebases, or errors — anything unexpected is just `pulled: 0`. */
export const githubAutoPull = (root: string) => http<GhSyncResult>("POST", "github/auto-pull", { root });
/** Disconnect the skill from its remote (nothing is removed on GitHub). */
export const githubUnlink = (root: string) =>
  http<{ ok: boolean }>("POST", "github/unlink", { root }).then(() => {});

// --- secret manager (machine-local env vars for skills) ---
export interface SecretEntry {
  key: string;
  value: string;
}
export interface AgentInstall {
  agent: string;
  /** The agent's home dir exists on this machine. */
  installed: boolean;
  /** The load-secrets activation skill is installed for this agent. */
  hasSkill: boolean;
}
export interface SecretsStatus {
  configured: boolean;
  storePath: string;
  envPath: string;
  count: number;
  agents: AgentInstall[];
}
export interface SetupResult {
  envPath: string;
  storePath: string;
  installedAgents: string[];
  skillInstalled: boolean;
}
export const secretsStatus = () => http<SecretsStatus>("GET", "secrets/status");
export const secretsList = () => http<SecretEntry[]>("GET", "secrets/list");
/** Parse a pasted/uploaded `.env` body into store-ready entries (with
 *  already-exists flags); apply the chosen ones with [`secretSet`]. */
export const secretsPreviewEnv = (data: string) =>
  http<ImportedSecret[]>("POST", "secrets/preview-env", { data });
export const secretSet = (key: string, value: string) => http<void>("POST", "secrets/set", { key, value });
export const secretDelete = (key: string) => http<void>("POST", "secrets/delete", { key });
export const secretsSetup = () => http<SetupResult>("POST", "secrets/setup");

// --- MCP connections (Studio-held OAuth for remote MCP servers) ---
// Studio is the OAuth client: tokens live server-side only, and agents reach a
// connected MCP through the loopback gateway `/gw/:id/mcp` (NOT under /api),
// which injects Authorization upstream. No token material ever crosses here.

export interface ConnectionInfo {
  id: string;
  label: string;
  host: string;
  scopes: string[];
  status: "connected" | "needs_reauth" | "error";
  createdAt: number;
  lastError?: string;
  /** Agents whose MCP config already points at this connection's gateway URL. */
  agentsConfigured: string[];
}
/** A begun OAuth attempt: open `authorizeUrl` in the browser, then poll
 *  [`connectionPending`] with `state` until it resolves. */
export interface ConnectionBegin {
  state: string;
  authorizeUrl: string;
}
export interface ConnectionPending {
  status: "waiting" | "done" | "denied" | "expired";
  /** The connection's id, present once `status` is "done". */
  id?: string;
}
/** Start OAuth for an MCP server URL. 400s carry a machine `error` code
 *  (no_pkce | discovery_failed | registration_failed) + a human `message`
 *  (surfaced via the thrown error's `detail`). */
export const connectionBegin = (url: string, origin: string, label?: string) =>
  http<ConnectionBegin>("POST", "connections/begin", { url, origin, label });
export const connectionPending = (state: string) =>
  http<ConnectionPending>("GET", `connections/pending?state=${encodeURIComponent(state)}`);
export const connectionsList = () => http<ConnectionInfo[]>("GET", "connections/list");
/** Redo OAuth for an existing connection — keeps the same id/slug, so the
 *  gateway URL already in agent configs stays valid. Errors as in begin. */
export const connectionReconnect = (id: string, origin: string) =>
  http<ConnectionBegin>("POST", "connections/reconnect", { id, origin });
export const connectionDelete = (id: string) =>
  http<{ ok: boolean }>("POST", "connections/delete", { id }).then(() => {});

// --- app-managed agent terminals (tmux-backed; survive UI disconnect) ---

/** A launchable agent in the "New session" picker. The same agent can appear as
 *  its PATH CLI *and* the build bundled inside a VS Code / Cursor extension. */
export interface AgentOption {
  /** Stable id passed back to terminalCreate (e.g. "claude:cli", "codex:ext:vs-code", "shell"). */
  id: string;
  /** Family: "claude" | "codex" | "shell". */
  agent: string;
  label: string;
  /** "cli" | "extension" | "shell". */
  flavor: string;
  /** Human flavor, e.g. "CLI" or "VS Code extension". */
  flavorLabel: string;
  bin: string;
  version: string | null;
  /** Whether this agent supports `--ide` (attach to a running editor extension). */
  supportsIde: boolean;
  /** Whether the server's agent registry can launch this family's TUI with an
   *  initial prompt pre-submitted — the gate for mining runs. */
  canMine: boolean;
}

/** One live tmux-backed terminal session. */
export interface TermSession {
  id: string;
  label: string;
  agent: string;
  cwd: string;
  /** Unix seconds (string) when the session was created — the rail's stable sort key. */
  created: string;
  /** Unix seconds (string) of the session's most recent tmux activity.
   *  Informational — NOT the unread signal (an idle TUI keeps repainting). */
  activity: string;
  /** Unix seconds (string) of the last turn-completion BELL the agent rang
   *  ("0" until the first). The rail compares it against a per-session "last
   *  viewed" mark for the unread dot — so the dot means "finished a turn". */
  bellAt: string;
  /** A short human title read server-side from the agent's own session store
   *  (Claude ai-title, Codex/Gemini/Cursor first prompt). Absent when the agent
   *  has no readable session yet — callers fall back to the cwd. */
  title?: string;
  /** The agent session id forced at launch (`claude --session-id`), used
   *  server-side to map the terminal to its own transcript. Empty/absent for
   *  shells, resumed, and pre-existing sessions. */
  sessionId?: string;
}

export interface CreateTermArgs {
  /** An AgentOption.id. */
  agent: string;
  cwd: string;
  cols: number;
  rows: number;
  ide: boolean;
  skipPermissions: boolean;
  /** Claude only: start in auto mode (`--permission-mode auto`). Mutually
   *  exclusive with skipPermissions. */
  autoMode: boolean;
  extraArgs: string[];
  /** API-only (no dialog control): reopen the agent's recorded session in
   *  `cwd` — the server's agent registry builds the resume line — instead of
   *  starting fresh. The other launch flags are ignored. */
  resume?: boolean;
}

// --- skill mining (a skill-miner run in an interactive agent terminal) ---

/** A transcript source the miner can read, with its in-window session count. */
export interface MineSource {
  id: string;
  label: string;
  sessions: number;
}

export interface MineState {
  /** "idle" (never ran) | "running" (the run's TUI is up in its terminal) |
   *  "ended" (the terminal or the agent in it is gone). */
  status: string;
  /** While running: "scanning" | "analyzing" | "reviewing". */
  stage?: string;
  /** Sessions found by the discover step. */
  found?: number;
  startedUnix?: number;
  terminalId?: string;
  /** Run parameters from the record (absent when idle); agent is an AgentOption id. */
  agent?: string;
  model?: string;
  effort?: string;
  days?: number;
  sources?: string[];
  /** The prompt the run was launched with — shown instead of a derived window
   *  (which a hand-edited prompt can diverge from). "" for pre-capture runs. */
  prompt?: string;
}

/** One file in the active run dir (the history archive is excluded). */
export interface MineFile {
  rel: string;
  size: number;
  modifiedUnix: number;
}
export interface MineFiles {
  runDir: string;
  files: MineFile[];
}

/** A past run archived under history/<id>/ — display-only summary for the
 *  mining page's "Past runs" list. `agent` is an AgentOption id. */
export interface MineHistoryEntry {
  id: string;
  agent: string;
  model: string;
  effort: string;
  days: number;
  sources: string[];
  startedUnix: number;
  /** The prompt this run was launched with ("" for pre-capture runs). */
  prompt: string;
  status: string;
}

export interface MineStartArgs {
  days: number;
  sources: string[];
  /** An AgentOption.id (the agent that runs the mine). */
  agent: string;
  /** Allow in-place edits to existing skills (default true; reviewed before saving). */
  improve?: boolean;
  /** Model for the run ("" = the agent CLI's default). */
  model?: string;
  /** Effort / reasoning level for the run ("" = the agent CLI's default). */
  effort?: string;
  /** The run prompt as shown (and possibly edited) in the dialog ("" = compose server-side). */
  prompt?: string;
}

export const mineSources = (days: number) => http<MineSource[]>("GET", `mine/sources?days=${days}`);
/** The prompt a run with these settings would send — shown in the dialog for review/editing. */
export const minePrompt = (args: { days: number; improve?: boolean }) =>
  http<{ prompt: string }>("POST", "mine/prompt", { ...args });
export const mineStart = (a: MineStartArgs) => http<MineState>("POST", "mine/start", { ...a });
/** Restore every installed copy of the skill-miner to the official bundled
 *  version (versioned copies keep their .git — the refresh shows as
 *  uncommitted changes). Returns the restored roots. */
export const mineReinstallMiner = () => http<{ roots: string[] }>("POST", "mine/reinstall-miner");
/** Whether an installed skill-miner copy differs from the bundled official version.
 *  `drifted` is neutral — the copy is editable, so it may be a local customization
 *  OR a shipped update not yet pulled. The dialog only offers reinstall when drifted. */
export interface MinerStatus {
  installed: boolean;
  drifted: boolean;
}
export const minerStatus = () => http<MinerStatus>("GET", "mine/miner-status");
export const mineState = () => http<MineState>("GET", "mine/state");
/** The active run dir's files — the mining page's artifacts listing. */
export const mineFiles = () => http<MineFiles>("GET", "mine/files");
/** Past runs (archived under history/<id>/), newest first — display-only. */
export const mineHistory = () => http<MineHistoryEntry[]>("GET", "mine/history");
export const mineStop = () => http<{ ok: boolean }>("POST", "mine/stop").then(() => {});
/** The run's conversation: returns its live terminal, or revives the recorded
 *  agent session in a fresh one (works after the original pane was closed). */
export const mineContinue = () => http<{ terminalId: string }>("POST", "mine/continue");

export const terminalAgents = () => http<AgentOption[]>("GET", "terminal/agents");
export const terminalList = () => http<TermSession[]>("GET", "terminal/list");
export const terminalCreate = (a: CreateTermArgs) => http<TermSession>("POST", "terminal/create", { ...a });
export const terminalKill = (id: string) =>
  http<{ ok: boolean }>("POST", "terminal/kill", { id }).then(() => {});

// --- agent turn-finish notifications ---
// The SPA decides WHEN a bell deserves a notification (lib/sessions.ts); these
// routes are pinned LOCAL (never proxied) because a toast/badge belongs to the
// machine whose screen you're looking at. A server with no native surface
// (standalone binary, browser mode) 404s them — the notifier falls back to the
// Web Notification API where the platform has one.

/** Whether this origin's server can show native OS notifications (the desktop
 *  shell). 404 = no. */
export const notifyStatus = () => http<{ native: boolean }>("GET", "notify/status");
export const notifyNative = (title: string, body: string) =>
  http<{ ok: boolean }>("POST", "notify", { title, body }).then(() => {});
/** Ask the OS for notification permission (macOS prompts; elsewhere a no-op). */
export const notifyPrime = () => http<{ ok: boolean }>("POST", "notify/prime").then(() => {});
/** Dock/taskbar unread-count badge; 0 clears. */
export const notifyBadge = (count: number) =>
  http<{ ok: boolean }>("POST", "notify/badge", { count }).then(() => {});

// --- open a folder in the local VS Code ---
// Pinned LOCAL like the notify routes: opening an editor belongs to the machine
// whose screen you're at, so a tailscale-fronted phone client 404s `editorStatus`
// (button hidden) and the desktop opens VS Code on its own screen.

/** Whether a local VS Code is installed & reachable from this client. A 404
 *  (phone/remote origin, or no server support) surfaces as a thrown error. */
export const editorStatus = () => http<{ available: boolean; name?: string }>("GET", "editor/status");
/** Launch VS Code on `path` (a session's working directory). */
export const editorOpen = (path: string) =>
  http<{ ok: boolean }>("POST", "editor/open", { path }).then(() => {});

// Web Push — notifications with the app closed. NOT pinned-local: with a remote
// hub connected these proxy to it, so subscriptions live next to the bell
// watcher that fires them (see lib/push.ts for the client flow).

/** The server's VAPID public key (base64url) — `PushManager.subscribe` input. */
export const pushKey = () => http<{ key: string }>("GET", "push/key");
export const pushSubscribe = (endpoint: string, keys: { p256dh: string; auth: string }) =>
  http<{ ok: boolean }>("POST", "push/subscribe", { endpoint, keys }).then(() => {});
export const pushUnsubscribe = (endpoint: string) =>
  http<{ ok: boolean }>("POST", "push/unsubscribe", { endpoint }).then(() => {});
/** Focus beacon: a recently-focused client suppresses pushes server-side. */
export const pushAttention = (client: string, focused: boolean) =>
  http<{ ok: boolean }>("POST", "push/attention", { client, focused }).then(() => {});

/** One terminal lifecycle event from `GET /api/events`. `at` is the session's
 *  `bellAt` (unix secs, string) at emit time. */
export interface TermEvent {
  id: string;
  label: string;
  agent: string;
  cwd: string;
  at: string;
  /** Bell frames only: a captured preview of the agent's last output line, for
   *  the notification body. Absent on opened/closed and on the poll backstop. */
  last?: string;
}

/**
 * Subscribe to the server's terminal lifecycle events (SSE): `bell` (an agent
 * finished a turn), `opened`, `closed`. Events are edge HINTS — callers should
 * re-fetch `terminalList` on each one rather than trusting the payload as state.
 * EventSource retries transient drops itself; those silent reconnects fire
 * `onOpen` (as does the first connect) so the caller can refetch what the gap
 * may have swallowed. A fatal close (route missing on an older server, topology
 * change mid-stream) fires `onDown` once and the caller decides whether/when to
 * resubscribe.
 */
export function terminalEvents(
  onEvent: (kind: "bell" | "opened" | "closed", e: TermEvent) => void,
  onDown: () => void,
  onOpen?: () => void,
): { close(): void } {
  const es = new EventSource(`${API_BASE}/api/events`);
  es.onopen = () => onOpen?.();
  let done = false;
  const forward = (kind: "bell" | "opened" | "closed") => (m: MessageEvent) => {
    try {
      onEvent(kind, JSON.parse(m.data as string) as TermEvent);
    } catch {
      /* malformed frame — skip */
    }
  };
  es.addEventListener("bell", forward("bell"));
  es.addEventListener("opened", forward("opened"));
  es.addEventListener("closed", forward("closed"));
  es.onerror = () => {
    log.debug("sse", `events readyState=${es.readyState}`);
    if (es.readyState === EventSource.CLOSED && !done) {
      done = true;
      es.close();
      onDown();
    }
  };
  return {
    close: () => {
      done = true;
      es.close();
    },
  };
}

// base64 ↔ bytes for the text-only transports (SSE data: frames / JSON input).
function bytesToB64(bytes: Uint8Array): string {
  let bin = "";
  const chunk = 0x8000;
  for (let i = 0; i < bytes.length; i += chunk) {
    bin += String.fromCharCode(...bytes.subarray(i, i + chunk));
  }
  return btoa(bin);
}
function b64ToBytes(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}
const strToB64 = (s: string) => bytesToB64(new TextEncoder().encode(s));

/** A bidirectional attachment to a live terminal; detaching keeps the session alive. */
export interface TerminalHandle {
  write(data: string): void;
  resize(cols: number, rows: number): void;
  detach(): void;
}

/**
 * Attach to a session and stream its output: SSE for output (auto-reconnecting)
 * + POST for input. Detaching keeps the (tmux-backed) session alive.
 */
export function attachTerminal(
  id: string,
  opts: {
    cols: number;
    rows: number;
    onData: (bytes: Uint8Array) => void;
    /** Fired when the stream ends fatally (e.g. the session is gone). */
    onClose?: () => void;
  },
): TerminalHandle {
  const q = new URLSearchParams({ id, cols: String(opts.cols), rows: String(opts.rows) });
  const es = new EventSource(`${API_BASE}/api/terminal/attach?${q.toString()}`);
  let closed = false;
  // Keystrokes ride individual POSTs, which carry no ordering guarantee once two
  // are in flight at the same time (each is its own request — and over a remote,
  // its own proxy thread), so fast typing could land out of order in the pty.
  // Send strictly one batch at a time, coalescing whatever arrives meanwhile —
  // ordered input, and far fewer round trips on a high-latency link.
  let pendingInput = "";
  let sendingInput = false;
  const pumpInput = async () => {
    if (sendingInput) return;
    sendingInput = true;
    while (pendingInput) {
      const batch = pendingInput;
      pendingInput = "";
      try {
        await http("POST", "terminal/input", { id, data: strToB64(batch) });
      } catch {
        // Drop the batch on a transport blip: losing keystrokes beats replaying
        // them late, out of order with whatever the user typed next.
      }
    }
    sendingInput = false;
  };
  // Resizes need the same one-in-flight discipline as input — the pane fires one
  // per animation frame while a window is dragged, far faster than a remote
  // round-trip, so parallel POSTs could otherwise reorder and leave the pty at a
  // stale size (a wrapped TUI that never self-corrects). Last-wins: only the
  // newest pending (cols,rows) survives, sent after the in-flight resize resolves.
  // Resizes only land while the stream is attached (the server 400s otherwise —
  // the pane's ResizeObserver fires before the SSE attach registers, and during
  // reconnect gaps), so they're gated on `streamOpen`; the latest size is kept
  // and re-asserted from es.onopen after every (re)connect — which also corrects
  // the PTY when EventSource auto-reconnects with the stale mount-time URL size.
  let streamOpen = false;
  let latestSize: { cols: number; rows: number } | null = null;
  let pendingResize: { cols: number; rows: number } | null = null;
  let sendingResize = false;
  const pumpResize = async () => {
    if (sendingResize) return;
    sendingResize = true;
    while (pendingResize && streamOpen && !closed) {
      const { cols, rows } = pendingResize;
      pendingResize = null;
      try {
        await http("POST", "terminal/resize", { id, cols, rows });
      } catch {
        /* transient blip — the next resize event will re-assert the size */
      }
    }
    sendingResize = false;
  };
  es.onopen = () => {
    streamOpen = true;
    if (latestSize) {
      pendingResize = latestSize;
      void pumpResize();
    }
  };
  es.onmessage = (e) => {
    if (e.data) opts.onData(b64ToBytes(e.data));
  };
  es.onerror = () => {
    // CLOSED ⇒ the browser gave up (e.g. a 4xx because the session is gone) and
    // won't reconnect; surface it once. CONNECTING ⇒ a transient blip, let it retry.
    log.debug("sse", `terminal/attach id=${id} readyState=${es.readyState}`);
    streamOpen = false;
    if (es.readyState === EventSource.CLOSED && !closed) {
      closed = true;
      opts.onClose?.();
    }
  };
  return {
    write: (data) => {
      // Once the stream is fatally closed we no longer render output; keep
      // feeding input and it would execute invisibly in a still-live session.
      if (closed) return;
      pendingInput += data;
      void pumpInput();
    },
    resize: (cols, rows) => {
      latestSize = { cols, rows };
      if (closed || !streamOpen) return; // re-asserted from es.onopen
      pendingResize = latestSize;
      void pumpResize();
    },
    detach: () => {
      closed = true;
      es.close();
    },
  };
}

/** Ship a pasted clipboard image to the backend — the machine the agent actually
 *  runs on (possibly remote) — and get back an absolute temp-file path there,
 *  ready to paste into the prompt the way drag-and-drop pastes a path. */
export const terminalPasteImage = (bytes: Uint8Array, mime: string) =>
  http<{ path: string }>("POST", "terminal/paste-image", { data: bytesToB64(bytes), mime });
