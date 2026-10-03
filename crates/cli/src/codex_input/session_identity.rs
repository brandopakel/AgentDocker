//! Give accounting the accepted conversation's identity without registering a
//! second process. Empty unused threads remain replaceable after restart.
use crate::client::{Backend, into_result};
use agentdocker_core::{AgentRecord, Request, Response};
use agentdocker_host::provider_input;
use anyhow::{Context, Result, ensure};
use std::time::Duration;

fn same_owner(current: &AgentRecord, expected: &AgentRecord) -> Result<()> {
    ensure!(
        current.id == expected.id
            && provider_input::is_codex_input(current)
            && current.status.is_live()
            && current.pid.is_some()
            && current.pid == expected.pid
            && current.process_started_at.is_some()
            && current.process_started_at == expected.process_started_at
            && current.owner == expected.owner
            && current.project == expected.project,
        "the prepared Codex conversation no longer belongs to this managed process"
    );
    Ok(())
}

pub(super) async fn bind(
    backend: &impl Backend,
    expected: &AgentRecord,
    thread: &str,
) -> Result<()> {
    ensure!(
        !thread.is_empty() && thread.len() <= 256 && !thread.chars().any(char::is_control),
        "Codex returned an invalid conversation identity"
    );
    // Bound the whole inspect/reconcile sequence. Initial binding happens after
    // turn acceptance, so a timeout adds no pre-submission recovery window.
    // A later restart reconciles the same retained thread without resubmission.
    tokio::time::timeout(Duration::from_secs(5), async {
        let Response::Agent { agent: current } = into_result(
            backend
                .call(Request::Inspect {
                    agent: expected.id.to_string(),
                })
                .await?,
        )?
        else {
            anyhow::bail!("the managed Codex owner could not be inspected");
        };
        same_owner(&current, expected)?;
        if let Some(existing) = current
            .spec
            .labels
            .get("session_id")
            .filter(|s| !s.is_empty())
        {
            ensure!(
                existing == thread,
                "the managed Codex owner already names another conversation"
            );
            return Ok(());
        }
        let mut spec = current.spec.clone();
        spec.labels.insert("session_id".into(), thread.into());
        // Register's existing same-process reconciliation atomically learns a
        // missing session label while retaining managed ownership and queues.
        let Response::Agent { agent: bound } = into_result(
            backend
                .call(Request::Register {
                    spec,
                    pid: current.pid,
                    session: None,
                })
                .await?,
        )?
        else {
            anyhow::bail!("the prepared Codex conversation could not be bound");
        };
        same_owner(&bound, expected)?;
        ensure!(
            bound.spec.labels.get("session_id").map(String::as_str) == Some(thread),
            "the daemon did not retain the prepared Codex conversation identity"
        );
        Ok(())
    })
    .await
    .context("Codex conversation identity did not reach the daemon")?
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::{AgentSpec, AgentStatus};
    use std::sync::Mutex;

    struct Fixture {
        agent: Mutex<AgentRecord>,
        writes: Mutex<usize>,
        wrong_reply: bool,
    }
    impl Backend for Fixture {
        async fn call(&self, request: Request) -> Result<Response> {
            let mut agent = self.agent.lock().unwrap();
            match request {
                Request::Inspect { agent: id } => assert_eq!(id, agent.id.as_str()),
                Request::Register { spec, pid, session } => {
                    assert_eq!(pid, agent.pid);
                    assert!(session.is_none());
                    let mut expected = agent.spec.clone();
                    expected.labels.insert("session_id".into(), "thread".into());
                    assert_eq!(spec, expected, "binding changes only the session label");
                    agent.spec = spec;
                    *self.writes.lock().unwrap() += 1;
                    if self.wrong_reply {
                        let mut wrong = agent.clone();
                        wrong.managed = false;
                        return Ok(Response::Agent { agent: wrong });
                    }
                }
                _ => panic!("unexpected identity request"),
            }
            Ok(Response::Agent {
                agent: agent.clone(),
            })
        }
    }

    fn fixture() -> (AgentRecord, Fixture) {
        let now = chrono::Utc::now();
        let mut spec = AgentSpec {
            runtime: "codex".into(),
            ..Default::default()
        };
        spec.env
            .insert(provider_input::CODEX_INPUT_ENV.into(), "1".into());
        let mut agent = AgentRecord::new(spec, true, now);
        agent.pid = Some(std::process::id());
        agent.process_started_at = Some(now);
        agent.status = AgentStatus::Running;
        let backend = Fixture {
            agent: Mutex::new(agent.clone()),
            writes: Mutex::new(0),
            wrong_reply: false,
        };
        (agent, backend)
    }

    #[tokio::test]
    async fn the_prepared_thread_is_bound_once_to_the_same_managed_record() {
        let (agent, backend) = fixture();
        bind(&backend, &agent, "thread").await.unwrap();
        bind(&backend, &agent, "thread").await.unwrap();
        assert_eq!(*backend.writes.lock().unwrap(), 1);
        assert_eq!(backend.agent.lock().unwrap().id, agent.id);
    }

    #[tokio::test]
    async fn another_thread_or_process_generation_is_refused_before_registration() {
        for changed_generation in [false, true] {
            let (agent, backend) = fixture();
            {
                let mut current = backend.agent.lock().unwrap();
                if changed_generation {
                    current.process_started_at =
                        Some(chrono::Utc::now() + chrono::Duration::seconds(1));
                } else {
                    current
                        .spec
                        .labels
                        .insert("session_id".into(), "other".into());
                }
            }
            assert!(bind(&backend, &agent, "thread").await.is_err());
            assert_eq!(*backend.writes.lock().unwrap(), 0);
        }
    }

    #[tokio::test]
    async fn a_registration_reply_cannot_replace_the_managed_owner() {
        let (agent, mut backend) = fixture();
        backend.wrong_reply = true;
        assert!(bind(&backend, &agent, "thread").await.is_err());
    }
}
