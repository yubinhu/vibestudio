//! Filesystem and launch defaults belong to the server whose files/agents they
//! reference. Visual layout preferences remain on the client.
use crate::{paths, pathsafe::resolve_root, state_store};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Default, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Preferences {
    pub picker_locations: BTreeMap<String, String>,
    pub terminal: BTreeMap<String, TerminalPrefs>,
    pub last_agent: Option<String>,
}

#[derive(Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TerminalPrefs {
    pub cwd: String,
    pub ide: bool,
    pub skip: bool,
    pub auto: bool,
    pub extra: String,
}

pub fn get() -> Result<Preferences, String> {
    state_store::read(&paths::config_dir()?.join("preferences.json"))
}

fn update(
    change: impl FnOnce(&mut Preferences) -> Result<(), String>,
) -> Result<Preferences, String> {
    state_store::update(
        &paths::ensure_config_dir()?.join("preferences.json"),
        change,
    )
}

pub fn remember_picker(context: &str, path: &str) -> Result<Preferences, String> {
    if !matches!(context, "open" | "session" | "import") {
        return Err("Unknown folder picker context.".into());
    }
    if path.trim().is_empty() {
        return Err("A selected folder needs a path.".into());
    }
    let dir = std::path::absolute(resolve_root(path)).map_err(|e| e.to_string())?;
    if !dir.is_dir() {
        return Err("The selected folder is no longer available.".into());
    }
    update(|prefs| {
        prefs
            .picker_locations
            .insert(context.into(), dir.to_string_lossy().into_owned());
        Ok(())
    })
}

pub fn remember_terminal(agent_id: &str, mut config: TerminalPrefs) -> Result<Preferences, String> {
    if agent_id.is_empty() || agent_id.len() > 256 {
        return Err("Invalid agent ID.".into());
    }
    // Match the launch dialog: automatic mode and skip-prompts are exclusive.
    if config.auto {
        config.skip = false;
    }
    update(|prefs| {
        prefs.terminal.insert(agent_id.into(), config);
        prefs.last_agent = Some(agent_id.into());
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state_store::tests::TempDir;

    #[test]
    fn round_trip_preserves_independent_defaults() {
        let dir = TempDir::new();
        let path = dir.0.join("preferences.json");
        state_store::update::<Preferences>(&path, |p| {
            p.picker_locations.insert("open".into(), "/docs".into());
            p.terminal.insert(
                "shell:cli".into(),
                TerminalPrefs {
                    cwd: "/work".into(),
                    ..Default::default()
                },
            );
            p.last_agent = Some("shell:cli".into());
            Ok(())
        })
        .unwrap();
        state_store::update::<Preferences>(&path, |p| {
            p.picker_locations
                .insert("import".into(), "/downloads".into());
            Ok(())
        })
        .unwrap();
        let loaded: Preferences = state_store::read(&path).unwrap();
        assert_eq!(loaded.picker_locations["open"], "/docs");
        assert_eq!(loaded.picker_locations["import"], "/downloads");
        assert_eq!(loaded.terminal["shell:cli"].cwd, "/work");
        assert_eq!(loaded.last_agent.as_deref(), Some("shell:cli"));
        // A different server's store starts empty; no client-origin state leaks.
        let other: Preferences = state_store::read(&dir.0.join("other.json")).unwrap();
        assert!(other.picker_locations.is_empty());
        assert!(other.terminal.is_empty());
    }

    #[test]
    fn older_partial_preferences_use_defaults() {
        let prefs: Preferences =
            serde_json::from_str(r#"{"pickerLocations":{"open":"/docs"}}"#).unwrap();
        assert!(prefs.terminal.is_empty());
        assert!(prefs.last_agent.is_none());
    }
}
