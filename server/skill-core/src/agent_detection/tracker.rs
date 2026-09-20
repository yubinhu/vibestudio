// Ported from Herdr, Copyright Herdr contributors, Apache-2.0.
// Upstream: 4b5e9bda239a0b6903889062d756424578e94691; see server/skill-core/src/agent_detection/NOTICE.txt.

use std::time::Instant;

use super::stabilization::{
    decide_detection_transition, stable_visible_signal_refresh_due, DetectionPublishState,
    DetectionTransitionDecision, DetectionTransitionInput, PendingIdleConfirmation,
};
use super::{AgentState, Detection};

/// A confirmed effective-state transition that deserves attention. Matches the
/// upstream NeedsAttention/Finished toast distinction; presentation stays local.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionKind {
    Request,
    Done,
}

#[derive(Debug, Clone)]
pub struct Transition {
    pub state: AgentState,
    pub attention: Option<AttentionKind>,
    pub detection: Detection,
}

/// Per-terminal detection publication state. The owning host samples its tmux
/// pane; this transport-independent tracker never reads files or starts timers.
///
/// The first observation seeds current state without replaying old attention.
/// Thereafter the pending-idle and repeated-signal policy follows the upstream
/// policy from pane/agent_detection.rs. The caller should sample again after
/// RECHECK_INTERVAL while needs_recheck() is true.
#[derive(Debug, Default)]
pub struct Tracker {
    previous: Option<DetectionPublishState>,
    agent: Option<String>,
    pending_idle: PendingIdleConfirmation,
    last_visible_signal_refresh: Option<Instant>,
}

impl Tracker {
    pub const RECHECK_INTERVAL: std::time::Duration =
        super::stabilization::AGENT_PENDING_IDLE_RECHECK;
    pub const STARTUP_GRACE: std::time::Duration = super::stabilization::AGENT_STARTUP_GRACE_WINDOW;

    pub fn new() -> Self {
        Self::default()
    }

    pub fn state(&self) -> Option<AgentState> {
        self.previous.map(|previous| previous.state)
    }

    pub fn needs_recheck(&self) -> bool {
        self.pending_idle.active()
    }

    /// `process_exited` must mean the agent process has exited, not an SSH/SSE
    /// attachment disconnect. As upstream, a real exit overrides the screen
    /// with visible idle, even if a transcript viewer is still displayed.
    pub fn observe(
        &mut self,
        mut detection: Detection,
        now: Instant,
        process_exited: bool,
    ) -> Option<Transition> {
        if process_exited {
            detection.state = AgentState::Idle;
            detection.visible_idle = true;
            detection.visible_blocker = false;
            detection.visible_working = false;
            detection.skip_state_update = false;
            detection.skipped_update_reason = None;
            detection.matched_rule = None;
            detection.fallback_reason = Some("process_exited".into());
        }
        if detection.skip_state_update {
            self.pending_idle.clear();
            return None;
        }

        let next = DetectionPublishState {
            state: detection.state,
            visible_idle: detection.visible_idle && detection.state == AgentState::Idle,
            visible_blocker: detection.visible_blocker && detection.state == AgentState::Blocked,
            visible_working: detection.visible_working && detection.state == AgentState::Working,
        };
        let Some(previous) = self.previous else {
            self.record(next, &detection, now);
            return Some(Transition {
                state: next.state,
                attention: None,
                detection,
            });
        };
        let agent_changed = self.agent != detection.agent;
        let stable_refresh_due = stable_visible_signal_refresh_due(
            previous,
            next,
            self.last_visible_signal_refresh,
            now,
        );
        if decide_detection_transition(
            DetectionTransitionInput {
                previous_publish: previous,
                next_publish: next,
                agent_changed,
                process_exited,
                stable_refresh_due,
                now,
            },
            &mut self.pending_idle,
        ) == DetectionTransitionDecision::NoPublish
        {
            return None;
        }

        // Upstream app/actions.rs: notification_toast_for_effective_state_change
        // and is_completion_transition_parts, with UI-specific suppression left
        // to the client. Same-state evidence refreshes never re-notify.
        let attention = if next.state == previous.state {
            None
        } else {
            match next.state {
                AgentState::Blocked => Some(AttentionKind::Request),
                AgentState::Idle
                    if matches!(previous.state, AgentState::Working | AgentState::Blocked)
                        || (previous.state == AgentState::Unknown
                            && self.agent.is_some()
                            && self.agent == detection.agent) =>
                {
                    Some(AttentionKind::Done)
                }
                _ => None,
            }
        };
        self.record(next, &detection, now);
        Some(Transition {
            state: next.state,
            attention,
            detection,
        })
    }

    fn record(&mut self, state: DetectionPublishState, detection: &Detection, now: Instant) {
        self.previous = Some(state);
        self.agent.clone_from(&detection.agent);
        self.last_visible_signal_refresh =
            (state.visible_blocker || state.visible_working).then_some(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_detection::detect;
    use std::time::Duration;

    #[test]
    fn initial_state_seeds_without_replaying_attention() {
        for title in ["project", "⠋ project", "Action Required"] {
            let first = Tracker::new()
                .observe(detect("codex", "", title, ""), Instant::now(), false)
                .unwrap();
            assert_eq!(first.attention, None);
        }
    }

    #[test]
    fn request_and_done_are_distinct_deduplicated_transitions() {
        let now = Instant::now();
        for (title, initial_state) in [
            ("project", AgentState::Idle),
            ("⠋ project", AgentState::Working),
        ] {
            let mut tracker = Tracker::new();
            tracker.observe(detect("codex", "", title, ""), now, false);
            assert_eq!(tracker.state(), Some(initial_state));
            let blocked = detect("codex", "", "Action Required", "");
            let request = tracker.observe(blocked.clone(), now, false).unwrap();
            assert_eq!(request.state, AgentState::Blocked);
            assert!(request.detection.visible_blocker);
            assert_eq!(request.attention, Some(AttentionKind::Request));
            assert!(tracker.observe(blocked.clone(), now, false).is_none());
            let refresh = tracker
                .observe(blocked, now + Duration::from_millis(800), false)
                .unwrap();
            assert_eq!(refresh.attention, None);
            let done = tracker
                .observe(
                    detect("codex", "", "project", ""),
                    now + Duration::from_secs(1),
                    false,
                )
                .unwrap();
            assert_eq!(done.attention, Some(AttentionKind::Done));
        }
    }

    #[test]
    fn upstream_known_agent_idle_fallback_waits_three_confirmations() {
        let now = Instant::now();
        let mut tracker = Tracker::new();
        tracker.observe(detect("codex", "", "⠋ project", ""), now, false);
        for millis in [0, 100, 200] {
            assert!(tracker
                .observe(
                    detect("codex", "", "", ""),
                    now + Duration::from_millis(millis),
                    false
                )
                .is_none());
            assert!(tracker.needs_recheck());
            assert_eq!(tracker.state(), Some(AgentState::Working));
        }
        let done = tracker
            .observe(
                detect("codex", "", "", ""),
                now + Duration::from_millis(300),
                false,
            )
            .unwrap();
        assert_eq!(done.attention, Some(AttentionKind::Done));
        assert!(!done.detection.visible_idle);
        assert!(!tracker.needs_recheck());
    }

    #[test]
    fn pending_idle_cap_and_confirmed_idle_match_upstream() {
        let now = Instant::now();
        for (plain_idle_first, delay, title) in [
            (true, 700, ""),
            (true, 1, "project"),
            (false, 0, "project"),
        ] {
            let mut tracker = Tracker::new();
            tracker.observe(detect("codex", "", "⠋ project", ""), now, false);
            if plain_idle_first {
                tracker.observe(detect("codex", "", "", ""), now, false);
            }
            let done = tracker
                .observe(
                    detect("codex", "", title, ""),
                    now + Duration::from_millis(delay),
                    false,
                )
                .unwrap();
            assert_eq!(done.attention, Some(AttentionKind::Done));
            assert_eq!(tracker.state(), Some(AgentState::Idle));
            assert_eq!(done.detection.visible_idle, !title.is_empty());
            assert!(!tracker.needs_recheck());
        }
    }

    #[test]
    fn transcript_viewer_preserves_live_state_but_process_exit_wins() {
        let now = Instant::now();
        let mut tracker = Tracker::new();
        tracker.observe(detect("codex", "", "⠋ project", ""), now, false);
        let viewer = detect(
            "codex",
            "› transcript\n↑/↓ to scroll · pgup/pgdn to move · home/end to jump · q to quit · esc to edit prev",
            "project",
            "",
        );
        assert!(viewer.skip_state_update);
        assert!(tracker.observe(viewer.clone(), now, false).is_none());
        assert_eq!(tracker.state(), Some(AgentState::Working));
        assert_eq!(
            tracker.observe(viewer, now, true).unwrap().attention,
            Some(AttentionKind::Done)
        );
    }

    #[test]
    fn interrupted_candidate_does_not_accumulate_idle_confirmations() {
        let now = Instant::now();
        let mut tracker = Tracker::new();
        tracker.observe(detect("codex", "", "⠋ project", ""), now, false);
        tracker.observe(detect("codex", "", "", ""), now, false);
        tracker.observe(detect("codex", "", "⠋ project", ""), now, false);
        assert!(!tracker.needs_recheck());
        assert!(tracker
            .observe(detect("codex", "", "", ""), now, false)
            .is_none());
        assert!(tracker.needs_recheck());
    }
}
