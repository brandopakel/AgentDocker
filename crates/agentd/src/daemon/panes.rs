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
        let inserted = lock(&self.state).insert_record_announcing(record.clone(), false, None);
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
            && let Err(reason) = self
                .ensure_watched(&record, super::WATCHER_LAUNCH_TIMEOUT)
                .await
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

        // The launch and its process's birth, read at once in the worker:
        // the birth is the leader's own identity, never tmux's start time.
        let worker = tokio::task::spawn_blocking(move || {
            let launch = tmux::new_session(&session, &workdir, &env, &command);
            let born = match &launch {
                tmux::Launch::Started(launched)
                | tmux::Launch::Indeterminate {
                    pane: Some(launched),
                    ..
                } => procinfo::start_time(launched.pid),
                _ => None,
            };
            (launch, born)
        })
        .await;
        let (launch, born) = worker.unwrap_or_else(|error| {
            // A worker that ran tmux may have made the session.
            let launch = tmux::Launch::Indeterminate {
                pane: None,
                output: None,
                why: format!("the tmux worker failed: {error}"),
            };
            (launch, None)
        });
        self.finish_launch(&record, launch, born, Arc::new(TmuxCleanup))
            .await
    }

    /// The launch's end, from its outcome: the record started, or ended
    /// durably, with what was cleaned up and what the person should do
    /// next. A pane this launch made is removed before the record is failed,
    /// and only on proof it is still this launch's; a checkout a launch may
    /// have used is kept.
    pub(super) async fn finish_launch(
        self: &Arc<Self>,
        record: &AgentRecord,
        launch: tmux::Launch,
        born: Option<DateTime<Utc>>,
        cleanup: Arc<dyn PaneCleanup>,
    ) -> Response {
        let session = tmux::session_name(&record.spec.name);
        match launch {
            tmux::Launch::Started(launched) => {
                self.finish_started(record, &launched, born, cleanup).await
            }
            tmux::Launch::NotStarted { why } => {
                let end = lock(&self.state).end_launch(&record.id, record, &why);
                let ended = matches!(end, LaunchEnd::Ended);
                // Nothing reached tmux, so nothing this launch started uses
                // the checkout: only then, and only once the end is kept.
                let isolate = if !record.spec.isolate {
                    None
                } else if ended && self.cleanup_isolate(record).await {
                    Some("removed")
                } else {
                    Some("kept")
                };
                let mut facts = serde_json::json!({
                    "launch": "not_started",
                    "session": session,
                    "end": end.word(),
                    "step": if ended { "retry" } else { "inspect" },
                });
                if let Some(isolate) = isolate {
                    facts["isolate"] = isolate.into();
                }
                let message = if ended {
                    format!("{why}; nothing reached tmux, so the launch can be tried again")
                } else {
                    format!("{why}; nothing reached tmux, and {}", end.said())
                };
                end.refusal_or(ErrorCode::LaunchFailed, message, facts)
            }
            tmux::Launch::Indeterminate { pane, output, why } => {
                let (removal, leader) = match &pane {
                    Some(launched) => {
                        let removal = cleanup_blocking(&cleanup, {
                            let launched = launched.clone();
                            move |cleanup| cleanup.remove(&launched)
                        })
                        .await
                        .unwrap_or_else(|why| tmux::Removal::Unknown { why });
                        let leader =
                            if matches!(removal, tmux::Removal::Removed | tmux::Removal::Gone) {
                                let pid = launched.pid;
                                cleanup_blocking(&cleanup, move |cleanup| cleanup.leader(pid, born))
                                    .await
                                    .unwrap_or_else(|why| tmux::Leader::Unknown { why })
                            } else {
                                tmux::Leader::Unknown {
                                    why: "its pane was not removed".to_owned(),
                                }
                            };
                        (Some(removal), Some(leader))
                    }
                    None => (None, None),
                };
                let end = lock(&self.state).end_launch(&record.id, record, &why);
                let ended = matches!(end, LaunchEnd::Ended);
                let review = ended
                    && matches!(removal, Some(tmux::Removal::Removed | tmux::Removal::Gone))
                    && leader == Some(tmux::Leader::Gone);
                let mut facts = serde_json::json!({
                    "launch": "indeterminate",
                    "session": session,
                    "end": end.word(),
                    "step": if review { "review" } else { "inspect" },
                });
                if let Some(launched) = &pane {
                    facts["pane"] = launched.pane.clone().into();
                    facts["server"] = serde_json::json!({
                        "socket": launched.socket,
                        "pid": launched.server.pid,
                        "started": launched.server.started,
                    });
                }
                if let Some(output) = &output {
                    facts["output"] = output.clone().into();
                }
                if let Some(removal) = &removal {
                    facts["removal"] = removal_facts(removal);
                }
                if let Some(leader) = &leader {
                    facts["leader"] = leader_facts(leader);
                }
                if record.spec.isolate {
                    facts["isolate"] = "kept".into();
                }
                let message = indeterminate_message(
                    record,
                    &session,
                    &why,
                    pane.as_ref(),
                    removal.as_ref(),
                    leader.as_ref(),
                    &end,
                    review,
                );
                end.refusal_or(ErrorCode::Unconfirmed, message, facts)
            }
        }
    }

    /// A pane made and named: its start recorded, or, refused, the pane
    /// retired on proof it is this launch's, its checkout kept either way.
    async fn finish_started(
        self: &Arc<Self>,
        record: &AgentRecord,
        launched: &tmux::Launched,
        born: Option<DateTime<Utc>>,
        cleanup: Arc<dyn PaneCleanup>,
    ) -> Response {
        let pane = launched.pane();
        let updated = lock(&self.state).commit_pane_start(&record.id, &pane, born);
        if !matches!(updated, Response::Error { .. }) {
            info!(
                agent = %record.id.short(),
                name = %record.spec.name,
                pane = %pane.id,
                session = %pane.session,
                pid = pane.pid,
                "agent started in a tmux pane"
            );
            return updated;
        }
        // The pane was made by this request and its start was refused:
        // retire it, but only while its process is the one launched, by its
        // birth here and by its whole identity inside tmux.
        let pid = launched.pid;
        let now_born = cleanup_blocking(&cleanup, move |cleanup| cleanup.born(pid))
            .await
            .ok()
            .flatten();
        let removal = if born.is_some() && now_born == born {
            let launched = launched.clone();
            cleanup_blocking(&cleanup, move |cleanup| cleanup.remove(&launched))
                .await
                .unwrap_or_else(|why| tmux::Removal::Unknown { why })
        } else {
            tmux::Removal::Unknown {
                why: "the pane process's identity could not be confirmed; not signalling it"
                    .to_owned(),
            }
        };
        if removal != tmux::Removal::Removed {
            warn!(agent = %record.id, pane = %pane.id, ?removal, "could not retire pane after recording its start failed");
        }
        // Its command ran: whatever the pane's fate, the checkout is kept.
        let mut facts = serde_json::json!({
            "launch": "started",
            "pane": launched.pane,
            "server": {"socket": launched.socket, "pid": launched.server.pid, "started": launched.server.started},
            "removal": removal_facts(&removal),
        });
        if record.spec.isolate {
            facts["isolate"] = "kept".into();
        }
        with_facts(updated, facts)
    }
}

/// What tells one run of a record from another: a run started, restarted or
/// resumed under the same id while tmux was asked changes one of these.
#[allow(clippy::type_complexity)]
fn run_identity(
    record: &AgentRecord,
) -> (
    &AgentStatus,
    Option<u32>,
    Option<DateTime<Utc>>,
    Option<u32>,
    Option<&agentdocker_core::multiplexer::Session>,
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
    u32,
) {
    (
        &record.status,
        record.pid,
        record.process_started_at,
        record.process_group,
        record.session.as_ref(),
        record.started_at,
        record.finished_at,
        record.restarts,
    )
}

/// The tmux and process I/O after a launch, behind a seam so each outcome
/// is tested without tmux. Every call blocks, and runs off the runtime.
pub(super) trait PaneCleanup: Send + Sync {
    /// Remove the launched pane, on proof it is still the launch's.
    fn remove(&self, launched: &tmux::Launched) -> tmux::Removal;
    /// Whether the pane's first process has ended, given a grace.
    fn leader(&self, pid: u32, born: Option<DateTime<Utc>>) -> tmux::Leader;
    /// The birth of the process with `pid` now.
    fn born(&self, pid: u32) -> Option<DateTime<Utc>>;
}

/// The real tmux and process table.
struct TmuxCleanup;

impl PaneCleanup for TmuxCleanup {
    fn remove(&self, launched: &tmux::Launched) -> tmux::Removal {
        tmux::remove_launched(launched)
    }
    fn leader(&self, pid: u32, born: Option<DateTime<Utc>>) -> tmux::Leader {
        tmux::leader(pid, born, tmux::LEADER_GRACE)
    }
    fn born(&self, pid: u32) -> Option<DateTime<Utc>> {
        procinfo::start_time(pid)
    }
}

/// One cleanup call off the runtime; a worker that failed is its why.
async fn cleanup_blocking<T: Send + 'static>(
    cleanup: &Arc<dyn PaneCleanup>,
    call: impl FnOnce(&dyn PaneCleanup) -> T + Send + 'static,
) -> Result<T, String> {
    let cleanup = Arc::clone(cleanup);
    tokio::task::spawn_blocking(move || call(cleanup.as_ref()))
        .await
        .map_err(|error| format!("the cleanup worker failed: {error}"))
}

/// What became of a launch's record ([`State::end_launch`]).
#[derive(Debug)]
pub(super) enum LaunchEnd {
    /// This call committed its Failed end and `agent_exited`.
    Ended,
    /// It had already ended, as found; this call wrote nothing.
    AlreadyEnded(AgentStatus),
    /// It no longer is the record this launch began with, and was left.
    Replaced,
    /// The store or a transfer fence refused the end: the refusal itself.
    Refused(Box<Response>),
}

impl LaunchEnd {
    fn word(&self) -> &'static str {
        match self {
            Self::Ended => "committed",
            Self::AlreadyEnded(_) => "already_ended",
            Self::Replaced => "replaced",
            Self::Refused(_) => "refused",
        }
    }

    /// The end, said, when it is not this call's committed one.
    fn said(&self) -> String {
        match self {
            Self::Ended => "its end is recorded".to_owned(),
            Self::AlreadyEnded(status) => {
                format!("its record had already ended ({status}); this launch wrote no end")
            }
            Self::Replaced => {
                "its record changed while tmux was asked, so it was left as it is".to_owned()
            }
            Self::Refused(_) => "its end could not be recorded".to_owned(),
        }
    }

    /// The refusal to answer with: the store's or fence's own when the end
    /// was refused, else `code` and `message`; the launch's facts added.
    fn refusal_or(self, code: ErrorCode, message: String, facts: serde_json::Value) -> Response {
        match self {
            Self::Refused(refusal) => with_facts(*refusal, facts),
            _ => with_facts(Response::error(code, message), facts),
        }
    }
}

/// `response` with `facts` added to its details, keeping every detail it
/// already carries.
fn with_facts(response: Response, facts: serde_json::Value) -> Response {
    let Response::Error {
        code,
        message,
        details,
    } = response
    else {
        return response;
    };
    let mut merged = match details {
        Some(serde_json::Value::Object(map)) => map,
        Some(other) => serde_json::Map::from_iter([("refusal".to_owned(), other)]),
        None => serde_json::Map::new(),
    };
    if let serde_json::Value::Object(facts) = facts {
        for (key, value) in facts {
            merged.entry(key).or_insert(value);
        }
    }
    Response::Error {
        code,
        message,
        details: Some(serde_json::Value::Object(merged)),
    }
}

fn removal_facts(removal: &tmux::Removal) -> serde_json::Value {
    match removal {
        tmux::Removal::Removed => serde_json::json!({"result": "removed"}),
        tmux::Removal::Gone => serde_json::json!({"result": "gone"}),
        tmux::Removal::NotReached { now } => serde_json::json!({
            "result": "not_reached",
            "now": now.map(|server| serde_json::json!({"pid": server.pid, "started": server.started})),
        }),
        tmux::Removal::Mismatch { pid } => serde_json::json!({"result": "mismatch", "pid": pid}),
        tmux::Removal::Unknown { why } => serde_json::json!({"result": "unknown", "why": why}),
    }
}

fn leader_facts(leader: &tmux::Leader) -> serde_json::Value {
    match leader {
        tmux::Leader::Gone => serde_json::json!({"result": "gone"}),
        tmux::Leader::Live => serde_json::json!({"result": "live"}),
        tmux::Leader::Unknown { why } => serde_json::json!({"result": "unknown", "why": why}),
    }
}

/// What an indeterminate launch did, what was found and done, and what to
/// do next, claiming no more than the evidence shows.
#[allow(clippy::too_many_arguments)]
fn indeterminate_message(
    record: &AgentRecord,
    session: &str,
    why: &str,
    pane: Option<&tmux::Launched>,
    removal: Option<&tmux::Removal>,
    leader: Option<&tmux::Leader>,
    end: &LaunchEnd,
    review: bool,
) -> String {
    let mut said = vec![format!("{why}; the launch may have run")];
    match (pane, removal) {
        (Some(launched), Some(removal)) => {
            let (id, socket) = (&launched.pane, launched.socket.display());
            said.push(match removal {
                tmux::Removal::Removed => format!("its pane {id} was removed at {socket}"),
                tmux::Removal::Gone => format!("its pane {id} is gone from its server at {socket}"),
                tmux::Removal::NotReached { now: None } => format!(
                    "its server was not reached: no server answers at {socket}, so nothing was removed"
                ),
                tmux::Removal::NotReached { now: Some(_) } => format!(
                    "its server was not reached: another server answers at {socket}, so nothing was removed"
                ),
                tmux::Removal::Mismatch { pid } => format!(
                    "pane {id} now holds another process (pid {pid}), so it was left alone"
                ),
                tmux::Removal::Unknown { why } => {
                    format!("whether its pane {id} was removed is not known ({why})")
                }
            });
        }
        _ => said.push("tmux named no pane, so nothing was removed".to_owned()),
    }
    match leader {
        Some(tmux::Leader::Gone) => said.push("its first process has ended".to_owned()),
        Some(tmux::Leader::Live) => {
            if let Some(launched) = pane {
                said.push(format!(
                    "its first process (pid {}) is still running",
                    launched.pid
                ));
            }
        }
        _ => {}
    }
    if !matches!(end, LaunchEnd::Ended) {
        said.push(end.said());
    }
    let checkout = record
        .spec
        .workdir
        .as_ref()
        .filter(|_| record.spec.isolate)
        .map(|path| path.display().to_string());
    if let Some(path) = &checkout {
        said.push(format!("the checkout at {path} is kept"));
    }
    if review {
        let mut step = "its command may have run: review its effects and any processes it left, which can outlive its pane, before launching again".to_owned();
        // Only an isolated launch's checkout is a worktree of its own.
        if checkout.is_some() {
            step.push_str(&format!(
                ", then remove the checkout with `agentdocker worktree-remove {}`",
                record.id
            ));
        }
        said.push(step);
    } else {
        said.push(match pane {
            Some(launched) => format!(
                "inspect the tmux server now at that socket (`tmux -S {} ls`), which may not be this launch's, before deciding what to do",
                tmux::shell_quoted(&launched.socket.display().to_string())
            ),
            None => format!(
                "the tmux server it used is not known: check `tmux ls`, and if a session named {} is there, review it before deciding what to do",
                tmux::shell_quoted(session)
            ),
        });
    }
    said.join("; ")
}

impl State {
    /// End the launch's record as Failed with `reason`, under one guard, and
    /// say what became of it: only [`LaunchEnd::Ended`] wrote the end. A
    /// record no longer the one `before` names (its pid, session or status
    /// changed while tmux was asked) is not ended.
    fn end_launch(&mut self, id: &AgentId, before: &AgentRecord, reason: &str) -> LaunchEnd {
        match self.registry.get(id) {
            None => return LaunchEnd::Replaced,
            Some(now) if !now.status.is_live() => {
                return LaunchEnd::AlreadyEnded(now.status.clone());
            }
            Some(now) if run_identity(now) != run_identity(before) => {
                return LaunchEnd::Replaced;
            }
            Some(_) => {}
        }
        let failed = AgentStatus::Failed {
            reason: reason.to_owned(),
        };
        match self.end_run(id, failed, None) {
            RunEnded::Committed(_) => LaunchEnd::Ended,
            RunEnded::NotLive(record) => match record {
                Some(record) => LaunchEnd::AlreadyEnded(record.status),
                None => LaunchEnd::Replaced,
            },
            RunEnded::NotWritten => {
                LaunchEnd::Refused(Box::new(self.write_failure().unwrap_or_else(|| {
                    Response::error(ErrorCode::Unavailable, "the launch's end was not recorded")
                })))
            }
        }
    }

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
            socket: pane.socket.clone(),
            evidence: agentdocker_core::multiplexer::Evidence::Environment,
        });
        agent.status = AgentStatus::Running;
        agent.started_at.get_or_insert(now);
        agent.last_seen = now;
        let event = Event::new(
            EventKind::AgentStarted {
                agent: id.clone(),
                pid: Some(pane.pid),
            },
            now,
        );
        let committed = self.transition(
            "pane start",
            event,
            |store, event| store.agent_transition(&agent, event),
            |state| {
                *state.registry.get_mut(id).expect("pane identity retained") = agent.clone();
            },
        );
        if committed != Persisted::Committed {
            return self
                .write_failure()
                .expect("refused pane start has a reason");
        }
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
                state.insert_record_announcing(before.clone(), false, None),
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
                socket: None,
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

    /// The tmux and process I/O, as told, with what was asked of it and
    /// whether the record was still live when its pane was removed.
    struct StandIn {
        removal: Option<tmux::Removal>,
        leader: tmux::Leader,
        born: Option<DateTime<Utc>>,
        watch: Option<(Arc<Daemon>, AgentId)>,
        calls: std::sync::Mutex<Vec<&'static str>>,
        live_at_removal: std::sync::Mutex<Option<bool>>,
    }

    impl StandIn {
        fn new(removal: Option<tmux::Removal>, leader: tmux::Leader) -> Self {
            StandIn {
                removal,
                leader,
                born: None,
                watch: None,
                calls: Default::default(),
                live_at_removal: Default::default(),
            }
        }
        fn calls(&self) -> Vec<&'static str> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl PaneCleanup for StandIn {
        fn remove(&self, _: &tmux::Launched) -> tmux::Removal {
            self.calls.lock().unwrap().push("remove");
            if let Some((daemon, id)) = &self.watch {
                *self.live_at_removal.lock().unwrap() = Some(daemon.is_live(id));
            }
            // None: the worker fails, as a panicked one does.
            self.removal.clone().expect("the cleanup worker fails")
        }
        fn leader(&self, _: u32, _: Option<DateTime<Utc>>) -> tmux::Leader {
            self.calls.lock().unwrap().push("leader");
            self.leader.clone()
        }
        fn born(&self, _: u32) -> Option<DateTime<Utc>> {
            self.calls.lock().unwrap().push("born");
            self.born
        }
    }

    fn launched() -> tmux::Launched {
        tmux::parse_launched("%7\t4242\t99\t1700000000\tpane-test\t/tmp/tmux-test/default").unwrap()
    }

    fn indeterminate(pane: Option<tmux::Launched>) -> tmux::Launch {
        tmux::Launch::Indeterminate {
            pane,
            output: None,
            why: "tmux made pane %7 but then reported a failure: hook".into(),
        }
    }

    fn git(cwd: &Path, args: &[&str]) -> bool {
        std::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
            .output()
            .is_ok_and(|output| output.status.success())
    }

    /// A daemon with one pending pane record, in its own isolated checkout
    /// when `isolate` (`None` when git is not here).
    async fn pending(isolate: bool) -> Option<(tempfile::TempDir, Arc<Daemon>, AgentRecord)> {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        if isolate
            && !(git(&repo, &["init", "-q"])
                && git(&repo, &["commit", "-q", "--allow-empty", "-m", "root"]))
        {
            return None;
        }
        let daemon =
            Arc::new(Daemon::open(dir.path().join("state"), dir.path().join("sock")).unwrap());
        let mut record = AgentRecord::new(
            AgentSpec {
                name: "pane-test".into(),
                command: vec!["sleep".into(), "300".into()],
                workdir: Some(repo),
                isolate,
                ..Default::default()
            },
            false,
            Utc::now(),
        );
        if isolate {
            record.spec.workdir = Some(daemon.isolate(&record).await.unwrap());
        }
        let workdir = record.spec.workdir.clone();
        record.project = daemon.project_for(workdir.clone(), true).await;
        record.vcs = Daemon::vcs_for(workdir).await;
        let inserted = lock(&daemon.state).insert_record_announcing(record, false, None);
        let Response::Agent { agent } = inserted else {
            panic!("{inserted:?}")
        };
        Some((dir, daemon, agent))
    }

    fn details(response: &Response) -> (ErrorCode, String, serde_json::Value) {
        let Response::Error {
            code,
            message,
            details,
        } = response
        else {
            panic!("{response:?}")
        };
        (*code, message.clone(), details.clone().unwrap_or_default())
    }

    fn exits(daemon: &Daemon, id: &AgentId) -> usize {
        daemon
            .recent_events(200)
            .iter()
            .filter(
                |event| matches!(&event.kind, EventKind::AgentExited { agent, .. } if agent == id),
            )
            .count()
    }

    /// Each outcome answers with its code, its step and its evidence; a
    /// known pane is removed before the record is failed; review is said
    /// only on a removed or gone pane whose leader ended.
    #[tokio::test]
    async fn each_launch_outcome_says_what_was_found_and_what_to_do() {
        use tmux::{Leader, Removal};
        let gone = Leader::Gone;
        type Case = (
            &'static str,
            tmux::Launch,
            Option<Removal>,
            Leader,
            ErrorCode,
            &'static str,
            &'static [&'static str],
        );
        let cases: Vec<Case> = vec![
            (
                "not started",
                tmux::Launch::NotStarted {
                    why: "tmux could not be run: no such file".into(),
                },
                None,
                gone.clone(),
                ErrorCode::LaunchFailed,
                "retry",
                &[],
            ),
            (
                "no pane",
                indeterminate(None),
                None,
                gone.clone(),
                ErrorCode::Unconfirmed,
                "inspect",
                &[],
            ),
            (
                "removed",
                indeterminate(Some(launched())),
                Some(Removal::Removed),
                gone.clone(),
                ErrorCode::Unconfirmed,
                "review",
                &["remove", "leader"],
            ),
            (
                "gone",
                indeterminate(Some(launched())),
                Some(Removal::Gone),
                gone.clone(),
                ErrorCode::Unconfirmed,
                "review",
                &["remove", "leader"],
            ),
            (
                "leader live",
                indeterminate(Some(launched())),
                Some(Removal::Removed),
                Leader::Live,
                ErrorCode::Unconfirmed,
                "inspect",
                &["remove", "leader"],
            ),
            (
                "leader unknown",
                indeterminate(Some(launched())),
                Some(Removal::Removed),
                Leader::Unknown {
                    why: "unread".into(),
                },
                ErrorCode::Unconfirmed,
                "inspect",
                &["remove", "leader"],
            ),
            (
                "not reached",
                indeterminate(Some(launched())),
                Some(Removal::NotReached {
                    now: Some(tmux::Server { pid: 5, started: 6 }),
                }),
                gone.clone(),
                ErrorCode::Unconfirmed,
                "inspect",
                &["remove"],
            ),
            (
                "mismatch",
                indeterminate(Some(launched())),
                Some(Removal::Mismatch { pid: 9 }),
                gone.clone(),
                ErrorCode::Unconfirmed,
                "inspect",
                &["remove"],
            ),
            (
                "lost reply",
                indeterminate(Some(launched())),
                Some(Removal::Unknown {
                    why: "tmux gave no answer".into(),
                }),
                gone.clone(),
                ErrorCode::Unconfirmed,
                "inspect",
                &["remove"],
            ),
            (
                "worker failed",
                indeterminate(Some(launched())),
                None,
                gone.clone(),
                ErrorCode::Unconfirmed,
                "inspect",
                &["remove"],
            ),
        ];
        for (case, launch, removal, leader, code, step, calls) in cases {
            let (_dir, daemon, record) = pending(false).await.unwrap();
            let mut stand_in = StandIn::new(removal, leader);
            stand_in.watch = Some((Arc::clone(&daemon), record.id.clone()));
            let stand_in = Arc::new(stand_in);
            let response = daemon
                .finish_launch(&record, launch, None, stand_in.clone())
                .await;
            let (got, message, facts) = details(&response);
            assert_eq!(got, code, "{case}: {message}");
            assert_eq!(facts["step"], step, "{case}: {facts}");
            assert_eq!(facts["end"], "committed", "{case}");
            assert_eq!(stand_in.calls(), calls, "{case}");
            if !calls.is_empty() {
                assert_eq!(
                    *stand_in.live_at_removal.lock().unwrap(),
                    Some(true),
                    "{case}: the pane is removed before the record is failed"
                );
            }
            assert!(!daemon.is_live(&record.id), "{case}");
            assert_eq!(exits(&daemon, &record.id), 1, "{case}");
            match case {
                "not started" => assert!(message.contains("can be tried again"), "{message}"),
                "no pane" => {
                    assert!(
                        message.contains("check `tmux ls`") && message.contains("'pane-test'"),
                        "{message}"
                    );
                    assert!(facts.get("removal").is_none());
                }
                "removed" => {
                    assert_eq!(facts["removal"]["result"], "removed");
                    assert_eq!(facts["leader"]["result"], "gone");
                    assert!(
                        message.contains("review its effects and any processes it left"),
                        "{message}"
                    );
                    assert!(
                        !message.contains("worktree-remove"),
                        "not isolated: {message}"
                    );
                }
                "not reached" => {
                    assert_eq!(facts["removal"]["result"], "not_reached");
                    assert!(
                        message.contains("another server answers")
                            && message.contains("nothing was removed"),
                        "{message}"
                    );
                    assert!(
                        message.contains("`tmux -S '/tmp/tmux-test/default' ls`")
                            && message.contains("may not be this launch's"),
                        "{message}"
                    );
                }
                "worker failed" => {
                    assert_eq!(facts["removal"]["result"], "unknown");
                    assert!(
                        facts["removal"]["why"]
                            .as_str()
                            .unwrap()
                            .contains("cleanup worker failed")
                    );
                }
                _ => {}
            }
            assert!(!message.contains("attach"), "{case}: {message}");
        }
    }

    /// A launch that may have run keeps its checkout even when its pane was
    /// removed and its leader ended (a child it left may still use it), and
    /// so does a started pane whose start was refused; only a launch that
    /// never reached tmux removes it.
    #[tokio::test]
    async fn a_checkout_a_launch_may_have_used_is_kept() {
        for case in ["removed and ended", "start refused", "not started"] {
            let Some((dir, daemon, record)) = pending(true).await else {
                return;
            };
            let path = record.spec.workdir.clone().unwrap();
            let mut stand_in = StandIn::new(Some(tmux::Removal::Removed), tmux::Leader::Gone);
            let born = Some(Utc::now());
            stand_in.born = born;
            let launch = match case {
                "removed and ended" => indeterminate(Some(launched())),
                "start refused" => {
                    lock(&daemon.state).offer_transfer(1).unwrap();
                    tmux::Launch::Started(launched())
                }
                _ => tmux::Launch::NotStarted {
                    why: "tmux could not be run".into(),
                },
            };
            let stand_in = Arc::new(stand_in);
            let response = daemon
                .finish_launch(&record, launch, born, stand_in.clone())
                .await;
            let (_, message, facts) = details(&response);
            let branch_kept = git(
                &dir.path().join("repo"),
                &["rev-parse", "--verify", "-q", "refs/heads/agent/pane-test"],
            );
            if case == "not started" {
                assert_eq!(facts["isolate"], "removed", "{case}");
                assert!(!path.exists(), "{case}");
                continue;
            }
            assert_eq!(facts["isolate"], "kept", "{case}: {facts}");
            assert!(path.exists(), "{case}: the worktree is kept");
            assert!(branch_kept, "{case}: the branch is kept");
            assert!(
                !daemon
                    .recent_events(200)
                    .iter()
                    .any(|event| matches!(event.kind, EventKind::WorktreeCleanup { .. })),
                "{case}"
            );
            if case == "removed and ended" {
                assert_eq!(facts["step"], "review");
                assert!(
                    message.contains(&format!("the checkout at {} is kept", path.display())),
                    "{message}"
                );
                assert!(
                    message.contains(&format!("agentdocker worktree-remove {}", record.id)),
                    "{message}"
                );
            } else {
                assert_eq!(stand_in.calls(), ["born", "remove"], "{case}");
                assert_eq!(facts["removal"]["result"], "removed", "{case}");
            }
        }
    }

    /// An end the store or a transfer fence refused is answered with that
    /// refusal and the cleanup evidence: no Failed record or exit event, the
    /// checkout kept, and neither retry nor review offered.
    #[tokio::test]
    async fn a_refused_end_claims_no_failure_and_offers_no_retry() {
        for (fence, launch) in [
            (false, indeterminate(Some(launched()))),
            (true, indeterminate(Some(launched()))),
            (
                false,
                tmux::Launch::NotStarted {
                    why: "tmux could not be run".into(),
                },
            ),
            (
                true,
                tmux::Launch::NotStarted {
                    why: "tmux could not be run".into(),
                },
            ),
        ] {
            let Some((_dir, daemon, record)) = pending(true).await else {
                return;
            };
            let path = record.spec.workdir.clone().unwrap();
            {
                let mut state = lock(&daemon.state);
                if fence {
                    state.offer_transfer(1).unwrap();
                } else {
                    state.store.reject_event_for_test("agent_exited");
                }
            }
            let stand_in = Arc::new(StandIn::new(
                Some(tmux::Removal::Removed),
                tmux::Leader::Gone,
            ));
            let response = daemon
                .finish_launch(&record, launch.clone(), None, stand_in)
                .await;
            let (code, _, facts) = details(&response);
            let case = format!("fence {fence}, {launch:?}");
            if fence {
                assert_eq!(code, ErrorCode::Transferring, "{case}");
            } else {
                assert!(
                    !matches!(code, ErrorCode::LaunchFailed | ErrorCode::Unconfirmed),
                    "{case}: {code:?}"
                );
            }
            assert_eq!(facts["end"], "refused", "{case}");
            assert_eq!(facts["step"], "inspect", "{case}");
            assert_eq!(facts["isolate"], "kept", "{case}");
            assert!(path.exists(), "{case}");
            assert!(daemon.is_live(&record.id), "{case}: no Failed was kept");
            assert_eq!(exits(&daemon, &record.id), 0, "{case}");
            if matches!(launch, tmux::Launch::Indeterminate { .. }) {
                assert_eq!(facts["removal"]["result"], "removed", "{case}");
            }
        }
    }

    /// A record already ended is reported as found, and one that changed
    /// while tmux was asked is not ended: neither writes an end.
    #[tokio::test]
    async fn a_record_already_ended_or_replaced_is_left_as_found() {
        for replaced in [false, true] {
            let (_dir, daemon, record) = pending(false).await.unwrap();
            let run = Utc::now();
            {
                let mut state = lock(&daemon.state);
                if replaced {
                    // Another run under the same id, as a restart makes it,
                    // its status unchanged: only the run tells it apart.
                    let now = state.registry.get_mut(&record.id).unwrap();
                    assert_eq!(now.status, record.status);
                    now.pid = Some(77);
                    now.process_started_at = Some(run);
                    now.started_at = Some(run);
                    now.restarts += 1;
                } else {
                    let _ = state.mark_exited(&record.id, AgentStatus::Exited { code: Some(0) });
                }
            }
            let before = exits(&daemon, &record.id);
            let stand_in = Arc::new(StandIn::new(None, tmux::Leader::Gone));
            let response = daemon
                .finish_launch(
                    &record,
                    tmux::Launch::NotStarted {
                        why: "tmux could not be run".into(),
                    },
                    None,
                    stand_in,
                )
                .await;
            let (code, message, facts) = details(&response);
            assert_eq!(code, ErrorCode::LaunchFailed);
            assert_eq!(
                facts["end"],
                if replaced {
                    "replaced"
                } else {
                    "already_ended"
                }
            );
            assert_eq!(facts["step"], "inspect");
            assert_eq!(exits(&daemon, &record.id), before, "no end written");
            if replaced {
                assert!(daemon.is_live(&record.id));
                assert!(
                    message.contains("changed while tmux was asked"),
                    "{message}"
                );
                // The newer run is left as it is.
                let state = lock(&daemon.state);
                let now = state.registry.get(&record.id).unwrap();
                assert_eq!(
                    (now.pid, now.process_started_at, now.restarts),
                    (Some(77), Some(run), record.restarts + 1)
                );
            } else {
                assert!(message.contains("had already ended"), "{message}");
            }
        }
    }
}
