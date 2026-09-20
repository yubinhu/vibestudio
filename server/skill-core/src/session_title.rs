//! A short human title for a live terminal, read from the agent's OWN session
//! record — far more meaningful than the raw cwd. Hung off `AgentDef.session_title`
//! (the common agent interface) so each agent contributes what it can and the rest
//! degrade to `None`. Claude, Gemini, Cursor parse their JSONL transcripts;
//! Codex and opencode also read their WAL SQLite stores (bundled SQLite). Only openclaw is still
//! uncovered — it isn't launchable and its on-disk format is unverified.
//!
//! Correlation: when the terminal recorded the session id we forced at launch
//! (`claude --session-id`), it maps to exactly that transcript — never another
//! session when that ID is missing. Codex's id is resolved from its
//! live process's open rollout; no id means no title rather than a cwd guess.
//! Otherwise (shells, resumes,
//! other agents, pre-existing terminals) it falls back to the newest-activity
//! session for the cwd.
//!
//! The same stores feed the turn-finish notification body: [`claude_last_message`]
//! reads the agent's last assistant message from its transcript, so the phone shows
//! what the agent SAID rather than a screen-scrape of the TUI's bottom line. Wired
//! through `AgentDef.last_message` (Claude for now; other families degrade to the
//! fixed summons until their reader lands).

use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

mod codex;
pub use codex::codex_title;

const MAX_LEN: usize = 72;

/// Longer budget for a turn-finish notification body (a phone banner shows a few
/// lines) than a one-line rail title — see [`claude_last_message`].
const PREVIEW_LEN: usize = 180;

fn home() -> Option<PathBuf> {
    dirs::home_dir()
}

/// Parsed titles keyed by file, precise modification time, and length. `terminal/list` is polled every few
/// seconds but a title only moves when the agent writes a new turn — so a file is
/// re-read only when its mtime advances, keeping polling cheap even on multi-MB
/// transcripts.
type FileStamp = (Option<SystemTime>, u64);
type TitleCache = HashMap<PathBuf, (Vec<Option<FileStamp>>, Option<String>)>;
static CACHE: LazyLock<Mutex<TitleCache>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn cached(file: &Path, parse: impl FnOnce(&Path) -> Option<String>) -> Option<String> {
    cached_with_dependencies(file, &[], parse)
}

fn file_stamp(file: &Path) -> Option<FileStamp> {
    let metadata = fs::metadata(file).ok()?;
    Some((metadata.modified().ok(), metadata.len()))
}

fn cached_with_dependencies(file: &Path, dependencies: &[&Path], parse: impl FnOnce(&Path) -> Option<String>) -> Option<String> {
    // Preserve sub-second writes and size changes: a rename can follow a title
    // read in the same second, and Claude also keeps a separate title sidecar.
    let mt: Vec<_> = std::iter::once(file).chain(dependencies.iter().copied()).map(file_stamp).collect();
    if let Ok(c) = CACHE.lock() {
        if let Some((cmt, title)) = c.get(file) {
            if *cmt == mt {
                return title.clone();
            }
        }
    }
    let title = parse(file);
    if let Ok(mut c) = CACHE.lock() {
        c.insert(file.to_path_buf(), (mt, title.clone()));
    }
    title
}

/// mtime (false) or btime/creation (true) as unix seconds.
fn file_time(p: &Path, creation: bool) -> Option<u64> {
    let m = fs::metadata(p).ok()?;
    let t = if creation { m.created().ok()? } else { m.modified().ok()? };
    t.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs())
}

/// The session file for a live terminal: the one being actively written (newest
/// activity) — a long-lived terminal outlives several agent sessions, so its
/// CURRENT session is the most recent, not the one born when the terminal started.
///
/// LIMITATION: several terminals in the SAME cwd can't be told apart here — they
/// resolve to the newest shared session. That's why `claude` terminals now carry
/// a forced `--session-id` (see `claude_from_dir`) for an exact match; this path
/// is only the fallback for sessions without one. `created` is kept for future use.
fn pick_for_terminal(files: Vec<PathBuf>, _created: i64) -> Option<PathBuf> {
    files
        .into_iter()
        .max_by_key(|p| file_time(p, false).unwrap_or(0))
}

fn tidy(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate(s: &str) -> String {
    truncate_to(s, MAX_LEN)
}

/// Truncate to `max` chars (chars, not bytes — the text is UTF-8), backing off to
/// the last space so a word is never sliced, and marking the cut with an ellipsis.
fn truncate_to(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max).collect();
    let trimmed = cut.rsplit_once(' ').map(|(a, _)| a).unwrap_or(&cut);
    format!("{}…", trimmed.trim_end())
}

/// Known system/slash-command wrappers a first user message may carry. A message
/// that is ONLY these (or starts with a caveat) is not a real prompt → skip it.
const SKIP_PREFIXES: &[&str] = &[
    "<local-command-caveat>",
    "<command-name>",
    "<command-message>",
    "<command-args>",
    "<local-command-stdout>",
    "<task-notification>",
    "<environment_context>",
    "Caveat:",
    "[Request interrupted",
    "# AGENTS.md",
    "# Context from my IDE setup:",
];

/// Remove `<tag …>…</tag>` blocks (system-reminder, ide_*, etc.) from a prompt so
/// the human text is left. Non-regex: repeatedly splice out the first `<…>…</…>`
/// whose open tag name is in `tags`.
fn strip_tag_blocks(text: &str, tags: &[&str]) -> String {
    let mut out = text.to_string();
    for tag in tags {
        let open = format!("<{tag}");
        let close = format!("</{tag}>");
        while let Some(a) = out.find(&open) {
            let Some(rel) = out[a..].find(&close) else { break };
            let b = a + rel + close.len();
            out.replace_range(a..b, " ");
        }
    }
    out
}

/// Clean a raw first-user-message into a title, or None if it's a system artifact.
fn clean_prompt(raw: &str) -> Option<String> {
    let stripped = strip_tag_blocks(raw, &["system-reminder", "ide_selection", "ide_opened"]);
    let t = tidy(&stripped);
    if t.is_empty() {
        return None;
    }
    if SKIP_PREFIXES.iter().any(|p| t.starts_with(p)) {
        return None;
    }
    Some(t)
}

/// Extract the human text of a Claude/OpenClaw-style `user` record's message.
/// Returns None when the content is entirely tool results (not a real turn).
fn claude_user_text(rec: &Value) -> Option<String> {
    let content = rec.get("message")?.get("content")?;
    if let Some(s) = content.as_str() {
        return Some(s.to_string());
    }
    let arr = content.as_array()?;
    let mut parts = Vec::new();
    let mut saw_non_tool = false;
    for b in arr {
        match b.get("type").and_then(|t| t.as_str()) {
            Some("text") => {
                saw_non_tool = true;
                if let Some(t) = b.get("text").and_then(|t| t.as_str()) {
                    parts.push(t);
                }
            }
            Some("tool_result") => {}
            _ => saw_non_tool = true,
        }
    }
    if !saw_non_tool {
        return None; // all tool_result → skip
    }
    Some(parts.join(" "))
}

fn jsonl_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = fs::read_dir(dir) else {
        return Vec::new();
    };
    rd.filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "jsonl"))
        .collect()
}

fn read_lines(file: &Path) -> Option<impl Iterator<Item = String>> {
    let f = fs::File::open(file).ok()?;
    Some(BufReader::new(f).lines().map_while(Result::ok))
}

// ─── Claude Code (and any Claude fork) ───

/// Claude's project slug (CLI 2.1.267 / current Agent SDK): replace each
/// non-ASCII-alphanumeric UTF-16 unit; long paths carry a base36 hash suffix.
fn claude_encode(cwd: &Path) -> String {
    let path = cwd.to_string_lossy();
    let mut slug: String = path.encode_utf16().map(|unit| {
        if unit <= 127 && (unit as u8).is_ascii_alphanumeric() { unit as u8 as char } else { '-' }
    }).collect();
    if slug.len() > 200 {
        let hash = path.encode_utf16().fold(0i32, |hash, unit| hash.wrapping_mul(31).wrapping_add(i32::from(unit)));
        let mut number = hash.unsigned_abs();
        let mut suffix = Vec::new();
        loop {
            suffix.push(b"0123456789abcdefghijklmnopqrstuvwxyz"[(number % 36) as usize] as char);
            number /= 36;
            if number == 0 { break; }
        }
        slug.truncate(200);
        slug.push('-');
        slug.extend(suffix.into_iter().rev());
    }
    slug
}

fn claude_projects() -> Option<PathBuf> {
    Some(std::env::var_os("CLAUDE_CONFIG_DIR").filter(|path| !path.is_empty())
        .map(PathBuf::from).or_else(|| home().map(|dir| dir.join(".claude")))?.join("projects"))
}

pub fn claude_title(cwd: &Path, created: i64, session_id: Option<&str>) -> Option<String> {
    claude_from_dir(claude_projects()?, cwd, created, session_id)
}

/// The transcript file for a live Claude terminal: the exact `<sid>.jsonl` when the
/// terminal recorded the id we forced at launch (`claude --session-id`) — the only
/// way to tell apart several sessions in one cwd — else the newest-activity
/// transcript in the cwd's project dir.
fn claude_file(projects: &Path, cwd: &Path, created: i64, session_id: Option<&str>) -> Option<PathBuf> {
    let dir = projects.join(claude_encode(cwd));
    if let Some(sid) = session_id.filter(|s| !s.is_empty()) {
        if !safe_session_id(sid) { return None; }
        let f = dir.join(format!("{sid}.jsonl"));
        if f.is_file() {
            return Some(f);
        }
        // /cd and cross-project resume can relocate the exact transcript.
        // Search only this ID and reject ambiguous copies, never borrow another
        // terminal's most recently written session when a known ID is missing.
        let mut matches = fs::read_dir(projects).ok()?.filter_map(Result::ok)
            .filter(|entry| entry.path().is_dir())
            .map(|entry| entry.path().join(format!("{sid}.jsonl")))
            .filter(|path| path.is_file());
        let found = matches.next()?;
        return matches.next().is_none().then_some(found);
    }
    pick_for_terminal(jsonl_files(&dir), created)
}

fn safe_session_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && id.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
}

fn claude_from_dir(projects: PathBuf, cwd: &Path, created: i64, session_id: Option<&str>) -> Option<String> {
    let file = claude_file(&projects, cwd, created, session_id)?;
    let sidecar = claude_title_sidecar(&file)?;
    cached_with_dependencies(&file, &[&sidecar], parse_claude)
}

fn claude_title_sidecar(file: &Path) -> Option<PathBuf> {
    Some(file.parent()?.join(file.file_stem()?).join("custom-title.json"))
}

/// The agent's last assistant message, as a one-line preview for a turn-finish
/// notification body — read from the SAME transcript as the title, so it reflects
/// what the agent actually SAID (its own structured message text) rather than
/// whatever furniture happens to sit at the bottom of the TUI. `None` for a session
/// with no assistant text yet (the notifier then falls back to its fixed phrase).
pub fn claude_last_message(cwd: &Path, created: i64, session_id: Option<&str>) -> Option<String> {
    let file = claude_file(&claude_projects()?, cwd, created, session_id)?;
    parse_claude_last(&file)
}

fn parse_claude_last(file: &Path) -> Option<String> {
    // Keep the newest assistant record that carried visible text: a turn ending in a
    // tool_use (or pure thinking) has no words to preview, so walk back past it.
    let mut last: Option<String> = None;
    for line in read_lines(file)? {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if v.get("type").and_then(|t| t.as_str()) != Some("assistant") {
            continue;
        }
        if let Some(text) = claude_assistant_text(&v) {
            let t = tidy(&text);
            if !t.is_empty() {
                last = Some(t);
            }
        }
    }
    Some(truncate_to(&last?, PREVIEW_LEN))
}

/// The visible text of a Claude `assistant` record — its `text` blocks joined.
/// `None` when the turn held only tool_use / thinking (nothing said).
fn claude_assistant_text(rec: &Value) -> Option<String> {
    let content = rec.get("message")?.get("content")?;
    if let Some(s) = content.as_str() {
        return Some(s.to_string());
    }
    let mut parts = Vec::new();
    for b in content.as_array()? {
        if b.get("type").and_then(|t| t.as_str()) == Some("text") {
            if let Some(t) = b.get("text").and_then(|t| t.as_str()) {
                parts.push(t);
            }
        }
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

fn parse_claude(file: &Path) -> Option<String> {
    let mut custom_title: Option<String> = None;
    let mut last_title: Option<String> = None;
    let mut summary: Option<String> = None;
    let mut first_user: Option<String> = None;
    for line in read_lines(file)? {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match v.get("type").and_then(|t| t.as_str()) {
            Some("custom-title") => {
                // An explicit empty name resets a prior custom title.
                if let Some(title) = v.get("customTitle").and_then(Value::as_str) {
                    custom_title = Some(title.into());
                }
            }
            Some("summary") => summary = v.get("summary").and_then(Value::as_str)
                .filter(|title| !title.trim().is_empty()).map(str::to_string),
            // Claude regenerates the title as the convo evolves → keep the last one.
            Some("ai-title") => {
                if let Some(t) = v.get("aiTitle").and_then(|x| x.as_str()) {
                    if !t.trim().is_empty() {
                        last_title = Some(t.trim().to_string());
                    }
                }
            }
            Some("user") if first_user.is_none() => {
                if v.get("isMeta").and_then(|x| x.as_bool()) == Some(true) {
                    continue;
                }
                if let Some(txt) = claude_user_text(&v) {
                    first_user = clean_prompt(&txt);
                }
            }
            _ => {}
        }
    }
    let sidecar = || {
        let value: Value = serde_json::from_slice(&fs::read(claude_title_sidecar(file)?).ok()?).ok()?;
        value.get("customTitle").and_then(Value::as_str).map(str::to_string)
    };
    // Match Claude's resume picker: a recent transcript name wins, followed by
    // the recovery sidecar, then an older transcript name. The sidecar survives
    // transcript append failures and can therefore be newer than the head.
    // User names outrank regenerated AI titles, regardless of append order.
    // Preserve authored titles for editing; only prompt fallbacks are shortened.
    claude_tail_custom_title(file).or_else(sidecar).or(custom_title).filter(|title| !title.trim().is_empty())
        .or(last_title).map(|title| tidy(&title))
        .or_else(|| summary.map(|title| truncate(&tidy(&title))))
        .or_else(|| first_user.map(|prompt| truncate(&prompt)))
}

fn claude_tail_custom_title(file: &Path) -> Option<String> {
    // Claude's readSessionLite reads the final 64 KiB for current metadata.
    let mut input = fs::File::open(file).ok()?;
    let start = input.metadata().ok()?.len().saturating_sub(65_536);
    input.seek(SeekFrom::Start(start)).ok()?;
    // The byte offset can split a UTF-8 character; lossy conversion only affects
    // the partial first line, which cannot be a complete JSON record anyway.
    let mut bytes = Vec::new();
    input.take(65_536).read_to_end(&mut bytes).ok()?;
    let tail = String::from_utf8_lossy(&bytes);
    tail.lines().rev().find_map(|line| {
        let value: Value = serde_json::from_str(line).ok()?;
        if value["type"] != "custom-title" { return None; }
        value["customTitle"].as_str().map(str::to_string)
    })
}

fn parse_codex(file: &Path) -> Option<String> {
    for line in read_lines(file)? {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let payload = &v["payload"];
        let text = if v["type"] == "event_msg" && payload["type"] == "user_message" {
            payload["message"].as_str().map(str::to_string)
        } else if v["type"] == "response_item" && payload["role"] == "user" {
            // Paginated Codex rollouts omit user_message events, retaining the
            // input_text blocks instead. Ignore image and tool-result blocks.
            payload["content"].as_array().map(|blocks| {
                blocks.iter().filter(|block| block["type"] == "input_text")
                    .filter_map(|block| block["text"].as_str()).collect::<Vec<_>>().join(" ")
            })
        } else { None };
        if let Some(text) = text {
            let text = strip_tag_blocks(&text, &["image"]);
            // IDE-launched Codex wraps the real ask under this marker.
            let ask = text.rsplit_once("## My request for Codex:").map(|(_, ask)| ask).unwrap_or(&text);
            if let Some(prompt) = clean_prompt(ask) {
                return Some(truncate(&prompt));
            }
        }
    }
    None
}

// ─── Gemini CLI ───
// Sessions live under ~/.gemini/tmp/<slug>/chats/; the slug maps to a cwd via a
// sibling `.project_root` file. Title = the LLM `summary` if present, else the
// first user message (a live session usually has no summary yet).

pub fn gemini_title(cwd: &Path, created: i64, session_id: Option<&str>) -> Option<String> {
    let root = std::env::var_os("GEMINI_CLI_HOME").filter(|path| !path.is_empty())
        .map(PathBuf::from).or_else(home)?;
    gemini_from_tmp(&root.join(".gemini/tmp"), cwd, created, session_id)
}

fn gemini_from_tmp(tmp: &Path, cwd: &Path, created: i64, session_id: Option<&str>) -> Option<String> {
    let want = cwd.to_string_lossy();
    let proj = fs::read_dir(tmp).ok()?.filter_map(|e| e.ok()).find(|e| {
        fs::read_to_string(e.path().join(".project_root"))
            .map(|c| c.trim() == want)
            .unwrap_or(false)
    })?;
    let chats = proj.path().join("chats");
    // Gemini 0.60 writes JSONL but can still resume older whole-record JSON.
    let files = fs::read_dir(chats).ok()?.filter_map(Result::ok).map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json" || ext == "jsonl"))
        .filter(|path| {
            let Some(identity) = gemini_identity(path) else { return false };
            !identity.subagent && session_id.filter(|id| !id.is_empty())
                .is_none_or(|id| identity.session_id.as_deref() == Some(id))
        }).collect();
    let file = pick_for_terminal(files, created)?;
    cached(&file, parse_gemini)
}

#[derive(Clone)]
struct GeminiIdentity {
    session_id: Option<String>,
    subagent: bool,
}

type GeminiIdentityCache = HashMap<PathBuf, (Option<FileStamp>, Option<GeminiIdentity>)>;
static GEMINI_IDENTITIES: LazyLock<Mutex<GeminiIdentityCache>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn gemini_identity(file: &Path) -> Option<GeminiIdentity> {
    // Legacy JSON can contain a multi-MB history. Keep only its small identity
    // in memory and avoid parsing unchanged neighboring sessions on every poll.
    let stamp = file_stamp(file);
    if let Ok(cache) = GEMINI_IDENTITIES.lock() {
        if let Some((previous, identity)) = cache.get(file) {
            if *previous == stamp { return identity.clone(); }
        }
    }
    let identity = gemini_metadata(file).map(|record| GeminiIdentity {
        session_id: record["sessionId"].as_str().map(str::to_string),
        subagent: record["kind"] == "subagent",
    });
    if let Ok(mut cache) = GEMINI_IDENTITIES.lock() {
        cache.insert(file.into(), (stamp, identity.clone()));
    }
    identity
}

fn gemini_metadata(file: &Path) -> Option<Value> {
    if file.extension().is_some_and(|ext| ext == "json") {
        return serde_json::from_slice(&fs::read(file).ok()?).ok();
    }
    read_lines(file)?.take(8).find_map(|line| {
        let value: Value = serde_json::from_str(&line).ok()?;
        value.get("sessionId")?;
        Some(value)
    })
}

fn parse_gemini(file: &Path) -> Option<String> {
    if file.extension().is_some_and(|ext| ext == "json") {
        let record = gemini_metadata(file)?;
        if let Some(summary) = record["summary"].as_str().filter(|text| !text.trim().is_empty()) {
            return Some(tidy(summary));
        }
        return record["messages"].as_array()?.iter().filter(|message| message["type"] == "user")
            .find_map(|message| gemini_user_text(message).and_then(|text| clean_prompt(&text)))
            .map(|prompt| truncate(&prompt));
    }
    let mut summary: Option<String> = None;
    let mut first_user: Option<String> = None;
    for line in read_lines(file)? {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some(s) = v.get("summary").and_then(|s| s.as_str()) {
            if !s.trim().is_empty() {
                summary = Some(s.trim().to_string());
            }
        }
        if let Some(set) = v.get("$set").and_then(|s| s.get("summary")).and_then(|s| s.as_str()) {
            if !set.trim().is_empty() {
                summary = Some(set.trim().to_string());
            }
        }
        if first_user.is_none() && v.get("type").and_then(|t| t.as_str()) == Some("user") {
            first_user = gemini_user_text(&v).and_then(|t| clean_prompt(&t));
        }
    }
    summary.map(|title| tidy(&title)).or_else(|| first_user.map(|prompt| truncate(&prompt)))
}

fn gemini_user_text(rec: &Value) -> Option<String> {
    let content = rec.get("content")?;
    if let Some(s) = content.as_str() {
        return Some(s.to_string());
    }
    let parts: Vec<&str> = content
        .as_array()?
        .iter()
        .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
        .collect();
    Some(parts.join(" "))
}

// ─── Cursor (cursor-agent) ───
// ~/.cursor/projects/<enc>/agent-transcripts/<uuid>/<uuid>.jsonl. No stored title;
// the first `<user_query>` is the prompt.

/// cwd → project dir name: runs of non-alphanumerics collapse to a single '-',
/// leading separators dropped.
fn cursor_encode(cwd: &Path) -> String {
    let s = cwd.to_string_lossy();
    let mut out = String::new();
    let mut dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            dash = false;
        } else if !dash {
            out.push('-');
            dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

pub fn cursor_title(cwd: &Path, created: i64, session_id: Option<&str>) -> Option<String> {
    let base = home()?
        .join(".cursor/projects")
        .join(cursor_encode(cwd))
        .join("agent-transcripts");
    cursor_from_base(&base, created, session_id)
}

fn cursor_from_base(base: &Path, created: i64, session_id: Option<&str>) -> Option<String> {
    if let Some(id) = session_id.filter(|id| !id.is_empty()) {
        if !safe_session_id(id) { return None; }
        let nested = base.join(id).join(format!("{id}.jsonl"));
        return cached(&nested, parse_cursor);
    }
    // each session is its own subdir holding <uuid>.jsonl
    let files: Vec<PathBuf> = fs::read_dir(base)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter_map(|e| jsonl_files(&e.path()).into_iter().next())
        .collect();
    let file = pick_for_terminal(files, created)?;
    cached(&file, parse_cursor)
}

fn parse_cursor(file: &Path) -> Option<String> {
    for line in read_lines(file)? {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if v.get("role").and_then(|r| r.as_str()) != Some("user") {
            continue;
        }
        let text: String = v
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        // The real prompt is inside <user_query>…</user_query>.
        let inner = between(&text, "<user_query>", "</user_query>").unwrap_or(&text);
        let cleaned = tidy(&strip_tag_blocks(inner, &["timestamp", "image_files"]));
        // Skip image-only / bare @mention lead turns.
        if cleaned.is_empty() || cleaned.starts_with('@') || cleaned == "[Image]" {
            continue;
        }
        return Some(truncate(&cleaned));
    }
    None
}

fn between<'a>(s: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let a = s.find(open)? + open.len();
    let b = s[a..].find(close)? + a;
    Some(s[a..b].trim())
}

// ─── opencode ───
// opencode records every session as a row in a WAL SQLite db, with the launch cwd
// in `directory` and a model-summarized `title`. Until it's summarized the title is
// a "New session - …" / "Child session - …" placeholder — fall back to the first
// user message then. Opened READ-ONLY (never disturb opencode's writer); any error
// (locked, schema drift, moved) degrades to None so the caller shows the cwd.

/// One database serves many sessions. Include the database path, cwd and exact
/// session ID in the cache key, with both DB and WAL changes invalidating it.
type OcCacheKey = (PathBuf, String, Option<String>);
type OcCache = HashMap<OcCacheKey, ((Option<FileStamp>, Option<FileStamp>), Option<String>)>;
static OC_CACHE: LazyLock<Mutex<OcCache>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// The WAL changes on new turns; the DB changes on checkpoint. Track both.
fn oc_sig(db: &Path) -> (Option<FileStamp>, Option<FileStamp>) {
    let wal = PathBuf::from(format!("{}-wal", db.display()));
    (file_stamp(db), file_stamp(&wal))
}

fn oc_placeholder(t: &str) -> bool {
    t.starts_with("New session - ") || t.starts_with("Child session - ")
}

pub fn opencode_title(cwd: &Path, _created: i64, session_id: Option<&str>) -> Option<String> {
    let data_home = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from);
    let configured = std::env::var_os("OPENCODE_DB").filter(|path| !path.is_empty()).map(PathBuf::from);
    let db = opencode_db_path(&home()?, data_home.as_deref(), configured.as_deref())?;
    opencode_from_db(&db, cwd, session_id)
}

fn opencode_db_path(home: &Path, data_home: Option<&Path>, configured: Option<&Path>) -> Option<PathBuf> {
    let data = data_home.filter(|path| path.is_absolute()).map(Path::to_path_buf)
        .unwrap_or_else(|| home.join(".local/share")).join("opencode");
    match configured {
        Some(path) if path == Path::new(":memory:") => None,
        Some(path) if path.is_absolute() => Some(path.into()),
        Some(path) => Some(data.join(path)),
        None => Some(data.join("opencode.db")),
    }
}

fn opencode_from_db(db: &Path, cwd: &Path, session_id: Option<&str>) -> Option<String> {
    if !db.is_file() {
        return None;
    }
    let sig = oc_sig(db);
    let key = (db.into(), cwd.to_string_lossy().to_string(), session_id.filter(|id| !id.is_empty()).map(str::to_string));
    if let Ok(c) = OC_CACHE.lock() {
        if let Some((s, title)) = c.get(&key) {
            if *s == sig {
                return title.clone();
            }
        }
    }
    let title = oc_query(db, &key.1, key.2.as_deref());
    if let Ok(mut c) = OC_CACHE.lock() {
        c.insert(key, (sig, title.clone()));
    }
    title
}

fn oc_query(db: &Path, cwd: &str, session_id: Option<&str>) -> Option<String> {
    // READ_ONLY still reads committed WAL frames; `immutable` would ignore the WAL
    // and read a stale checkpoint, so don't set it.
    let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    let _ = conn.busy_timeout(Duration::from_millis(200));
    // Newest top-level session for this cwd; parent_id IS NULL drops subagent sessions.
    let (id, title): (String, String) = if let Some(id) = session_id {
        conn.query_row("SELECT id, title FROM session WHERE id = ?1 AND parent_id IS NULL", [id], |r| Ok((r.get(0)?, r.get(1)?))).ok()?
    } else {
        let query = if conn.prepare("SELECT time_archived FROM session LIMIT 0").is_ok() {
            "SELECT id, title FROM session WHERE directory = ?1 AND parent_id IS NULL AND time_archived IS NULL ORDER BY time_updated DESC LIMIT 1"
        } else {
            "SELECT id, title FROM session WHERE directory = ?1 AND parent_id IS NULL ORDER BY time_updated DESC LIMIT 1"
        };
        conn
        .query_row(
            query,
            [cwd],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok()? };
    let title = title.trim();
    if !title.is_empty() && !oc_placeholder(title) {
        return Some(tidy(title));
    }
    oc_first_user_text(&conn, &id)
}

/// The earliest user message's text parts, joined — the fallback when the title is
/// still a placeholder. (json_extract is built into bundled SQLite.)
fn oc_first_user_text(conn: &Connection, session_id: &str) -> Option<String> {
    // Released OpenCode 1.18 uses an event-sourced message projection alongside
    // the older message/part tables. New user messages store their text here.
    if let Some(prompt) = conn.query_row(
        "SELECT data FROM session_message WHERE session_id = ?1 AND type = 'user' ORDER BY seq LIMIT 1",
        [session_id], |row| row.get::<_, String>(0),
    ).ok().and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|record| record["text"].as_str().and_then(clean_prompt)) {
        return Some(truncate(&prompt));
    }
    let msg_id: String = conn
        .query_row(
            "SELECT id FROM message \
             WHERE session_id = ?1 AND json_extract(data, '$.role') = 'user' \
             ORDER BY time_created LIMIT 1",
            [session_id],
            |r| r.get(0),
        )
        .ok()?;
    let mut stmt = conn
        .prepare("SELECT data FROM part WHERE message_id = ?1 ORDER BY time_created")
        .ok()?;
    let rows = stmt.query_map([&msg_id], |r| r.get::<_, String>(0)).ok()?;
    let mut parts = Vec::new();
    for data in rows.flatten() {
        if let Ok(v) = serde_json::from_str::<Value>(&data) {
            if v.get("type").and_then(|t| t.as_str()) == Some("text") {
                if let Some(t) = v.get("text").and_then(|t| t.as_str()) {
                    parts.push(t.to_string());
                }
            }
        }
    }
    clean_prompt(&parts.join(" ")).map(|c| truncate(&c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_encoding() {
        assert_eq!(
            claude_encode(Path::new("/home/harvey/projects/skillviewer")),
            "-home-harvey-projects-skillviewer"
        );
        assert_eq!(
            claude_encode(Path::new("/home/harvey/.agents/skills")),
            "-home-harvey--agents-skills"
        );
    }

    #[test]
    fn cursor_encoding() {
        assert_eq!(
            cursor_encode(Path::new("/home/harvey/projects/skillviewer")),
            "home-harvey-projects-skillviewer"
        );
    }

    #[test]
    fn cleans_and_truncates() {
        assert_eq!(clean_prompt("<local-command-caveat>Caveat: …"), None);
        assert_eq!(clean_prompt("   "), None);
        assert_eq!(
            clean_prompt("Fix the <system-reminder>noise</system-reminder> renderer"),
            Some("Fix the renderer".to_string())
        );
        let long = "word ".repeat(40);
        assert!(truncate(&long).ends_with('…'));
        assert!(truncate("short title").eq("short title"));
    }

    #[test]
    fn claude_maps_by_session_id_not_newest() {
        // Two sessions in one cwd: the recorded session id must pick ITS transcript,
        // not whichever was written last (the same-cwd collision this fix targets).
        let base = std::env::temp_dir().join(format!("stitle-{}-{}", std::process::id(), line!()));
        let cwd = Path::new("/tmp/proj/x");
        let dir = base.join(claude_encode(cwd));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("aaa.jsonl"),
            r#"{"type":"user","message":{"role":"user","content":"Alpha task"}}"#,
        )
        .unwrap();
        fs::write(
            dir.join("bbb.jsonl"),
            r#"{"type":"user","message":{"role":"user","content":"Bravo task"}}"#,
        )
        .unwrap();

        // Exact id → that session's own title, regardless of which file is newest.
        assert_eq!(claude_from_dir(base.clone(), cwd, 0, Some("aaa")), Some("Alpha task".into()));
        assert_eq!(claude_from_dir(base.clone(), cwd, 0, Some("bbb")), Some("Bravo task".into()));
        // A known missing ID must never borrow another terminal's name.
        assert_eq!(claude_from_dir(base.clone(), cwd, 0, Some("missing")), None);
        assert!(claude_from_dir(base.clone(), cwd, 0, None).is_some());

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn claude_last_message_reads_newest_assistant_text() {
        let file = std::env::temp_dir().join(format!("slast-{}-{}.jsonl", std::process::id(), line!()));
        fs::write(
            &file,
            concat!(
                r#"{"type":"user","message":{"role":"user","content":"do the thing"}}"#,
                "\n",
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"First reply."}]}}"#,
                "\n",
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"All   done — tests   pass."}]}}"#,
                "\n",
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Bash","input":{}}]}}"#,
                "\n",
            ),
        )
        .unwrap();
        // Newest assistant TEXT wins; the trailing tool_use-only turn is skipped and
        // inner whitespace collapses to a single glanceable line.
        assert_eq!(parse_claude_last(&file), Some("All done — tests pass.".into()));
        let _ = fs::remove_file(&file);
    }

    #[test]
    fn claude_last_message_none_when_nothing_said_and_truncates_long() {
        // A turn that produced only a tool_use → no words → None (the notifier keeps
        // its fixed summons).
        let f1 = std::env::temp_dir().join(format!("slast-{}-{}.jsonl", std::process::id(), line!()));
        fs::write(
            &f1,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Read","input":{}}]}}"#,
        )
        .unwrap();
        assert_eq!(parse_claude_last(&f1), None);
        let _ = fs::remove_file(&f1);

        // A long message is capped to the preview budget with a word-boundary ellipsis.
        let f2 = std::env::temp_dir().join(format!("slast-{}-{}.jsonl", std::process::id(), line!()));
        let long = "word ".repeat(80); // ~400 chars
        fs::write(
            &f2,
            format!(
                r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"{long}"}}]}}}}"#
            ),
        )
        .unwrap();
        let out = parse_claude_last(&f2).unwrap();
        assert!(out.chars().count() <= PREVIEW_LEN + 1);
        assert!(out.ends_with('…'));
        let _ = fs::remove_file(&f2);
    }

    #[test]
    fn extracts_user_query() {
        assert_eq!(
            between("a<user_query>\n hi there \n</user_query>b", "<user_query>", "</user_query>"),
            Some("hi there")
        );
    }

    #[test]
    fn opencode_title_and_fallback() {
        let dir = std::env::temp_dir().join(format!("octitle-{}-{}", std::process::id(), line!()));
        fs::create_dir_all(&dir).unwrap();
        let db = dir.join("opencode.db");
        // Unique cwds (the OC_CACHE is a process-wide static keyed by cwd).
        let a = format!("/octest/{}/a", std::process::id());
        let b = format!("/octest/{}/b", std::process::id());
        {
            let c = Connection::open(&db).unwrap();
            c.execute_batch(
                "CREATE TABLE session (id TEXT PRIMARY KEY, parent_id TEXT, directory TEXT, title TEXT, time_updated INTEGER);
                 CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, data TEXT);
                 CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT, session_id TEXT, time_created INTEGER, data TEXT);",
            )
            .unwrap();
            let ins = |q: &str| c.execute(q, []).unwrap();
            // cwd a: newest real title wins over an older one; a subsession is ignored.
            ins(&format!("INSERT INTO session VALUES ('s1',NULL,'{a}','Fix the parser',100)"));
            ins(&format!("INSERT INTO session VALUES ('s0',NULL,'{a}','Older title',50)"));
            ins(&format!("INSERT INTO session VALUES ('sub','s1','{a}','Sub work',999)"));
            // cwd b: placeholder title → fall back to the first user message's text parts.
            ins(&format!("INSERT INTO session VALUES ('s2',NULL,'{b}','New session - 2026-06-22T04:07:02.329Z',100)"));
            ins("INSERT INTO message VALUES ('m1','s2',10,'{\"role\":\"assistant\"}')");
            ins("INSERT INTO message VALUES ('m2','s2',20,'{\"role\":\"user\"}')");
            ins("INSERT INTO part VALUES ('p1','m2','s2',20,'{\"type\":\"text\",\"text\":\"Add dark mode\"}')");
            ins("INSERT INTO part VALUES ('p2','m2','s2',21,'{\"type\":\"file\"}')");
        }
        assert_eq!(opencode_from_db(&db, Path::new(&a), None), Some("Fix the parser".into()));
        assert_eq!(opencode_from_db(&db, Path::new(&b), None), Some("Add dark mode".into()));
        assert_eq!(opencode_from_db(&db, Path::new("/octest/none"), None), None);
        assert!(opencode_from_db(Path::new("/no/such.db"), Path::new(&a), None).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn claude_custom_names_survive_ai_updates_and_keep_their_full_length() {
        let dir = crate::state_store::tests::TempDir::new();
        let file = dir.0.join("session.jsonl");
        let name = "Readable personal session name ".repeat(5).trim().to_string();
        fs::write(&file, format!("{}\n{}\n{}\n",
            serde_json::json!({"type":"user","message":{"content":"Original prompt"}}),
            serde_json::json!({"type":"custom-title","customTitle":name}),
            serde_json::json!({"type":"ai-title","aiTitle":"Later generated title"}),
        )).unwrap();
        assert_eq!(parse_claude(&file), Some(name));
        use std::io::Write;
        writeln!(fs::OpenOptions::new().append(true).open(&file).unwrap(),
            "{}", serde_json::json!({"type":"custom-title","customTitle":""})).unwrap();
        assert_eq!(parse_claude(&file), Some("Later generated title".into()));
    }

    #[test]
    fn claude_title_sidecar_and_subsecond_transcript_writes_invalidate_cache() {
        let dir = crate::state_store::tests::TempDir::new();
        let cwd = Path::new("/project");
        let project = dir.0.join(claude_encode(cwd));
        fs::create_dir_all(project.join("session")).unwrap();
        let file = project.join("session.jsonl");
        let write_title = |title: &str, nanos| {
            fs::write(&file, serde_json::json!({"type":"ai-title","aiTitle":title}).to_string()).unwrap();
            fs::File::open(&file).unwrap().set_times(fs::FileTimes::new()
                .set_modified(UNIX_EPOCH + Duration::new(1_700_000_000, nanos))).unwrap();
        };
        write_title("First", 1);
        assert_eq!(claude_from_dir(dir.0.clone(), cwd, 0, Some("session")), Some("First".into()));
        write_title("Other", 2);
        assert_eq!(claude_from_dir(dir.0.clone(), cwd, 0, Some("session")), Some("Other".into()));
        let sidecar = project.join("session/custom-title.json");
        fs::write(&sidecar, r#"{"customTitle":"Recovered custom name"}"#).unwrap();
        assert_eq!(claude_from_dir(dir.0.clone(), cwd, 0, Some("session")), Some("Recovered custom name".into()));
        fs::remove_file(sidecar).unwrap();
        assert_eq!(claude_from_dir(dir.0.clone(), cwd, 0, Some("session")), Some("Other".into()));
    }

    #[test]
    fn claude_recovery_sidecar_overrides_old_metadata_but_not_a_recent_rename() {
        let dir = crate::state_store::tests::TempDir::new();
        let file = dir.0.join("session.jsonl");
        fs::create_dir(dir.0.join("session")).unwrap();
        fs::write(dir.0.join("session/custom-title.json"), r#"{"customTitle":"Recovered name"}"#).unwrap();
        fs::write(&file, format!("{}\n{}\n{}\n",
            serde_json::json!({"type":"custom-title","customTitle":"Old name"}),
            serde_json::json!({"type":"assistant","message":{"content":"x".repeat(70_000)}}),
            serde_json::json!({"type":"ai-title","aiTitle":"Generated name"}),
        )).unwrap();
        assert_eq!(parse_claude(&file), Some("Recovered name".into()));
        use std::io::Write;
        let mut transcript = fs::OpenOptions::new().append(true).open(&file).unwrap();
        writeln!(transcript, "{}", serde_json::json!({"type":"custom-title","customTitle":"Recent name"})).unwrap();
        assert_eq!(parse_claude(&file), Some("Recent name".into()));
        writeln!(transcript, "{}", serde_json::json!({"type":"custom-title","customTitle":""})).unwrap();
        assert_eq!(parse_claude(&file), Some("Generated name".into()));
    }

    #[test]
    fn claude_paths_handle_special_characters_long_slugs_and_relocated_exact_ids() {
        assert_eq!(claude_encode(Path::new("/work/my_app (demo)/é")), "-work-my-app--demo---");
        let long = format!("/{}", "a".repeat(220));
        assert_eq!(claude_encode(Path::new(&long)), format!("{}-7upm6p", &long.replace('/', "-")[..200]));
        let dir = crate::state_store::tests::TempDir::new();
        let moved = dir.0.join("moved-project");
        fs::create_dir(&moved).unwrap();
        fs::write(moved.join("known.jsonl"), r#"{"type":"ai-title","aiTitle":"Moved session"}"#).unwrap();
        assert_eq!(claude_from_dir(dir.0.clone(), Path::new("/old/location"), 0, Some("known")), Some("Moved session".into()));
        assert_eq!(claude_from_dir(dir.0.clone(), Path::new("/old/location"), 0, Some("../known")), None);
        fs::create_dir(dir.0.join("duplicate")).unwrap();
        fs::copy(moved.join("known.jsonl"), dir.0.join("duplicate/known.jsonl")).unwrap();
        assert_eq!(claude_from_dir(dir.0.clone(), Path::new("/old/location"), 0, Some("known")), None);
    }

    #[test]
    fn gemini_reads_legacy_json_and_current_jsonl_without_ignoring_exact_ids() {
        let dir = crate::state_store::tests::TempDir::new();
        let cwd = Path::new("/gemini/project");
        let project = dir.0.join("project");
        fs::create_dir_all(project.join("chats")).unwrap();
        fs::write(project.join(".project_root"), cwd.to_string_lossy().as_bytes()).unwrap();
        fs::write(project.join("chats/legacy.json"), serde_json::json!({
            "sessionId":"legacy", "messages":[{"type":"user","content":[{"text":"Original legacy task"}]}]
        }).to_string()).unwrap();
        fs::write(project.join("chats/current.jsonl"), concat!(
            "{\"sessionId\":\"current\",\"kind\":\"main\"}\n",
            "{\"type\":\"user\",\"content\":\"Original current task\"}\n",
            "{\"$set\":{\"summary\":\"Current generated summary\"}}\n"
        )).unwrap();
        fs::write(project.join("chats/child.jsonl"), "{\"sessionId\":\"child\",\"kind\":\"subagent\",\"summary\":\"Child title\"}\n").unwrap();
        assert_eq!(gemini_from_tmp(&dir.0, cwd, 0, Some("legacy")), Some("Original legacy task".into()));
        assert_eq!(gemini_from_tmp(&dir.0, cwd, 0, Some("current")), Some("Current generated summary".into()));
        assert_eq!(gemini_from_tmp(&dir.0, cwd, 0, Some("missing")), None);
        assert_eq!(gemini_from_tmp(&dir.0, cwd, 0, Some("child")), None);
        // A rewritten history must invalidate both its identity and title.
        fs::write(project.join("chats/legacy.json"), serde_json::json!({
            "sessionId":"replacement", "summary":"Replaced legacy summary", "messages":[]
        }).to_string()).unwrap();
        assert_eq!(gemini_from_tmp(&dir.0, cwd, 0, Some("legacy")), None);
        assert_eq!(gemini_from_tmp(&dir.0, cwd, 0, Some("replacement")), Some("Replaced legacy summary".into()));
    }

    #[test]
    fn opencode_current_projection_exact_ids_and_database_specific_cache() {
        let dir = crate::state_store::tests::TempDir::new();
        let db = dir.0.join("opencode.db");
        let connection = Connection::open(&db).unwrap();
        connection.execute_batch("PRAGMA journal_mode=WAL;
            CREATE TABLE session (id TEXT PRIMARY KEY, parent_id TEXT, directory TEXT, title TEXT, time_updated INTEGER, time_archived INTEGER);
            CREATE TABLE session_message (id TEXT PRIMARY KEY, session_id TEXT, type TEXT, seq INTEGER, data TEXT);
            INSERT INTO session VALUES ('one', NULL, '/same', 'First title', 1, NULL), ('two', NULL, '/same', 'New session - today', 2, NULL), ('old', NULL, '/same', 'Archived session', 999, 999);
            INSERT INTO session_message VALUES ('m1', 'two', 'user', 1, '{\"text\":\"Current OpenCode task\"}');").unwrap();
        assert_eq!(opencode_from_db(&db, Path::new("/same"), Some("one")), Some("First title".into()));
        assert_eq!(opencode_from_db(&db, Path::new("/same"), None), Some("Current OpenCode task".into()));
        assert_eq!(opencode_from_db(&db, Path::new("/same"), Some("missing")), None);
        let long = "An authored OpenCode title ".repeat(5).trim().to_string();
        connection.execute("UPDATE session SET title = ?1 WHERE id = 'one'", [&long]).unwrap();
        assert_eq!(opencode_from_db(&db, Path::new("/same"), Some("one")), Some(long));
        connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
        let other = dir.0.join("other.db");
        fs::copy(&db, &other).unwrap();
        Connection::open(&other).unwrap().execute("UPDATE session SET title = 'Other database' WHERE id = 'one'", []).unwrap();
        assert_eq!(opencode_from_db(&other, Path::new("/same"), Some("one")), Some("Other database".into()));
    }

    #[test]
    fn opencode_honors_xdg_data_and_database_overrides() {
        let home = Path::new("/home/test");
        assert_eq!(opencode_db_path(home, None, None), Some(home.join(".local/share/opencode/opencode.db")));
        assert_eq!(opencode_db_path(home, Some(Path::new("/data")), Some(Path::new("custom.db"))), Some(PathBuf::from("/data/opencode/custom.db")));
        assert_eq!(opencode_db_path(home, None, Some(Path::new("/other/session.db"))), Some(PathBuf::from("/other/session.db")));
        assert_eq!(opencode_db_path(home, None, Some(Path::new(":memory:"))), None);
    }

    #[test]
    fn cursor_exact_ids_never_fall_back_to_a_neighboring_transcript() {
        let dir = crate::state_store::tests::TempDir::new();
        for (id, prompt) in [("first", "Fix the first task"), ("second", "Fix the second task")] {
            fs::create_dir(dir.0.join(id)).unwrap();
            fs::write(dir.0.join(id).join(format!("{id}.jsonl")), serde_json::json!({
                "role":"user", "message":{"content":[{"type":"text","text":format!("<user_query>{prompt}</user_query>")}]}
            }).to_string()).unwrap();
        }
        assert_eq!(cursor_from_base(&dir.0, 0, Some("first")), Some("Fix the first task".into()));
        assert_eq!(cursor_from_base(&dir.0, 0, Some("second")), Some("Fix the second task".into()));
        assert_eq!(cursor_from_base(&dir.0, 0, Some("missing")), None);
        assert_eq!(cursor_from_base(&dir.0, 0, Some("../first")), None);
    }
}
