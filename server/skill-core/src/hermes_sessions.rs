//! Read-only Hermes session metadata (upstream `hermes_state_common.py` and
//! `hermes_cli/terminal_breadcrumbs.py`, verified against v2026.9.14).
//!
//! Hermes does not expose a forced-new-session ID flag. Its per-TTY breadcrumb
//! supplies the exact ID, including after /new or compression. Without that ID
//! we deliberately return no native title instead of guessing from cwd. Older
//! Hermes versions or disabled breadcrumbs still support VibeStudio's names.
//!
//! Upstream contracts:
//! <https://github.com/NousResearch/hermes-agent/blob/v2026.9.14/hermes_state_common.py>
//! <https://github.com/NousResearch/hermes-agent/blob/v2026.9.14/hermes_cli/terminal_breadcrumbs.py>
//! <https://github.com/NousResearch/hermes-agent/blob/v2026.9.14/hermes_cli/profiles.py>

use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

const MAX_PROFILES: usize = 64;

/// Effective home of a normal `hermes` invocation, including the sticky active
/// profile. An explicitly supplied fixture home never reads the user's env.
pub fn home_dir(home: &Path) -> PathBuf {
    let configured = (dirs::home_dir().as_deref() == Some(home))
        .then(|| std::env::var("HERMES_HOME").ok())
        .flatten();
    resolve_home(home, configured.as_deref())
}

fn resolve_home(home: &Path, configured: Option<&str>) -> PathBuf {
    #[cfg(windows)]
    let default = home.join("AppData/Local/hermes");
    #[cfg(not(windows))]
    let default = home.join(".hermes");
    let root = configured
        .filter(|s| !s.trim().is_empty())
        .map(|s| {
            let s = s.trim();
            if s == "~" {
                home.to_path_buf()
            } else if let Some(rest) = s.strip_prefix("~/") {
                home.join(rest)
            } else {
                PathBuf::from(s)
            }
        })
        .unwrap_or(default);
    if root
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|n| n == "profiles")
    {
        return root;
    }
    let Some(profile) = read_small(&root.join("active_profile"), 256) else {
        return root;
    };
    let profile = profile.trim().to_ascii_lowercase();
    if profile == "default" || !valid_profile_name(&profile) {
        return root;
    }
    let named = root.join("profiles").join(&profile);
    if named.is_dir()
        && [
            "config.yaml",
            ".env",
            "SOUL.md",
            "profile.yaml",
            "auth.json",
            "state.db",
        ]
        .iter()
        .any(|marker| named.join(marker).symlink_metadata().is_ok())
        && !root.join("profiles/.deleted").join(profile).exists()
    {
        named
    } else {
        root
    }
}

fn valid_profile_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.as_bytes()[0].is_ascii_alphanumeric()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

fn read_small(path: &Path, max: u64) -> Option<String> {
    let mut text = String::new();
    fs::File::open(path)
        .ok()?
        .take(max + 1)
        .read_to_string(&mut text)
        .ok()?;
    (text.len() as u64 <= max).then_some(text)
}

/// Include sibling profiles so switching the active profile does not rename an
/// existing terminal. Only exact IDs are queried; duplicate IDs are ambiguous.
fn homes_from(selected: &Path) -> Vec<PathBuf> {
    let root = if selected
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|n| n == "profiles")
    {
        selected.parent().and_then(Path::parent).unwrap_or(selected)
    } else {
        selected
    };
    let mut homes = vec![selected.to_path_buf(), root.to_path_buf()];
    if let Ok(entries) = fs::read_dir(root.join("profiles")) {
        for entry in entries.flatten().take(MAX_PROFILES) {
            let name = entry.file_name().to_string_lossy().into_owned();
            if valid_profile_name(&name)
                && !root.join("profiles/.deleted").join(&name).exists()
                && entry.path().is_dir()
            {
                homes.push(entry.path());
            }
        }
    }
    let mut seen = HashSet::new();
    homes.retain(|p| seen.insert(fs::canonicalize(p).unwrap_or_else(|_| p.clone())));
    homes
}

fn homes() -> Option<Vec<PathBuf>> {
    Some(homes_from(&home_dir(&dirs::home_dir()?)))
}

/// Read the same per-TTY identity Hermes uses for `-c`. The timestamp fence
/// rejects breadcrumbs left behind by an earlier user of a recycled TTY.
pub fn session_id_for_terminal(tty: &str, created_unix: i64) -> Option<String> {
    breadcrumb_from(&homes()?, tty, created_unix)
}

fn breadcrumb_from(homes: &[PathBuf], tty: &str, created_unix: i64) -> Option<String> {
    if !tty.starts_with("/dev/") || created_unix <= 0 {
        return None;
    }
    let sanitized: String = tty
        .trim()
        .trim_matches('/')
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .take(120)
        .collect();
    let filename = format!("tty-{sanitized}");
    let mut latest: Option<(f64, String)> = None;
    for home in homes {
        let Some(raw) = read_small(&home.join("terminal-sessions").join(&filename), 8192) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        let (Some(id), Some(ts)) = (value["session_id"].as_str(), value["ts"].as_f64()) else {
            continue;
        };
        if id.trim().is_empty() || id.len() > 256 || ts < created_unix as f64 || !ts.is_finite() {
            continue;
        }
        if latest.as_ref().is_none_or(|(old, _)| ts > *old) {
            latest = Some((ts, id.to_string()));
        } else if latest
            .as_ref()
            .is_some_and(|(old, old_id)| ts == *old && old_id != id)
        {
            // Equal-time conflicting identities are not sufficient correlation.
            return None;
        }
    }
    latest.map(|(_, id)| id)
}

pub fn title(_cwd: &Path, _created_unix: i64, session_id: Option<&str>) -> Option<String> {
    read_session(&homes()?, session_id?, false)
}

pub fn last_message(_cwd: &Path, _created_unix: i64, session_id: Option<&str>) -> Option<String> {
    read_session(&homes()?, session_id?, true)
}

fn open_db(path: &Path) -> Option<Connection> {
    let db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    db.busy_timeout(Duration::from_millis(30)).ok()?;
    Some(db)
}

fn read_session(homes: &[PathBuf], id: &str, assistant: bool) -> Option<String> {
    if id.is_empty() || id.len() > 256 {
        return None;
    }
    let mut found = None;
    for home in homes {
        let Some(db) = open_db(&home.join("state.db")) else {
            continue;
        };
        let Ok(Some(native_title)) = db
            .query_row("SELECT title FROM sessions WHERE id = ?1", [id], |r| {
                r.get::<_, Option<String>>(0)
            })
            .optional()
        else {
            continue;
        };
        if found.is_some() {
            return None;
        }
        let text = if assistant {
            message(&db, id, "assistant", 180)
        } else {
            native_title
                .and_then(|s| nonempty(&s))
                .or_else(|| message(&db, id, "user", 72))
        };
        // Distinguish an existing unnamed session from an absent session.
        found = Some(text);
    }
    found.flatten()
}

fn nonempty(text: &str) -> Option<String> {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!text.is_empty()).then_some(text)
}

fn message(db: &Connection, id: &str, role: &str, max_chars: usize) -> Option<String> {
    let columns: HashSet<String> = db
        .prepare("PRAGMA table_info(messages)")
        .ok()?
        .query_map([], |r| r.get(1))
        .ok()?
        .filter_map(Result::ok)
        .collect();
    let mut filter = String::new();
    for (column, predicate) in [
        (
            "display_kind",
            " AND COALESCE(display_kind, '') <> 'hidden'",
        ),
        ("active", " AND COALESCE(active, 1) <> 0"),
        (
            "_compressed_summary",
            " AND COALESCE(_compressed_summary, 0) = 0",
        ),
    ] {
        if columns.contains(column) {
            filter.push_str(predicate);
        }
    }
    let order = if role == "assistant" { "DESC" } else { "ASC" };
    let sql = format!("SELECT substr(content, 1, 8192) FROM messages WHERE session_id = ?1 AND role = ?2 AND content IS NOT NULL{filter} ORDER BY timestamp {order}, id {order} LIMIT 12");
    let mut statement = db.prepare(&sql).ok()?;
    let rows = statement
        .query_map([id, role], |r| r.get::<_, String>(0))
        .ok()?;
    for raw in rows.flatten() {
        let text = match serde_json::from_str::<Value>(&raw) {
            Ok(Value::Array(blocks)) => blocks
                .iter()
                .filter_map(|b| (b["type"] == "text").then(|| b["text"].as_str()).flatten())
                .collect::<Vec<_>>()
                .join(" "),
            Ok(Value::String(text)) => text,
            _ => raw,
        };
        let Some(text) = nonempty(&text) else {
            continue;
        };
        if text.chars().count() <= max_chars {
            return Some(text);
        }
        return Some(format!(
            "{}…",
            text.chars().take(max_chars).collect::<String>().trim_end()
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state_store::tests::TempDir;

    fn database(home: &Path) -> Connection {
        fs::create_dir_all(home).unwrap();
        let db = Connection::open(home.join("state.db")).unwrap();
        db.execute_batch(
            "PRAGMA journal_mode=WAL;
            CREATE TABLE sessions(id TEXT PRIMARY KEY, title TEXT, cwd TEXT);
            CREATE TABLE messages(id INTEGER PRIMARY KEY, session_id TEXT, role TEXT,
                content TEXT, timestamp REAL, active INTEGER DEFAULT 1,
                display_kind TEXT, _compressed_summary INTEGER DEFAULT 0);",
        )
        .unwrap();
        db
    }

    #[test]
    fn profiles_follow_sticky_selection_and_explicit_profile_wins() {
        let dir = TempDir::new();
        let root = dir.0.join("custom");
        let named = root.join("profiles/work");
        fs::create_dir_all(&named).unwrap();
        fs::write(named.join("config.yaml"), "").unwrap();
        fs::write(root.join("active_profile"), "work\n").unwrap();
        assert_eq!(resolve_home(&dir.0, root.to_str()), named);
        fs::write(root.join("active_profile"), "another").unwrap();
        assert_eq!(resolve_home(&dir.0, named.to_str()), named);
        fs::write(root.join("active_profile"), "../../outside").unwrap();
        assert_eq!(resolve_home(&dir.0, root.to_str()), root);
        fs::write(root.join("active_profile"), "work").unwrap();
        fs::create_dir_all(root.join("profiles/.deleted")).unwrap();
        fs::write(root.join("profiles/.deleted/work"), "").unwrap();
        assert_eq!(resolve_home(&dir.0, root.to_str()), root);
    }

    #[test]
    fn exact_ids_never_borrow_another_session_and_wal_renames_refresh() {
        let dir = TempDir::new();
        let db = database(&dir.0);
        db.execute(
            "INSERT INTO sessions VALUES('a','First title','/same'),('b','Wrong title','/same')",
            [],
        )
        .unwrap();
        let homes = vec![dir.0.clone()];
        assert_eq!(read_session(&homes, "missing", false), None);
        assert_eq!(read_session(&homes, "", false), None);
        assert_eq!(
            read_session(&homes, "a", false).as_deref(),
            Some("First title")
        );
        let full = "A deliberately long user chosen title whose complete text should survive beyond seventy two characters";
        db.execute("UPDATE sessions SET title=?1 WHERE id='a'", [full])
            .unwrap();
        assert_eq!(read_session(&homes, "a", false).as_deref(), Some(full));
        let profile = dir.0.join("profiles/work");
        let other = database(&profile);
        other
            .execute(
                "INSERT INTO sessions VALUES('a','Duplicate identity','/same')",
                [],
            )
            .unwrap();
        assert_eq!(read_session(&homes_from(&profile), "a", false), None);
    }

    #[test]
    fn messages_ignore_hidden_inactive_and_summary_records() {
        let dir = TempDir::new();
        let db = database(&dir.0);
        db.execute("INSERT INTO sessions VALUES('a',NULL,'/project')", [])
            .unwrap();
        db.execute_batch("INSERT INTO messages(id,session_id,role,content,timestamp,active,display_kind,_compressed_summary) VALUES
            (1,'a','user','Internal scaffolding',1,1,'hidden',0),
            (2,'a','user','Fix the connection retry',2,1,NULL,0),
            (3,'a','assistant','[{\"type\":\"text\",\"text\":\"The retry now works.\"},{\"type\":\"image\",\"data\":\"private\"}]',3,1,NULL,0),
            (4,'a','assistant','Old branch',4,0,NULL,0),
            (5,'a','assistant','Internal summary',5,1,NULL,1),
            (6,'a','assistant','Hidden data',6,1,'hidden',0);").unwrap();
        let homes = vec![dir.0.clone()];
        assert_eq!(
            read_session(&homes, "a", false).as_deref(),
            Some("Fix the connection retry")
        );
        assert_eq!(
            read_session(&homes, "a", true).as_deref(),
            Some("The retry now works.")
        );
    }

    #[test]
    fn breadcrumb_rejects_recycled_tty_and_follows_profile_switches() {
        let dir = TempDir::new();
        let profile = dir.0.join("profiles/work");
        for root in [&dir.0, &profile] {
            fs::create_dir_all(root.join("terminal-sessions")).unwrap();
        }
        let filename = "terminal-sessions/tty-dev-pts-7";
        fs::write(dir.0.join(filename), r#"{"session_id":"old","ts":99.5}"#).unwrap();
        let homes = homes_from(&dir.0);
        assert_eq!(breadcrumb_from(&homes, "/dev/pts/7", 100), None);
        fs::write(profile.join(filename), r#"{"session_id":"new","ts":101.2}"#).unwrap();
        assert_eq!(
            breadcrumb_from(&homes, "/dev/pts/7", 100).as_deref(),
            Some("new")
        );
        fs::write(
            profile.join(filename),
            r#"{"session_id":"after-new","ts":105.1}"#,
        )
        .unwrap();
        assert_eq!(
            breadcrumb_from(&homes, "/dev/pts/7", 100).as_deref(),
            Some("after-new")
        );
        assert_eq!(breadcrumb_from(&homes, "../pts/7", 100), None);
    }

    #[test]
    fn absent_stores_are_not_created() {
        let dir = TempDir::new();
        assert_eq!(
            read_session(std::slice::from_ref(&dir.0), "absent", false),
            None
        );
        assert!(!dir.0.join("state.db").exists());
    }
}
