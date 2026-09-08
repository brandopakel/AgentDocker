//! Codex lifecycle activity only. This adapter never reads a transcript,
//! consumes messages, claims files or changes provider permission decisions.

use std::path::PathBuf;

use agentdocker_core::{ActivityObservation, AgentSpec, ReportedActivity, Request, Response};
use anyhow::{Context, Result, bail, ensure};
use chrono::Utc;
use serde::Deserialize;

use crate::client::{Backend, Client};

#[derive(Debug, Deserialize)]
pub(super) struct Input {
    pub hook_event_name: String,
    pub session_id: String,
    pub cwd: PathBuf,
}

pub(super) fn activity(event: &str) -> Option<ReportedActivity> {
    match event {
        "UserPromptSubmit" | "PreToolUse" | "PostToolUse" | "PreCompact" | "PostCompact" => {
            Some(ReportedActivity::Working)
        }
        // Stop is a stop attempt: another hook can continue the turn. A later
        // tool/prompt report supersedes it, and every observation expires.
        "Stop" | "Interrupt" => Some(ReportedActivity::Idle),
        _ => None,
    }
}

pub(super) async fn report<B: Backend>(
    backend: &B,
    input: &Input,
    pid: u32,
    process_started_at: chrono::DateTime<Utc>,
    observed_at: chrono::DateTime<Utc>,
) -> Result<()> {
    let Some(activity) = activity(&input.hook_event_name) else {
        return Ok(());
    };
    ensure!(
        !input.session_id.is_empty() && input.session_id.len() <= 256,
        "invalid session identity"
    );
    ensure!(input.cwd.is_absolute(), "hook cwd must be absolute");
    let checkout = input
        .cwd
        .canonicalize()
        .context("cannot resolve hook checkout")?;
    let name = format!("codex-{pid}");
    let mut agent = match backend
        .call(Request::Inspect {
            agent: name.clone(),
        })
        .await?
    {
        Response::Agent { agent } => agent,
        Response::Error {
            code: agentdocker_core::ErrorCode::NotFound,
            ..
        } => {
            let spec = AgentSpec {
                name,
                runtime: "codex".into(),
                workdir: Some(checkout.clone()),
                labels: [
                    ("via".into(), "hook".into()),
                    ("session_id".into(), input.session_id.clone()),
                ]
                .into(),
                ..AgentSpec::default()
            };
            match backend
                .call(Request::Register {
                    spec,
                    pid: Some(pid),
                    session: agentdocker_host::multiplexer::own(),
                })
                .await?
            {
                Response::Agent { agent } => agent,
                Response::Error { message, .. } => bail!("registration refused: {message}"),
                _ => bail!("unexpected registration response"),
            }
        }
        Response::Error { message, .. } => bail!("identity lookup refused: {message}"),
        _ => bail!("unexpected identity response"),
    };
    ensure!(
        agent.status.is_live()
            && agent.pid == Some(pid)
            && agent.process_started_at == Some(process_started_at)
            && agent.spec.runtime == "codex"
            && agent
                .spec
                .labels
                .get("session_id")
                .is_none_or(|session| session == &input.session_id),
        "hook identity does not match the live Codex process"
    );
    let registered_checkout = agent
        .spec
        .workdir
        .as_ref()
        .context("registered Codex has no verified checkout")?
        .canonicalize()
        .context("cannot resolve registered Codex checkout")?;
    ensure!(
        registered_checkout == checkout,
        "hook checkout does not match registered Codex checkout"
    );
    if !agent.spec.labels.contains_key("session_id") {
        // Adopted/MCP-first identities must bind the session through the
        // daemon's atomic registration path before activity can target them.
        let id = agent.id.clone();
        let mut spec = agent.spec.clone();
        spec.labels
            .insert("session_id".into(), input.session_id.clone());
        agent = match backend
            .call(Request::Register {
                spec,
                pid: Some(pid),
                session: agentdocker_host::multiplexer::own(),
            })
            .await?
        {
            Response::Agent { agent } => agent,
            Response::Error { message, .. } => bail!("session binding refused: {message}"),
            _ => bail!("unexpected session binding response"),
        };
        ensure!(agent.id == id, "session binding changed the Codex identity");
    }
    ensure!(
        agent.status.is_live()
            && agent.pid == Some(pid)
            && agent.process_started_at == Some(process_started_at)
            && agent.spec.runtime == "codex"
            && agent
                .spec
                .workdir
                .as_ref()
                .and_then(|p| p.canonicalize().ok())
                .as_ref()
                == Some(&checkout)
            && agent.spec.labels.get("session_id") == Some(&input.session_id),
        "Codex activity requires an exact verified session binding"
    );
    match backend
        .call(Request::ReportActivity {
            agent: agent.id.to_string(),
            observation: ActivityObservation {
                activity,
                observed_at,
            },
        })
        .await?
    {
        Response::Ok => Ok(()),
        Response::Error { message, .. } => bail!("activity report refused: {message}"),
        _ => bail!("unexpected activity response"),
    }
}

pub(super) async fn run(client: &Client) -> Result<()> {
    // stdin is a provider-owned pipe. Poll before every read, with bounded
    // retained bytes; a malformed or never-closed stream cannot hang a turn.
    let input = read_input(0, std::time::Duration::from_secs(1))?;
    if activity(&input.hook_event_name).is_none() {
        return Ok(());
    }
    let observed_at = Utc::now();
    let table = agentdocker_host::procinfo::processes().context("cannot inspect hook parent")?;
    let mut pid = std::os::unix::process::parent_id();
    let mut host = None;
    for _ in 0..12 {
        let Some(process) = table.iter().find(|p| p.pid == pid) else {
            break;
        };
        if agentdocker_host::procinfo::runtime_of(&process.argv) == Some("codex") {
            host = Some(pid);
            break;
        }
        if process.ppid == pid {
            break;
        }
        pid = process.ppid;
    }
    let pid = host.context("hook has no Codex CLI ancestor")?;
    let started_at =
        agentdocker_host::procinfo::start_time(pid).context("cannot verify Codex process birth")?;
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        report(client, &input, pid, started_at, observed_at),
    )
    .await
    .context("activity exceeded the one-second IPC budget")?
}

fn read_input(fd: i32, timeout: std::time::Duration) -> Result<Input> {
    super::input::read(fd, timeout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::mock::Mock;

    #[test]
    fn input_is_bounded_and_never_waits_forever_for_eof() {
        use std::io::Write;
        use std::os::fd::AsRawFd;
        let (reader, _held_open) = std::os::unix::net::UnixStream::pair().unwrap();
        assert!(
            read_input(reader.as_raw_fd(), std::time::Duration::from_millis(20))
                .unwrap_err()
                .to_string()
                .contains("timed out")
        );
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(&vec![b' '; 1024 * 1024 + 1]).unwrap();
        use std::io::{Seek, SeekFrom};
        file.seek(SeekFrom::Start(0)).unwrap();
        assert!(
            read_input(file.as_raw_fd(), std::time::Duration::from_secs(1))
                .unwrap_err()
                .to_string()
                .contains("exceeds 1 MiB")
        );
    }

    #[tokio::test]
    async fn a_reused_pid_cannot_update_the_previous_agent() {
        let checkout = tempfile::tempdir().unwrap();
        let now = Utc::now();
        let mut agent = agentdocker_core::AgentRecord::new(
            AgentSpec {
                name: "codex-42".into(),
                runtime: "codex".into(),
                workdir: Some(checkout.path().to_owned()),
                ..AgentSpec::default()
            },
            false,
            now,
        );
        agent.pid = Some(42);
        agent.process_started_at = Some(now - chrono::Duration::hours(1));
        agent.status = agentdocker_core::AgentStatus::Running;
        let backend = Mock::with(vec![Response::Agent { agent }]);
        let input: Input = serde_json::from_value(serde_json::json!({"hook_event_name":"PostToolUse", "session_id":"fixture", "cwd":checkout.path(), "tool_input":{"private":"NEVER-SEND-THIS"}, "transcript_path":"/never/read/transcript"})).unwrap();
        assert!(report(&backend, &input, 42, now, now).await.is_err());
        assert_eq!(backend.requests().len(), 1);
        assert!(
            !serde_json::to_string(&backend.requests())
                .unwrap()
                .contains("NEVER-SEND-THIS")
        );
    }

    #[tokio::test]
    async fn adopted_codex_keeps_its_identity_and_reports_only_activity() {
        let checkout = tempfile::tempdir().unwrap();
        let now = Utc::now();
        let mut agent = agentdocker_core::AgentRecord::new(
            AgentSpec {
                name: "codex-42".into(),
                runtime: "codex".into(),
                workdir: Some(checkout.path().to_owned()),
                ..AgentSpec::default()
            },
            false,
            now,
        );
        agent.pid = Some(42);
        agent.process_started_at = Some(now);
        agent.status = agentdocker_core::AgentStatus::Running;
        let id = agent.id.to_string();
        let mut bound = agent.clone();
        bound
            .spec
            .labels
            .insert("session_id".into(), "test-session".into());
        let backend = Mock::with(vec![
            Response::Agent { agent },
            Response::Agent { agent: bound },
            Response::Ok,
        ]);
        report(
            &backend,
            &Input {
                hook_event_name: "PreToolUse".into(),
                session_id: "test-session".into(),
                cwd: checkout.path().to_owned(),
            },
            42,
            now,
            now,
        )
        .await
        .unwrap();
        let calls = backend.requests();
        assert_eq!(calls.len(), 3);
        assert!(
            matches!(&calls[1], Request::Register { spec, pid: Some(42), .. }
            if spec.labels.get("session_id").is_some_and(|s| s == "test-session"))
        );
        assert!(
            matches!(&calls[2], Request::ReportActivity { agent, observation } if agent == &id && observation.activity == ReportedActivity::Working)
        );
    }

    #[tokio::test]
    async fn checkout_aliases_match_but_another_checkout_cannot_report() {
        let root = tempfile::tempdir().unwrap();
        let checkout = root.path().join("checkout");
        let different = root.path().join("other-checkout");
        let alias = root.path().join("alias");
        std::fs::create_dir(&checkout).unwrap();
        std::fs::create_dir(&different).unwrap();
        std::os::unix::fs::symlink(&checkout, &alias).unwrap();
        let now = Utc::now();
        let mut record = agentdocker_core::AgentRecord::new(
            AgentSpec {
                name: "codex-42".into(),
                runtime: "codex".into(),
                workdir: Some(checkout.canonicalize().unwrap()),
                ..Default::default()
            },
            false,
            now,
        );
        record
            .spec
            .labels
            .insert("session_id".into(), "fixture".into());
        record.pid = Some(42);
        record.process_started_at = Some(now);
        record.status = agentdocker_core::AgentStatus::Running;
        let mut input = Input {
            hook_event_name: "PostToolUse".into(),
            session_id: "fixture".into(),
            cwd: alias,
        };
        let backend = Mock::with(vec![
            Response::Agent {
                agent: record.clone(),
            },
            Response::Ok,
        ]);
        report(&backend, &input, 42, now, now).await.unwrap();
        assert!(matches!(
            &backend.requests()[1],
            Request::ReportActivity { .. }
        ));
        input.cwd = different;
        let backend = Mock::with(vec![Response::Agent { agent: record }]);
        assert!(
            report(&backend, &input, 42, now, now)
                .await
                .unwrap_err()
                .to_string()
                .contains("checkout does not match")
        );
        assert_eq!(backend.requests().len(), 1);
    }

    #[test]
    fn stop_and_interrupt_are_observations_but_startup_is_not_work() {
        assert_eq!(activity("Stop"), Some(ReportedActivity::Idle));
        assert_eq!(activity("Interrupt"), Some(ReportedActivity::Idle));
        assert_eq!(activity("PostToolUse"), Some(ReportedActivity::Working));
        assert_eq!(activity("SessionStart"), None);
        assert_eq!(activity("SessionEnd"), None);
    }
}
