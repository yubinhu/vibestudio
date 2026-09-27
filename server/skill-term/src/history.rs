//! Open an agent's saved conversation in the existing tmux terminal workflow.
//! Discovery belongs to skill-core; this layer adds executable/live-terminal
//! availability and owns the exact-ID launch. It never copies transcripts.

use serde::Serialize;
use skill_core::agents::{by_family, ExactResumeCtx};
use skill_core::session_history::{self, HistoryEntry};

use crate::{AgentOption, SessionInfo};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistorySession {
    #[serde(flatten)]
    entry: HistoryEntry,
    can_resume: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    resume_unavailable_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    active_terminal_id: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryPage {
    sessions: Vec<HistorySession>,
    warnings: Vec<String>,
    has_more: bool,
    truncated: bool,
}

pub fn list(
    query: &str,
    agent: Option<&str>,
    offset: usize,
    limit: usize,
) -> Result<HistoryPage, String> {
    let mut page = session_history::list(query, agent, offset, limit)?;
    let (live, inventory_error) = match resumable_terminals() {
        Ok(live) => (live, None),
        Err(error) => {
            page.warnings.push(error.clone());
            (vec![], Some(error))
        }
    };
    // Metadata-only discovery refreshes installs/removals without launching
    // version probes each time the user searches their saved conversations.
    let agents = crate::compute_agents(false);
    let sessions = page.sessions.into_iter().map(|entry| {
        let active = matching_session(&live, &entry.agent, &entry.session_id);
        let reason = if active.is_some() { None } else {
            inventory_error.clone().or_else(|| unavailable_reason(&entry, &agents))
        };
        HistorySession {
            active_terminal_id: active.map(|session| session.id.clone()),
            can_resume: reason.is_none(),
            resume_unavailable_reason: reason,
            entry,
        }
    }).collect();
    Ok(HistoryPage {
        sessions,
        warnings: page.warnings,
        has_more: page.has_more,
        truncated: page.truncated,
    })
}

/// Serialize lookup + creation across host processes. Retrying a request after
/// its response was lost opens the existing native conversation terminal.
pub fn resume(agent: &str, session_id: &str) -> Result<SessionInfo, String> {
    let def = by_family(agent).filter(|def| def.family == agent)
        .ok_or("Unknown history provider.")?;
    let capability = def.history.as_ref().ok_or("This agent does not support session history yet.")?;
    // Resolve from the provider's saved metadata, never a client-provided cwd.
    let entry = session_history::find(agent, session_id)?;
    let lock_path = skill_core::paths::ensure_config_dir()?.join("history-resume.lock");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options.open(lock_path).map_err(|error| error.to_string())?;
    lock.lock().map_err(|error| error.to_string())?;
    let live = resumable_terminals()?;
    if let Some(existing) = matching_session(&live, agent, session_id) {
        return Ok(existing.clone());
    }
    let agents = crate::compute_agents(false);
    if let Some(reason) = unavailable_reason(&entry, &agents) {
        return Err(reason);
    }
    let opt = agent_option(&agents, agent).ok_or("The agent is no longer installed on this host.")?;
    let command = (capability.resume)(&ExactResumeCtx { bin: &opt.bin, session_id });
    crate::create_session_inner(opt, &entry.cwd, 80, 24, command, Some(session_id))
}

fn matching_session<'a>(live: &'a [SessionInfo], agent: &str, id: &str) -> Option<&'a SessionInfo> {
    live.iter().find(|session| session.agent == agent && session.session_id == id)
}

fn resumable_terminals() -> Result<Vec<SessionInfo>, String> {
    let mut live = crate::list_sessions_result()?;
    crate::resolve_codex_session_ids(&mut live);
    live.retain(|session| {
        if session.session_id.is_empty() { return false; }
        let marker = crate::tmux().args([
            "show-options", "-t", &session.id, "-v", "@ass_agent_exited",
        ]).output().ok();
        match marker.as_ref().filter(|output| output.status.success())
            .and_then(|output| std::str::from_utf8(&output.stdout).ok()).map(str::trim) {
            // Set before the window starts, so immediate retries cannot spawn
            // a duplicate while the login shell is still launching the agent.
            Some("0") => true,
            Some("1") => false,
            // Terminals created by earlier versions have no lifecycle marker.
            _ => !crate::agent_exited(&session.id),
        }
    });
    Ok(live)
}

fn agent_option<'a>(agents: &'a [AgentOption], family: &str) -> Option<&'a AgentOption> {
    agents.iter().filter(|agent| agent.agent == family)
        .min_by_key(|agent| agent.flavor != "cli")
}

fn unavailable_reason(entry: &HistoryEntry, agents: &[AgentOption]) -> Option<String> {
    if agent_option(agents, &entry.agent).is_none() {
        return Some("Install this agent on the connected host to resume this session.".into());
    }
    if !std::path::Path::new(&entry.cwd).is_dir() {
        return Some("This session's working folder is no longer available.".into());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_match_requires_provider_and_exact_id_not_shared_directory() {
        let make = |id: &str, agent: &str, native: &str| SessionInfo {
            id: id.into(), label: "Same title".into(), agent: agent.into(),
            cwd: "/shared".into(), created: "1".into(), activity: "2".into(),
            bell_at: "0".into(), session_id: native.into(),
        };
        let live = vec![make("first", "claude", "same"), make("second", "codex", "same")];
        assert_eq!(matching_session(&live, "codex", "same").unwrap().id, "second");
        assert!(matching_session(&live, "codex", "other").is_none());
    }
}
