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
        if !tmux::available() {
            return Response::error(
                ErrorCode::Unavailable,
                "tmux is not on this machine's PATH, so there is no pane to run in",
            );
        }
        let workdir = match spec.workdir.clone() {
            Some(dir) => dir,
            None => {
                return Response::error(ErrorCode::Invalid, "an agent in a pane needs a workdir");
            }
        };

        // The record first, so the child can be told its own id — the
        // same order `run` uses, and for the same reason.
        let mut record = AgentRecord::new(spec.clone(), false, Utc::now());
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
        let inserted = lock(&self.state).insert_record(record.clone());
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
                return Response::error(ErrorCode::Internal, reason);
            }
        };

        let updated = {
            let mut state = lock(&self.state);
            if let Some(stored) = state.registry.get_mut(&record.id) {
                stored.pid = Some(pane.pid);
                stored.process_started_at = procinfo::start_time(pane.pid);
                // Not a process group of ours: tmux made it, and signalling
                // the pid is what `stop` should do.
                stored.session = Some(agentdocker_core::multiplexer::Session {
                    kind: "tmux".to_owned(),
                    session: Some(pane.session.clone()),
                    pane: Some(pane.id.clone()),
                    evidence: agentdocker_core::multiplexer::Evidence::Environment,
                });
            }
            let updated = state
                .registry
                .set_status(&record.id, AgentStatus::Running, Utc::now());
            if let Some(agent) = &updated {
                state.persist("agent", |store| store.upsert_agent(agent));
            }
            state.emit(EventKind::AgentStarted {
                agent: record.id.clone(),
                pid: Some(pane.pid),
            });
            updated
        };
        info!(
            agent = %record.id.short(),
            name = %record.spec.name,
            pane = %pane.id,
            session = %pane.session,
            pid = pane.pid,
            "agent started in a tmux pane"
        );
        match updated {
            Some(agent) => Response::Agent { agent },
            None => Response::error(ErrorCode::NotFound, "agent vanished"),
        }
    }
}
