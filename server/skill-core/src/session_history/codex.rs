use super::*;
use rusqlite::{Connection, OpenFlags};
use crate::session_title::{clean_codex_prompt, codex_homes, codex_saved_title, codex_state_db};

pub fn codex_history() -> Result<HistoryPage, String> {
    let (base, database_home) = homes()?;
    from_home(&base, &database_home, None)
}

pub fn codex_find(id: &str) -> Result<Option<HistoryEntry>, String> {
    if !valid_session_id(id) { return Err("Invalid saved session ID.".into()); }
    let (base, database_home) = homes()?;
    let page = from_home(&base, &database_home, Some(id))?;
    if page.sessions.is_empty() && page.truncated {
        return Err("Codex's history scan limit prevented locating this conversation; resume it through Codex.".into());
    }
    Ok(page.sessions.into_iter().next())
}

fn homes() -> Result<(PathBuf, PathBuf), String> {
    codex_homes().ok_or_else(|| "Cannot determine the agent's home directory.".into())
}

fn from_home(base: &Path, database_home: &Path, id: Option<&str>) -> Result<HistoryPage, String> {
    if let Some(database) = codex_state_db(database_home) {
        match from_database(base, &database, id) {
            Ok(page) => return Ok(page),
            Err(error) => {
                let mut page = from_rollouts(base, id)?;
                page.warnings.push(format!("Codex's history index could not be read ({error}); showing available transcript metadata."));
                return Ok(page);
            }
        }
    }
    from_rollouts(base, id)
}

fn from_database(base: &Path, database: &Path, id: Option<&str>) -> Result<HistoryPage, String> {
    let conn = Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|error| error.to_string())?;
    conn.busy_timeout(Duration::from_millis(50)).map_err(|error| error.to_string())?;
    let columns: HashSet<String> = conn.prepare("PRAGMA table_info(threads)").map_err(|error| error.to_string())?
        .query_map([], |row| row.get(1)).map_err(|error| error.to_string())?
        .collect::<Result<_, _>>().map_err(|error| error.to_string())?;
    for required in ["id", "cwd", "title", "created_at", "updated_at", "source"] {
        if !columns.contains(required) { return Err(format!("Unsupported history index: missing {required}")); }
    }
    let optional = |column: &str, fallback: &str| if columns.contains(column) { column.to_string() } else { fallback.to_string() };
    let created = if columns.contains("created_at_ms") { "COALESCE(created_at_ms / 1000, created_at)" } else { "created_at" };
    let updated = if columns.contains("updated_at_ms") { "COALESCE(updated_at_ms / 1000, updated_at)" } else { "updated_at" };
    // Newer Codex projections keep has_user_event at its legacy default even
    // for established conversations. Native prompt/name metadata also proves a
    // populated session; never require this one legacy flag by itself.
    let name = optional("name", "NULL");
    let first_prompt = optional("first_user_message", "''");
    let populated = format!("({} <> 0 OR TRIM(COALESCE({first_prompt}, '')) <> '' OR TRIM(COALESCE(title, '')) <> '' OR TRIM(COALESCE({name}, '')) <> '')", optional("has_user_event", "0"));
    let query = format!("SELECT id, cwd, {name}, title, {created}, {updated}, source, {}, {}, {first_prompt} FROM threads WHERE {} = 0 AND {populated} AND (?2 IS NULL OR id = ?2) ORDER BY {updated} DESC, id LIMIT ?1",
        optional("thread_source", "NULL"), optional("rollout_path", "NULL"), optional("archived", "0"));
    let mut statement = conn.prepare(&query).map_err(|error| error.to_string())?;
    let mut rows = statement.query(rusqlite::params![(MAX_CANDIDATES + 1) as i64, id]).map_err(|error| error.to_string())?;
    let mut page = HistoryPage::default();
    let mut names = None;
    let mut count = 0;
    while let Some(row) = rows.next().map_err(|error| error.to_string())? {
        count += 1;
        if count > MAX_CANDIDATES || page.sessions.len() >= MAX_SESSIONS { page.truncated = true; break; }
        let id: String = row.get(0).map_err(|error| error.to_string())?;
        let cwd: String = row.get(1).map_err(|error| error.to_string())?;
        let name: Option<String> = row.get(2).map_err(|error| error.to_string())?;
        let prompt: String = row.get(3).map_err(|error| error.to_string())?;
        let source: String = row.get(6).map_err(|error| error.to_string())?;
        let thread_source: Option<String> = row.get(7).map_err(|error| error.to_string())?;
        let rollout: Option<String> = row.get(8).map_err(|error| error.to_string())?;
        let first_prompt: Option<String> = row.get(9).map_err(|error| error.to_string())?;
        if !valid_session_id(&id) || !Path::new(&cwd).is_absolute() || !top_level_source(&source)
            || thread_source.as_deref().is_some_and(|source| source.to_ascii_lowercase().contains("subagent")) { continue; }
        if rollout.as_deref().is_some_and(|path| !Path::new(path).is_file()) { continue; }
        let title = name.as_deref().and_then(codex_saved_title).or_else(|| {
            let names = names.get_or_insert_with(|| index_names(&base.join("session_index.jsonl")));
            names.get(&id).and_then(|name| codex_saved_title(name))
        }).or_else(|| codex_prompt_title(&prompt))
            .or_else(|| first_prompt.as_deref().and_then(codex_prompt_title))
            .unwrap_or_else(|| "Codex session".into());
        page.sessions.push(HistoryEntry { agent: "codex".into(), session_id: id, title, cwd,
            created_at: row.get(4).map_err(|error| error.to_string())?, updated_at: row.get(5).map_err(|error| error.to_string())? });
    }
    if page.truncated { page.warnings.push("Codex history reached its scan limit; some older conversations may be omitted.".into()); }
    Ok(page)
}

fn top_level_source(source: &str) -> bool {
    let source = source.trim();
    !source.is_empty() && !source.to_ascii_lowercase().contains("subagent")
        && !source.starts_with('{') && !source.starts_with('[')
}

fn codex_prompt_title(value: &str) -> Option<String> {
    clean_codex_prompt(value).map(|title| truncate_to(&title, 120))
}

fn index_names(file: &Path) -> HashMap<String, String> {
    // Current installs store names in SQLite. Older name indexes are append-only:
    // the tail contains the newest renames, and is bounded even after years of use.
    let mut names = HashMap::new();
    let Ok(mut input) = fs::File::open(file) else { return names };
    let Ok(metadata) = input.metadata() else { return names };
    const LIMIT: u64 = 8 * 1024 * 1024;
    if input.seek(SeekFrom::Start(metadata.len().saturating_sub(LIMIT))).is_err() { return names; }
    let mut bytes = Vec::new();
    if input.take(LIMIT).read_to_end(&mut bytes).is_err() { return names; }
    for value in records(&bytes) {
        if let (Some(id), Some(name)) = (value["id"].as_str(), value["thread_name"].as_str()) {
            names.insert(id.into(), name.into());
        }
    }
    names
}

fn from_rollouts(base: &Path, id: Option<&str>) -> Result<HistoryPage, String> {
    let root = base.join("sessions");
    match fs::metadata(&root) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(HistoryPage::default()),
        Err(error) => return Err(format!("Cannot read saved conversations: {error}")),
    }
    let mut page = HistoryPage::default();
    let mut files = Vec::new();
    let mut failures = 0usize;
    for (count, entry) in walkdir::WalkDir::new(root).max_depth(5).into_iter().enumerate() {
        if count >= MAX_CANDIDATES { page.truncated = true; break; }
        let entry = match entry { Ok(entry) => entry, Err(_) => { failures += 1; continue } };
        if !entry.file_type().is_file() { continue; }
        let name = entry.file_name().to_string_lossy();
        if !name.starts_with("rollout-") || !name.ends_with(".jsonl") { continue; }
        if id.is_some_and(|id| !name.ends_with(&format!("-{id}.jsonl"))) { continue; }
        files.push((unix_time(entry.metadata().ok().and_then(|meta| meta.modified().ok())), entry.path().to_path_buf()));
    }
    files.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let names = index_names(&base.join("session_index.jsonl"));
    let mut budget = READ_BUDGET;
    for (_, file) in files {
        if page.sessions.len() >= MAX_SESSIONS || budget == 0 { page.truncated = true; break; }
        match cached_transcript(&file, "codex", &mut budget) {
            Ok(Some(mut entry)) => {
                if let Some(title) = names.get(&entry.session_id).and_then(|name| codex_saved_title(name)) { entry.title = title; }
                page.sessions.push(entry);
            }
            Ok(None) => {}
            Err(_) => failures += 1,
        }
    }
    page.truncated |= budget == 0;
    if page.truncated { page.warnings.push("Codex history reached its scan limit; some older conversations may be omitted.".into()); }
    if failures > 0 {
        page.truncated = true;
        page.warnings.push(format!("Codex: {failures} saved history files or folders could not be read."));
    }
    Ok(page)
}

pub(super) fn parse_rollout(file: &Path, head: &[Value], tail: &[Value], modified: i64) -> Option<HistoryEntry> {
    let metadata = head.first()?;
    if metadata["type"] != "session_meta" { return None; }
    let payload = &metadata["payload"];
    let id = payload["id"].as_str()?;
    if !valid_session_id(id) || !file.file_name()?.to_str()?.ends_with(&format!("-{id}.jsonl"))
        || !payload["source"].as_str().is_some_and(top_level_source)
        || payload["thread_source"].as_str().is_some_and(|value| value.to_ascii_lowercase().contains("subagent")) { return None; }
    let cwd = payload["cwd"].as_str().filter(|cwd| Path::new(cwd).is_absolute())?.to_string();
    let created_at = payload["timestamp"].as_str().or_else(|| metadata["timestamp"].as_str()).and_then(parse_timestamp).unwrap_or(modified);
    let mut title = None;
    let mut has_conversation = false;
    let mut updated_at = created_at;
    for record in head.iter().chain(tail) {
        if let Some(time) = record["timestamp"].as_str().and_then(parse_timestamp) { updated_at = updated_at.max(time); }
        let payload = &record["payload"];
        // Long context/tool records can put every user prompt between the
        // bounded windows. Real turn/assistant evidence still makes this a
        // resumable conversation; its name index can supply the title below.
        has_conversation |= (record["type"] == "response_item"
            && (payload["role"] == "assistant"
                || matches!(payload["type"].as_str(), Some("function_call" | "custom_tool_call" | "local_shell_call"))))
            || (record["type"] == "event_msg"
                && matches!(payload["type"].as_str(), Some("task_started" | "task_complete" | "agent_message")));
        if title.is_none() {
            title = if record["type"] == "event_msg" && payload["type"] == "user_message" {
                payload["message"].as_str().and_then(codex_prompt_title)
            } else if record["type"] == "response_item" && payload["role"] == "user" {
                user_text(&payload["content"]).as_deref().and_then(codex_prompt_title)
            } else { None };
        }
    }
    if title.is_none() && !has_conversation { return None; }
    Some(HistoryEntry { agent: "codex".into(), session_id: id.into(), title: title.unwrap_or_else(|| "Codex session".into()), cwd,
        created_at, updated_at: updated_at.max(modified) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const FIRST: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    const SECOND: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    const CHILD: &str = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
    const ARCHIVED: &str = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";

    fn database(path: &Path) -> Connection {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE threads (id TEXT PRIMARY KEY, cwd TEXT, title TEXT, name TEXT, created_at INTEGER, updated_at INTEGER, source TEXT, archived INTEGER, has_user_event INTEGER, thread_source TEXT);").unwrap();
        for (id, name, source, archived, time) in [
            (FIRST, Some("First named task"), "cli", 0, 1),
            (SECOND, None, "vscode", 0, 2),
            (CHILD, Some("Child task"), "{\"subagent\":\"review\"}", 0, 3),
            (ARCHIVED, Some("Archived task"), "cli", 1, 4),
        ] {
            conn.execute("INSERT INTO threads VALUES (?1, '/same/project', 'Original prompt', ?2, ?3, ?3, ?4, ?5, 1, NULL)", rusqlite::params![id, name, time, source, archived]).unwrap();
        }
        conn
    }

    #[test]
    fn sqlite_reads_wal_names_excludes_subagents_and_archives_and_finds_exact_old_ids() {
        let fixture = crate::state_store::tests::TempDir::new();
        let native_home = fixture.0.join("native-home");
        let sqlite_home = fixture.0.join("different-db-home");
        fs::create_dir_all(&native_home).unwrap();
        fs::create_dir_all(&sqlite_home).unwrap();
        let _old = database(&sqlite_home.join("state_9.sqlite"));
        let current = database(&sqlite_home.join("state_10.sqlite"));
        fs::write(native_home.join("session_index.jsonl"), format!("{}\n", json!({"id":SECOND,"thread_name":"Indexed second name"}))).unwrap();
        current.execute("UPDATE threads SET name = 'WAL updated name' WHERE id = ?1", [FIRST]).unwrap();
        let page = from_home(&native_home, &sqlite_home, None).unwrap();
        assert_eq!(page.sessions.len(), 2);
        assert_eq!(page.sessions[0].title, "Indexed second name");
        assert_eq!(page.sessions[1].title, "WAL updated name");
        let exact = from_home(&native_home, &sqlite_home, Some(FIRST)).unwrap();
        assert_eq!(exact.sessions.len(), 1);
        assert_eq!(exact.sessions[0].session_id, FIRST);
        assert!(from_home(&native_home, &sqlite_home, Some(ARCHIVED)).unwrap().sessions.is_empty());
    }

    #[test]
    fn sqlite_prompt_cleanup_and_authored_names_match_live_title_rules() {
        let fixture = crate::state_store::tests::TempDir::new();
        let current = database(&fixture.0.join("state_5.sqlite"));
        let prompt = "# Context from my IDE setup:\n## My request for Codex:\n<image name=example>attachment</image>Fix the <system-reminder>noise</system-reminder> parser";
        current.execute("UPDATE threads SET name = NULL, title = ?1 WHERE id = ?2", [prompt, FIRST]).unwrap();
        let name = "Keep   <image>literal</image> in my authored session name ".repeat(12).trim().to_string();
        assert!(name.chars().count() > 400);
        current.execute("UPDATE threads SET name = ?1 WHERE id = ?2", [&name, SECOND]).unwrap();
        let page = from_home(&fixture.0, &fixture.0, None).unwrap();
        assert_eq!(page.sessions.iter().find(|entry| entry.session_id == FIRST).unwrap().title, "Fix the parser");
        assert_eq!(page.sessions.iter().find(|entry| entry.session_id == SECOND).unwrap().title, name);

        fs::write(fixture.0.join("session_index.jsonl"), format!("{}\n", json!({"id":FIRST,"thread_name":"Indexed   personal task"}))).unwrap();
        let exact = from_home(&fixture.0, &fixture.0, Some(FIRST)).unwrap();
        assert_eq!(exact.sessions[0].title, "Indexed   personal task");
    }

    #[test]
    fn native_prompt_metadata_survives_unset_legacy_user_flags_without_listing_empty_prelaunches() {
        let fixture = crate::state_store::tests::TempDir::new();
        let current = database(&fixture.0.join("state_5.sqlite"));
        current.execute_batch("ALTER TABLE threads ADD COLUMN first_user_message TEXT NOT NULL DEFAULT ''; UPDATE threads SET has_user_event = 0;").unwrap();
        current.execute("UPDATE threads SET title = '', name = NULL, first_user_message = 'Actual saved user request' WHERE id = ?1", [FIRST]).unwrap();
        current.execute("INSERT INTO threads (id,cwd,title,created_at,updated_at,source,archived,has_user_event) VALUES ('eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee', '/same/project', '', 5, 5, 'cli', 0, 0)", []).unwrap();
        let page = from_home(&fixture.0, &fixture.0, None).unwrap();
        assert_eq!(page.sessions.len(), 2);
        assert_eq!(page.sessions.iter().find(|entry| entry.session_id == FIRST).unwrap().title, "Actual saved user request");
        assert_eq!(page.sessions.iter().find(|entry| entry.session_id == SECOND).unwrap().title, "Original prompt");
        assert_eq!(from_home(&fixture.0, &fixture.0, Some(FIRST)).unwrap().sessions.len(), 1);
    }

    fn rollout(base: &Path, id: &str, source: Value, prompt: &str) -> PathBuf {
        let folder = base.join("sessions/2026/09/20");
        fs::create_dir_all(&folder).unwrap();
        let file = folder.join(format!("rollout-time-{id}.jsonl"));
        let records = [
            json!({"type":"session_meta","timestamp":"2026-09-20T12:00:00Z","payload":{"id":id,"cwd":"/project","source":source}}),
            json!({"type":"response_item","payload":{"role":"user","content":[{"type":"input_text","text":"# AGENTS.md instructions"}]}}),
            json!({"type":"response_item","payload":{"role":"user","content":[{"type":"input_text","text":prompt}]}}),
        ];
        fs::write(&file, records.iter().map(Value::to_string).collect::<Vec<_>>().join("\n")).unwrap();
        file
    }

    #[test]
    fn legacy_rollouts_keep_same_cwd_sessions_separate_and_skip_context_and_children() {
        let fixture = crate::state_store::tests::TempDir::new();
        rollout(&fixture.0, FIRST, json!("cli"), "Fix the original problem");
        rollout(&fixture.0, SECOND, json!("vscode"), "## My request for Codex:\n Add dark mode");
        rollout(&fixture.0, CHILD, json!({"subagent":"review"}), "Hidden child");
        let page = from_home(&fixture.0, &fixture.0, None).unwrap();
        assert_eq!(page.sessions.len(), 2);
        assert_eq!(page.sessions.iter().find(|entry| entry.session_id == FIRST).unwrap().title, "Fix the original problem");
        assert_eq!(page.sessions.iter().find(|entry| entry.session_id == SECOND).unwrap().title, "Add dark mode");
        assert_eq!(from_home(&fixture.0, &fixture.0, Some(FIRST)).unwrap().sessions[0].session_id, FIRST);
        assert!(!fixture.0.join("state_5.sqlite").exists());
    }

    #[test]
    fn rollout_prompt_cleanup_ignores_image_blocks_and_ide_wrappers() {
        let fixture = crate::state_store::tests::TempDir::new();
        let first = rollout(&fixture.0, FIRST, json!("cli"), "Unused fixture prompt");
        let records = [
            json!({"type":"session_meta","payload":{"id":FIRST,"cwd":"/project","source":"cli"}}),
            json!({"type":"response_item","payload":{"role":"user","content":[
                {"type":"input_text","text":"<image name=example path=/tmp/example.png>"},
                {"type":"input_image","image_url":"unused"},
                {"type":"input_text","text":"</image>"},
                {"type":"input_text","text":"Fix the parser"},
            ]}}),
        ];
        fs::write(first, records.iter().map(Value::to_string).collect::<Vec<_>>().join("\n")).unwrap();
        rollout(&fixture.0, SECOND, json!("vscode"),
            "# Context from my IDE setup:\n## My request for Codex:\n<ide_opened>file.rs</ide_opened> Add dark mode");
        let page = from_home(&fixture.0, &fixture.0, None).unwrap();
        assert_eq!(page.sessions.iter().find(|entry| entry.session_id == FIRST).unwrap().title, "Fix the parser");
        assert_eq!(page.sessions.iter().find(|entry| entry.session_id == SECOND).unwrap().title, "Add dark mode");
    }

    #[test]
    fn broken_index_falls_back_with_a_warning_and_deleted_transcripts_disappear() {
        let fixture = crate::state_store::tests::TempDir::new();
        let file = rollout(&fixture.0, FIRST, json!("cli"), "Still available task");
        fs::write(fixture.0.join("state_5.sqlite"), "not a database").unwrap();
        let page = from_home(&fixture.0, &fixture.0, None).unwrap();
        assert_eq!(page.sessions.len(), 1);
        assert_eq!(page.warnings.len(), 1);
        fs::remove_file(file).unwrap();
        assert!(from_home(&fixture.0, &fixture.0, Some(FIRST)).unwrap().sessions.is_empty());
    }

    #[test]
    fn bounded_rollouts_keep_conversations_with_prompts_outside_windows_but_skip_metadata_only() {
        let fixture = crate::state_store::tests::TempDir::new();
        let file = rollout(&fixture.0, FIRST, json!("cli"), "Unused fixture prompt");
        let metadata = |id| json!({"type":"session_meta","timestamp":"2026-09-20T12:00:00Z","payload":{"id":id,"cwd":"/project","source":"cli"}});
        let records = [
            metadata(FIRST),
            json!({"type":"response_item","payload":{"role":"developer","content":"x".repeat(200_000)}}),
            json!({"type":"event_msg","payload":{"type":"user_message","message":"Prompt hidden between windows"}}),
            json!({"type":"response_item","payload":{"type":"function_call_output","output":"x".repeat(200_000)}}),
            json!({"type":"response_item","payload":{"role":"assistant","content":[{"type":"output_text","text":"Completed result"}]}}),
        ];
        fs::write(&file, records.iter().map(Value::to_string).collect::<Vec<_>>().join("\n")).unwrap();
        fs::write(file.parent().unwrap().join(format!("rollout-time-{SECOND}.jsonl")), metadata(SECOND).to_string()).unwrap();
        fs::write(fixture.0.join("session_index.jsonl"), format!("{}\n", json!({"id":FIRST,"thread_name":"Native name survives bounded parsing"}))).unwrap();
        let page = from_home(&fixture.0, &fixture.0, None).unwrap();
        assert_eq!(page.sessions.len(), 1);
        assert_eq!(page.sessions[0].title, "Native name survives bounded parsing");
        fs::remove_file(fixture.0.join("session_index.jsonl")).unwrap();
        let page = from_home(&fixture.0, &fixture.0, None).unwrap();
        assert_eq!(page.sessions.len(), 1);
        assert_eq!(page.sessions[0].title, "Codex session");
    }
}
