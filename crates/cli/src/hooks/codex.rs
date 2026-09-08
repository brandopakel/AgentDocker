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
    let name = format!("codex-{pid}");
    let agent = match backend
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
                workdir: Some(input.cwd.clone()),
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
            && agent.spec.workdir.as_ref() == Some(&input.cwd)
            && agent
                .spec
                .labels
                .get("session_id")
                .is_none_or(|session| session == &input.session_id),
        "hook identity does not match the live Codex process"
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
    const MAX_INPUT: usize = 1024 * 1024;
    let deadline = std::time::Instant::now() + timeout;
    let mut bytes = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        ensure!(!remaining.is_zero(), "hook input timed out");
        let mut descriptor = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one initialized, borrowed descriptor; no fd ownership changes.
        let ready = unsafe {
            libc::poll(
                &mut descriptor,
                1,
                remaining.as_millis().min(i32::MAX as u128).max(1) as i32,
            )
        };
        if ready < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        ensure!(ready > 0, "hook input timed out or is unavailable");
        let mut chunk = [0u8; 4096];
        // SAFETY: this invocation is the sole stdin reader; poll established
        // readability, and chunk is valid for the requested length.
        let count = unsafe { libc::read(fd, chunk.as_mut_ptr().cast(), chunk.len()) };
        if count < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        ensure!(count >= 0, "hook input failed");
        if count == 0 {
            break;
        }
        ensure!(
            bytes.len() + count as usize <= MAX_INPUT,
            "hook input exceeds 1 MiB"
        );
        bytes.extend_from_slice(&chunk[..count as usize]);
    }
    serde_json::from_slice(&bytes).context("invalid Codex hook event")
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
        let now = Utc::now();
        let mut agent = agentdocker_core::AgentRecord::new(
            AgentSpec {
                name: "codex-42".into(),
                runtime: "codex".into(),
                workdir: Some(PathBuf::from("/fixture")),
                ..AgentSpec::default()
            },
            false,
            now,
        );
        agent.pid = Some(42);
        agent.process_started_at = Some(now - chrono::Duration::hours(1));
        agent.status = agentdocker_core::AgentStatus::Running;
        let backend = Mock::with(vec![Response::Agent { agent }]);
        let input: Input = serde_json::from_value(serde_json::json!({"hook_event_name":"PostToolUse", "session_id":"fixture", "cwd":"/fixture", "tool_input":{"private":"NEVER-SEND-THIS"}, "transcript_path":"/never/read/transcript"})).unwrap();
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
        let now = Utc::now();
        let mut agent = agentdocker_core::AgentRecord::new(
            AgentSpec {
                name: "codex-42".into(),
                runtime: "codex".into(),
                workdir: Some(PathBuf::from("/fixture")),
                ..AgentSpec::default()
            },
            false,
            now,
        );
        agent.pid = Some(42);
        agent.process_started_at = Some(now);
        agent.status = agentdocker_core::AgentStatus::Running;
        let id = agent.id.to_string();
        let backend = Mock::with(vec![Response::Agent { agent }, Response::Ok]);
        report(
            &backend,
            &Input {
                hook_event_name: "PreToolUse".into(),
                session_id: "test-session".into(),
                cwd: PathBuf::from("/fixture"),
            },
            42,
            now,
            now,
        )
        .await
        .unwrap();
        let calls = backend.requests();
        assert_eq!(calls.len(), 2);
        assert!(
            matches!(&calls[1], Request::ReportActivity { agent, observation } if agent == &id && observation.activity == ReportedActivity::Working)
        );
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
