//! Journal checkpoints are persisted before optional lease release. A replacement
//! explicitly accepts only after content verification; acceptance never transfers locks.
use super::*;
use agentdocker_core::{Checkpoint, ReadMark, Recovery, Validation};
use agentdocker_host::content;
use std::os::unix::process::CommandExt;
use std::process::Stdio;

impl Daemon {
    pub(super) fn checkpoints(&self, reference: Option<&str>) -> Response {
        let mut state = lock(&self.state);
        let agent = match reference.map(|r| state.resolve(r)).transpose() {
            Ok(a) => a,
            Err(e) => return *e,
        };
        match state
            .store
            .documents::<Checkpoint>("checkpoint", agent.as_ref())
        {
            Ok(checkpoints) => Response::Checkpoints { checkpoints },
            Err(e) => internal(e),
        }
    }

    pub(super) async fn checkpoint(
        &self,
        reference: &str,
        key: String,
        task: String,
        assumptions: Vec<String>,
        next_steps: Vec<String>,
        release: bool,
    ) -> Response {
        if key.is_empty()
            || key.len() > 128
            || !key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
        {
            return Response::error(
                ErrorCode::Invalid,
                "checkpoint key must be 1–128 letters, digits, hyphens or underscores",
            );
        }
        if task.is_empty()
            || task.len()
                + assumptions
                    .iter()
                    .chain(&next_steps)
                    .map(String::len)
                    .sum::<usize>()
                > 65536
        {
            return Response::error(
                ErrorCode::Invalid,
                "checkpoint needs a task and at most 64 KiB of notes",
            );
        }
        let (agent, checkout, _) = match self.reader_checkout(reference) {
            Ok(v) => v,
            Err(e) => return *e,
        };
        if release {
            // The release barrier, passed before any lock is taken: changes
            // the watcher is still debouncing reach the ledger first, so the
            // release entry written under the lock below sees them.
            self.flush_watcher().await;
        }
        let id = format!("{}:{key}", agent.as_str());
        {
            let state = lock(&self.state);
            match state.store.document::<Checkpoint>("checkpoint", &id) {
                Ok(Some(checkpoint)) => {
                    return checkpoint_retry(checkpoint, release, &task, &assumptions, &next_steps);
                }
                Ok(None) => {}
                Err(e) => return internal(e),
            }
        }
        let root = checkout.clone();
        let version = match tokio::task::spawn_blocking(move || content::fingerprint(&root)).await {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => return internal(e),
            Err(e) => return internal(e),
        };
        let mut state = lock(&self.state);
        if !state
            .registry
            .get(&agent)
            .is_some_and(|a| a.status == AgentStatus::Running)
        {
            return Response::error(ErrorCode::Forbidden, "agent stopped during checkpoint");
        }
        // Recheck under the mutation lock after host I/O: concurrent retries must
        // not overwrite the winning checkpoint or its acknowledgement.
        match state.store.document::<Checkpoint>("checkpoint", &id) {
            Ok(Some(checkpoint)) => {
                return checkpoint_retry(checkpoint, release, &task, &assumptions, &next_steps);
            }
            Ok(None) => {}
            Err(e) => return internal(e),
        }
        let reads = match state
            .store
            .document::<Vec<ReadMark>>("reads", agent.as_str())
        {
            Ok(reads) => reads.unwrap_or_default(),
            Err(e) => return internal(e),
        };
        let checkpoint = Checkpoint {
            id: id.clone(),
            from: agent.clone(),
            checkout,
            created_at: Utc::now(),
            task,
            assumptions,
            next_steps,
            reads,
            version,
            accepted_by: None,
            release_leases: release,
        };
        if let Err(e) = state.store.put_document("checkpoint", &id, &checkpoint) {
            return internal(e);
        }
        state.emit(EventKind::CheckpointSaved {
            agent: agent.clone(),
            checkpoint: id,
        });
        if release {
            state.release_all(
                agent.as_str(),
                None,
                agentdocker_core::SummarySource::Explicit,
            );
        }
        Response::Checkpoint { checkpoint }
    }

    pub(super) async fn resume(&self, reference: &str, id: &str, acknowledge: bool) -> Response {
        let (agent, checkout, _) = match self.reader_checkout(reference) {
            Ok(v) => v,
            Err(e) => return *e,
        };
        let checkpoint = match lock(&self.state)
            .store
            .document::<Checkpoint>("checkpoint", id)
        {
            Ok(Some(c)) => c,
            Ok(None) => return Response::error(ErrorCode::NotFound, "checkpoint not found"),
            Err(e) => return internal(e),
        };
        if checkpoint.checkout != checkout {
            return Response::error(
                ErrorCode::Forbidden,
                "handoff belongs to a different physical checkout; integrate it before resuming",
            );
        }
        let saved = checkpoint.clone();
        let checked = tokio::task::spawn_blocking(move || {
            let stale = super::working::check_reads(&saved.reads);
            let same = content::fingerprint(&saved.checkout).is_ok_and(|v| v == saved.version);
            (stale, same)
        })
        .await;
        let (stale, checkout_matches) = match checked {
            Ok(v) => v,
            Err(e) => return internal(e),
        };
        let mut state = lock(&self.state);
        if !state
            .registry
            .get(&agent)
            .is_some_and(|a| a.status == AgentStatus::Running)
        {
            return Response::error(
                ErrorCode::Forbidden,
                "replacement stopped during verification",
            );
        }
        let mut checkpoint = match state.store.document::<Checkpoint>("checkpoint", id) {
            Ok(Some(c)) => c,
            Ok(None) => return Response::error(ErrorCode::NotFound, "checkpoint disappeared"),
            Err(e) => return internal(e),
        };
        if acknowledge {
            if !stale.is_empty() || !checkout_matches {
                return Response::error(
                    ErrorCode::Conflict,
                    "handoff content changed; review it, reread, and create a new checkpoint before accepting",
                );
            }
            if checkpoint.accepted_by.as_ref().is_some_and(|a| a != &agent) {
                return Response::error(
                    ErrorCode::Conflict,
                    "handoff was already accepted by another session",
                );
            }
            if checkpoint.accepted_by.is_none() {
                // Do not overwrite observations the replacement already made.
                let current = match state
                    .store
                    .document::<Vec<ReadMark>>("reads", agent.as_str())
                {
                    Ok(v) => v.unwrap_or_default(),
                    Err(e) => return internal(e),
                };
                let mut reads: BTreeMap<_, _> = checkpoint
                    .reads
                    .iter()
                    .cloned()
                    .map(|m| (m.path.clone(), m))
                    .collect();
                for mark in current {
                    reads.insert(mark.path.clone(), mark);
                }
                if reads.len() > 1000 {
                    return Response::error(
                        ErrorCode::Invalid,
                        "combined read set exceeds capacity",
                    );
                }
                checkpoint.accepted_by = Some(agent.clone());
                if let Err(e) = state.store.accept_handoff(
                    &checkpoint,
                    &agent,
                    &reads.into_values().collect::<Vec<_>>(),
                ) {
                    return internal(e);
                }
                state.emit(EventKind::HandoffAccepted {
                    agent,
                    checkpoint: id.into(),
                });
            }
        }
        let validations = match state
            .store
            .matching_validations(&checkpoint.checkout, &checkpoint.version)
        {
            Ok(v) => v,
            Err(e) => return internal(e),
        };
        Response::Recovery {
            recovery: Recovery {
                checkpoint,
                stale,
                checkout_matches,
                validations,
            },
        }
    }

    pub(super) fn validations(&self, reference: &str) -> Response {
        let mut state = lock(&self.state);
        let agent = match state.resolve(reference) {
            Ok(a) => a,
            Err(e) => return *e,
        };
        match state
            .store
            .documents::<Validation>("validation", Some(&agent))
        {
            Ok(validations) => Response::Validations { validations },
            Err(e) => internal(e),
        }
    }

    pub(super) async fn validate(
        &self,
        reference: &str,
        command: Vec<String>,
        timeout_secs: u64,
    ) -> Response {
        if command.first().is_none_or(String::is_empty)
            || command.len() > 256
            || timeout_secs == 0
            || timeout_secs > 600
        {
            return Response::error(
                ErrorCode::Invalid,
                "validation needs a command and a timeout of 1–600 seconds",
            );
        }
        let (agent, checkout, _) = match self.reader_checkout(reference) {
            Ok(v) => v,
            Err(e) => return *e,
        };
        let root = checkout.clone();
        let (before, head) = match tokio::task::spawn_blocking(move || {
            content::fingerprint(&root)
                .map(|version| (version, vcs::state(&root).and_then(|v| v.head)))
        })
        .await
        {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => return internal(e),
            Err(e) => return internal(e),
        };
        let id = MessageId::generate().to_string();
        let log = self.home.join("logs").join(format!("validation-{id}.log"));
        if let Err(e) = std::fs::create_dir_all(self.home.join("logs")) {
            return internal(e);
        }
        let output = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&log)
        {
            Ok(f) => f,
            Err(e) => return internal(e),
        };
        let error = match output.try_clone() {
            Ok(f) => f,
            Err(e) => return internal(e),
        };
        let mut validation = Validation {
            id: id.clone(),
            agent: agent.clone(),
            checkout: checkout.clone(),
            command: command.clone(),
            started_at: Utc::now(),
            finished_at: Utc::now(),
            head,
            before,
            after: None,
            exit_code: None,
            timed_out: false,
            descendants_survived: false,
            log,
        };
        {
            let state = lock(&self.state);
            if !state
                .registry
                .get(&agent)
                .is_some_and(|a| a.status == AgentStatus::Running)
            {
                return Response::error(
                    ErrorCode::Forbidden,
                    "agent stopped before validation started",
                );
            }
            if let Err(e) = state.store.put_document("validation", &id, &validation) {
                return internal(e);
            }
        }
        let mut cmd = tokio::process::Command::new(&command[0]);
        cmd.args(&command[1..])
            .current_dir(&checkout)
            .stdin(Stdio::null())
            .stdout(output)
            .stderr(error)
            .kill_on_drop(true);
        cmd.as_std_mut().process_group(0);
        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => return internal(e),
        };
        let pid = child.id().and_then(signal_pid).unwrap();
        let mut group = ValidationGroup(Some(Pid::from_raw(-pid.as_raw())));
        match tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), child.wait()).await
        {
            Ok(Ok(status)) => validation.exit_code = status.code(),
            Ok(Err(e)) => return internal(e),
            Err(_) => {
                validation.timed_out = true;
                let _ = kill(group.0.unwrap(), Signal::SIGKILL);
                let _ = child.wait().await;
            }
        }
        if !validation.timed_out && kill(group.0.unwrap(), None).is_ok() {
            validation.descendants_survived = true;
            let _ = kill(group.0.unwrap(), Signal::SIGKILL);
        }
        group.0 = None;
        validation.finished_at = Utc::now();
        validation.after = tokio::task::spawn_blocking(move || content::fingerprint(&checkout))
            .await
            .ok()
            .and_then(Result::ok);
        let passed = validation.passed();
        let mut state = lock(&self.state);
        if let Err(e) = state.store.put_document("validation", &id, &validation) {
            return internal(e);
        }
        state.emit(EventKind::ValidationFinished {
            agent,
            validation: id,
            passed,
        });
        Response::Validation { validation, passed }
    }
}

fn checkpoint_retry(
    checkpoint: Checkpoint,
    release: bool,
    task: &str,
    assumptions: &[String],
    next_steps: &[String],
) -> Response {
    if checkpoint.release_leases == release
        && checkpoint.task == task
        && checkpoint.assumptions == assumptions
        && checkpoint.next_steps == next_steps
    {
        Response::Checkpoint { checkpoint }
    } else {
        Response::error(
            ErrorCode::Conflict,
            "checkpoint key already names different content",
        )
    }
}

struct ValidationGroup(Option<Pid>);
impl Drop for ValidationGroup {
    fn drop(&mut self) {
        if let Some(group) = self.0 {
            let _ = kill(group, Signal::SIGKILL);
        }
    }
}
fn internal(error: impl std::fmt::Display) -> Response {
    Response::error(ErrorCode::Internal, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn fixture(tmp: &tempfile::TempDir) -> (Arc<Daemon>, PathBuf) {
        let root = tmp.path().join("checkout");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("file"), "one").unwrap();
        let daemon =
            Arc::new(Daemon::open(tmp.path().join("state"), tmp.path().join("sock")).unwrap());
        for name in ["original", "replacement", "other"] {
            daemon
                .handle(Request::Register {
                    spec: AgentSpec {
                        name: name.into(),
                        workdir: Some(root.clone()),
                        ..AgentSpec::default()
                    },
                    pid: None,
                })
                .await;
        }
        (daemon, root)
    }
    #[tokio::test]
    async fn handoff_is_durable_idempotent_and_refuses_stale_content() {
        let tmp = tempfile::tempdir().unwrap();
        let (daemon, root) = fixture(&tmp).await;
        daemon.observe("original", vec!["file".into()]).await;
        let save = || Request::Checkpoint {
            agent: "original".into(),
            key: "step1".into(),
            task: "fix parser".into(),
            assumptions: vec!["file is one".into()],
            next_steps: vec!["add regression".into()],
            release_leases: false,
        };
        let Response::Checkpoint { checkpoint } = daemon.handle(save()).await else {
            panic!()
        };
        assert_eq!(
            daemon.handle(save()).await,
            Response::Checkpoint {
                checkpoint: checkpoint.clone()
            }
        );
        std::fs::write(root.join("file"), "two").unwrap();
        let Response::Recovery { recovery } =
            daemon.resume("replacement", &checkpoint.id, false).await
        else {
            panic!()
        };
        assert_eq!(recovery.stale.len(), 1);
        assert!(!recovery.checkout_matches);
        assert!(matches!(
            daemon.resume("replacement", &checkpoint.id, true).await,
            Response::Error {
                code: ErrorCode::Conflict,
                ..
            }
        ));
        std::fs::write(root.join("file"), "one").unwrap();
        drop(daemon);
        let daemon =
            Arc::new(Daemon::open(tmp.path().join("state"), tmp.path().join("sock")).unwrap());
        for _ in 0..2 {
            assert!(
                matches!(daemon.resume("replacement", &checkpoint.id, true).await, Response::Recovery { recovery } if recovery.checkpoint.accepted_by.is_some())
            );
        }
        assert!(matches!(
            daemon.resume("other", &checkpoint.id, true).await,
            Response::Error {
                code: ErrorCode::Conflict,
                ..
            }
        ));
        assert!(
            matches!(daemon.reads("replacement"), Response::Reads { reads } if reads.len() == 1)
        );
    }
    #[tokio::test]
    async fn validation_binds_success_to_unchanged_content_and_timeout() {
        let tmp = tempfile::tempdir().unwrap();
        let (daemon, _) = fixture(&tmp).await;
        let command = |text: &str| vec!["sh".into(), "-c".into(), text.into()];
        assert!(matches!(
            daemon
                .validate("original", command("test -f file"), 5)
                .await,
            Response::Validation { passed: true, .. }
        ));
        assert!(
            matches!(daemon.validate("original", command("printf changed > file"), 5).await, Response::Validation { passed: false, validation } if Some(&validation.before) != validation.after.as_ref())
        );
        assert!(
            matches!(daemon.validate("original", command("sleep 10"), 1).await, Response::Validation { passed: false, validation } if validation.timed_out)
        );
        assert!(
            matches!(daemon.validations("original"), Response::Validations { validations } if validations.len() == 3)
        );
    }
    #[tokio::test]
    async fn checkpoint_is_journalled_before_releasing_leases() {
        let tmp = tempfile::tempdir().unwrap();
        let (daemon, _) = fixture(&tmp).await;
        daemon
            .handle(Request::Claim {
                agent: "original".into(),
                resource: "task:parse".into(),
                mode: LeaseMode::Exclusive,
                ttl_secs: 60,
                note: None,
                wait_secs: 0,
            })
            .await;
        assert!(matches!(
            daemon
                .checkpoint(
                    "original",
                    "barrier".into(),
                    "task".into(),
                    vec![],
                    vec![],
                    true
                )
                .await,
            Response::Checkpoint { .. }
        ));
        let events = daemon.recent_events(100);
        let saved = events
            .iter()
            .position(|e| matches!(e.kind, EventKind::CheckpointSaved { .. }))
            .unwrap();
        let released = events
            .iter()
            .position(|e| matches!(e.kind, EventKind::LeaseReleased { .. }))
            .unwrap();
        assert!(saved < released);
        assert!(events[saved].seq < events[released].seq);
    }
}
