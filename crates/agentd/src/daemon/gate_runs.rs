//! Local gate runs (`scripts/verify.sh check`, `quick`, `test`), recorded as
//! their runner reported them and read back by branch or head. A record is
//! the runner's report of its own run: the daemon checks its form, names
//! its project from its checkout and its requester from the request, and
//! keeps it as given. It never re-runs anything.

use super::*;
use agentdocker_core::verification::{self, GateRun, GateRunReport};

/// The document kind a run is kept as.
pub(super) const GATE_RUN: &str = "gate_run";

/// How many runs a project keeps; older ones are forgotten.
pub(super) const GATE_RUNS_PER_PROJECT: usize = 500;

impl Daemon {
    /// Record a run. `agent` is the caller the run was started for, through
    /// the identity guard; `runner` is the temporary agent it registered to
    /// hold the build slot, which is never `by`.
    pub(super) async fn record_gate_run(
        self: &Arc<Self>,
        agent: Option<String>,
        runner: Option<String>,
        report: GateRunReport,
    ) -> Response {
        let received_at = Utc::now();
        if let Err(why) = verification::validate(&report, received_at) {
            return Response::invalid(why);
        }
        let Some(project) = self.project_for(Some(report.checkout.clone()), false).await else {
            return Response::invalid(format!(
                "{} is in no project this daemon knows",
                report.checkout.display()
            ));
        };
        lock(&self.state).record_gate_run(project.id(), agent, runner, report, received_at)
    }

    /// The runs of a project, newest first.
    pub(super) async fn gate_runs(
        self: &Arc<Self>,
        agent: Option<&str>,
        project: Option<String>,
        branch: Option<String>,
        head: Option<String>,
        limit: Option<usize>,
    ) -> Response {
        let project = match project {
            Some(selector) => match self.resolve_project(&selector).await {
                Ok(project) => project,
                Err(response) => return *response,
            },
            None => {
                let own = agent.and_then(|agent| lock(&self.state).actor(agent).ok()?.project);
                match own {
                    Some(project) => project,
                    None => return Response::invalid("name a project"),
                }
            }
        };
        let mut state = lock(&self.state);
        let runs = match state.project_gate_runs(&project) {
            Ok(runs) => runs,
            Err(response) => return *response,
        };
        let runs = runs
            .into_iter()
            .rev()
            .filter(|run| {
                branch
                    .as_ref()
                    .is_none_or(|b| run.report.branch.as_ref() == Some(b))
            })
            .filter(|run| {
                head.as_ref()
                    .is_none_or(|h| run.report.start.head.as_ref() == Some(h))
            })
            .take(limit.unwrap_or(GATE_RUNS_PER_PROJECT))
            .collect();
        Response::GateRuns {
            runs,
            retained: GATE_RUNS_PER_PROJECT,
        }
    }
}

impl State {
    /// A project's runs, oldest first.
    fn project_gate_runs(&mut self, project: &ProjectId) -> Result<Vec<GateRun>, Box<Response>> {
        self.store_read("gate runs", |store| {
            store.documents_where::<GateRun>(
                GATE_RUN,
                "project",
                project.as_str(),
                GATE_RUNS_PER_PROJECT + 1,
            )
        })
        .ok_or_else(|| {
            Box::new(self.storage_failure().unwrap_or_else(|| {
                Response::error(
                    ErrorCode::StorageUnavailable,
                    "the recorded gate runs could not be read",
                )
            }))
        })
    }

    fn record_gate_run(
        &mut self,
        project: ProjectId,
        agent: Option<String>,
        runner: Option<String>,
        report: GateRunReport,
        received_at: DateTime<Utc>,
    ) -> Response {
        let (by, by_name) = match &agent {
            Some(agent) => match self.actor(agent) {
                Ok(actor) => {
                    let id = AgentId::from(actor.id.as_str());
                    let name = match self.registry.get(&id) {
                        Some(record) => record.spec.name.clone(),
                        None if actor.human => "person".to_owned(),
                        None => actor.id.clone(),
                    };
                    (Some(id), Some(name))
                }
                Err(response) => return *response,
            },
            None => (None, None),
        };
        let runner = match &runner {
            Some(runner) => match self.registry.resolve(runner) {
                Ok(id) => Some(id),
                Err(_) => return Response::invalid(format!("no agent {runner} ran this")),
            },
            None => None,
        };
        // The agent that holds the slot for an unknown caller is the
        // runner, never the author.
        let (by, by_name) = if by.is_some() && by == runner {
            (None, None)
        } else {
            (by, by_name)
        };
        let runs = match self.project_gate_runs(&project) {
            Ok(runs) => runs,
            Err(response) => return *response,
        };
        // The same run again: answered with the first, only if nothing it
        // says has changed. Bounded by retention, as every read here is.
        if let Some(first) = runs.iter().find(|run| run.report.run_key == report.run_key) {
            if first.report == report && first.by == by && first.runner == runner {
                return Response::GateRun { run: first.clone() };
            }
            return Response::error(
                ErrorCode::Conflict,
                format!(
                    "run {} is already recorded with another report or requester",
                    report.run_key
                ),
            );
        }
        let run = GateRun {
            id: agentdocker_core::gate::verdict_id(received_at),
            project: project.clone(),
            report,
            by,
            by_name,
            runner,
            received_at,
        };
        let forget: Vec<(&str, String)> = runs
            .iter()
            .take((runs.len() + 1).saturating_sub(GATE_RUNS_PER_PROJECT))
            .map(|old| (GATE_RUN, old.id.clone()))
            .collect();
        let put = vec![(
            GATE_RUN,
            run.id.clone(),
            serde_json::to_string(&run).expect("a gate run serializes"),
        )];
        let event = Event::new(
            EventKind::GateRunRecorded {
                run: Box::new(run.clone()),
            },
            received_at,
        );
        let committed = self.commit(
            "gate run",
            vec![event],
            |store, events| store.documents_change(&put, &forget, events),
            |_, _, ()| {},
        );
        if committed != Persisted::Committed {
            return self.write_failure().unwrap_or_else(|| {
                Response::error(
                    ErrorCode::StorageUnavailable,
                    "the gate run was not recorded",
                )
            });
        }
        Response::GateRun { run }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::verification::{Boundary, GateMode, Step, StepName, TreeStatus};

    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn report(checkout: &Path, key: &str, branch: &str, head: &str) -> GateRunReport {
        let now = Utc::now();
        GateRunReport {
            gate_version: 1,
            run_key: key.into(),
            checkout: checkout.to_path_buf(),
            branch: Some(branch.into()),
            mode: GateMode::Test,
            scope: None,
            start: Boundary {
                head: Some(head.into()),
                status: TreeStatus::Clean,
            },
            end: Boundary {
                head: Some(head.into()),
                status: TreeStatus::Clean,
            },
            started_at: now - chrono::Duration::seconds(30),
            finished_at: now,
            exit: 0,
            completed: true,
            steps: [StepName::Nextest, StepName::Doctests]
                .iter()
                .enumerate()
                .map(|(index, name)| Step {
                    order: index as u32 + 1,
                    name: *name,
                    program: None,
                    started_at: now - chrono::Duration::seconds(20),
                    finished_at: Some(now - chrono::Duration::seconds(10)),
                    status: Some(0),
                    counts: None,
                })
                .collect(),
        }
    }

    /// A daemon with a project at `repo`, an agent registered in it, and
    /// the checkout.
    async fn daemon() -> (tempfile::TempDir, Arc<Daemon>, PathBuf, AgentId) {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let daemon =
            Arc::new(Daemon::open(dir.path().join("state"), dir.path().join("sock")).unwrap());
        let Response::Agent { agent } = daemon
            .handle(Request::Register {
                spec: AgentSpec {
                    name: "author".into(),
                    workdir: Some(repo.clone()),
                    ..Default::default()
                },
                pid: None,
                session: None,
            })
            .await
        else {
            panic!("registered")
        };
        (dir, daemon, repo, agent.id)
    }

    async fn record(
        daemon: &Arc<Daemon>,
        agent: Option<&AgentId>,
        runner: Option<&AgentId>,
        run: GateRunReport,
    ) -> Response {
        daemon
            .handle(Request::RecordGateRun {
                agent: agent.map(|id| id.to_string()),
                runner: runner.map(|id| id.to_string()),
                run,
            })
            .await
    }

    fn recorded(daemon: &Daemon) -> usize {
        daemon
            .recent_events(1000)
            .iter()
            .filter(|event| matches!(event.kind, EventKind::GateRunRecorded { .. }))
            .count()
    }

    /// A run is kept with its event and read back by branch and head; the
    /// same run again answers the first without a second event; the same
    /// key with another report is a conflict, the first unchanged.
    #[tokio::test]
    async fn a_run_is_recorded_once_and_read_by_branch_and_head() {
        let (_dir, daemon, repo, author) = daemon().await;
        let run = report(&repo, "run-key-0001", "feature", A);
        let Response::GateRun { run: first } =
            record(&daemon, Some(&author), None, run.clone()).await
        else {
            panic!("recorded")
        };
        assert_eq!(first.by.as_ref(), Some(&author));
        assert_eq!(first.by_name.as_deref(), Some("author"));
        assert_eq!(recorded(&daemon), 1);
        let Response::GateRun { run: again } =
            record(&daemon, Some(&author), None, run.clone()).await
        else {
            panic!("answered")
        };
        assert_eq!(again, first, "the first, with its received time");
        assert_eq!(recorded(&daemon), 1, "no second event");
        let mut changed = run.clone();
        changed.exit = 1;
        let refused = record(&daemon, Some(&author), None, changed).await;
        assert!(
            matches!(
                &refused,
                Response::Error {
                    code: ErrorCode::Conflict,
                    ..
                }
            ),
            "{refused:?}"
        );
        // Another requester for the same run is a conflict too.
        let refused = record(&daemon, None, None, run).await;
        assert!(matches!(
            &refused,
            Response::Error {
                code: ErrorCode::Conflict,
                ..
            }
        ));
        // Read by branch and by head.
        let Response::GateRuns { runs, .. } = daemon
            .gate_runs(
                Some(author.as_str()),
                None,
                Some("feature".into()),
                Some(A.into()),
                None,
            )
            .await
        else {
            panic!("read")
        };
        assert_eq!(runs, vec![first.clone()]);
        let Response::GateRuns { runs, .. } = daemon
            .gate_runs(
                Some(author.as_str()),
                None,
                Some("other".into()),
                None,
                None,
            )
            .await
        else {
            panic!("read")
        };
        assert!(runs.is_empty());
    }

    /// The caller is `by`; a run with no caller has none, never the
    /// person; a runner is kept apart and is never `by`.
    #[tokio::test]
    async fn the_caller_is_by_and_the_runner_is_only_the_runner() {
        let (_dir, daemon, repo, author) = daemon().await;
        let Response::Agent { agent: transient } = daemon
            .handle(Request::Register {
                spec: AgentSpec {
                    name: "verify-1".into(),
                    workdir: Some(repo.clone()),
                    ..Default::default()
                },
                pid: None,
                session: None,
            })
            .await
        else {
            panic!("registered")
        };
        let cases = [
            ("known", Some(&author), None, Some(&author), None),
            ("unknown", None, None, None, None),
            (
                "runner",
                None,
                Some(&transient.id),
                None,
                Some(&transient.id),
            ),
            (
                "known with runner",
                Some(&author),
                Some(&transient.id),
                Some(&author),
                Some(&transient.id),
            ),
            (
                "runner as caller",
                Some(&transient.id),
                Some(&transient.id),
                None,
                Some(&transient.id),
            ),
        ];
        for (index, (case, agent, runner, by, kept_runner)) in cases.into_iter().enumerate() {
            let run = report(&repo, &format!("run-key-100{index}"), "feature", A);
            let Response::GateRun { run } = record(&daemon, agent, runner, run).await else {
                panic!("{case}")
            };
            assert_eq!(run.by.as_ref(), by, "{case}");
            assert_eq!(run.runner.as_ref(), kept_runner, "{case}");
            assert_ne!(run.by_name.as_deref(), Some("person"), "{case}");
        }
    }

    /// A malformed run is refused, and nothing is recorded: a short head,
    /// and counts that cannot add up.
    #[tokio::test]
    async fn a_malformed_run_is_refused() {
        let (_dir, daemon, repo, author) = daemon().await;
        let mut bad = report(&repo, "run-key-0001", "feature", A);
        bad.start.head = Some("abc".into());
        let refused = record(&daemon, Some(&author), None, bad).await;
        assert!(matches!(
            &refused,
            Response::Error {
                code: ErrorCode::Invalid,
                ..
            }
        ));
        // Counts that cannot add up are refused before anything is kept.
        let mut overflowing = report(&repo, "run-key-0002", "feature", A);
        overflowing.steps[0].counts = Some(agentdocker_core::verification::Counts {
            run: Some(0),
            passed: Some(u64::MAX),
            failed: Some(0),
            skipped: Some(1),
        });
        let refused = record(&daemon, Some(&author), None, overflowing).await;
        assert!(
            matches!(
                &refused,
                Response::Error {
                    code: ErrorCode::Invalid,
                    ..
                }
            ),
            "{refused:?}"
        );
        assert_eq!(recorded(&daemon), 0);
        let Response::GateRuns { runs, .. } = daemon
            .gate_runs(Some(author.as_str()), None, None, None, None)
            .await
        else {
            panic!("read")
        };
        assert!(runs.is_empty(), "nothing stored");
    }

    /// The newest runs are kept, the oldest forgotten past the bound.
    #[tokio::test]
    async fn a_project_keeps_its_newest_runs() {
        let (_dir, daemon, repo, author) = daemon().await;
        for index in 0..GATE_RUNS_PER_PROJECT + 2 {
            let run = report(&repo, &format!("run-key-{index:06}"), "feature", A);
            assert!(matches!(
                record(&daemon, Some(&author), None, run).await,
                Response::GateRun { .. }
            ));
        }
        let Response::GateRuns { runs, .. } = daemon
            .gate_runs(Some(author.as_str()), None, None, None, None)
            .await
        else {
            panic!("read")
        };
        assert_eq!(runs.len(), GATE_RUNS_PER_PROJECT);
        assert_eq!(
            runs[0].report.run_key,
            format!("run-key-{:06}", GATE_RUNS_PER_PROJECT + 1)
        );
        assert!(
            runs.iter()
                .all(|run| run.report.run_key != "run-key-000000")
        );
    }
}
