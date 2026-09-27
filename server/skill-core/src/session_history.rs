//! Past conversations remain owned by the agent. Read metadata from its native
//! store and resume its exact ID; never copy transcripts or guess by directory.
//! Bounded head/tail reads and file caches keep large histories inexpensive.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use crate::session_title::{claude_saved_title, clean_prompt, truncate_to};

mod codex;
pub use codex::{codex_find, codex_history};

const MAX_CANDIDATES: usize = 20_000;
const MAX_SESSIONS: usize = 5_000;
const WINDOW_BYTES: usize = 65_536;
const READ_BUDGET: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub agent: String,
    pub session_id: String,
    pub title: String,
    pub cwd: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryPage {
    pub sessions: Vec<HistoryEntry>,
    pub warnings: Vec<String>,
    pub truncated: bool,
    pub has_more: bool,
}

/// List native histories on this host. Search precedes pagination and includes
/// the provider label, folder, title, and exact ID. A failing provider does not
/// hide healthy providers. `truncated` signals source limits, not another page.
pub fn list(query: &str, agent: Option<&str>, offset: usize, limit: usize) -> Result<HistoryPage, String> {
    let selected = agent.filter(|agent| !agent.is_empty());
    if let Some(agent) = selected {
        if crate::agents::by_family(agent).and_then(|def| def.history).is_none() {
            return Err(format!("Saved session history is unavailable for {agent}."));
        }
    }
    let mut page = HistoryPage::default();
    for def in crate::agents::AGENTS {
        if selected.is_some_and(|agent| crate::agents::by_family(agent).map(|a| a.family) != Some(def.family)) {
            continue;
        }
        let Some(capability) = def.history else { continue };
        match (capability.list)() {
            Ok(provider) => {
                page.sessions.extend(provider.sessions);
                page.warnings.extend(provider.warnings);
                page.truncated |= provider.truncated;
            }
            Err(error) => {
                page.truncated = true;
                page.warnings.push(format!("{} history: {error}", def.label));
            }
        }
    }
    paginate(page, query, offset, limit)
}

fn paginate(mut page: HistoryPage, query: &str, offset: usize, limit: usize) -> Result<HistoryPage, String> {
    let terms: Vec<_> = query.split_whitespace().map(str::to_lowercase).collect();
    page.sessions.retain(|session| {
        let label = crate::agents::by_family(&session.agent).map(|a| a.label).unwrap_or(&session.agent);
        let text = format!("{} {} {} {} {}", session.agent, label, session.session_id, session.title, session.cwd).to_lowercase();
        terms.iter().all(|term| text.contains(term))
    });
    page.sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at)
        .then_with(|| b.created_at.cmp(&a.created_at))
        .then_with(|| a.agent.cmp(&b.agent)).then_with(|| a.session_id.cmp(&b.session_id)));
    let mut seen = HashSet::new();
    page.sessions.retain(|entry| seen.insert((entry.agent.clone(), entry.session_id.clone())));
    let limit = limit.clamp(1, 200);
    page.has_more = page.sessions.len() > offset.saturating_add(limit);
    page.sessions = page.sessions.into_iter().skip(offset).take(limit).collect();
    Ok(page)
}

/// Resolve a selected native ID again at resume time. Never accept an arbitrary
/// command argument/path or substitute another session when the store changed.
pub fn find(agent: &str, session_id: &str) -> Result<HistoryEntry, String> {
    if !valid_session_id(session_id) {
        return Err("Invalid saved session ID.".into());
    }
    let def = crate::agents::by_family(agent).ok_or("Unknown agent.")?;
    let capability = def.history.ok_or("This agent cannot resume saved sessions yet.")?;
    (capability.find)(session_id)?
        .ok_or_else(|| "This conversation is no longer available in the agent's saved history. Refresh history and try again.".into())
}

pub fn valid_session_id(id: &str) -> bool {
    id.len() == 36 && id.bytes().enumerate().all(|(i, byte)| {
        if matches!(i, 8 | 13 | 18 | 23) { byte == b'-' } else { byte.is_ascii_hexdigit() }
    })
}

fn claude_projects() -> Result<PathBuf, String> {
    crate::session_title::claude_projects()
        .ok_or_else(|| "Cannot determine the agent's home directory.".into())
}

pub fn claude_history() -> Result<HistoryPage, String> {
    claude_from_projects(&claude_projects()?)
}

pub fn claude_find(id: &str) -> Result<Option<HistoryEntry>, String> {
    claude_find_in(&claude_projects()?, id)
}

fn claude_find_in(projects: &Path, id: &str) -> Result<Option<HistoryEntry>, String> {
    if !valid_session_id(id) { return Err("Invalid saved session ID.".into()); }
    let directories = match fs::read_dir(projects) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("Cannot read saved conversations: {error}")),
    };
    let mut found = None;
    for (count, directory) in directories.enumerate() {
        if count >= MAX_CANDIDATES { return Err("Too many Claude project folders to safely locate this conversation.".into()); }
        let directory = directory.map_err(|error| error.to_string())?;
        if !directory.file_type().map_err(|error| error.to_string())?.is_dir() { continue; }
        let path = directory.path().join(format!("{id}.jsonl"));
        if !path.is_file() { continue; }
        if found.is_some() { return Err("This Claude conversation has duplicate history files; resume it through Claude Code to choose the intended copy.".into()); }
        found = Some(path);
    }
    let mut budget = READ_BUDGET;
    match found {
        Some(path) => cached_transcript(&path, "claude", &mut budget),
        None => Ok(None),
    }
}

fn claude_from_projects(projects: &Path) -> Result<HistoryPage, String> {
    let mut page = HistoryPage::default();
    let directories = match fs::read_dir(projects) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(page),
        Err(error) => return Err(format!("Cannot read saved conversations: {error}")),
    };
    let mut files = Vec::new();
    let mut failures = 0usize;
    let mut visited = 0;
    'projects: for directory in directories {
        let directory = match directory { Ok(entry) => entry, Err(_) => { failures += 1; continue } };
        if !directory.file_type().map(|kind| kind.is_dir()).unwrap_or(false) { continue; }
        let entries = match fs::read_dir(directory.path()) {
            Ok(entries) => entries,
            Err(_) => { failures += 1; continue; }
        };
        for entry in entries {
            visited += 1;
            if visited > MAX_CANDIDATES { page.truncated = true; break 'projects; }
            let entry = match entry { Ok(entry) => entry, Err(_) => { failures += 1; continue } };
            let path = entry.path();
            if path.extension().is_none_or(|extension| extension != "jsonl") { continue; }
            // Only a project's direct UUID transcripts, never subagent folders.
            if !path.file_stem().and_then(|stem| stem.to_str()).is_some_and(valid_session_id) { continue; }
            let metadata = match entry.metadata() { Ok(meta) if meta.is_file() => meta, _ => { failures += 1; continue } };
            files.push((unix_time(metadata.modified().ok()), path));
        }
    }
    files.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let mut budget = READ_BUDGET;
    for (_, file) in files {
        if page.sessions.len() >= MAX_SESSIONS || budget == 0 { page.truncated = true; break; }
        match cached_transcript(&file, "claude", &mut budget) {
            Ok(Some(entry)) => page.sessions.push(entry),
            Ok(None) => {}
            Err(_) => failures += 1,
        }
    }
    page.truncated |= budget == 0;
    if page.truncated { page.warnings.push("Claude Code history reached its scan limit; some older conversations may be omitted.".into()); }
    if failures > 0 {
        page.truncated = true;
        page.warnings.push(format!("Claude Code: {failures} saved history files or folders could not be read."));
    }
    Ok(page)
}

#[derive(Clone, PartialEq, Eq)]
struct Stamp(Option<SystemTime>, u64);
fn stamp(file: &Path) -> Option<Stamp> {
    fs::metadata(file).ok().map(|meta| Stamp(meta.modified().ok(), meta.len()))
}
type TranscriptCache = HashMap<(String, PathBuf), (Stamp, Option<Stamp>, Option<HistoryEntry>)>;
static TRANSCRIPTS: LazyLock<Mutex<TranscriptCache>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn claude_sidecar(file: &Path) -> Option<PathBuf> {
    Some(file.parent()?.join(file.file_stem()?).join("custom-title.json"))
}

fn cached_transcript(file: &Path, agent: &str, budget: &mut usize) -> Result<Option<HistoryEntry>, String> {
    let signature = stamp(file).ok_or("Saved conversation is unavailable.")?;
    let sidecar = if agent == "claude" { claude_sidecar(file) } else { None };
    let sidecar_stamp = sidecar.as_deref().and_then(stamp);
    let key = (agent.to_string(), file.to_path_buf());
    if let Ok(cache) = TRANSCRIPTS.lock() {
        if let Some((previous, old_sidecar, entry)) = cache.get(&key) {
            if *previous == signature && *old_sidecar == sidecar_stamp { return Ok(entry.clone()); }
        }
    }
    let needed = signature.1.min((WINDOW_BYTES * 2) as u64) as usize;
    if needed > *budget { *budget = 0; return Ok(None); }
    *budget -= needed;
    let (head, tail) = read_windows(file, signature.1)?;
    let modified = unix_time(signature.0);
    let entry = if agent == "claude" {
        let sidecar_title = sidecar.and_then(|path| read_json_small(&path, WINDOW_BYTES))
            .and_then(|value| value["customTitle"].as_str().map(str::to_owned));
        parse_claude(file, &head, &tail, sidecar_title, modified)
    } else {
        codex::parse_rollout(file, &head, &tail, modified)
    };
    if let Ok(mut cache) = TRANSCRIPTS.lock() {
        if cache.len() >= MAX_CANDIDATES { cache.clear(); }
        cache.insert(key, (signature, sidecar_stamp, entry.clone()));
    }
    Ok(entry)
}

fn read_json_small(file: &Path, limit: usize) -> Option<Value> {
    let file = fs::File::open(file).ok()?;
    if file.metadata().ok()?.len() > limit as u64 { return None; }
    serde_json::from_reader(file.take(limit as u64)).ok()
}

fn read_windows(file: &Path, size: u64) -> Result<(Vec<Value>, Vec<Value>), String> {
    let mut input = fs::File::open(file).map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    let head_limit = if size <= (WINDOW_BYTES * 2) as u64 { WINDOW_BYTES * 2 } else { WINDOW_BYTES };
    (&mut input).take(head_limit as u64).read_to_end(&mut bytes).map_err(|error| error.to_string())?;
    let head = records(&bytes);
    if size <= (WINDOW_BYTES * 2) as u64 {
        let tail = records(&bytes[bytes.len().saturating_sub(WINDOW_BYTES)..]);
        return Ok((head, tail));
    }
    let start = size.saturating_sub(WINDOW_BYTES as u64).max(WINDOW_BYTES as u64);
    input.seek(SeekFrom::Start(start)).map_err(|error| error.to_string())?;
    bytes.clear();
    input.take(WINDOW_BYTES as u64).read_to_end(&mut bytes).map_err(|error| error.to_string())?;
    Ok((head, records(&bytes)))
}

fn records(bytes: &[u8]) -> Vec<Value> {
    bytes.split(|byte| *byte == b'\n').filter_map(|line| serde_json::from_slice(line).ok()).collect()
}

fn parse_claude(file: &Path, head: &[Value], tail: &[Value], sidecar: Option<String>, modified: i64) -> Option<HistoryEntry> {
    let id = file.file_stem()?.to_str()?;
    if !valid_session_id(id) { return None; }
    let mut cwd = None;
    let mut created = None;
    let mut updated = None;
    let mut first_prompt = None;
    let mut custom = None;
    let mut generated = None;
    let mut summary = None;
    let mut has_user = false;
    for record in head.iter().chain(tail) {
        if record["isSidechain"].as_bool() == Some(true) { return None; }
        if let Some(record_id) = record["sessionId"].as_str() {
            if record_id != id { return None; }
        }
        if let Some(path) = record["cwd"].as_str().filter(|path| Path::new(path).is_absolute()) { cwd = Some(path.to_string()); }
        if let Some(time) = record["timestamp"].as_str().and_then(parse_timestamp) {
            created = Some(created.map_or(time, |value: i64| value.min(time)));
            updated = Some(updated.map_or(time, |value: i64| value.max(time)));
        }
        match record["type"].as_str() {
            Some("custom-title") => custom = record["customTitle"].as_str().map(str::to_owned),
            Some("ai-title") => {
                if let Some(title) = record["aiTitle"].as_str().and_then(claude_saved_title) {
                    generated = Some(title);
                }
            }
            Some("summary") => summary = record["summary"].as_str().and_then(prompt_title),
            Some("user") if record["isMeta"].as_bool() != Some(true) && record["isCompactSummary"].as_bool() != Some(true) => {
                if let Some(prompt) = user_text(&record["message"]["content"]).as_deref().and_then(prompt_title) {
                    has_user = true;
                    first_prompt.get_or_insert(prompt);
                }
            }
            _ => {}
        }
    }
    let recent_custom = tail.iter().rev().find(|record| record["type"] == "custom-title")
        .and_then(|record| record["customTitle"].as_str()).map(str::to_owned);
    let title = recent_custom.or(sidecar).or(custom).as_deref().and_then(claude_saved_title)
        .or(generated).or(summary).or(first_prompt);
    // Metadata-only prelaunch files and tool-only sidechains are not history.
    if !has_user && title.is_none() { return None; }
    Some(HistoryEntry { agent: "claude".into(), session_id: id.into(),
        title: title.unwrap_or_else(|| "Claude Code session".into()), cwd: cwd?,
        created_at: created.unwrap_or(modified), updated_at: updated.unwrap_or(modified).max(modified) })
}

fn user_text(value: &Value) -> Option<String> {
    value.as_str().map(str::to_owned).or_else(|| value.as_array().map(|parts| {
        parts.iter().filter(|part| matches!(part["type"].as_str(), Some("text" | "input_text")))
            .filter_map(|part| part["text"].as_str()).collect::<Vec<_>>().join(" ")
    }))
}

fn prompt_title(value: &str) -> Option<String> {
    clean_prompt(value).map(|title| truncate_to(&title, 120))
}

fn unix_time(time: Option<SystemTime>) -> i64 {
    time.and_then(|value| value.duration_since(UNIX_EPOCH).ok()).map(|duration| duration.as_secs() as i64).unwrap_or(0)
}

/// Provider timestamps use RFC3339. Fractional seconds do not affect the API's
/// seconds precision; accept UTC and explicit offsets without a date dependency.
fn parse_timestamp(value: &str) -> Option<i64> {
    let number = |start, end| value.get(start..end)?.parse::<i64>().ok();
    let (mut year, month, day) = (number(0, 4)?, number(5, 7)?, number(8, 10)?);
    let (hour, minute, second) = (number(11, 13)?, number(14, 16)?, number(17, 19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60 { return None; }
    let suffix = value.get(19..)?;
    let zone = suffix.find(['Z', '+', '-'])?;
    let zone = &suffix[zone..];
    let offset = if zone == "Z" { 0 } else {
        let hours = zone.get(1..3)?.parse::<i64>().ok()?;
        let minutes = zone.get(4..6)?.parse::<i64>().ok()?;
        if hours > 23 || minutes > 59 { return None; }
        (hours * 3600 + minutes * 60) * if zone.starts_with('-') { -1 } else { 1 }
    };
    year -= i64::from(month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let adjusted_month = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * adjusted_month + 2) / 5 + day - 1;
    let days = era * 146097 + year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year - 719468;
    Some(days * 86400 + hour * 3600 + minute * 60 + second - offset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ALPHA: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    const BETA: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";

    fn transcript(project: &Path, id: &str, prompt: &str) -> PathBuf {
        fs::create_dir_all(project).unwrap();
        let path = project.join(format!("{id}.jsonl"));
        fs::write(&path, format!("{}\n", json!({"type":"user", "sessionId":id, "cwd":"/same/project", "timestamp":"2026-09-20T12:00:00.000Z", "message":{"content":prompt}}))).unwrap();
        path
    }

    #[test]
    fn claude_catalog_keeps_exact_ids_excludes_children_and_uses_native_names() {
        let fixture = crate::state_store::tests::TempDir::new();
        let projects = fixture.0.join("custom-claude-home/projects");
        let project = projects.join("-same-project");
        let first = transcript(&project, ALPHA, "Fix alpha");
        transcript(&project, BETA, "Fix beta");
        transcript(&project.join(ALPHA).join("subagents"), ALPHA, "Hidden child");
        use std::io::Write;
        writeln!(fs::OpenOptions::new().append(true).open(&first).unwrap(), "{}", json!({"type":"ai-title","aiTitle":"Named alpha task"})).unwrap();
        let page = claude_from_projects(&projects).unwrap();
        assert_eq!(page.sessions.len(), 2);
        assert_eq!(page.sessions.iter().find(|entry| entry.session_id == ALPHA).unwrap().title, "Named alpha task");
        assert_eq!(claude_find_in(&projects, BETA).unwrap().unwrap().title, "Fix beta");
        assert!(claude_find_in(&projects, "../../bad").is_err());
        assert!(find("claude", "--continue").is_err());
    }

    #[test]
    fn claude_sidecar_and_appended_names_invalidate_precise_cache() {
        let fixture = crate::state_store::tests::TempDir::new();
        let projects = fixture.0.join("projects");
        let file = transcript(&projects.join("project"), ALPHA, "Original prompt");
        assert_eq!(claude_find_in(&projects, ALPHA).unwrap().unwrap().title, "Original prompt");
        let sidecar = claude_sidecar(&file).unwrap();
        fs::create_dir_all(sidecar.parent().unwrap()).unwrap();
        fs::write(&sidecar, r#"{"customTitle":"Recovered name"}"#).unwrap();
        assert_eq!(claude_find_in(&projects, ALPHA).unwrap().unwrap().title, "Recovered name");
        use std::io::Write;
        writeln!(fs::OpenOptions::new().append(true).open(&file).unwrap(), "{}", json!({"type":"custom-title","customTitle":"New explicit name"})).unwrap();
        assert_eq!(claude_find_in(&projects, ALPHA).unwrap().unwrap().title, "New explicit name");
        writeln!(fs::OpenOptions::new().append(true).open(&file).unwrap(), "{}", json!({"type":"custom-title","customTitle":""})).unwrap();
        assert_eq!(claude_find_in(&projects, ALPHA).unwrap().unwrap().title, "Original prompt");
    }

    #[test]
    fn claude_history_cleans_prompt_wrappers_but_preserves_authored_names() {
        let fixture = crate::state_store::tests::TempDir::new();
        let projects = fixture.0.join("projects");
        let project = projects.join("project");
        transcript(&project, ALPHA, "<system-reminder>Only context</system-reminder><ide_opened>file.rs</ide_opened>");
        assert!(claude_find_in(&projects, ALPHA).unwrap().is_none());
        let file = transcript(&project, ALPHA,
            "<ide_selection>irrelevant code</ide_selection>Fix the <system-reminder>noise</system-reminder> renderer");
        assert_eq!(claude_find_in(&projects, ALPHA).unwrap().unwrap().title, "Fix the renderer");

        // A saved name is authored text, even when it resembles prompt markup.
        // It must survive unchanged beyond the old history-only 400-char cap.
        let name = "Keep <image>literal</image> in my authored session name ".repeat(12).trim().to_string();
        assert!(name.chars().count() > 400);
        use std::io::Write;
        let mut output = fs::OpenOptions::new().append(true).open(&file).unwrap();
        for record in [
            json!({"type":"custom-title","customTitle":name}),
            json!({"type":"ai-title","aiTitle":"Generated fallback"}),
        ] {
            writeln!(output, "{record}").unwrap();
        }
        assert_eq!(claude_find_in(&projects, ALPHA).unwrap().unwrap().title, name);
        // Resetting the explicit name reveals the last nonempty generated name.
        for record in [
            json!({"type":"custom-title","customTitle":""}),
            json!({"type":"ai-title","aiTitle":""}),
        ] {
            writeln!(output, "{record}").unwrap();
        }
        assert_eq!(claude_find_in(&projects, ALPHA).unwrap().unwrap().title, "Generated fallback");
    }

    #[test]
    fn claude_large_transcripts_are_bounded_and_exact_lookup_rejects_duplicate_copies() {
        let fixture = crate::state_store::tests::TempDir::new();
        let projects = fixture.0.join("projects");
        let file = transcript(&projects.join("one"), ALPHA, "Large task");
        use std::io::Write;
        let mut output = fs::OpenOptions::new().append(true).open(&file).unwrap();
        writeln!(output, "{}", json!({"type":"assistant","message":{"content":"x".repeat(3_000_000)}})).unwrap();
        writeln!(output, "{}", json!({"type":"ai-title","aiTitle":"Latest title"})).unwrap();
        let mut budget = WINDOW_BYTES * 2;
        assert_eq!(cached_transcript(&file, "claude", &mut budget).unwrap().unwrap().title, "Latest title");
        assert_eq!(budget, 0);
        let mut cached_budget = 0;
        assert!(cached_transcript(&file, "claude", &mut cached_budget).unwrap().is_some());
        fs::create_dir(projects.join("two")).unwrap();
        fs::copy(&file, projects.join("two").join(format!("{ALPHA}.jsonl"))).unwrap();
        assert!(claude_find_in(&projects, ALPHA).unwrap_err().contains("duplicate"));
    }

    #[test]
    fn search_happens_before_pagination_and_more_is_separate_from_scan_limits() {
        let entries = (0..6).map(|i| HistoryEntry { agent: "claude".into(), session_id: format!("id{i}"), title: if i % 2 == 0 { "Match me" } else { "Other task" }.into(), cwd: "/project".into(), created_at: i, updated_at: i }).collect();
        let source = HistoryPage { sessions: entries, truncated: true, ..Default::default() };
        let first = paginate(source.clone(), "match CLAUDE", 0, 2).unwrap();
        assert_eq!(first.sessions.iter().map(|entry| entry.session_id.as_str()).collect::<Vec<_>>(), ["id4", "id2"]);
        assert!(first.has_more && first.truncated);
        let second = paginate(source, "match", 2, 2).unwrap();
        assert_eq!(second.sessions[0].session_id, "id0");
        assert!(!second.has_more && second.truncated);
    }

    #[test]
    fn exact_resume_capabilities_preserve_identity_and_quote_binary() {
        for (agent, argument) in [("claude", "--resume"), ("codex", "resume")] {
            let capability = crate::agents::by_family(agent).unwrap().history.unwrap();
            let command = (capability.resume)(&crate::agents::ExactResumeCtx { bin: "/agent folder/cli", session_id: ALPHA });
            assert!(command.contains("'/agent folder/cli'"));
            assert!(command.contains(argument));
            assert!(command.contains(ALPHA));
            assert!(!command.contains("--last") && !command.contains("--continue") && !command.contains("--session-id"));
        }
        assert!(crate::agents::by_family("gemini").unwrap().history.is_none());
    }

    #[test]
    fn provider_timestamps_keep_timezone_and_seconds_precision() {
        assert_eq!(parse_timestamp("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_timestamp("2026-09-20T12:34:56.789Z"), parse_timestamp("2026-09-20T05:34:56-07:00"));
        assert_eq!(parse_timestamp("2024-03-01T00:00:00Z").unwrap() - parse_timestamp("2024-02-28T00:00:00Z").unwrap(), 2 * 86400);
        assert_eq!(parse_timestamp("bad data"), None);
    }
}
