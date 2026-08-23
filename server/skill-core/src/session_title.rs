//! A short human title for a live terminal, read from the agent's OWN session
//! record — far more meaningful than the raw cwd. Hung off `AgentDef.session_title`
//! (the common agent interface) so each agent contributes what it can and the rest
//! degrade to `None`. Claude, Codex, Gemini, Cursor parse their JSONL transcripts;
//! opencode reads its WAL SQLite store (bundled SQLite). Only openclaw is still
//! uncovered — it isn't launchable and its on-disk format is unverified.
//!
//! Correlation: when the terminal recorded the session id we forced at launch
//! (`claude --session-id`), it maps to exactly that transcript — the only way to
//! tell apart several sessions in the same cwd. Otherwise (shells, resumes,
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
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, UNIX_EPOCH};

const MAX_LEN: usize = 72;

/// Longer budget for a turn-finish notification body (a phone banner shows a few
/// lines) than a one-line rail title — see [`claude_last_message`].
const PREVIEW_LEN: usize = 180;

fn home() -> Option<PathBuf> {
    dirs::home_dir()
}

/// Parsed titles keyed by (file, mtime). `terminal/list` is polled every few
/// seconds but a title only moves when the agent writes a new turn — so a file is
/// re-read only when its mtime advances, keeping polling cheap even on multi-MB
/// transcripts.
type TitleCache = HashMap<PathBuf, (u64, Option<String>)>;
static CACHE: LazyLock<Mutex<TitleCache>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn cached(file: &Path, parse: impl FnOnce(&Path) -> Option<String>) -> Option<String> {
    let mt = file_time(file, false).unwrap_or(0);
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

/// cwd → project dir name: every '/' and '.' becomes '-'.
fn claude_encode(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect()
}

pub fn claude_title(cwd: &Path, created: i64, session_id: Option<&str>) -> Option<String> {
    claude_from_dir(home()?.join(".claude/projects"), cwd, created, session_id)
}

/// The transcript file for a live Claude terminal: the exact `<sid>.jsonl` when the
/// terminal recorded the id we forced at launch (`claude --session-id`) — the only
/// way to tell apart several sessions in one cwd — else the newest-activity
/// transcript in the cwd's project dir.
fn claude_file(projects: &Path, cwd: &Path, created: i64, session_id: Option<&str>) -> Option<PathBuf> {
    let dir = projects.join(claude_encode(cwd));
    if let Some(sid) = session_id.filter(|s| !s.is_empty()) {
        let f = dir.join(format!("{sid}.jsonl"));
        if f.is_file() {
            return Some(f);
        }
    }
    pick_for_terminal(jsonl_files(&dir), created)
}

fn claude_from_dir(projects: PathBuf, cwd: &Path, created: i64, session_id: Option<&str>) -> Option<String> {
    cached(&claude_file(&projects, cwd, created, session_id)?, parse_claude)
}

/// The agent's last assistant message, as a one-line preview for a turn-finish
/// notification body — read from the SAME transcript as the title, so it reflects
/// what the agent actually SAID (its own structured message text) rather than
/// whatever furniture happens to sit at the bottom of the TUI. `None` for a session
/// with no assistant text yet (the notifier then falls back to its fixed phrase).
pub fn claude_last_message(cwd: &Path, created: i64, session_id: Option<&str>) -> Option<String> {
    let file = claude_file(&home()?.join(".claude/projects"), cwd, created, session_id)?;
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
    let mut last_title: Option<String> = None;
    let mut first_user: Option<String> = None;
    for line in read_lines(file)? {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match v.get("type").and_then(|t| t.as_str()) {
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
    let raw = last_title.or(first_user)?;
    Some(truncate(&tidy(&raw)))
}

// ─── Codex ───
// Rollout transcripts are date-organized (not by cwd), so scan the most recently
// active ones, keep those whose session_meta.cwd matches, and title from the first
// human `user_message` event. (The state_*.sqlite store holds nicer titles but is
// SQLite — deferred.)

pub fn codex_title(cwd: &Path, created: i64, _session_id: Option<&str>) -> Option<String> {
    let base = home()?.join(".codex/sessions");
    let mut rollouts: Vec<PathBuf> = walkdir::WalkDir::new(&base)
        .into_iter()
        .filter_map(|e| e.ok())
        .map(|e| e.into_path())
        .filter(|p| {
            p.is_file()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("rollout-") && n.ends_with(".jsonl"))
        })
        .collect();
    // Bound the scan to the most recently active rollouts.
    rollouts.sort_by_key(|p| std::cmp::Reverse(file_time(p, false).unwrap_or(0)));
    rollouts.truncate(48);
    let want = cwd.to_string_lossy();
    let mine: Vec<PathBuf> = rollouts
        .into_iter()
        .filter(|p| codex_meta_cwd(p).as_deref() == Some(&want))
        .collect();
    let file = pick_for_terminal(mine, created)?;
    cached(&file, parse_codex)
}

fn parse_codex(file: &Path) -> Option<String> {
    for line in read_lines(file)? {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        // The `user_message` event carries the clean typed human text.
        if v.get("type").and_then(|t| t.as_str()) == Some("event_msg") {
            let pl = v.get("payload");
            if pl.and_then(|p| p.get("type")).and_then(|t| t.as_str()) == Some("user_message") {
                if let Some(m) = pl.and_then(|p| p.get("message")).and_then(|m| m.as_str()) {
                    // IDE-launched Codex wraps the real ask under this marker,
                    // after an "## Open tabs:" context dump.
                    let ask = m
                        .rsplit_once("## My request for Codex:")
                        .map(|(_, a)| a)
                        .unwrap_or(m);
                    if let Some(c) = clean_prompt(ask) {
                        return Some(truncate(&c));
                    }
                }
            }
        }
    }
    None
}

fn codex_meta_cwd(file: &Path) -> Option<String> {
    // session_meta is the first line.
    let first = read_lines(file)?.next()?;
    let v: Value = serde_json::from_str(&first).ok()?;
    v.get("payload")?
        .get("cwd")?
        .as_str()
        .map(|s| s.to_string())
}

// ─── Gemini CLI ───
// Sessions live under ~/.gemini/tmp/<slug>/chats/; the slug maps to a cwd via a
// sibling `.project_root` file. Title = the LLM `summary` if present, else the
// first user message (a live session usually has no summary yet).

pub fn gemini_title(cwd: &Path, created: i64, _session_id: Option<&str>) -> Option<String> {
    let tmp = home()?.join(".gemini/tmp");
    let want = cwd.to_string_lossy();
    let proj = fs::read_dir(&tmp).ok()?.filter_map(|e| e.ok()).find(|e| {
        fs::read_to_string(e.path().join(".project_root"))
            .map(|c| c.trim() == want)
            .unwrap_or(false)
    })?;
    let chats = proj.path().join("chats");
    let file = pick_for_terminal(jsonl_files(&chats), created)?;
    cached(&file, parse_gemini)
}

fn parse_gemini(file: &Path) -> Option<String> {
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
    let raw = summary.or(first_user)?;
    Some(truncate(&tidy(&raw)))
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

pub fn cursor_title(cwd: &Path, created: i64, _session_id: Option<&str>) -> Option<String> {
    let base = home()?
        .join(".cursor/projects")
        .join(cursor_encode(cwd))
        .join("agent-transcripts");
    // each session is its own subdir holding <uuid>.jsonl
    let files: Vec<PathBuf> = fs::read_dir(&base)
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

/// One shared db serves every cwd, so the file-mtime `cached()` can't key it — keep
/// a cwd-keyed cache instead, busted when the db advances (see `oc_sig`).
type OcCache = HashMap<String, (u64, Option<String>)>;
static OC_CACHE: LazyLock<Mutex<OcCache>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Newest-write signature: the `-wal` moves on every turn, the db itself on
/// checkpoint — take the max so a fresh turn always re-queries.
fn oc_sig(db: &Path) -> u64 {
    let wal = PathBuf::from(format!("{}-wal", db.display()));
    file_time(&wal, false).unwrap_or(0).max(file_time(db, false).unwrap_or(0))
}

fn oc_placeholder(t: &str) -> bool {
    t.starts_with("New session - ") || t.starts_with("Child session - ")
}

pub fn opencode_title(cwd: &Path, _created: i64, _session_id: Option<&str>) -> Option<String> {
    opencode_from_db(&home()?.join(".local/share/opencode/opencode.db"), cwd)
}

fn opencode_from_db(db: &Path, cwd: &Path) -> Option<String> {
    if !db.is_file() {
        return None;
    }
    let sig = oc_sig(db);
    let key = cwd.to_string_lossy().to_string();
    if let Ok(c) = OC_CACHE.lock() {
        if let Some((s, title)) = c.get(&key) {
            if *s == sig {
                return title.clone();
            }
        }
    }
    let title = oc_query(db, &key);
    if let Ok(mut c) = OC_CACHE.lock() {
        c.insert(key, (sig, title.clone()));
    }
    title
}

fn oc_query(db: &Path, cwd: &str) -> Option<String> {
    // READ_ONLY still reads committed WAL frames; `immutable` would ignore the WAL
    // and read a stale checkpoint, so don't set it.
    let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    let _ = conn.busy_timeout(Duration::from_millis(200));
    // Newest top-level session for this cwd; parent_id IS NULL drops subagent sessions.
    let (id, title): (String, String) = conn
        .query_row(
            "SELECT id, title FROM session \
             WHERE directory = ?1 AND parent_id IS NULL \
             ORDER BY time_updated DESC LIMIT 1",
            [cwd],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok()?;
    let title = title.trim();
    if !title.is_empty() && !oc_placeholder(title) {
        return Some(truncate(&tidy(title)));
    }
    oc_first_user_text(&conn, &id)
}

/// The earliest user message's text parts, joined — the fallback when the title is
/// still a placeholder. (json_extract is built into bundled SQLite.)
fn oc_first_user_text(conn: &Connection, session_id: &str) -> Option<String> {
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
        // Unknown / empty id → falls back to newest-activity, never panics.
        assert!(claude_from_dir(base.clone(), cwd, 0, Some("missing")).is_some());
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
        assert_eq!(opencode_from_db(&db, Path::new(&a)), Some("Fix the parser".into()));
        assert_eq!(opencode_from_db(&db, Path::new(&b)), Some("Add dark mode".into()));
        assert_eq!(opencode_from_db(&db, Path::new("/octest/none")), None);
        assert!(opencode_from_db(Path::new("/no/such.db"), Path::new(&a)).is_none());
        let _ = fs::remove_dir_all(&dir);
    }
}
