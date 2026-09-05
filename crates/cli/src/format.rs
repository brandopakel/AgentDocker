//! Terminal output helpers.

use agentdocker_core::{Envelope, Event, EventKind, ResourceKey};
use chrono::{DateTime, Local, Utc};
use serde_json::Value;

/// Print rows as left-aligned columns, `docker ps` style.
pub fn table(headers: &[&str], rows: &[Vec<String>]) {
    table_dimming(headers, rows, |_| false);
}

/// Like [`table`], with rows for which `dim` is true rendered faint when
/// stdout is a terminal (and plainly otherwise, so pipes see clean text).
pub fn table_dimming(headers: &[&str], rows: &[Vec<String>], dim: impl Fn(usize) -> bool) {
    use std::io::IsTerminal;

    let mut widths: Vec<usize> = headers.iter().map(|h| h.len()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i < widths.len() {
                widths[i] = widths[i].max(cell.chars().count());
            }
        }
    }
    let render = |cells: Vec<&str>| {
        let line: Vec<String> = cells
            .iter()
            .enumerate()
            .map(|(i, cell)| format!("{:<width$}", cell, width = widths[i]))
            .collect();
        line.join("   ").trim_end().to_owned()
    };
    let tty = std::io::stdout().is_terminal();
    println!("{}", render(headers.to_vec()));
    for (i, row) in rows.iter().enumerate() {
        let line = render(row.iter().map(String::as_str).collect());
        if tty && dim(i) {
            println!("\x1b[2m{line}\x1b[0m");
        } else {
            println!("{line}");
        }
    }
}

/// "5s ago", "3m ago", "2h ago", "4d ago".
pub fn ago(at: DateTime<Utc>) -> String {
    let secs = (Utc::now() - at).num_seconds().max(0);
    format!("{} ago", span(secs))
}

/// "in 4m", or "expired".
pub fn until(at: DateTime<Utc>) -> String {
    let secs = (at - Utc::now()).num_seconds();
    if secs <= 0 {
        "expired".to_owned()
    } else {
        format!("in {}", span(secs))
    }
}

pub fn span_secs(secs: u64) -> String {
    span(i64::try_from(secs).unwrap_or(i64::MAX))
}

fn span(secs: i64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

/// First 12 characters of an id-like string, without splitting a char.
fn short(s: &str) -> &str {
    let end = s.char_indices().nth(12).map_or(s.len(), |(i, _)| i);
    &s[..end]
}

/// `file:` keys carry a full project id; show twelve characters of it.
pub fn resource(key: &ResourceKey) -> String {
    if key.kind() != "file" {
        return key.to_string();
    }
    let (project, rest) = key
        .value()
        .split_once('/')
        .map_or((key.value(), ""), |(p, r)| (p, r));
    if rest.is_empty() {
        format!("file:{}", short(project))
    } else {
        format!("file:{}/{rest}", short(project))
    }
}

pub fn clock(at: DateTime<Utc>) -> String {
    at.with_timezone(&Local).format("%H:%M:%S").to_string()
}

/// `{"text": "..."}` payloads print as plain text; anything else as JSON.
pub fn payload_text(payload: &Value) -> String {
    match payload.get("text").and_then(Value::as_str) {
        Some(text) if payload.as_object().is_some_and(|o| o.len() == 1) => text.to_owned(),
        _ => payload.to_string(),
    }
}

pub fn message_line(message: &Envelope) -> String {
    let from = short(&message.from);
    let reply = message
        .reply_to
        .as_ref()
        .map(|id| format!(" re:{id}"))
        .unwrap_or_default();
    format!(
        "{}  {from} → {}  [{}{reply}]  {}",
        clock(message.sent_at),
        message.to,
        message.kind,
        payload_text(&message.payload)
    )
}

pub fn event_line(event: &Event) -> String {
    let body = match &event.kind {
        EventKind::WorktreeCreated { agent, path } => {
            format!("{agent} created worktree {}", path.display())
        }
        EventKind::IntegrationPrepared {
            agent,
            source_head,
            clean,
        } => format!("{agent} prepared integration of {source_head}, clean={clean}"),
        EventKind::AccessGranted { agent, grant } => {
            format!("{agent} received restricted access {grant}")
        }
        EventKind::AccessRevoked { grant } => format!("restricted access {grant} revoked"),
        EventKind::CheckpointSaved { agent, checkpoint } => {
            format!("{agent} saved checkpoint {checkpoint}")
        }
        EventKind::HandoffAccepted { agent, checkpoint } => {
            format!("{agent} accepted handoff {checkpoint}")
        }
        EventKind::ValidationFinished {
            agent,
            validation,
            passed,
        } => format!("{agent} validation {validation} passed={passed}"),
        EventKind::WatcherGap { reason } => {
            format!("watcher coverage gap: {reason}; verify content with stale")
        }
        EventKind::ReadsObserved { agent, paths } => {
            format!("{agent} observed {} paths", paths.len())
        }
        EventKind::AgentStale { agent, paths } => format!(
            "{agent} has stale context for {} paths; reread",
            paths.len()
        ),

        EventKind::InboxAcknowledged { agent, messages } => {
            format!("{agent} acknowledged {} messages", messages.len())
        }
        EventKind::AgentCreated {
            agent,
            name,
            project,
        } => {
            let project = project
                .as_ref()
                .map(|p| format!(" in {}", p.short()))
                .unwrap_or_default();
            format!("agent created    {} ({name}){project}", agent.short())
        }
        EventKind::AgentStarted { agent, pid } => {
            let pid = pid.map(|p| format!(" pid {p}")).unwrap_or_default();
            format!("agent started    {}{pid}", agent.short())
        }
        EventKind::AgentStopping { agent, force } => {
            format!("agent stopping   {} force={force}", agent.short())
        }
        EventKind::AgentExited { agent, status } => {
            format!("agent exited     {} {status}", agent.short())
        }
        EventKind::AgentRemoved { agent } => format!("agent removed    {}", agent.short()),
        EventKind::MessageSent {
            message,
            from,
            to,
            kind,
        } => {
            format!("message sent     {message} {} → {to} [{kind}]", short(from))
        }
        EventKind::LeaseClaimed { lease } => format!(
            "lease claimed    {} {} {} by {}",
            lease.id,
            lease.mode,
            resource(&lease.resource),
            lease.holder.short()
        ),
        EventKind::LeaseRenewed { lease } => format!(
            "lease renewed    {} {} by {}",
            lease.id,
            resource(&lease.resource),
            lease.holder.short()
        ),
        EventKind::LeaseReleased { lease } => format!(
            "lease released   {} {} by {}",
            lease.id,
            resource(&lease.resource),
            lease.holder.short()
        ),
        EventKind::LeaseExpired { lease } => format!(
            "lease expired    {} {} held by {}",
            lease.id,
            resource(&lease.resource),
            lease.holder.short()
        ),
        EventKind::LeaseConflict {
            resource,
            requester,
            held_by,
        } => {
            let holders: Vec<&str> = held_by.iter().map(|h| h.short()).collect();
            format!(
                "lease conflict   {} wanted by {} held by {}",
                self::resource(resource),
                requester.short(),
                holders.join(", ")
            )
        }
        EventKind::ProjectDiscovered { project } => format!(
            "project found    {} {} ({})",
            project.id().short(),
            project.root.display(),
            project.name()
        ),
        EventKind::FileChanged { change } => {
            let by = change
                .by
                .agent()
                .map_or_else(|| "external".to_owned(), |a| a.short().to_owned());
            format!(
                "file {:<9} {} by {by} [{}]",
                change.kind.to_string(),
                change.path.display(),
                change.project.short()
            )
        }
        EventKind::AgentVcsChanged { agent, vcs } => {
            format!("checkout moved   {} {}", agent.short(), vcs.describe())
        }
        EventKind::DaemonStopping { reason } => format!("daemon stopping  ({reason})"),
    };
    format!("{}  {}", clock(event.at), single_line(&body))
}

/// Escape control characters so untrusted metadata cannot create terminal commands or extra rows.
fn single_line(text: &str) -> String {
    text.chars()
        .flat_map(|c| {
            if c.is_control() {
                c.escape_default().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn control_characters_cannot_inject_event_rows() {
        let event = agentdocker_core::Event::new(
            agentdocker_core::EventKind::AgentVcsChanged {
                agent: "test".into(),
                vcs: agentdocker_core::VcsState {
                    branch: Some("x\n\u{1b}[2J".into()),
                    head: None,
                    dirty: None,
                    updated_at: chrono::Utc::now(),
                },
            },
            chrono::Utc::now(),
        );
        let text = super::event_line(&event);
        assert!(!text.chars().any(char::is_control));
        assert!(text.contains("\\n"));
    }
}
