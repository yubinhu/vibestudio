//! Terminal lifecycle events over SSE (`GET /api/events`) — the push channel the
//! agent-attention notifier rides. An owning-host watcher samples visible tmux
//! screens through the bundled Herdr detector, independently of attachments.
//! Terminal bells remain a fallback for agents without a detector. Events are HINTS, not state: the
//! client re-fetches `/api/terminal/list` on (re)connect and on every event, so
//! there is no replay buffer and a missed frame costs nothing.
//!
//! The watcher also feeds `push::notify_bells` on every bell edge — Web Push
//! must fire precisely when NO browser is connected, so it runs from server
//! boot ([`start`]) and never pauses.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Mutex, MutexGuard, Once, OnceLock};
use std::time::Duration;
use std::time::Instant;

use serde_json::json;
use skill_term::SessionInfo;

mod detection;
pub(crate) use detection::attention_for;
use detection::{detector_for, DetectorFleet, SessionAttention};

/// Watcher cadence: one `tmux list-sessions` per tick, which also bounds the
/// bell → SSE-push latency.
const TICK: Duration = Duration::from_secs(1);
// Only pending ambiguous Idle confirmations sample this frequently. Normal
// sessions remain on a one-second schedule with unchanged-screen caching.
const RECHECK_TICK: Duration = Duration::from_millis(100);

fn subscribers() -> MutexGuard<'static, Vec<Sender<String>>> {
    static SUBS: OnceLock<Mutex<Vec<Sender<String>>>> = OnceLock::new();
    SUBS.get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

/// Start the watcher (idempotent). Called at server boot so bell edges reach
/// Web Push subscribers with zero browsers connected.
pub(crate) fn start() {
    static START: Once = Once::new();
    START.call_once(|| {
        std::thread::spawn(watcher_loop);
    });
}

/// Register a stream, and push a comment frame through the registry so senders
/// whose stream died between events get pruned even on a quiet server.
pub(crate) fn subscribe() -> Receiver<String> {
    start();
    let (tx, rx) = mpsc::channel();
    subscribers().push(tx);
    emit(": sub\n\n".to_string());
    rx
}

/// Fan a pre-framed SSE string out to every subscriber, pruning dead senders.
fn emit(frame: String) {
    subscribers().retain(|tx| tx.send(frame.clone()).is_ok());
}

/// One SSE frame: a named event plus one JSON data line, so the client demuxes
/// with `EventSource.addEventListener(<event>, …)`.
fn frame(event: &str, data: &serde_json::Value) -> String {
    format!("event: {event}\ndata: {data}\n\n")
}

fn payload(s: &SessionInfo, last: Option<&str>) -> serde_json::Value {
    let mut v =
        json!({ "id": s.id, "label": s.label, "agent": s.agent, "cwd": s.cwd, "at": s.bell_at });
    // Only bells carry a preview of the agent's last line (opened/closed pass None).
    if let Some(last) = last {
        v["last"] = json!(last);
    }
    v
}

fn attention_payload(
    s: &SessionInfo,
    attention: &SessionAttention,
    last: Option<&str>,
) -> serde_json::Value {
    let mut value = payload(s, last);
    value["at"] = json!((attention.changed_at / 1000).to_string());
    value["attention"] = json!(attention);
    value
}

fn bell_secs(s: &SessionInfo) -> u64 {
    s.bell_at.trim().parse().unwrap_or(0)
}

/// Sessions whose bell advanced between two snapshots — present in BOTH: one
/// that arrives already-belled is just `opened`, and its stale bell must not
/// re-announce (or re-push) on a server restart.
pub(crate) fn bell_edges<'a>(
    prev: &HashMap<String, SessionInfo>,
    now: &'a [SessionInfo],
) -> Vec<&'a SessionInfo> {
    now.iter()
        .filter(|s| prev.get(&s.id).is_some_and(|p| bell_secs(s) > bell_secs(p)))
        .collect()
}

fn fallback_bell_edges<'a>(
    prev: &HashMap<String, SessionInfo>,
    now: &'a [SessionInfo],
) -> Vec<&'a SessionInfo> {
    bell_edges(prev, now)
        .into_iter()
        .filter(|session| detector_for(&session.agent).is_none())
        .collect()
}

fn notification(
    session: &SessionInfo,
    kind: Option<skill_core::agent_detection::AttentionKind>,
) -> crate::push::Bell {
    let created = session.created.trim().parse().unwrap_or(0);
    let sid = Some(session.session_id.as_str()).filter(|value| !value.is_empty());
    let last = skill_core::agents::last_message_for(&session.agent, &session.cwd, created, sid);
    crate::push::Bell {
        id: session.id.clone(),
        label: session.label.clone(),
        last,
        kind,
    }
}

/// Opened/closed frames between two snapshots. Bell frames are built in the
/// watcher instead ([`watcher_loop`]) — each carries a captured preview of the
/// agent's last line, which needs a tmux read `diff` deliberately stays free of.
pub(crate) fn diff(prev: &HashMap<String, SessionInfo>, now: &[SessionInfo]) -> Vec<String> {
    let mut frames = Vec::new();
    for s in now {
        if !prev.contains_key(&s.id) {
            frames.push(frame("opened", &payload(s, None)));
        }
    }
    for (id, p) in prev {
        if !now.iter().any(|s| &s.id == id) {
            frames.push(frame("closed", &payload(p, None)));
        }
    }
    frames
}

/// Seed silently, then emit edges each tick — continuously from boot: pausing
/// while unsubscribed (as this once did) would blind Web Push exactly when it
/// matters, and a paused-then-resumed snapshot would burst-replay stale edges.
fn watcher_loop() {
    let mut prev: Option<HashMap<String, SessionInfo>> = None;
    let mut detectors = DetectorFleet::new();
    let mut next_list = Instant::now();
    let mut next_prune = Instant::now() + Duration::from_secs(30);
    loop {
        // A dead stream's Sender lingers until a send fails, which a quiet server
        // may never do — periodically push a comment frame purely to prune.
        let now = Instant::now();
        if now >= next_prune {
            emit(": prune\n\n".to_string());
            next_prune = now + Duration::from_secs(30);
        }
        let mut notifications = Vec::new();
        if now >= next_list {
            next_list = now + TICK;
            // Preserve the previous baseline if tmux cannot even be queried.
            if let Some(sessions) = skill_term::list_sessions_checked() {
                detectors.sync_sessions(&sessions, now);
                if let Some(previous) = &prev {
                    for event in diff(previous, &sessions) {
                        emit(event);
                    }
                    for session in fallback_bell_edges(previous, &sessions) {
                        let notice = notification(session, None);
                        emit(frame("bell", &payload(session, notice.last.as_deref())));
                        notifications.push(notice);
                    }
                }
                prev = Some(
                    sessions
                        .into_iter()
                        .map(|session| (session.id.clone(), session))
                        .collect(),
                );
            }
        }
        for (id, attention) in detectors.poll(now) {
            let Some(session) = prev.as_ref().and_then(|sessions| sessions.get(&id)) else {
                continue;
            };
            let notice = attention.kind.map(|kind| notification(session, Some(kind)));
            emit(frame(
                "attention",
                &attention_payload(
                    session,
                    &attention,
                    notice.as_ref().and_then(|n| n.last.as_deref()),
                ),
            ));
            if let Some(notice) = notice {
                notifications.push(notice);
            }
        }
        crate::push::notify_bells(notifications);
        std::thread::sleep(RECHECK_TICK);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sess(id: &str, bell: &str) -> SessionInfo {
        SessionInfo {
            id: id.into(),
            label: format!("Claude Code · {id}"),
            agent: "claude".into(),
            cwd: "/tmp".into(),
            created: "100".into(),
            activity: "200".into(),
            bell_at: bell.into(),
            session_id: String::new(),
        }
    }

    fn snap(list: &[SessionInfo]) -> HashMap<String, SessionInfo> {
        list.iter().map(|s| (s.id.clone(), s.clone())).collect()
    }

    #[test]
    fn new_session_is_opened_never_bell() {
        // Even with a nonzero bell: pre-existing bells are state, not an edge.
        let frames = diff(&HashMap::new(), &[sess("ass-1", "500")]);
        assert_eq!(frames.len(), 1);
        assert!(frames[0].starts_with("event: opened\n"), "{frames:?}");
    }

    #[test]
    fn bell_fires_only_on_increase() {
        // Bells are edges the watcher turns into frames (with a captured preview);
        // `diff` no longer emits them, so assert on `bell_edges` directly.
        let prev = snap(&[sess("ass-1", "500"), sess("ass-2", "0"), sess("ass-3", "")]);
        let now = [
            sess("ass-1", "500"),
            sess("ass-2", "600"),
            sess("ass-3", ""),
        ];
        let edges = bell_edges(&prev, &now);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].id, "ass-2");
        assert_eq!(edges[0].bell_at, "600");
        // diff itself is silent when only a bell advanced (no open/close).
        assert!(diff(&prev, &now).is_empty());
    }

    #[test]
    fn vanished_session_is_closed() {
        let prev = snap(&[sess("ass-1", "0"), sess("ass-2", "0")]);
        let frames = diff(&prev, &[sess("ass-1", "0")]);
        assert_eq!(frames.len(), 1, "{frames:?}");
        assert!(frames[0].starts_with("event: closed\n"));
        assert!(frames[0].contains("\"id\":\"ass-2\""));
    }

    #[test]
    fn unchanged_snapshot_is_silent() {
        let list = [sess("ass-1", "500"), sess("ass-2", "0")];
        assert!(diff(&snap(&list), &list).is_empty());
    }

    #[test]
    fn frames_are_well_formed_sse() {
        let frames = diff(&HashMap::new(), &[sess("ass-1", "0")]);
        assert!(frames[0].ends_with("\n\n"));
        let data_line = frames[0].lines().nth(1).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(data_line.strip_prefix("data: ").unwrap()).unwrap();
        assert_eq!(v["label"], "Claude Code · ass-1");
        assert_eq!(v["agent"], "claude");
    }

    #[test]
    fn detector_sessions_do_not_double_notify_their_terminal_bell() {
        let mut fallback = sess("ass-2", "500");
        fallback.agent = "shell".into();
        let previous = snap(&[sess("ass-1", "0"), {
            let mut s = fallback.clone();
            s.bell_at = "0".into();
            s
        }]);
        let sessions = [sess("ass-1", "500"), fallback];
        let bells = fallback_bell_edges(&previous, &sessions);
        assert_eq!(bells.len(), 1);
        assert_eq!(bells[0].id, "ass-2");
    }
}
