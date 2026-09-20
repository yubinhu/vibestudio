//! Codex's generated/renamed thread name is separate from its first-prompt
//! `title`. Read names without starting Codex or making another model request.

use super::*;

pub fn codex_title(_cwd: &Path, _created: i64, session_id: Option<&str>) -> Option<String> {
    // skill-term resolves the actual process's rollout UUID. Without it, even a
    // single recent conversation in this cwd might belong to another terminal.
    let id = session_id.filter(|s| !s.is_empty())?;
    let base = std::env::var_os("CODEX_HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| home().map(|h| h.join(".codex")))?;
    let db_dir = (|| {
        let config: toml::Value = fs::read_to_string(base.join("config.toml"))
            .ok()?
            .parse()
            .ok()?;
        let path = PathBuf::from(config.get("sqlite_home")?.as_str()?);
        Some(if path.is_absolute() {
            path
        } else {
            base.join(path)
        })
    })()
    .or_else(|| {
        std::env::var_os("CODEX_SQLITE_HOME")
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
    })
    .unwrap_or_else(|| base.clone());
    from_home(&base, &db_dir, id)
}

struct Thread {
    name: Option<String>,
    prompt: String,
    rollout: PathBuf,
}

fn from_home(base: &Path, db_dir: &Path, id: &str) -> Option<String> {
    let thread = state_db(db_dir).and_then(|db| query_thread(&db, id));
    // `name` is the current generated/user-renamed title. Older Codex versions
    // only keep it in the append-only name index. `title` is a prompt fallback.
    if let Some(name) = thread
        .as_ref()
        .and_then(|t| t.name.as_deref())
        .and_then(display_name)
    {
        return Some(name);
    }
    if let Some(name) = index_name(&base.join("session_index.jsonl"), id) {
        return Some(name);
    }
    if let Some(thread) = thread {
        if let Some(prompt) = clean_prompt(&thread.prompt) {
            return Some(truncate(&prompt));
        }
        return cached(&thread.rollout, parse_codex);
    }

    // Pre-SQLite installs or a temporarily unavailable/moved DB. An exact id
    // must never fall back to another conversation, even in the same directory.
    cached(&rollout(base, id)?, parse_codex)
}

fn display_name(raw: &str) -> Option<String> {
    // Saved names may be user-authored. Preserve the full value for editing;
    // the UI truncates visually, while prompt fallbacks keep their short budget.
    let name = raw.trim();
    (!name.is_empty()).then(|| name.to_string())
}

fn state_db(dir: &Path) -> Option<PathBuf> {
    fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let version = path
                .file_name()?
                .to_str()?
                .strip_prefix("state_")?
                .strip_suffix(".sqlite")?
                .parse::<u32>()
                .ok()?;
            path.is_file().then_some((version, path))
        })
        .max_by_key(|(version, _)| *version)
        .map(|(_, path)| path)
}

fn query_thread(db: &Path, id: &str) -> Option<Thread> {
    // Read committed WAL frames too; immutable=1 would hide newly generated names.
    let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    conn.busy_timeout(Duration::from_millis(50)).ok()?;
    let mut columns = conn.prepare("PRAGMA table_info(threads)").ok()?;
    let has_name = columns
        .query_map([], |r| r.get::<_, String>(1))
        .ok()?
        .flatten()
        .any(|column| column == "name");
    let name = if has_name { "name" } else { "NULL" };
    conn.query_row(
        &format!("SELECT {name}, title, rollout_path FROM threads WHERE id = ?1"),
        [id],
        |row| {
            Ok(Thread {
                name: row.get(0)?,
                prompt: row.get(1)?,
                rollout: PathBuf::from(row.get::<_, String>(2)?),
            })
        },
    )
    .ok()
}

// The index is shared by every terminal. Parse it once per write, with both
// nanosecond mtime and size so a rename within the same second is visible.
type IndexCache = HashMap<PathBuf, ((std::time::SystemTime, u64), HashMap<String, String>)>;
static INDEX_CACHE: LazyLock<Mutex<IndexCache>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn index_name(file: &Path, id: &str) -> Option<String> {
    let metadata = fs::metadata(file).ok()?;
    let signature = (metadata.modified().ok()?, metadata.len());
    let mut cache = INDEX_CACHE.lock().ok()?;
    if let Some((sig, names)) = cache.get(file) {
        if *sig == signature {
            return names.get(id).and_then(|name| display_name(name));
        }
    }
    let mut names = HashMap::new();
    for line in read_lines(file)? {
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let (Some(id), Some(name)) = (value["id"].as_str(), value["thread_name"].as_str()) {
            names.insert(id.to_string(), name.to_string()); // last appended rename wins
        }
    }
    let result = names.get(id).and_then(|name| display_name(name));
    cache.insert(file.to_path_buf(), (signature, names));
    result
}

fn rollout(base: &Path, id: &str) -> Option<PathBuf> {
    let suffix = format!("-{id}.jsonl");
    for dir in ["sessions", "archived_sessions"] {
        for entry in walkdir::WalkDir::new(base.join(dir))
            .max_depth(5)
            .into_iter()
            .filter_map(Result::ok)
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let file = entry.path();
            let name = entry.file_name().to_string_lossy();
            if !name.starts_with("rollout-") || !name.ends_with(&suffix) {
                continue;
            }
            let Some(first) = read_lines(file).and_then(|mut lines| lines.next()) else {
                continue;
            };
            let Ok(meta) = serde_json::from_str::<Value>(&first) else {
                continue;
            };
            if meta["type"] != "session_meta" {
                continue;
            }
            if meta["payload"]["id"].as_str() == Some(id) {
                return Some(file.to_path_buf());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "codex-titles-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn db(&self, names: bool) -> Connection {
            let conn = Connection::open(self.0.join("state_5.sqlite")).unwrap();
            conn.execute_batch("PRAGMA journal_mode=WAL;
                CREATE TABLE threads (id TEXT PRIMARY KEY, title TEXT, rollout_path TEXT);
                INSERT INTO threads VALUES ('alpha', 'Please fix the alpha parser for me', 'missing-alpha');
                INSERT INTO threads VALUES ('beta', 'Help investigate the other task', 'missing-beta');").unwrap();
            if names {
                conn.execute_batch(
                    "ALTER TABLE threads ADD COLUMN name TEXT;
                    UPDATE threads SET name = 'Fix alpha parser' WHERE id = 'alpha';
                    UPDATE threads SET name = 'Investigate beta task' WHERE id = 'beta';",
                )
                .unwrap();
            }
            conn
        }
        fn title(&self, id: &str) -> Option<String> {
            from_home(&self.0, &self.0, id)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn generated_names_are_exact_and_follow_wal_renames() {
        let f = Fixture::new();
        let conn = f.db(true);
        fs::write(
            f.0.join("session_index.jsonl"),
            "{\"id\":\"alpha\",\"thread_name\":\"Outdated indexed name\"}\n",
        )
        .unwrap();
        assert_eq!(f.title("alpha").as_deref(), Some("Fix alpha parser"));
        assert_eq!(f.title("beta").as_deref(), Some("Investigate beta task"));
        assert_eq!(f.title("unknown"), None);
        // Keep the writer open: the rename is in the WAL, not checkpointed.
        conn.execute(
            "UPDATE threads SET name = ?1 WHERE id = 'alpha'",
            ["Repair renamed parser"],
        )
        .unwrap();
        assert_eq!(f.title("alpha").as_deref(), Some("Repair renamed parser"));
        let long_name = "Review saved session title ".repeat(6).trim().to_string();
        conn.execute("UPDATE threads SET name = ?1 WHERE id = 'alpha'", [&long_name]).unwrap();
        assert_eq!(f.title("alpha"), Some(long_name));
        assert_eq!(codex_title(Path::new("/same/project"), 0, None), None);
    }

    #[test]
    fn legacy_db_uses_index_then_prompt_and_refreshes_appended_names() {
        let f = Fixture::new();
        let _conn = f.db(false);
        let index = f.0.join("session_index.jsonl");
        fs::write(
            &index,
            concat!(
                "{\"id\":\"alpha\",\"thread_name\":\"First name\"}\n",
                "broken record\n",
                "{\"id\":\"alpha\",\"thread_name\":\"  Fix   alpha parser  \"}\n"
            ),
        )
        .unwrap();
        assert_eq!(f.title("alpha").as_deref(), Some("Fix   alpha parser"));
        let mut file = fs::OpenOptions::new().append(true).open(&index).unwrap();
        writeln!(
            file,
            "{{\"id\":\"alpha\",\"thread_name\":\"Rename alpha task\"}}"
        )
        .unwrap();
        assert_eq!(f.title("alpha").as_deref(), Some("Rename alpha task"));
        assert_eq!(
            f.title("beta").as_deref(),
            Some("Help investigate the other task")
        );
        drop(_conn);
        fs::remove_file(f.0.join("state_5.sqlite")).unwrap();
        assert_eq!(f.title("alpha").as_deref(), Some("Rename alpha task"));
    }

    #[test]
    fn transcript_fallback_reads_paginated_user_content_and_legacy_events() {
        let f = Fixture::new();
        let dir = f.0.join("sessions/2026/09/19");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("rollout-time-alpha.jsonl"), concat!(
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"alpha\",\"cwd\":\"/same/project\"}}\n",
            "{\"type\":\"response_item\",\"payload\":{\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"# AGENTS.md instructions\"}]}}\n",
            "{\"type\":\"response_item\",\"payload\":{\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"<environment_context>setup</environment_context>\"}]}}\n",
            "{\"type\":\"response_item\",\"payload\":{\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"<image name=example path=/tmp/example.png>\"},{\"type\":\"input_image\",\"image_url\":\"unused\"},{\"type\":\"input_text\",\"text\":\"</image>\"},{\"type\":\"input_text\",\"text\":\"Fix the parser\"}]}}\n"
        )).unwrap();
        fs::write(dir.join("rollout-time-beta.jsonl"), concat!(
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"beta\",\"cwd\":\"/same/project\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"## Open tabs: context\\n## My request for Codex:\\n Add dark mode\"}}\n"
        )).unwrap();
        assert_eq!(f.title("alpha").as_deref(), Some("Fix the parser"));
        assert_eq!(f.title("beta").as_deref(), Some("Add dark mode"));
        assert_eq!(f.title("unknown"), None);
        assert!(
            !f.0.join("state_5.sqlite").exists(),
            "readers never create Codex stores"
        );
    }

    #[test]
    fn state_database_versions_sort_numerically() {
        let f = Fixture::new();
        for name in [
            "state_9.sqlite",
            "state_10.sqlite",
            "state_10.sqlite-wal",
            "logs_100.sqlite",
        ] {
            fs::write(f.0.join(name), "").unwrap();
        }
        assert_eq!(state_db(&f.0), Some(f.0.join("state_10.sqlite")));
    }
}
