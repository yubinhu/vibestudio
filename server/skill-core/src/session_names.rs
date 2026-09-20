//! User-chosen terminal names belong to the active host. Keep them separate
//! from generated agent titles so removing an override restores the latest title.
use crate::{paths, state_store};
use std::collections::BTreeMap;
use std::path::Path;

pub const MAX_TITLE_CHARS: usize = 200;
const STORE_FILE: &str = "session-titles.json";

pub fn get() -> Result<BTreeMap<String, String>, String> {
    state_store::read(&paths::config_dir()?.join(STORE_FILE))
}

/// Set an override for a terminal's stable ID; `None` restores its automatic name.
/// The HTTP handler verifies that the terminal still exists before calling this.
pub fn set(id: &str, title: Option<&str>) -> Result<Option<String>, String> {
    let title = normalize(title)?;
    save_at(&paths::ensure_config_dir()?.join(STORE_FILE), id, title)
}

/// Validate before an agent-native rename, using the same rules as local storage.
pub fn normalize(title: Option<&str>) -> Result<Option<String>, String> {
    let Some(title) = title else { return Ok(None) };
    if title
        .chars()
        .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
    {
        return Err("Session names must be a single line without control characters.".into());
    }
    let title = title.trim();
    if title.is_empty() {
        return Err("Enter a session name, or use the automatic name.".into());
    }
    if title.chars().count() > MAX_TITLE_CHARS {
        return Err(format!(
            "Session names must be {MAX_TITLE_CHARS} characters or fewer."
        ));
    }
    Ok(Some(title.into()))
}

fn save_at(path: &Path, id: &str, title: Option<String>) -> Result<Option<String>, String> {
    state_store::update::<BTreeMap<String, String>>(path, |names| {
        if let Some(title) = &title {
            names.insert(id.into(), title.clone());
        } else {
            names.remove(id);
        }
        Ok(())
    })?;
    Ok(title)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state_store::tests::TempDir;

    #[test]
    fn rename_and_reset_survive_reload_without_changing_other_sessions() {
        let dir = TempDir::new();
        let path = dir.0.join(STORE_FILE);
        let first = "ass-1-1784338461-0";
        let second = "ass-1-1784338461-1";
        save_at(
            &path,
            first,
            normalize(Some("  Fix session naming  ")).unwrap(),
        )
        .unwrap();
        save_at(&path, second, normalize(Some("检查日志 🔎")).unwrap()).unwrap();
        let loaded: BTreeMap<String, String> = state_store::read(&path).unwrap();
        assert_eq!(loaded[first], "Fix session naming");
        assert_eq!(loaded[second], "检查日志 🔎");

        save_at(&path, first, normalize(None).unwrap()).unwrap();
        let loaded: BTreeMap<String, String> = state_store::read(&path).unwrap();
        assert!(!loaded.contains_key(first));
        assert_eq!(loaded[second], "检查日志 🔎");
        let other_host: BTreeMap<String, String> =
            state_store::read(&dir.0.join("other-host.json")).unwrap();
        assert!(other_host.is_empty());
    }

    #[test]
    fn title_validation_counts_unicode_characters_and_rejects_multiline_names() {
        assert_eq!(normalize(None).unwrap(), None);
        assert_eq!(
            normalize(Some("  A name  ")).unwrap().as_deref(),
            Some("A name")
        );
        assert!(normalize(Some(&"🔎".repeat(MAX_TITLE_CHARS))).is_ok());
        assert!(normalize(Some(&"🔎".repeat(MAX_TITLE_CHARS + 1))).is_err());
        for title in [
            "",
            "   ",
            "two\nlines",
            "name\r",
            "\tname",
            "name\0",
            "\u{7f}",
            "a\u{2028}b",
            "a\u{2029}b",
        ] {
            assert!(normalize(Some(title)).is_err(), "accepted {title:?}");
        }
    }

    #[test]
    fn failed_save_preserves_malformed_state() {
        let dir = TempDir::new();
        let path = dir.0.join(STORE_FILE);
        for original in ["broken json", r#"{"ass-1-2-3":42}"#] {
            std::fs::write(&path, original).unwrap();
            assert!(save_at(&path, "ass-1-2-3", Some("New name".into())).is_err());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        }
    }

    #[test]
    fn concurrent_session_renames_preserve_each_other() {
        let dir = TempDir::new();
        let path = dir.0.join(STORE_FILE);
        std::thread::scope(|scope| {
            for n in 0..12 {
                let path = &path;
                scope.spawn(move || {
                    save_at(path, &format!("ass-1-2-{n}"), Some(format!("Session {n}"))).unwrap();
                });
            }
        });
        let loaded: BTreeMap<String, String> = state_store::read(&path).unwrap();
        assert_eq!(loaded.len(), 12);
        for n in 0..12 {
            assert_eq!(loaded[&format!("ass-1-2-{n}")], format!("Session {n}"));
        }
    }
}
