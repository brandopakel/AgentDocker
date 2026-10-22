//! `agentdocker codex-threads`: the conversations Codex's shared app-server
//! has loaded, read through the profile's control socket, each with its
//! status and the AgentDocker record keyed by it. Read-only: the server is
//! asked `thread/loaded/list` and `thread/read`, nothing else, and the
//! daemon is asked only if it is already running.
use crate::client::Client;
use agentdocker_core::{AgentRecord, Request, Response};
use agentdocker_host::codex_control::{Session, Thread};
use anyhow::{Context, Result};
use serde_json::json;
use std::path::{Path, PathBuf};

#[derive(clap::Args)]
pub struct Args {
    /// The Codex profile; `CODEX_HOME`, else `~/.codex`.
    #[arg(long)]
    pub profile: Option<PathBuf>,
    /// One JSON array instead of a table.
    #[arg(long)]
    pub json: bool,
}

/// The live record keyed by this profile and thread, when there is one.
fn record_of<'a>(
    agents: &'a [AgentRecord],
    profile: &Path,
    thread: &str,
) -> Option<&'a AgentRecord> {
    agents.iter().find(|agent| {
        agent.status.is_live()
            && agent.session_key().is_some_and(|key| {
                key.runtime == "codex"
                    && key.session == thread
                    && key.profile.as_deref().map(Path::new) == Some(profile)
            })
    })
}

pub async fn run(client: &Client, args: Args) -> Result<()> {
    let profile = args
        .profile
        .or_else(|| std::env::var_os("CODEX_HOME").map(PathBuf::from))
        .or_else(|| std::env::home_dir().map(|home| home.join(".codex")))
        .context("no Codex profile: pass --profile")?
        .canonicalize()
        .context("the Codex profile does not exist")?;
    let Some(mut session) = Session::open(&profile).await? else {
        eprintln!(
            "{} has no control socket: no shared Codex app-server runs for it",
            profile.display()
        );
        if args.json {
            println!("[]");
        }
        return Ok(());
    };
    let mut threads: Vec<Thread> = Vec::new();
    let read = async {
        for id in session.loaded().await? {
            threads.push(session.read(&id).await?);
        }
        Ok::<_, std::io::Error>(())
    }
    .await;
    session.close().await;
    read?;
    // A diagnostic never starts a coordinator: without a running daemon the
    // listing still stands, it only names no records.
    let agents = match client
        .clone()
        .with_start_timeout(None)
        .call(&Request::List {
            all: false,
            project: None,
            labels: Default::default(),
        })
        .await
    {
        Ok(Response::Agents { agents, .. }) => agents,
        _ => Vec::new(),
    };
    if args.json {
        let rows: Vec<_> = threads
            .iter()
            .map(|thread| {
                json!({
                    "thread": thread.id,
                    "status": thread.status.label(),
                    "cwd": thread.cwd,
                    "parent": thread.parent,
                    "agent": record_of(&agents, &profile, &thread.id).map(|a| a.id.to_string()),
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if threads.is_empty() {
        println!("The shared Codex app-server has no conversation loaded.");
        return Ok(());
    }
    // What the server says is shown as text, never as terminal control.
    let plain = |text: &str| text.chars().filter(|c| !c.is_control()).collect::<String>();
    let rows: Vec<Vec<String>> = threads
        .iter()
        .map(|thread| {
            let agent = match (&thread.parent, record_of(&agents, &profile, &thread.id)) {
                (Some(_), _) => "(sub-agent)".to_owned(),
                (None, Some(agent)) => agent.spec.name.clone(),
                (None, None) => "-".to_owned(),
            };
            vec![
                plain(&thread.id),
                thread.status.label().to_owned(),
                plain(&agent),
                thread
                    .cwd
                    .as_deref()
                    .map(|cwd| plain(&cwd.display().to_string()))
                    .unwrap_or_default(),
            ]
        })
        .collect();
    crate::format::table(&["THREAD", "STATUS", "AGENT", "CHECKOUT"], &rows);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::{AgentSpec, agent::PROFILE_LABEL};

    #[test]
    fn a_thread_names_only_the_live_record_of_its_own_profile() {
        let record = |profile: &str, thread: &str| {
            AgentRecord::new(
                AgentSpec {
                    runtime: "codex".into(),
                    labels: [
                        ("session_id".to_owned(), thread.to_owned()),
                        (PROFILE_LABEL.to_owned(), profile.to_owned()),
                    ]
                    .into(),
                    ..Default::default()
                },
                false,
                chrono::Utc::now(),
            )
        };
        let agents = vec![record("/other", "t"), record("/p", "t"), record("/p", "u")];
        assert_eq!(
            record_of(&agents, Path::new("/p"), "t").map(|a| &a.id),
            Some(&agents[1].id)
        );
        assert!(record_of(&agents, Path::new("/p"), "v").is_none());
        assert!(record_of(&agents, Path::new("/q"), "t").is_none());
    }
}
