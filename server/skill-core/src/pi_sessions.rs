//! Read Pi's native JSONL sessions without modifying its store.
//!
//! Contract verified against @earendil-works/pi-coding-agent 0.86.1:
//! https://github.com/earendil-works/pi/blob/main/packages/coding-agent/src/core/session-manager.ts
//! Pi's `--session-id` gives normal launches an exact identity. Without a known
//! ID or explicit session file, return no native metadata: matching cwd and
//! creation time cannot prove that a neighboring terminal owns the session.

use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::SystemTime;

/// Metadata marker for launches whose persistence/root cannot be correlated.
/// A colon is forbidden in Pi session IDs, so it cannot name a real session.
pub const UNTRACKED_SESSION_ID: &str = "vibestudio:untracked";

const MAX_SESSION_BYTES: u64 = 64 * 1024 * 1024;
const MAX_HEADER_BYTES: u64 = 64 * 1024;
const MAX_CANDIDATES: usize = 8192;

#[derive(Clone)]
struct Header {
    id: String,
    cwd: PathBuf,
}

#[derive(Clone, Default)]
struct Display {
    title: Option<String>,
    last_message: Option<String>,
}

type FileStamp = (Option<SystemTime>, u64);
type DisplayCache = HashMap<PathBuf, (FileStamp, Option<Display>)>;
static CACHE: LazyLock<Mutex<DisplayCache>> = LazyLock::new(|| Mutex::new(HashMap::new()));
type HeaderCache = HashMap<PathBuf, (FileStamp, Option<Header>)>;
static HEADERS: LazyLock<Mutex<HeaderCache>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Pi's global configuration/skills directory, including its supported override.
pub fn agent_dir(home: &Path) -> PathBuf {
    (dirs::home_dir().as_deref() == Some(home))
        .then(|| std::env::var_os("PI_CODING_AGENT_DIR"))
        .flatten()
        .filter(|p| !p.is_empty())
        .map(|p| expand_home(Path::new(&p), home))
        .unwrap_or_else(|| home.join(".pi/agent"))
}

pub fn title(cwd: &Path, created_unix: i64, session_id: Option<&str>) -> Option<String> {
    display(cwd, created_unix, session_id)?.title
}

pub fn last_message(cwd: &Path, created_unix: i64, session_id: Option<&str>) -> Option<String> {
    display(cwd, created_unix, session_id)?.last_message
}

fn display(cwd: &Path, created: i64, id: Option<&str>) -> Option<Display> {
    let id = exact_id(id)?;
    let home = dirs::home_dir()?;
    let dir = session_dir(cwd, &home, &agent_dir(&home));
    from_dir(&dir, cwd, created, Some(id))
}

fn exact_id(id: Option<&str>) -> Option<&str> {
    id.filter(|id| !id.is_empty() && *id != UNTRACKED_SESSION_ID)
}

fn expand_home(path: &Path, home: &Path) -> PathBuf {
    match path.strip_prefix("~") {
        Ok(rest) => home.join(rest),
        Err(_) => path.to_path_buf(),
    }
}

fn absolute(path: &Path, cwd: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

fn session_dir(cwd: &Path, home: &Path, agent: &Path) -> PathBuf {
    let env_dir = std::env::var_os("PI_CODING_AGENT_SESSION_DIR")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from);
    configured_session_dir(cwd, home, agent, env_dir.as_deref())
}

fn configured_session_dir(
    cwd: &Path,
    home: &Path,
    agent: &Path,
    env_dir: Option<&Path>,
) -> PathBuf {
    let override_dir = env_dir.map(Path::to_path_buf).or_else(|| {
        settings_session_dir(&cwd.join(".pi/settings.json"))
            .or_else(|| settings_session_dir(&agent.join("settings.json")))
            .flatten()
    });
    if let Some(path) = override_dir {
        return absolute(&expand_home(&path, home), cwd);
    }
    default_session_dir(cwd, agent)
}

fn settings_session_dir(file: &Path) -> Option<Option<PathBuf>> {
    let file = File::open(file).ok()?;
    let value: Value = serde_json::from_reader(file.take(MAX_HEADER_BYTES)).ok()?;
    // An explicitly empty project value clears the global override in Pi's
    // settings merge; a missing field inherits it.
    value
        .get("sessionDir")
        .map(|value| value.as_str().filter(|p| !p.is_empty()).map(PathBuf::from))
}

fn default_session_dir(cwd: &Path, agent: &Path) -> PathBuf {
    // Match Pi's path encoding, including Windows drive separators. The header
    // remains authoritative because this encoding can collide (a/b and a-b).
    let text = cwd.to_string_lossy();
    let stripped = text.strip_prefix(['/', '\\']).unwrap_or(&text);
    let encoded = stripped.replace(['/', '\\', ':'], "-");
    agent.join("sessions").join(format!("--{encoded}--"))
}

fn same_cwd(a: &Path, b: &Path) -> bool {
    a == b || matches!((fs::canonicalize(a), fs::canonicalize(b)), (Ok(a), Ok(b)) if a == b)
}

fn from_dir(dir: &Path, cwd: &Path, _created: i64, id: Option<&str>) -> Option<Display> {
    let id = exact_id(id)?;
    // A recorded explicit --session file can be read directly. Other IDs are
    // compared as data; never concatenate an untrusted ID into a native path.
    let path = Path::new(id);
    if path.is_absolute() {
        let header = read_header(path)?;
        return same_cwd(&header.cwd, cwd)
            .then(|| cached_display(path))
            .flatten();
    }
    let mut candidate = None;
    for (count, entry) in fs::read_dir(dir).ok()?.enumerate() {
        if count >= MAX_CANDIDATES {
            return None;
        }
        let path = entry.ok()?.path();
        if path.extension().and_then(|v| v.to_str()) != Some("jsonl") {
            continue;
        }
        let Some(header) = read_header(&path) else {
            continue;
        };
        if !same_cwd(&header.cwd, cwd) {
            continue;
        }
        if header.id == id {
            if candidate.is_some() {
                // Duplicate IDs cannot be disambiguated.
                return None;
            }
            candidate = Some(path);
        }
    }
    cached_display(&candidate?)
}

fn read_header(path: &Path) -> Option<Header> {
    let metadata = fs::metadata(path).ok()?;
    if !metadata.is_file() {
        return None;
    }
    let stamp = (metadata.modified().ok(), metadata.len());
    if let Ok(cache) = HEADERS.lock() {
        if let Some((old_stamp, header)) = cache.get(path) {
            if *old_stamp == stamp {
                return header.clone();
            }
        }
    }
    let header = parse_header(path);
    if let Ok(mut cache) = HEADERS.lock() {
        if cache.len() >= MAX_CANDIDATES {
            cache.clear();
        }
        cache.insert(path.to_path_buf(), (stamp, header.clone()));
    }
    header
}

fn parse_header(path: &Path) -> Option<Header> {
    let file = File::open(path).ok()?;
    let mut line = String::new();
    BufReader::new(file.take(MAX_HEADER_BYTES))
        .read_line(&mut line)
        .ok()?;
    let value: Value = serde_json::from_str(&line).ok()?;
    if value.get("type")?.as_str()? != "session" {
        return None;
    }
    Some(Header {
        id: value.get("id")?.as_str()?.to_owned(),
        cwd: PathBuf::from(value.get("cwd")?.as_str()?),
    })
}

fn cached_display(path: &Path) -> Option<Display> {
    let metadata = fs::metadata(path).ok()?;
    if metadata.len() > MAX_SESSION_BYTES {
        return None;
    }
    let stamp = (metadata.modified().ok(), metadata.len());
    if let Ok(cache) = CACHE.lock() {
        if let Some((old_stamp, result)) = cache.get(path) {
            if *old_stamp == stamp {
                return result.clone();
            }
        }
    }
    let display = parse_display(path);
    if let Ok(mut cache) = CACHE.lock() {
        if cache.len() >= 256 {
            cache.clear();
        }
        cache.insert(path.to_path_buf(), (stamp, display.clone()));
    }
    display
}

struct Entry {
    parent: Option<String>,
    assistant: Option<String>,
}

fn parse_display(path: &Path) -> Option<Display> {
    let file = File::open(path).ok()?;
    let mut named_title = None;
    let mut first_prompt = None;
    let mut entries = HashMap::new();
    let mut leaf = None;
    let mut legacy_last = None;
    let mut legacy = false;
    for line in BufReader::new(file.take(MAX_SESSION_BYTES)).lines() {
        let line = line.ok()?;
        // A writer can leave a partial final record; ignore malformed lines.
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let kind = value.get("type").and_then(Value::as_str);
        if kind == Some("session") {
            legacy = value.get("version").and_then(Value::as_u64).unwrap_or(1) < 2;
            continue;
        }
        if kind == Some("session_info") {
            // Latest empty name clears an earlier native title.
            named_title = value.get("name").and_then(Value::as_str).and_then(tidy);
        }
        let mut assistant = None;
        if kind == Some("message") {
            if let Some(message) = value.get("message") {
                match message.get("role").and_then(Value::as_str) {
                    Some("user") if first_prompt.is_none() => {
                        first_prompt =
                            content_text(message.get("content")).map(|s| shorten(&s, 72));
                    }
                    Some("assistant") => {
                        assistant = content_text(message.get("content")).map(|s| shorten(&s, 180));
                        // In an older linear session, keep the latest actual text.
                        if assistant.is_some() {
                            legacy_last.clone_from(&assistant);
                        }
                    }
                    _ => {}
                }
            }
        }
        if let Some(id) = value.get("id").and_then(Value::as_str) {
            leaf = Some(id.to_owned());
            entries.insert(
                id.to_owned(),
                Entry {
                    parent: value
                        .get("parentId")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    assistant,
                },
            );
        }
    }
    let last_message = if legacy {
        legacy_last
    } else {
        // Pi sessions branch in place. Follow the persisted active branch rather
        // than showing the latest physical assistant row from an abandoned fork.
        let mut current = leaf;
        let mut visited = HashSet::new();
        let mut last = None;
        while let Some(id) = current {
            if !visited.insert(id.clone()) {
                break;
            }
            let Some(entry) = entries.get(&id) else { break };
            if entry.assistant.is_some() {
                last.clone_from(&entry.assistant);
                break;
            }
            current = entry.parent.clone();
        }
        last
    };
    Some(Display {
        title: named_title.or(first_prompt),
        last_message,
    })
}

fn content_text(content: Option<&Value>) -> Option<String> {
    let content = content?;
    if let Some(text) = content.as_str() {
        return tidy(text);
    }
    let text = content
        .as_array()?
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join(" ");
    tidy(&text)
}

fn tidy(text: &str) -> Option<String> {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!text.is_empty()).then_some(text)
}

fn shorten(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let prefix: String = text.chars().take(max).collect();
    let cut = prefix
        .rsplit_once(' ')
        .map_or(prefix.as_str(), |(head, _)| head);
    format!("{cut}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "vibestudio-pi-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn write(
            &self,
            filename: &str,
            id: &str,
            cwd: &str,
            timestamp: &str,
            rows: &[Value],
        ) -> PathBuf {
            let path = self.0.join(filename);
            let mut file = File::create(&path).unwrap();
            writeln!(
                file,
                "{}",
                json!({"type":"session","version":3,"id":id,"cwd":cwd,"timestamp":timestamp})
            )
            .unwrap();
            for row in rows {
                writeln!(file, "{row}").unwrap();
            }
            path
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn message(id: &str, parent: Option<&str>, role: &str, text: &str) -> Value {
        json!({"type":"message","id":id,"parentId":parent,"message":{"role":role,"content":[{"type":"text","text":text}]}})
    }

    #[test]
    fn untracked_launches_never_read_a_native_store() {
        for id in [None, Some(""), Some(UNTRACKED_SESSION_ID)] {
            assert!(title(Path::new("/work"), 1789891200, id).is_none());
            assert!(last_message(Path::new("/work"), 1789891200, id).is_none());
        }
    }

    #[test]
    fn exact_identity_and_full_native_title_win_over_newer_neighbour() {
        let f = Fixture::new();
        let title = "A deliberately long native name that keeps all of the words the user supplied instead of truncating at seventy two characters";
        f.write(
            "old.jsonl",
            "wanted",
            "/work",
            "2026-09-20T08:00:00.000Z",
            &[
                message("u", None, "user", "Initial prompt"),
                json!({"type":"session_info","id":"n","parentId":"u","name":title}),
            ],
        );
        f.write(
            "new.jsonl",
            "other",
            "/work",
            "2026-09-20T08:01:00.000Z",
            &[json!({"type":"session_info","name":"Do not borrow this title"})],
        );
        assert_eq!(
            from_dir(&f.0, Path::new("/work"), 0, Some("wanted"))
                .unwrap()
                .title
                .as_deref(),
            Some(title)
        );
        assert!(from_dir(&f.0, Path::new("/work"), 0, Some("missing")).is_none());
        assert!(from_dir(&f.0, Path::new("/other"), 0, Some("wanted")).is_none());
    }

    #[test]
    fn missing_identity_never_borrows_a_unique_neighbor_regardless_of_launch_time() {
        let f = Fixture::new();
        let path = f.write(
            "neighbor.jsonl",
            "theirs",
            "/work",
            "2026-09-20T08:00:00.000Z",
            &[
                message("u", None, "user", "Neighbor's task"),
                message("a", Some("u"), "assistant", "Neighbor's answer"),
            ],
        );
        let native = cached_display(&path).unwrap();
        assert_eq!(native.title.as_deref(), Some("Neighbor's task"));
        assert_eq!(native.last_message.as_deref(), Some("Neighbor's answer"));
        // The only file may belong to a neighbor while our launch is still in
        // setup. Even an identical creation second cannot establish ownership.
        for created in [0, 1789891199, 1789891200, 1789891201, 1789891290] {
            for id in [None, Some(""), Some(UNTRACKED_SESSION_ID)] {
                assert!(from_dir(&f.0, Path::new("/work"), created, id).is_none());
            }
        }
    }

    #[test]
    fn latest_name_clear_and_same_second_append_invalidate_cache() {
        let f = Fixture::new();
        let path = f.write(
            "test.jsonl",
            "id",
            "/work",
            "2026-09-20T08:00:00Z",
            &[
                message("u", None, "user", "First actual prompt"),
                json!({"type":"session_info","id":"n","parentId":"u","name":"First name"}),
            ],
        );
        assert_eq!(
            cached_display(&path).unwrap().title.as_deref(),
            Some("First name")
        );
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(
            file,
            "{}",
            json!({"type":"session_info","id":"clear","parentId":"n","name":""})
        )
        .unwrap();
        assert_eq!(
            cached_display(&path).unwrap().title.as_deref(),
            Some("First actual prompt")
        );
        writeln!(
            file,
            "{}",
            json!({"type":"session_info","id":"again","parentId":"clear","name":"Latest name"})
        )
        .unwrap();
        assert_eq!(
            cached_display(&path).unwrap().title.as_deref(),
            Some("Latest name")
        );
    }

    #[test]
    fn notification_follows_active_branch_and_ignores_thinking_tools_and_partial_tail() {
        let f = Fixture::new();
        let path = f.write("branch.jsonl", "id", "/work", "2026-09-20T08:00:00Z", &[
            message("u", None, "user", "Hello"),
            message("a", Some("u"), "assistant", "Current ancestor answer"),
            message("fork", Some("a"), "assistant", "Abandoned branch"),
            json!({"type":"message","id":"b","parentId":"a","message":{"role":"assistant","content":[{"type":"thinking","thinking":"Private reasoning"},{"type":"toolCall","name":"bash"}]}}),
            message("tool", Some("b"), "toolResult", "Tool output is not the answer"),
        ]);
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        write!(file, "{{\"type\":\"message\"").unwrap();
        assert_eq!(
            cached_display(&path).unwrap().last_message.as_deref(),
            Some("Current ancestor answer")
        );
    }

    #[test]
    fn duplicate_ids_are_ambiguous_and_explicit_file_is_exact() {
        let f = Fixture::new();
        let path = f.write(
            "one.jsonl",
            "same",
            "/work",
            "2026-09-20T08:00:00Z",
            &[message("u", None, "user", "Exact file")],
        );
        f.write("two.jsonl", "same", "/work", "2026-09-20T08:00:00Z", &[]);
        assert!(from_dir(&f.0, Path::new("/work"), 0, Some("same")).is_none());
        assert_eq!(
            from_dir(&f.0, Path::new("/work"), 0, path.to_str())
                .unwrap()
                .title
                .as_deref(),
            Some("Exact file")
        );
    }

    #[test]
    fn configured_roots_follow_native_precedence_without_touching_real_home() {
        let f = Fixture::new();
        let home = f.0.join("home");
        let cwd = f.0.join("project");
        let agent = home.join(".pi/agent");
        fs::create_dir_all(&agent).unwrap();
        fs::create_dir_all(cwd.join(".pi")).unwrap();
        assert_eq!(agent_dir(&home), agent);
        fs::write(
            agent.join("settings.json"),
            r#"{"sessionDir":"~/global-sessions"}"#,
        )
        .unwrap();
        assert_eq!(
            configured_session_dir(&cwd, &home, &agent, None),
            home.join("global-sessions")
        );
        fs::write(
            cwd.join(".pi/settings.json"),
            r#"{"sessionDir":"project-sessions"}"#,
        )
        .unwrap();
        assert_eq!(
            configured_session_dir(&cwd, &home, &agent, None),
            cwd.join("project-sessions")
        );
        assert_eq!(
            configured_session_dir(&cwd, &home, &agent, Some(Path::new("~/env-sessions"))),
            home.join("env-sessions")
        );
        fs::write(cwd.join(".pi/settings.json"), r#"{"sessionDir":""}"#).unwrap();
        assert_eq!(
            configured_session_dir(&cwd, &home, &agent, None),
            default_session_dir(&cwd, &agent)
        );
    }

    #[test]
    fn native_path_encoding_matches_pi() {
        assert_eq!(
            default_session_dir(Path::new("/work/project"), Path::new("/config")),
            PathBuf::from("/config/sessions/--work-project--")
        );
        assert_eq!(
            default_session_dir(Path::new("C:\\work\\project"), Path::new("/config")),
            PathBuf::from("/config/sessions/--C--work-project--")
        );
        assert_eq!(
            expand_home(Path::new("~/pi-config"), Path::new("/fake-home")),
            PathBuf::from("/fake-home/pi-config")
        );
    }
}
