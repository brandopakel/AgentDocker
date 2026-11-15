//! In-flight markers that clear themselves. Each marker is set before its
//! task and released by a guard the task owns, so it is cleared when the
//! task ends however it ends: returned, panicked, or dropped before it
//! ran. A marker means what it meant before; only who clears it changed.
//! A guard that takes the state lock is never dropped by a holder of a
//! state guard: each is made after the hold that set its marker ends.
use super::*;
use std::sync::Weak;
use std::sync::atomic::{AtomicBool, Ordering};

/// The daemon's run-one-at-a-time flags.
#[derive(Clone, Copy, Debug)]
pub(in crate::daemon) enum Flag {
    Mirroring,
    Judging,
    JudgeWatching,
}

impl Flag {
    fn of(self, daemon: &Daemon) -> &AtomicBool {
        match self {
            Self::Mirroring => &daemon.mirroring,
            Self::Judging => &daemon.judging,
            Self::JudgeWatching => &daemon.judge_watching,
        }
    }
}

/// One run that holds its flag until it ends.
pub(in crate::daemon) struct Running {
    daemon: Arc<Daemon>,
    flag: Flag,
}

impl Running {
    /// Set the flag and hold it for a run, or `None` while another run
    /// holds it.
    pub(in crate::daemon) fn take(daemon: &Arc<Daemon>, flag: Flag) -> Option<Self> {
        (!flag.of(daemon).swap(true, Ordering::SeqCst)).then(|| Self {
            daemon: daemon.clone(),
            flag,
        })
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.flag.of(&self.daemon).store(false, Ordering::SeqCst);
    }
}

/// A card an agent is being started on, as `starting_cards` marks it.
pub(in crate::daemon) struct StartingCard {
    daemon: Arc<Daemon>,
    task: agentdocker_core::TaskId,
}

impl StartingCard {
    /// Take over the release of a marker its caller has just set.
    pub(in crate::daemon) fn held(daemon: &Arc<Daemon>, task: agentdocker_core::TaskId) -> Self {
        Self {
            daemon: daemon.clone(),
            task,
        }
    }
}

impl Drop for StartingCard {
    fn drop(&mut self) {
        lock(&self.daemon.state).starting_cards.remove(&self.task);
    }
}

/// An agent waiting for its restart, as `pending_restarts` marks it. Weak,
/// as the waiting task is: a pending restart must not keep a daemon alive.
pub(in crate::daemon) struct PendingRestart {
    daemon: Weak<Daemon>,
    id: Option<AgentId>,
}

impl PendingRestart {
    /// Take over the release of a marker its caller has just set.
    pub(in crate::daemon) fn held(daemon: &Arc<Daemon>, id: AgentId) -> Self {
        Self {
            daemon: Arc::downgrade(daemon),
            id: Some(id),
        }
    }

    /// The restart ran, and its task released the marker itself under the
    /// hold that read what came of it, before it may set a new one.
    pub(in crate::daemon) fn released(mut self) {
        self.id = None;
    }
}

impl Drop for PendingRestart {
    fn drop(&mut self) {
        if let (Some(id), Some(daemon)) = (self.id.take(), self.daemon.upgrade()) {
            // Removed and said in one hold: a task that ended without
            // releasing leaves no restart scheduled, and readers learn so.
            let mut state = lock(&daemon.state);
            state.pending_restarts.remove(&id);
            state.publish_restart(&id);
        }
    }
}

impl Daemon {
    /// Where a test may make a task fail as it stands: a panic, as a bug
    /// would raise there.
    #[cfg(test)]
    pub(in crate::daemon) fn fail_point(&self, name: &str) {
        if lock(&self.fail_points).contains(name) {
            panic!("{name} failed, as the test asked");
        }
    }

    #[cfg(not(test))]
    #[inline(always)]
    pub(in crate::daemon) fn fail_point(&self, _name: &str) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn open(dir: &TempDir) -> Arc<Daemon> {
        let home = dir.path().to_path_buf();
        Arc::new(Daemon::open(home.clone(), home.join("sock")).unwrap())
    }

    fn fail(daemon: &Daemon, name: &'static str) {
        lock(&daemon.fail_points).insert(name);
    }

    /// Give the spawned tasks a moment to run, and say whether `done`
    /// came true within it.
    async fn within_a_moment(done: impl Fn() -> bool) -> bool {
        for _ in 0..300 {
            if done() {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        done()
    }

    /// A judge pass, or the judge's own read of its file, that panics
    /// leaves its flag clear, so the next tick starts it again.
    #[tokio::test]
    async fn a_panicking_judge_task_leaves_its_flag_clear() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        fail(&daemon, "judge watching");
        fail(&daemon, "judging");
        let both = |daemon: &Daemon| {
            (
                daemon.judge_watching.load(Ordering::SeqCst),
                daemon.judging.load(Ordering::SeqCst),
            )
        };
        for _tick in 0..2 {
            daemon.judge_screens();
            assert_eq!(both(&daemon), (true, true), "both runs started");
            assert!(within_a_moment(|| both(&daemon) == (false, false)).await);
        }
    }

    /// A mirror run that panics leaves its flag clear.
    #[tokio::test]
    async fn a_panicking_mirror_run_leaves_its_flag_clear() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        fail(&daemon, "mirroring");
        for _tick in 0..2 {
            daemon.mirror_memories();
            assert!(daemon.mirroring.load(Ordering::SeqCst), "the run started");
            assert!(within_a_moment(|| !daemon.mirroring.load(Ordering::SeqCst)).await);
        }
    }

    /// A restart task that panics no longer marks its agent as waiting
    /// for a restart, so the next look at the agent can schedule one.
    #[tokio::test]
    async fn a_panicking_restart_task_leaves_its_agent_unmarked() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let mut record = AgentRecord::new(
            AgentSpec {
                name: "restarted".into(),
                command: vec!["sleep".into(), "30".into()],
                restart: agentdocker_core::RestartPolicy::OnFailure { max: 1 },
                ..Default::default()
            },
            true,
            Utc::now(),
        );
        record.status = AgentStatus::Exited { code: Some(1) };
        // Long finished: its backoff is already spent.
        record.finished_at = Some(Utc::now() - Duration::hours(1));
        assert!(matches!(
            lock(&daemon.state).insert_record(record.clone()),
            Response::Agent { .. }
        ));
        fail(&daemon, "restart");
        let pending = || {
            lock(&daemon.state)
                .pending_restarts
                .contains_key(&record.id)
        };
        for _look in 0..2 {
            daemon.consider_restart(&record.id, &record.status);
            assert!(pending(), "marked as waiting for its restart");
            assert!(within_a_moment(|| !pending()).await);
        }
    }
}
