//! `run --in-pane`: hand the agent to `tmux` and keep the coordination.
//!
//! The other half of recognising a multiplexer. We do not write one, and
//! we do not want to own terminals — so when a person wants to watch an
//! agent in the tool they already use, the daemon asks tmux to start it
//! and then registers what tmux started.
//!
//! That division is the whole design, and it decides everything awkward
//! about this path. tmux owns the process, so the agent is **registered,
//! not supervised**: there is no captured log, because the output is on
//! tmux's terminal and `tmux capture-pane` is where it lives; the agent
//! ends when its command ends, and the liveness sweep notices as it does
//! for any other agent nobody here started; and `--restore` means
//! nothing, because a daemon that did not start it cannot bring it back.
//! Those are stated in the refusals and the docs rather than discovered.
//!
//! What the agent does get is everything that matters: an identity, the
//! project, its leases, its read set, its journal cursor — and the pane
//! recorded on its record, so `ps` says where to find it.

use super::*;
use agentdocker_host::multiplexer::tmux;

impl Daemon {
    /// Start the agent in a new detached tmux session and register it.
    pub(super) async fn run_in_pane(self: &Arc<Self>, spec: AgentSpec) -> Response {
        if spec.command.first().is_none_or(String::is_empty) {
            return Response::error(ErrorCode::Invalid, "run needs a nonempty command");
        }
        if spec.tty {
            return Response::error(
                ErrorCode::Invalid,
                "--in-pane and --tty ask for two terminals; tmux provides the one",
            );
        }
        if spec.restore {
            return Response::error(
                ErrorCode::Invalid,
                "--in-pane and --restore cannot combine: tmux owns the process, so a restarted \
                 daemon has nothing to bring back",
            );
        }
        // Every argument check first, and only then the machine. What is
        // wrong with a request does not depend on whether tmux happens
        // to be installed: a request with no workdir is invalid on a
        // host with tmux and on a host without it, and answering
        // `unavailable` there would send the caller after the wrong
        // problem.
        let workdir = match spec.workdir.clone() {
            Some(dir) => dir,
            None => {
                return Response::error(ErrorCode::Invalid, "an agent in a pane needs a workdir");
            }
        };
        let mut record = AgentRecord::new(spec.clone(), false, Utc::now());
        if let Err(response) = self.admit_run(&mut record).await {
            return *response;
        }
        // Probing tmux runs a process and waits for it, so it does not
        // belong on a runtime worker. Asked before the record exists, so
        // "tmux is too old" is never reported as a failed agent.
        match tokio::task::spawn_blocking(tmux::usable).await {
            Ok(Ok(())) => {}
            Ok(Err(reason)) => return Response::error(ErrorCode::Unavailable, reason),
            Err(error) => {
                return Response::error(
                    ErrorCode::Internal,
                    format!("could not ask tmux for its version: {error}"),
                );
            }
        }

        // The record first, so the child can be told its own id — the
        // same order `run` uses, and for the same reason.
        if record.spec.isolate {
            match self.isolate(&record).await {
                Ok(path) => record.spec.workdir = Some(path),
                Err(response) => return *response,
            }
        }
        let workdir = record.spec.workdir.clone().unwrap_or(workdir);
        record.project = self.project_for(Some(workdir.clone()), true).await;
        record.vcs = Self::vcs_for(Some(workdir.clone())).await;
        // Bound before the match: a `lock(...)` temporary would live for
        // the whole expression, and one arm awaits.
        // Not announced yet: tmux has not been asked for the process, so
        // there is nothing to say started. The event goes out below, once.
        let inserted = lock(&self.state).insert_record_announcing(record.clone(), false);
        let record = match inserted {
            Response::Agent { agent } => agent,
            other => {
                self.cleanup_isolate(&record).await;
                return other;
            }
        };
        // Watched before the process exists, so its first edit is seen —
        // as with `run`.
        if watchable(&record)
            && let Err(reason) = self.ensure_watched(&record).await
        {
            self.mark_exited(
                &record.id,
                AgentStatus::Failed {
                    reason: reason.clone(),
                },
            );
            self.cleanup_isolate(&record).await;
            return Response::error(ErrorCode::Unavailable, reason);
        }

        let mut env = record.spec.env.clone();
        env.insert(
            "AGENTDOCKER_SOCKET".to_owned(),
            self.socket.display().to_string(),
        );
        env.insert(
            "AGENTDOCKER_AGENT_ID".to_owned(),
            record.id.as_str().to_owned(),
        );
        env.insert(
            "AGENTDOCKER_AGENT_NAME".to_owned(),
            record.spec.name.clone(),
        );
        let session = tmux::session_name(&record.spec.name);
        let command = record.spec.command.clone();

        self.refresh_policy_for(record.project.as_ref());
        let refused = lock(&self.state).run_refusal(&record);
        if let Some(response) = refused {
            self.mark_exited(
                &record.id,
                AgentStatus::Failed {
                    reason: "tmux launch refused by admission policy".into(),
                },
            );
            self.cleanup_isolate(&record).await;
            return response;
        }

        let pane = tokio::task::spawn_blocking(move || {
            tmux::new_session(&session, &workdir, &env, &command)
        })
        .await;
        let pane = match pane {
            Ok(Ok(pane)) => pane,
            Ok(Err(error)) => {
                let reason = error.to_string();
                self.mark_exited(
                    &record.id,
                    AgentStatus::Failed {
                        reason: reason.clone(),
                    },
                );
                self.cleanup_isolate(&record).await;
                return Response::error(ErrorCode::Internal, reason);
            }
            Err(error) => {
                let reason = format!("tmux worker failed: {error}");
                self.mark_exited(
                    &record.id,
                    AgentStatus::Failed {
                        reason: reason.clone(),
                    },
                );
                self.cleanup_isolate(&record).await;
                return Response::error(ErrorCode::Internal, reason);
            }
        };

        // Read before the lock: inspecting the process table is host I/O,
        // and the rule here is that it never holds the coordination guard
        // — every `claim`, `release` and `send` would queue behind it.
        let started_at = procinfo::start_time(pane.pid);
        let updated = lock(&self.state).commit_pane_start(&record.id, &pane, started_at);
        if matches!(updated, Response::Error { .. }) {
            // The pane was created by this request. Retire it on refusal,
            // checking both its tmux identity and process birth first.
            let owned = pane.clone();
            let cleanup = tokio::task::spawn_blocking(move || {
                if started_at.is_some() && procinfo::start_time(owned.pid) == started_at {
                    tmux::remove_owned_pane(&owned)
                } else if !procinfo::alive(owned.pid) {
                    Ok(())
                } else {
                    Err(std::io::Error::other(
                        "pane process identity changed; not signalling it",
                    ))
                }
            })
            .await;
            if !matches!(cleanup, Ok(Ok(()))) {
                warn!(agent = %record.id, pane = %pane.id, ?cleanup, "could not retire pane after recording its start failed");
            }
            return updated;
        }
        info!(
            agent = %record.id.short(),
            name = %record.spec.name,
            pane = %pane.id,
            session = %pane.session,
            pid = pane.pid,
            "agent started in a tmux pane"
        );
        updated
    }
}

impl State {
    /// Publish the started pane only when its record and event both commit.
    fn commit_pane_start(
        &mut self,
        id: &AgentId,
        pane: &tmux::Pane,
        started_at: Option<DateTime<Utc>>,
    ) -> Response {
        let Some(mut agent) = self.registry.get(id).cloned() else {
            return Response::error(ErrorCode::NotFound, "agent vanished");
        };
        let now = Utc::now();
        agent.pid = Some(pane.pid);
        agent.process_started_at = started_at;
        agent.session = Some(agentdocker_core::multiplexer::Session {
            kind: "tmux".to_owned(),
            session: Some(pane.session.clone()),
            pane: Some(pane.id.clone()),
            evidence: agentdocker_core::multiplexer::Evidence::Environment,
        });
        agent.status = AgentStatus::Running;
        agent.started_at.get_or_insert(now);
        agent.last_seen = now;
        let mut event = Event::new(
            EventKind::AgentStarted {
                agent: id.clone(),
                pid: Some(pane.pid),
            },
            now,
        );
        event.seq = self.next_seq;
        if self.persist("pane start", |store| store.agent_transition(&agent, &event))
            != Persisted::Committed
        {
            return self
                .write_failure()
                .expect("refused pane start has a reason");
        }
        *self.registry.get_mut(id).expect("pane identity retained") = agent.clone();
        self.next_seq += 1;
        let _ = self.events.send(event);
        Response::Agent { agent }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_start_refusal_preserves_record_and_publishes_nothing() {
        for fenced in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let daemon = Daemon::open(dir.path().join("state"), dir.path().join("sock")).unwrap();
            let mut state = lock(&daemon.state);
            let before = AgentRecord::new(
                AgentSpec {
                    name: "pane-test".into(),
                    ..Default::default()
                },
                false,
                Utc::now(),
            );
            assert!(matches!(
                state.insert_record_announcing(before.clone(), false),
                Response::Agent { .. }
            ));
            if fenced {
                state.offer_transfer(1).unwrap();
            } else {
                state.store.reject_event_for_test("agent_started");
            }
            let mut events = state.events.subscribe();
            let seq = state.next_seq;
            let pane = tmux::Pane {
                id: "%42".into(),
                session: "owned-pane".into(),
                pid: 42,
            };
            assert!(matches!(
                state.commit_pane_start(&before.id, &pane, Some(Utc::now())),
                Response::Error { .. }
            ));
            assert_eq!(
                serde_json::to_value(state.registry.get(&before.id).unwrap()).unwrap(),
                serde_json::to_value(&before).unwrap()
            );
            assert_eq!(
                serde_json::to_value(&state.store.load_agents().unwrap()[0]).unwrap(),
                serde_json::to_value(&before).unwrap()
            );
            assert_eq!(state.next_seq, seq);
            assert!(events.try_recv().is_err());
        }
    }
}
