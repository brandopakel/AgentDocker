//! `agentdocker top`: the fleet, live, in a terminal.
//!
//! `ps` and `activity` answer a question once. This answers it
//! continuously, which is a different thing to want: you leave it open
//! beside the work and glance at it, the way you leave `top` open.
//!
//! It is not a TUI framework and does not want to be. The screen is a
//! handful of ANSI escapes — home, clear to end, hide the cursor — and
//! the content is the same tables the one-shot commands print. That
//! keeps one renderer to keep correct instead of two, and it means
//! anything `ps` learns to show, this shows.
//!
//! Redraws are driven by the daemon's own event stream rather than a
//! timer, so a lease taken or an agent blocked appears at once; a slow
//! tick underneath it keeps relative times ("3m ago") honest when
//! nothing is happening.

use std::io::{IsTerminal, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use agentdocker_core::{Activity, AgentActivity, AgentRecord, Lease, Request, Response};
use anyhow::Result;

use crate::client::Client;
use crate::format;

/// How often the screen is redrawn when nothing is happening, so the
/// "seen 3m ago" column stays true.
const TICK: std::time::Duration = std::time::Duration::from_secs(2);

/// Home the cursor and clear what was there. Not a full clear: repainting
/// over the old frame avoids the flicker a clear-then-draw gives.
const HOME: &str = "\x1b[H\x1b[J";
const HIDE_CURSOR: &str = "\x1b[?25l";
const SHOW_CURSOR: &str = "\x1b[?25h";

pub async fn run(client: &Client) -> Result<()> {
    let interactive = std::io::stdout().is_terminal();
    if !interactive {
        // Piped: draw once and leave, so `top | head` is not a hang.
        let frame = frame(client).await?;
        print!("{frame}");
        return Ok(());
    }
    // The cursor comes back however this ends — Ctrl-C included.
    let _cursor = CursorGuard::new();
    let dirty = Arc::new(AtomicBool::new(true));

    // Events only mark the screen dirty; the draw happens on this task,
    // so a burst of events is one repaint rather than fifty.
    let watching = {
        let client = client.clone();
        let dirty = dirty.clone();
        tokio::spawn(async move {
            let _ = client
                .stream(
                    &Request::Events {
                        replay: 0,
                        ready: false,
                    },
                    move |response| {
                        if matches!(response, Response::Event { .. }) {
                            dirty.store(true, Ordering::Relaxed);
                        }
                        Ok(true)
                    },
                )
                .await;
        })
    };

    loop {
        // Redrawn on every tick as well as on every event: relative
        // times go stale on their own, so "nothing happened" is still a
        // reason to repaint.
        dirty.store(false, Ordering::Relaxed);
        let frame = frame(client).await?;
        {
            let mut out = std::io::stdout().lock();
            write!(out, "{HOME}{frame}")?;
            out.flush()?;
        }
        // Wake early when the daemon says something changed, so a lease
        // taken or an agent blocked shows at once rather than in two
        // seconds.
        let woken = async {
            while !dirty.load(Ordering::Relaxed) {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            () = tokio::time::sleep(TICK) => {}
            () = woken => {}
        }
    }
    watching.abort();
    Ok(())
}

/// One screen's worth: what is running, grouped by project, and what is
/// held or waited on underneath it.
async fn frame(client: &Client) -> Result<String> {
    let agents = match client
        .call(&Request::List {
            all: false,
            project: None,
            labels: Default::default(),
        })
        .await?
    {
        Response::Agents { agents, .. } => agents,
        _ => Vec::new(),
    };
    let activity = match client
        .call(&Request::Activity {
            agent: None,
            project: None,
            all: false,
        })
        .await
    {
        Ok(Response::Activity { activity }) => activity,
        _ => Vec::new(),
    };
    let leases = match client
        .call(&Request::Leases {
            agent: None,
            resource: None,
        })
        .await
    {
        Ok(Response::Leases { leases }) => leases,
        _ => Vec::new(),
    };
    let waiting = match client.call(&Request::Waiting).await {
        Ok(Response::Waiting { waiting }) => waiting,
        _ => Vec::new(),
    };
    Ok(render(&agents, &activity, &leases, &waiting))
}

/// The frame as text. Pure, so what this shows can be tested without a
/// daemon or a terminal.
pub fn render(
    agents: &[AgentRecord],
    activity: &[AgentActivity],
    leases: &[Lease],
    waiting: &[agentdocker_core::Waiter],
) -> String {
    use std::collections::BTreeMap;
    use std::fmt::Write as _;

    let mut out = String::new();
    let blocked = activity
        .iter()
        .filter(|a| matches!(a.activity, Activity::Blocked { .. }))
        .count();
    let _ = writeln!(
        out,
        "{} agent(s) · {} lease(s) · {} waiting{}",
        agents.len(),
        leases.len(),
        waiting.len(),
        if blocked > 0 {
            format!(" · {blocked} blocked")
        } else {
            String::new()
        }
    );

    // Grouped by project, because that is how somebody running several
    // of them thinks about the fleet.
    let mut groups: BTreeMap<String, Vec<&AgentRecord>> = BTreeMap::new();
    for agent in agents {
        let name = agent
            .project
            .as_ref()
            .map(agentdocker_core::ProjectRef::name)
            .unwrap_or_else(|| "no project".to_owned());
        groups.entry(name).or_default().push(agent);
    }
    for (project, in_project) in &groups {
        let _ = writeln!(out, "\n{project}");
        for agent in in_project {
            let doing = activity
                .iter()
                .find(|a| a.agent == agent.id)
                .map(|a| match &a.activity {
                    Activity::Blocked { resource, .. } => {
                        format!("blocked on {}", format::resource(resource))
                    }
                    other => other.label().to_owned(),
                })
                .unwrap_or_else(|| agent.status.to_string());
            let held = leases.iter().filter(|l| l.holder == agent.id).count();
            let _ = writeln!(
                out,
                "  {:<20} {:<14} {:<32} {:>2} lease(s)  {}",
                truncate(&agent.spec.name, 20),
                truncate(&agent.spec.runtime, 14),
                truncate(&doing, 32),
                held,
                format::ago(agent.last_seen)
            );
        }
    }
    if !waiting.is_empty() {
        let _ = writeln!(out, "\nWaiting");
        for (place, waiter) in waiting.iter().enumerate() {
            let _ = writeln!(
                out,
                "  {}. {:<20} {}",
                place + 1,
                truncate(waiter.agent.short(), 20),
                format::resource(&waiter.resource)
            );
        }
    }
    let _ = writeln!(out, "\nCtrl-C to leave.");
    out
}

/// Cut to a width without splitting a character.
fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(width.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// The cursor is hidden while this lives and shown again when it dies,
/// however the command ends.
struct CursorGuard;

impl CursorGuard {
    fn new() -> Self {
        print!("{HIDE_CURSOR}");
        let _ = std::io::stdout().flush();
        Self
    }
}

impl Drop for CursorGuard {
    fn drop(&mut self) {
        print!("{SHOW_CURSOR}");
        let _ = std::io::stdout().flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::{AgentId, AgentSpec, LeaseMode, ResourceKey};

    fn agent(name: &str) -> AgentRecord {
        let mut record = AgentRecord::new(
            AgentSpec {
                name: name.to_owned(),
                runtime: "claude-code".to_owned(),
                ..AgentSpec::default()
            },
            true,
            chrono::Utc::now(),
        );
        record.status = agentdocker_core::AgentStatus::Running;
        record
    }

    #[test]
    fn the_frame_says_what_is_running_and_what_is_stuck() {
        let writer = agent("writer");
        let reviewer = agent("reviewer");
        let activity = vec![
            AgentActivity {
                agent: writer.id.clone(),
                name: "writer".into(),
                project: None,
                activity: Activity::Working {
                    since: chrono::Utc::now(),
                },
            },
            AgentActivity {
                agent: reviewer.id.clone(),
                name: "reviewer".into(),
                project: None,
                activity: Activity::Blocked {
                    resource: ResourceKey::new("task:parser"),
                    held_by: vec![writer.id.clone()],
                    since: chrono::Utc::now(),
                },
            },
        ];
        let frame = render(&[writer, reviewer], &activity, &[], &[]);
        assert!(frame.contains("2 agent(s)"), "{frame}");
        assert!(frame.contains("1 blocked"), "{frame}");
        assert!(frame.contains("blocked on task:parser"), "{frame}");
        assert!(frame.contains("working"), "{frame}");
        assert!(frame.contains("no project"), "{frame}");
    }

    #[test]
    fn waiters_are_listed_in_the_order_they_will_be_served() {
        let first = AgentId::from("aaaa");
        let second = AgentId::from("bbbb");
        let waiter = |agent: AgentId, ticket| agentdocker_core::Waiter {
            ticket,
            agent,
            resource: ResourceKey::new("task:x"),
            mode: LeaseMode::Exclusive,
            since: chrono::Utc::now(),
        };
        let frame = render(&[], &[], &[], &[waiter(first, 0), waiter(second, 1)]);
        let one = frame.find("1. aaaa").expect("the first waiter");
        let two = frame.find("2. bbbb").expect("the second");
        assert!(one < two, "oldest first:\n{frame}");
    }

    #[test]
    fn an_empty_fleet_still_renders() {
        let frame = render(&[], &[], &[], &[]);
        assert!(frame.contains("0 agent(s)"), "{frame}");
        assert!(frame.contains("Ctrl-C"), "{frame}");
    }

    #[test]
    fn long_names_are_cut_rather_than_wrapped() {
        assert_eq!(truncate("short", 20), "short");
        assert_eq!(truncate("aaaaaaaaaa", 5), "aaaa…");
        // Multi-byte text is cut on a character, not a byte.
        assert_eq!(truncate("ααααα", 3), "αα…");
    }
}
