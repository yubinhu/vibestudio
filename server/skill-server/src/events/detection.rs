//! Cached owning-host state detection. HTTP list calls never capture terminal
//! contents; this watcher stays alive even with zero browser/SSE subscribers.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use skill_core::agent_detection::{self, AgentState, AttentionKind, Tracker};
use skill_term::detection_snapshot::{self, AgentProcess, DetectionSnapshot, ProcessSnapshot};
use skill_term::SessionInfo;

const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
const PENDING_IDLE_RECHECK: Duration = Tracker::RECHECK_INTERVAL;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionAttention {
    pub state: AgentState,
    /// Opaque boot identity + monotonic counter. Consumers may compare counters
    /// only within the same boot; a new boot is a silent baseline.
    pub sequence: String,
    pub changed_at: u64,
    pub kind: Option<AttentionKind>,
    pub matched_rule: Option<String>,
}

fn cache() -> &'static Mutex<HashMap<String, SessionAttention>> {
    static CACHE: OnceLock<Mutex<HashMap<String, SessionAttention>>> = OnceLock::new();
    CACHE.get_or_init(Mutex::default)
}

fn boot_id() -> &'static str {
    static BOOT: OnceLock<String> = OnceLock::new();
    BOOT.get_or_init(|| {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        format!("{now:x}-{:x}", std::process::id())
    })
}

pub(crate) fn attention_for(id: &str, agent: &str) -> Option<SessionAttention> {
    let value = cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(id)
        .cloned();
    value.or_else(|| {
        detector_for(agent).map(|_| SessionAttention {
            state: AgentState::Unknown,
            sequence: format!("{}:0", boot_id()),
            changed_at: 0,
            kind: None,
            matched_rule: None,
        })
    })
}

pub(super) fn detector_for(agent: &str) -> Option<&'static str> {
    skill_core::agents::by_family(agent).and_then(|definition| definition.attention_detector)
}

struct Entry {
    agent: String,
    detector: &'static str,
    tracker: Tracker,
    last_sample: Option<DetectionSnapshot>,
    next_sample: Instant,
    startup_until: Option<Instant>,
    newly_created: bool,
    process: Option<AgentProcess>,
    ever_acquired: bool,
    release_pending: bool,
    stale_title: Option<String>,
    stale_screen: Option<String>,
    attention: Option<SessionAttention>,
}

pub(super) struct DetectorFleet {
    entries: HashMap<String, Entry>,
    boot: String,
    sequence: u64,
    inventoried: bool,
}

impl DetectorFleet {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            boot: boot_id().to_string(),
            sequence: 0,
            inventoried: false,
        }
    }

    pub fn sync_sessions(&mut self, sessions: &[SessionInfo], now: Instant) {
        self.entries.retain(|id, entry| {
            sessions
                .iter()
                .any(|s| s.id == *id && s.agent == entry.agent)
        });
        for session in sessions {
            let Some(detector) = detector_for(&session.agent) else {
                continue;
            };
            let startup_until = startup_until(session, now);
            self.entries
                .entry(session.id.clone())
                .or_insert_with(|| Entry {
                    agent: session.agent.clone(),
                    detector,
                    tracker: Tracker::new(),
                    last_sample: None,
                    next_sample: now,
                    startup_until,
                    newly_created: self.inventoried && startup_until.is_some(),
                    process: None,
                    ever_acquired: false,
                    release_pending: false,
                    stale_title: None,
                    stale_screen: None,
                    attention: None,
                });
        }
        self.inventoried = true;
        self.publish_cache();
    }

    pub fn poll(&mut self, now: Instant) -> Vec<(String, SessionAttention)> {
        let due: Vec<String> = self
            .entries
            .iter()
            .filter(|(_, entry)| entry.next_sample <= now)
            .map(|(id, _)| id.clone())
            .collect();
        if due.is_empty() {
            return Vec::new();
        }
        // One process-table read covers all due sessions. Failure is missing
        // evidence; retain existing state and retry at the normal cadence.
        let processes = ProcessSnapshot::capture();
        let mut edges = Vec::new();
        for id in due {
            let sample = processes
                .as_ref()
                .map_err(|e| e.clone())
                .and_then(|processes| {
                    detection_snapshot::capture(
                        &id,
                        self.entries[&id].detector,
                        self.entries[&id].process.is_some() && !self.entries[&id].release_pending,
                        processes,
                    )
                });
            if let Some(attention) = self.observe(&id, sample, Instant::now(), unix_ms()) {
                edges.push((id, attention));
            }
        }
        self.publish_cache();
        edges
    }

    /// The transport-independent boundary also lets tests cover failed captures,
    /// duplicate screens and actual upstream transitions without touching tmux.
    fn observe(
        &mut self,
        id: &str,
        sample: Result<DetectionSnapshot, String>,
        now: Instant,
        changed_at: u64,
    ) -> Option<SessionAttention> {
        let entry = self.entries.get_mut(id)?;
        entry.next_sample = now + SAMPLE_INTERVAL;
        let sample = match sample {
            Ok(sample) => sample,
            Err(error) => {
                log::debug!("agent detection unavailable ({id}): {error}");
                return None;
            }
        };
        let mut process_exited = false;
        match sample.agent_process.as_ref() {
            None if entry.process.is_some() && !entry.release_pending => {
                // Publish the original agent's exit once before releasing its
                // identity. Retained OSC titles cannot override proven exit.
                entry.release_pending = true;
                entry.startup_until = None;
                process_exited = true;
            }
            None => {
                if entry.process.take().is_some() {
                    quarantine_previous_generation(entry);
                    entry.tracker = Tracker::new();
                    entry.startup_until = None;
                    entry.release_pending = false;
                }
                entry.last_sample = Some(sample);
                // Plain shells and unrelated jobs must never reactivate the
                // launched family's detector against the old agent's screen.
                return publish_state(
                    entry,
                    &self.boot,
                    &mut self.sequence,
                    AgentState::Unknown,
                    None,
                    None,
                    changed_at,
                );
            }
            Some(process) if entry.process.as_ref() != Some(process) || entry.release_pending => {
                let replacement = entry.ever_acquired || entry.last_sample.is_some();
                if replacement {
                    quarantine_previous_generation(entry);
                    entry.tracker = Tracker::new();
                    entry.last_sample = None;
                    entry.newly_created = true;
                }
                entry.process = Some(process.clone());
                entry.ever_acquired = true;
                entry.release_pending = false;
                // Grace starts at process acquisition, not at tmux creation:
                // launch wrappers and replacement agents may start much later.
                if replacement || entry.newly_created || entry.startup_until.is_some() {
                    entry.startup_until = Some(now + Tracker::STARTUP_GRACE);
                }
                if replacement {
                    refresh_quarantine(entry, &sample);
                    entry.last_sample = Some(sample);
                    return publish_state(
                        entry,
                        &self.boot,
                        &mut self.sequence,
                        AgentState::Unknown,
                        None,
                        None,
                        changed_at,
                    );
                }
            }
            Some(_) => {}
        }
        refresh_quarantine(entry, &sample);
        if entry.startup_until.is_some_and(|deadline| now < deadline) && !process_exited {
            entry.last_sample = Some(sample);
            return None;
        }
        entry.startup_until = None;
        if entry.last_sample.as_ref() == Some(&sample)
            && entry.tracker.state() == Some(AgentState::Idle)
            && !entry.tracker.needs_recheck()
            && !process_exited
        {
            return None;
        }
        // tmux retains OSC titles across process replacement. Quarantine only
        // the prior generation's identical evidence until the new agent changes
        // it; do not mutate the terminal or manufacture OSC progress signals.
        let title = if entry.stale_title.is_some() {
            ""
        } else {
            &sample.title
        };
        let screen = if entry.stale_screen.is_some() {
            ""
        } else {
            &sample.screen
        };
        if entry.stale_screen.is_some() && title.is_empty() && !process_exited {
            return None;
        }
        let seed = entry.tracker.state().is_none();
        let detection = agent_detection::detect(entry.detector, screen, title, "");
        let transition = entry.tracker.observe(detection, now, process_exited);
        entry.last_sample = Some(sample);
        if entry.tracker.needs_recheck() {
            entry.next_sample = now + PENDING_IDLE_RECHECK;
        }
        let transition = transition?;
        let kind = if seed && entry.newly_created && transition.state == AgentState::Blocked {
            Some(AttentionKind::Request)
        } else {
            transition.attention
        };
        publish_state(
            entry,
            &self.boot,
            &mut self.sequence,
            transition.state,
            kind,
            transition.detection.matched_rule.map(|rule| rule.id),
            changed_at,
        )
    }

    fn publish_cache(&self) {
        let mut cached = cache().lock().unwrap_or_else(|e| e.into_inner());
        *cached = self
            .entries
            .iter()
            .filter_map(|(id, entry)| {
                entry
                    .attention
                    .as_ref()
                    .map(|attention| (id.clone(), attention.clone()))
            })
            .collect();
    }
}

fn quarantine_previous_generation(entry: &mut Entry) {
    if let Some(previous) = &entry.last_sample {
        entry.stale_title = Some(previous.title.clone());
        entry.stale_screen = Some(previous.screen.clone());
    }
}

fn refresh_quarantine(entry: &mut Entry, sample: &DetectionSnapshot) {
    // Observe new-generation evidence even during startup grace. If a title
    // changes A → B → A while classification is paused, the later A is fresh.
    if entry
        .stale_title
        .as_ref()
        .is_some_and(|old| old != &sample.title)
    {
        entry.stale_title = None;
    }
    if entry
        .stale_screen
        .as_ref()
        .is_some_and(|old| old != &sample.screen)
    {
        entry.stale_screen = None;
    }
}

fn publish_state(
    entry: &mut Entry,
    boot: &str,
    sequence: &mut u64,
    state: AgentState,
    kind: Option<AttentionKind>,
    matched_rule: Option<String>,
    changed_at: u64,
) -> Option<SessionAttention> {
    if entry
        .attention
        .as_ref()
        .is_some_and(|attention| attention.state == state)
    {
        return None; // same-state upstream evidence refresh is not a wire edge
    }
    let initial = entry.attention.is_none();
    *sequence = sequence.saturating_add(1);
    let attention = SessionAttention {
        state,
        sequence: format!("{boot}:{sequence}"),
        changed_at,
        kind,
        matched_rule,
    };
    entry.attention = Some(attention.clone());
    // Existing discovery silently establishes a baseline. An explicit Request
    // seed is reserved for an observed new launch/acquisition after discovery.
    (!initial || kind.is_some()).then_some(attention)
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn startup_until(session: &SessionInfo, now: Instant) -> Option<Instant> {
    let created = session.created.parse::<u64>().ok()?.saturating_mul(1000);
    let remaining = created
        .saturating_add(Tracker::STARTUP_GRACE.as_millis() as u64)
        .saturating_sub(unix_ms());
    (remaining > 0).then(|| now + Duration::from_millis(remaining).min(Tracker::STARTUP_GRACE))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(screen: &str) -> Result<DetectionSnapshot, String> {
        Ok(DetectionSnapshot {
            screen: screen.into(),
            title: String::new(),
            foreground_command: "codex".into(),
            pane_pid: 100,
            process_exited: false,
            agent_process: Some(AgentProcess {
                pid: 101,
                started_at: "first".into(),
            }),
        })
    }

    fn fleet(now: Instant) -> DetectorFleet {
        let mut fleet = DetectorFleet::new();
        fleet.entries.insert(
            "ass-1".into(),
            Entry {
                agent: "codex".into(),
                detector: "codex",
                tracker: Tracker::new(),
                last_sample: None,
                next_sample: now,
                startup_until: None,
                newly_created: false,
                process: None,
                ever_acquired: false,
                release_pending: false,
                stale_title: None,
                stale_screen: None,
                attention: None,
            },
        );
        fleet
    }

    #[test]
    fn initial_capture_is_silent_and_failed_capture_keeps_state() {
        let now = Instant::now();
        let mut fleet = fleet(now);
        assert!(fleet
            .observe(
                "ass-1",
                fixture("• Working (1s • esc to interrupt)"),
                now,
                1000
            )
            .is_none());
        let first = fleet.entries["ass-1"].attention.as_ref().unwrap().clone();
        assert_eq!(first.state, AgentState::Working);
        assert_eq!(first.kind, None);
        assert!(fleet
            .observe(
                "ass-1",
                Err("tmux unavailable".into()),
                now + SAMPLE_INTERVAL,
                2000
            )
            .is_none());
        assert_eq!(
            fleet.entries["ass-1"].attention.as_ref().unwrap().sequence,
            first.sequence
        );
    }

    #[test]
    fn ambiguous_idle_rechecks_unchanged_screen_and_announces_once() {
        let now = Instant::now();
        let mut fleet = fleet(now);
        fleet.observe(
            "ass-1",
            fixture("• Working (1s • esc to interrupt)"),
            now,
            1000,
        );
        let plain = "some ordinary output";
        for index in 0..3 {
            assert!(fleet
                .observe(
                    "ass-1",
                    fixture(plain),
                    now + SAMPLE_INTERVAL + PENDING_IDLE_RECHECK * index,
                    2000 + index as u64 * 100
                )
                .is_none());
            assert!(fleet.entries["ass-1"].tracker.needs_recheck());
        }
        let edge = fleet
            .observe(
                "ass-1",
                fixture(plain),
                now + SAMPLE_INTERVAL + PENDING_IDLE_RECHECK * 3,
                2300,
            )
            .unwrap();
        assert_eq!(edge.state, AgentState::Idle);
        assert_eq!(edge.kind, Some(AttentionKind::Done));
        assert!(fleet
            .observe("ass-1", fixture(plain), now + SAMPLE_INTERVAL * 3, 4000)
            .is_none());
    }

    #[test]
    fn returning_to_work_clears_previous_attention() {
        let now = Instant::now();
        let mut fleet = fleet(now);
        fleet.observe("ass-1", fixture("ordinary output"), now, 1000);
        let edge = fleet
            .observe(
                "ass-1",
                fixture("• Working (1s • esc to interrupt)"),
                now + SAMPLE_INTERVAL,
                2000,
            )
            .unwrap();
        assert_eq!(edge.state, AgentState::Working);
        assert_eq!(edge.kind, None);
        assert!(edge.sequence.ends_with(":2"));
    }

    #[test]
    fn new_launch_initial_blocker_requests_input_but_restored_blocker_is_silent() {
        let now = Instant::now();
        let mut restored = fleet(now);
        let mut blocked = fixture("").unwrap();
        blocked.title = "Action Required".into();
        assert!(restored
            .observe("ass-1", Ok(blocked.clone()), now, 1000)
            .is_none());
        assert_eq!(
            restored.entries["ass-1"].attention.as_ref().unwrap().kind,
            None
        );
        let mut new = fleet(now);
        new.entries.get_mut("ass-1").unwrap().newly_created = true;
        assert!(new
            .observe("ass-1", Ok(blocked.clone()), now, 1000)
            .is_none());
        let request = new
            .observe("ass-1", Ok(blocked), now + Tracker::STARTUP_GRACE, 4000)
            .unwrap();
        assert_eq!(request.kind, Some(AttentionKind::Request));
    }

    #[test]
    fn new_agent_grace_does_not_classify_launch_shell_or_partial_screen() {
        let now = Instant::now();
        let mut fleet = fleet(now);
        fleet.entries.get_mut("ass-1").unwrap().startup_until = Some(now + Tracker::STARTUP_GRACE);
        let mut shell = fixture("").unwrap();
        shell.process_exited = true;
        shell.agent_process = None;
        assert!(fleet.observe("ass-1", Ok(shell), now, 1000).is_none());
        assert!(fleet
            .observe(
                "ass-1",
                fixture("partial startup output"),
                now + Duration::from_secs(1),
                2000
            )
            .is_none());
        assert_eq!(
            fleet.entries["ass-1"].attention.as_ref().unwrap().state,
            AgentState::Unknown
        );
        fleet.observe(
            "ass-1",
            fixture("• Working (1s • esc to interrupt)"),
            now + Tracker::STARTUP_GRACE + Duration::from_secs(1),
            5000,
        );
        assert_eq!(
            fleet.entries["ass-1"].attention.as_ref().unwrap().state,
            AgentState::Working
        );
    }

    #[test]
    fn stable_blocker_evidence_refresh_keeps_request_sequence() {
        let now = Instant::now();
        let mut fleet = fleet(now);
        fleet.observe("ass-1", fixture("ordinary output"), now, 1000);
        let mut blocked = fixture("").unwrap();
        blocked.title = "Action Required".into();
        let request = fleet
            .observe("ass-1", Ok(blocked.clone()), now + SAMPLE_INTERVAL, 2000)
            .unwrap();
        assert!(fleet
            .observe("ass-1", Ok(blocked), now + SAMPLE_INTERVAL * 2, 3000)
            .is_none());
        let cached = fleet.entries["ass-1"].attention.as_ref().unwrap();
        assert_eq!(cached.sequence, request.sequence);
        assert_eq!(cached.kind, Some(AttentionKind::Request));
    }

    #[test]
    fn exit_releases_identity_and_unrelated_jobs_cannot_replay_stale_title() {
        let now = Instant::now();
        let mut fleet = fleet(now);
        fleet.observe(
            "ass-1",
            fixture("• Working (1s • esc to interrupt)"),
            now,
            1000,
        );
        let mut shell = fixture("old permission prompt").unwrap();
        shell.agent_process = None;
        shell.process_exited = true;
        shell.title = "Action Required".into();
        shell.foreground_command = "bash".into();
        let done = fleet
            .observe("ass-1", Ok(shell.clone()), now + SAMPLE_INTERVAL, 2000)
            .unwrap();
        assert_eq!(done.state, AgentState::Idle);
        assert_eq!(done.kind, Some(AttentionKind::Done));
        shell.foreground_command = "sleep".into();
        let released = fleet
            .observe("ass-1", Ok(shell.clone()), now + SAMPLE_INTERVAL * 2, 3000)
            .unwrap();
        assert_eq!(released.state, AgentState::Unknown);
        assert_eq!(released.kind, None);
        for index in 3..6 {
            assert!(fleet
                .observe(
                    "ass-1",
                    Ok(shell.clone()),
                    now + SAMPLE_INTERVAL * index,
                    1000 + index as u64 * 1000
                )
                .is_none());
        }
        assert!(fleet.entries["ass-1"].process.is_none());
        assert_eq!(
            fleet.entries["ass-1"].attention.as_ref().unwrap().sequence,
            released.sequence
        );
    }

    #[test]
    fn replacement_resets_pending_idle_and_starts_grace_at_acquisition() {
        let now = Instant::now();
        let mut fleet = fleet(now);
        fleet.observe(
            "ass-1",
            fixture("• Working (1s • esc to interrupt)"),
            now,
            1000,
        );
        fleet.observe(
            "ass-1",
            fixture("ordinary output"),
            now + SAMPLE_INTERVAL,
            2000,
        );
        assert!(fleet.entries["ass-1"].tracker.needs_recheck());
        let mut replacement = fixture("new agent").unwrap();
        // Same PID, different start token is a new generation too.
        replacement.agent_process.as_mut().unwrap().started_at = "replacement".into();
        replacement.title = "Action Required".into();
        let acquired = now + SAMPLE_INTERVAL * 2;
        let unknown = fleet
            .observe("ass-1", Ok(replacement.clone()), acquired, 3000)
            .unwrap();
        assert_eq!(unknown.state, AgentState::Unknown);
        assert_eq!(unknown.kind, None);
        assert!(!fleet.entries["ass-1"].tracker.needs_recheck());
        assert!(fleet
            .observe(
                "ass-1",
                Ok(replacement.clone()),
                acquired + SAMPLE_INTERVAL * 2,
                5000
            )
            .is_none());
        let request = fleet
            .observe(
                "ass-1",
                Ok(replacement),
                acquired + Tracker::STARTUP_GRACE,
                6000,
            )
            .unwrap();
        assert_eq!(request.state, AgentState::Blocked);
        assert_eq!(request.kind, Some(AttentionKind::Request));
    }

    #[test]
    fn reacquisition_quarantines_previous_screen_and_osc_until_they_change() {
        let now = Instant::now();
        let mut fleet = fleet(now);
        fleet.observe(
            "ass-1",
            fixture("• Working (1s • esc to interrupt)"),
            now,
            1000,
        );
        let mut old = fixture("old permission prompt").unwrap();
        old.agent_process = None;
        old.process_exited = true;
        old.title = "Action Required".into();
        fleet.observe("ass-1", Ok(old.clone()), now + SAMPLE_INTERVAL, 2000);
        fleet.observe("ass-1", Ok(old.clone()), now + SAMPLE_INTERVAL * 2, 3000);
        old.agent_process = Some(AgentProcess {
            pid: 202,
            started_at: "replacement".into(),
        });
        old.process_exited = false;
        let acquired = now + SAMPLE_INTERVAL * 3;
        assert!(fleet
            .observe("ass-1", Ok(old.clone()), acquired, 4000)
            .is_none());
        assert!(fleet
            .observe(
                "ass-1",
                Ok(old.clone()),
                acquired + Tracker::STARTUP_GRACE,
                7000
            )
            .is_none());
        assert_eq!(
            fleet.entries["ass-1"].attention.as_ref().unwrap().state,
            AgentState::Unknown
        );
        old.screen = "• Working (1s • esc to interrupt)".into();
        let working = fleet
            .observe(
                "ass-1",
                Ok(old.clone()),
                acquired + SAMPLE_INTERVAL * 4,
                8000,
            )
            .unwrap();
        assert_eq!(working.state, AgentState::Working);
        assert_eq!(working.kind, None); // retained Action Required was ignored
        old.title.clear();
        fleet.observe(
            "ass-1",
            Ok(old.clone()),
            acquired + SAMPLE_INTERVAL * 5,
            9000,
        );
        old.title = "Action Required".into();
        let request = fleet
            .observe("ass-1", Ok(old), acquired + SAMPLE_INTERVAL * 6, 10000)
            .unwrap();
        assert_eq!(request.kind, Some(AttentionKind::Request));
    }

    #[test]
    fn title_changes_during_grace_release_quarantine_before_returning_to_old_value() {
        let now = Instant::now();
        let mut fleet = fleet(now);
        let mut old = fixture("same prompt screen").unwrap();
        old.title = "Action Required".into();
        fleet.observe("ass-1", Ok(old.clone()), now, 1000);
        let mut next = old.clone();
        next.agent_process.as_mut().unwrap().started_at = "replacement".into();
        let acquired = now + SAMPLE_INTERVAL;
        fleet.observe("ass-1", Ok(next.clone()), acquired, 2000);
        next.title = "Codex".into();
        assert!(fleet
            .observe("ass-1", Ok(next.clone()), acquired + SAMPLE_INTERVAL, 3000)
            .is_none());
        next.title = "Action Required".into();
        let request = fleet
            .observe("ass-1", Ok(next), acquired + Tracker::STARTUP_GRACE, 5000)
            .unwrap();
        assert_eq!(request.state, AgentState::Blocked);
        assert_eq!(request.kind, Some(AttentionKind::Request));
    }

    #[test]
    fn configured_detector_owns_bell_before_its_first_capture() {
        let placeholder = attention_for("ass-absent", "codex").unwrap();
        assert_eq!(placeholder.state, AgentState::Unknown);
        assert_eq!(placeholder.kind, None);
        assert_eq!(placeholder.changed_at, 0);
        assert!(placeholder.sequence.ends_with(":0"));
        assert!(attention_for("ass-absent", "shell").is_none());
    }
}
