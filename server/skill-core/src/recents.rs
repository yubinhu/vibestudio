//! Recently opened skills/markdown, persisted SERVER-SIDE so they belong to the
//! machine whose files they point at. The client reaches every `/api/*` through the
//! active server (the local one, or a remote it's proxying to), so this list follows
//! the connection automatically: you see a machine's recents whether you opened it
//! locally or over SSH. Stored as `recents.json` in the config dir; newest-first,
//! deduped by `root`, capped at `MAX`.
use serde::{Deserialize, Serialize};

use crate::{paths, pathsafe::resolve_root, state_store};

const MAX: usize = 30;

#[derive(Serialize, Deserialize, Clone)]
pub struct Recent {
    /// The opened skill folder, or a loose markdown file's absolute path. Also the
    /// dedup/identity key.
    pub root: String,
    pub name: String,
    /// "skill" (default when absent) or "markdown" — how the client routes the open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Unix seconds of the last successful open. Older history has no timestamp.
    #[serde(default, rename = "openedAt", skip_serializing_if = "Option::is_none")]
    pub opened_at: Option<u64>,
}

fn store_path() -> Result<std::path::PathBuf, String> {
    Ok(paths::config_dir()?.join("recents.json"))
}

/// The stored list, newest-first. Missing history is empty; unreadable or corrupt
/// history is reported so the client can distinguish it from a fresh workspace.
pub fn list() -> Result<Vec<Recent>, String> {
    let mut items: Vec<Recent> = state_store::read(&store_path()?)?;
    items.truncate(MAX);
    Ok(items)
}

fn update(
    change: impl FnOnce(&mut Vec<Recent>) -> Result<(), String>,
) -> Result<Vec<Recent>, String> {
    paths::ensure_config_dir()?;
    state_store::update(&store_path()?, change)
}

fn prepend(items: &mut Vec<Recent>, recent: Recent) {
    items.retain(|r| r.root != recent.root);
    items.insert(0, recent);
    items.truncate(MAX);
}

/// Add (or move to the front) an entry, returning the updated list. Dedup is by
/// `root`, so re-opening the same skill just bumps it to the top.
pub fn add(root: &str, name: &str, kind: Option<&str>) -> Result<Vec<Recent>, String> {
    if root.trim().is_empty() {
        return Err("A recent item needs a path.".into());
    }
    if !matches!(kind, None | Some("skill" | "markdown")) {
        return Err("Unknown recent item kind.".into());
    }
    let root = std::path::absolute(resolve_root(root))
        .map_err(|e| e.to_string())?
        .to_string_lossy()
        .into_owned();
    let opened_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs());
    update(|items| {
        prepend(
            items,
            Recent {
                root,
                name: name.into(),
                kind: kind.map(Into::into),
                opened_at,
            },
        );
        Ok(())
    })
}

/// Drop the entry with this root, returning the updated list.
pub fn remove(root: &str) -> Result<Vec<Recent>, String> {
    let normalized = std::path::absolute(resolve_root(root))
        .map_err(|e| e.to_string())?
        .to_string_lossy()
        .into_owned();
    update(|items| {
        items.retain(|r| r.root != root && r.root != normalized);
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state_store::tests::TempDir;

    #[test]
    fn old_history_round_trips_with_markdown_and_new_timestamps() {
        let dir = TempDir::new();
        let path = dir.0.join("recents.json");
        std::fs::write(&path, r#"[{"root":"/skill","name":"Old skill"}]"#).unwrap();
        state_store::update::<Vec<Recent>>(&path, |items| {
            prepend(
                items,
                Recent {
                    root: "/notes.md".into(),
                    name: "notes.md".into(),
                    kind: Some("markdown".into()),
                    opened_at: Some(123),
                },
            );
            Ok(())
        })
        .unwrap();
        let items: Vec<Recent> = state_store::read(&path).unwrap();
        assert_eq!(items[0].kind.as_deref(), Some("markdown"));
        assert_eq!(items[0].opened_at, Some(123));
        assert!(items[1].opened_at.is_none());
        assert!(items[1].kind.is_none());
    }

    #[test]
    fn history_is_bounded_and_reopening_bumps_without_duplicates() {
        let mut items = Vec::new();
        for n in 0..40 {
            prepend(
                &mut items,
                Recent {
                    root: format!("/{n}"),
                    name: n.to_string(),
                    kind: None,
                    opened_at: Some(n),
                },
            );
        }
        prepend(
            &mut items,
            Recent {
                root: "/20".into(),
                name: "Renamed".into(),
                kind: None,
                opened_at: Some(100),
            },
        );
        assert_eq!(items.len(), MAX);
        assert_eq!(items[0].name, "Renamed");
        assert_eq!(items[0].opened_at, Some(100));
        assert_eq!(items.iter().filter(|r| r.root == "/20").count(), 1);
        assert!(!items.iter().any(|r| r.root == "/0"));
    }
}
