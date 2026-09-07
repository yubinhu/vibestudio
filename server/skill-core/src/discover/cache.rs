//! Fast discovery reads known locations. Only the detached worker searches home
//! for new projects; its location index belongs to this server, not the browser.
use super::{auto_track_globals, collect_globals, scan_known_projects, scan_projects, AgentSkills, Groups};
use crate::{paths, state_store};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const RESCAN_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Serialize)]
pub struct DiscoverySnapshot {
    pub groups: Vec<AgentSkills>,
    /// More project locations may arrive. Read again while true; ordinary reads
    /// do not restart a completed crawl during the rescan interval.
    pub scanning: bool,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
struct ProjectIndex {
    home: PathBuf,
    roots: BTreeSet<PathBuf>,
}

#[derive(Default)]
struct State {
    roots: BTreeSet<PathBuf>,
    running: bool,
    finished_at: Option<Instant>,
}

impl State {
    fn begin(&mut self, force: bool, now: Instant) -> bool {
        if self.running
            || (!force
                && self
                    .finished_at
                    .is_some_and(|finished| now.duration_since(finished) < RESCAN_INTERVAL))
        {
            return false;
        }
        self.running = true;
        true
    }
}

struct Discovery {
    home: PathBuf,
    index_path: Option<PathBuf>,
    state: Mutex<State>,
}

impl Discovery {
    fn new(home: PathBuf, index_path: Option<PathBuf>) -> Arc<Self> {
        let index = index_path
            .as_deref()
            .and_then(|path| match state_store::read::<ProjectIndex>(path) {
                Ok(index) => Some(index),
                Err(error) => {
                    log::warn!("Could not load project skill locations: {error}");
                    None
                }
            })
            .unwrap_or_default();
        let roots = if index.home == home {
            index.roots
        } else {
            BTreeSet::new()
        };
        Arc::new(Self {
            home,
            index_path,
            state: Mutex::new(State {
                roots,
                ..State::default()
            }),
        })
    }

    fn snapshot(self: &Arc<Self>, force: bool) -> DiscoverySnapshot {
        // Copy locations and progress together. If a crawl finishes while we
        // read these locations, scanning stays true for this response so the
        // client fetches the newly merged locations on its next poll.
        let (roots, mut scanning, start) = {
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            let start = state.begin(force, Instant::now());
            (state.roots.clone(), state.running, start)
        };

        let started = Instant::now();
        let mut groups = Groups::default();
        let mut seen = HashSet::new();
        collect_globals(&self.home, &mut groups, &mut seen);
        scan_known_projects(&roots, &self.home, &mut groups, &mut seen);
        log::debug!(
            "Skill discovery: {} skills in {:?} from global and {} known project locations",
            seen.len(),
            started.elapsed(),
            roots.len()
        );

        // Start after the fast read so the broad crawl doesn't compete for disk
        // access with the first inventory. Never hold the state lock during I/O.
        if start {
            let discovery = Arc::clone(self);
            if let Err(error) = std::thread::Builder::new()
                .name("skill-discovery".into())
                .spawn(move || {
                    let result = std::panic::catch_unwind(|| discovery.scan());
                    if result.is_err() {
                        log::warn!("Background project skill discovery panicked");
                    }
                    let mut state = discovery.state.lock().unwrap_or_else(|p| p.into_inner());
                    state.running = false;
                    state.finished_at = Some(Instant::now());
                })
            {
                log::warn!("Could not start background project skill discovery: {error}");
                let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
                state.running = false;
                state.finished_at = Some(Instant::now());
                scanning = false;
            }
        }

        DiscoverySnapshot {
            groups: groups.into_agents(),
            scanning,
        }
    }

    fn scan(&self) {
        let started = Instant::now();
        let roots = scan_projects(
            &self.home,
            &self.home,
            &mut Groups::default(),
            &mut HashSet::new(),
        );
        let found = roots.len();
        self.merge_locations(roots);
        log::debug!(
            "Background skill discovery: {found} project locations in {:?}",
            started.elapsed()
        );
    }

    fn merge_locations(&self, roots: BTreeSet<PathBuf>) {
        let mut merged = {
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            // The crawler is bounded and may not reach every known location.
            // Keep those locations: foreground reads revalidate their contents,
            // so deleted skills never survive as stale inventory entries.
            state.roots.extend(roots);
            state.roots.clone()
        };
        if let Some(path) = &self.index_path {
            let saved = (|| {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                state_store::update::<ProjectIndex>(path, |index| {
                    if index.home != self.home {
                        index.home = self.home.clone();
                        index.roots.clear();
                    }
                    // Other desktop/standalone processes can index the same
                    // host concurrently. Merge under the store's file lock.
                    index.roots.extend(merged.iter().cloned());
                    Ok(())
                })
            })();
            match saved {
                Ok(index) => merged = index.roots,
                Err(error) => log::warn!("Could not save project skill locations: {error}"),
            }
        }
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .roots
            .extend(merged);
    }
}

/// Discover globals and previously indexed project locations, and schedule a
/// coalesced background search for new projects. `force` bypasses the cooldown
/// for an explicit Refresh. The index is private to the answering server.
pub fn discover_progressive(force: bool) -> Result<DiscoverySnapshot, String> {
    static DISCOVERY: OnceLock<Arc<Discovery>> = OnceLock::new();
    let discovery = match DISCOVERY.get() {
        Some(discovery) => discovery,
        None => {
            let home = dirs::home_dir().ok_or_else(|| "No home directory.".to_string())?;
            let index_path = paths::config_dir()
                .ok()
                .map(|p| p.join("skill-projects.json"));
            DISCOVERY.get_or_init(|| Discovery::new(home, index_path))
        }
    };
    let snapshot = discovery.snapshot(force);
    auto_track_globals(&snapshot.groups);
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state_store::tests::TempDir;
    use std::path::Path;

    fn plant(path: &Path, name: &str) {
        std::fs::create_dir_all(path).unwrap();
        std::fs::write(path.join("SKILL.md"), format!("---\nname: {name}\n---\n")).unwrap();
    }

    fn finish(discovery: &Discovery) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while discovery.state.lock().unwrap().running {
            assert!(
                Instant::now() < deadline,
                "background discovery did not finish"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn names(snapshot: &DiscoverySnapshot) -> BTreeSet<&str> {
        snapshot
            .groups
            .iter()
            .flat_map(|g| &g.skills)
            .filter_map(|s| s.name.as_deref())
            .collect()
    }

    #[test]
    fn first_snapshot_is_fast_then_background_locations_survive_restart() {
        let dir = TempDir::new();
        let home = dir.0.join("home");
        let index = dir.0.join("config/skill-projects.json");
        plant(&home.join(".codex/skills/.system/global"), "global");
        plant(&home.join("work/repo/.agents/skills/project"), "project");
        let discovery = Discovery::new(home.clone(), Some(index.clone()));

        let first = discovery.snapshot(false);
        assert!(first.scanning);
        assert_eq!(names(&first), BTreeSet::from(["global"]));
        finish(&discovery);
        let complete = discovery.snapshot(false);
        assert!(!complete.scanning, "polling must not restart the crawl");
        assert_eq!(names(&complete), BTreeSet::from(["global", "project"]));

        let restarted = Discovery::new(home, Some(index));
        let first = restarted.snapshot(false);
        assert!(first.scanning);
        assert_eq!(names(&first), BTreeSet::from(["global", "project"]));
        finish(&restarted);
    }

    #[test]
    fn scan_coalescing_and_cooldown_allow_explicit_refresh() {
        let mut state = State::default();
        let now = Instant::now();
        assert!(state.begin(false, now));
        assert!(!state.begin(false, now));
        assert!(
            !state.begin(true, now),
            "force still shares an active crawl"
        );
        state.running = false;
        state.finished_at = Some(now);
        assert!(!state.begin(false, now + Duration::from_secs(59)));
        assert!(state.begin(true, now));
        state.running = false;
        assert!(state.begin(false, now + RESCAN_INTERVAL));
    }

    #[test]
    fn partial_scans_and_concurrent_servers_merge_locations() {
        let dir = TempDir::new();
        let home = dir.0.join("home");
        let index = dir.0.join("skill-projects.json");
        let a = home.join("one/.agents/skills");
        let b = home.join("two/.claude/skills");
        let first = Discovery::new(home.clone(), Some(index.clone()));
        let second = Discovery::new(home.clone(), Some(index.clone()));
        first.merge_locations(BTreeSet::from([a.clone()]));
        second.merge_locations(BTreeSet::from([b.clone()]));
        first.merge_locations(BTreeSet::new());
        let restarted = Discovery::new(home, Some(index));
        assert_eq!(
            restarted.state.lock().unwrap().roots,
            BTreeSet::from([a, b])
        );
    }

    #[test]
    fn unavailable_cache_does_not_block_discovery_and_hosts_stay_separate() {
        let dir = TempDir::new();
        let home = dir.0.join("home");
        plant(&home.join(".codex/skills/.system/global"), "global");
        let corrupt = dir.0.join("skill-projects.json");
        std::fs::write(&corrupt, "invalid cache").unwrap();
        let discovery = Discovery::new(home.clone(), Some(corrupt.clone()));
        let first = discovery.snapshot(false);
        assert_eq!(names(&first), BTreeSet::from(["global"]));
        finish(&discovery);
        assert_eq!(std::fs::read_to_string(corrupt).unwrap(), "invalid cache");

        let index = dir.0.join("valid.json");
        let local = Discovery::new(home, Some(index.clone()));
        local.merge_locations(BTreeSet::from([dir.0.join("home/repo/.agents/skills")]));
        let other = Discovery::new(dir.0.join("other-home"), Some(index));
        assert!(other.state.lock().unwrap().roots.is_empty());
    }
}
