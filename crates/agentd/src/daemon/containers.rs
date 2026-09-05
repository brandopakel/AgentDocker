//! Persist intent before engine side effects; release protection only on confirmed exit.
use super::*;
use agentdocker_core::{
    ImageBuild,
    container::{ContainerIntent, ManagedContainer},
};
use agentdocker_host::containers::{ContainerBackend, ContainerError, ContainerState, Inspection};

struct Busy {
    daemon: Arc<Daemon>,
    id: AgentId,
}
impl Drop for Busy {
    fn drop(&mut self) {
        lock(&self.daemon.state).container_busy.remove(&self.id);
    }
}

impl Daemon {
    pub fn container_record(&self, id: &AgentId) -> Option<AgentRecord> {
        lock(&self.state)
            .registry
            .get(id)
            .filter(|a| a.container.is_some())
            .cloned()
    }

    fn current_container(&self, id: &AgentId) -> Result<AgentRecord, ContainerError> {
        let state = lock(&self.state);
        if let Some(error) = &state.storage_error {
            return Err(ContainerError(error.clone()));
        }
        state
            .registry
            .get(id)
            .filter(|a| a.container.is_some())
            .cloned()
            .ok_or_else(|| ContainerError("container agent vanished".into()))
    }

    async fn engine_call<T: Send + 'static>(
        &self,
        record: AgentRecord,
        call: impl FnOnce(&dyn ContainerBackend, &AgentRecord) -> Result<T, ContainerError>
        + Send
        + 'static,
    ) -> Result<T, ContainerError> {
        let backend = self.container_backend.clone();
        tokio::task::spawn_blocking(move || call(backend.as_ref(), &record))
            .await
            .map_err(|e| ContainerError(e.to_string()))?
    }

    fn update_container(
        &self,
        id: &AgentId,
        change: impl FnOnce(&mut AgentRecord),
    ) -> Result<(), ContainerError> {
        let mut state = lock(&self.state);
        if let Some(error) = &state.storage_error {
            return Err(ContainerError(error.clone()));
        }
        let mut record = state
            .registry
            .get(id)
            .cloned()
            .ok_or_else(|| ContainerError("container agent vanished".into()))?;
        let before = record.clone();
        change(&mut record);
        if record != before {
            state.save_container_transition(record);
        }
        if let Some(error) = &state.storage_error {
            return Err(ContainerError(error.clone()));
        }
        Ok(())
    }

    pub(super) async fn run_container(
        self: &Arc<Self>,
        spec: AgentSpec,
        build: String,
    ) -> Response {
        if spec.command.first().is_none_or(String::is_empty)
            || spec.command.iter().any(|s| s.contains('\0'))
            || spec
                .env
                .iter()
                .any(|(k, v)| k.is_empty() || k.contains(['=', '\0']) || v.contains('\0'))
        {
            return Response::error(
                ErrorCode::Invalid,
                "a container needs a command and valid environment pairs",
            );
        }
        let build = {
            let state = lock(&self.state);
            match state.store.documents::<ImageBuild>("image_build", None) {
                Ok(builds) => match builds.into_iter().find(|b| b.id == build) {
                    Some(build) => build,
                    None => return Response::error(ErrorCode::NotFound, "unknown image build ID"),
                },
                Err(e) => return Response::error(ErrorCode::StorageUnavailable, e.to_string()),
            }
        };
        let project = self.project_for(spec.workdir.clone(), true).await;
        let vcs = Self::vcs_for(spec.workdir.clone()).await;
        let mut record = AgentRecord::new(spec, true, Utc::now());
        record.project = project;
        record.vcs = vcs;
        record.container = Some(ManagedContainer {
            build: build.id,
            engine: build.spec.engine,
            connection: build.spec.connection,
            image_id: build.image_id,
            name: format!("agentdocker-{}", record.id),
            owner: AgentId::generate().to_string(),
            id: None,
            intent: ContainerIntent::Run,
            start_attempted: false,
            last_error: None,
        });
        let record = {
            let mut state = lock(&self.state);
            match state.insert_record(record) {
                Response::Agent { agent } => {
                    state.container_busy.insert(agent.id.clone());
                    agent
                }
                other => return other,
            }
        };
        if let Err(e) = self.drive_container(record.id.clone(), true).await {
            return Response::error(
                ErrorCode::EngineUnavailable,
                format!("agent {} retained for reconciliation: {e}", record.id),
            );
        }
        lock(&self.state).inspect(record.id.as_str())
    }

    pub(super) fn request_container_stop(
        &self,
        id: &AgentId,
        force: bool,
    ) -> Result<(), ContainerError> {
        self.update_container(id, |record| {
            if record.status.is_live() {
                let c = record.container.as_mut().unwrap();
                c.intent = if force || c.intent == ContainerIntent::Kill {
                    ContainerIntent::Kill
                } else {
                    ContainerIntent::Stop
                };
                record.status = AgentStatus::Stopping;
            }
        })
    }

    pub(super) async fn stop_agent(self: &Arc<Self>, reference: &str, force: bool) -> Response {
        let id = match self.resolve(reference) {
            Ok(id) => id,
            Err(e) => return *e,
        };
        if self.container_record(&id).is_none() {
            return self.stop(reference, force);
        }
        if let Err(e) = self.request_container_stop(&id, force) {
            return Response::error(ErrorCode::StorageUnavailable, e.to_string());
        }
        if let Err(e) = self.drive_container(id.clone(), false).await {
            return Response::error(
                ErrorCode::EngineUnavailable,
                format!("stop remains requested for {id}: {e}"),
            );
        }
        lock(&self.state).inspect(id.as_str())
    }

    pub(super) async fn restart_container(self: &Arc<Self>, reference: &str) -> Response {
        let id = match self.resolve(reference) {
            Ok(id) => id,
            Err(e) => return *e,
        };
        let Some(record) = self.container_record(&id) else {
            return Response::error(
                ErrorCode::Invalid,
                "restart requires a managed container agent",
            );
        };
        if record.status.is_live() {
            if let error @ Response::Error { .. } = self.stop_agent(id.as_str(), false).await {
                return error;
            }
        }
        // A replacement must not overlap a still-running writer. The caller may retry.
        if self.is_live(&id) {
            return Response::error(
                ErrorCode::Conflict,
                "container exit is not confirmed; retry restart after reconciliation",
            );
        }
        self.run_container(record.spec, record.container.unwrap().build)
            .await
    }

    pub fn reconcile_containers(self: &Arc<Self>) {
        let ids: Vec<_> = {
            let state = lock(&self.state);
            if state.storage_error.is_some() {
                return;
            }
            state
                .registry
                .live()
                .filter(|a| a.container.is_some() && !state.container_busy.contains(&a.id))
                .map(|a| a.id.clone())
                .collect()
        };
        for id in ids {
            let daemon = self.clone();
            tokio::spawn(async move {
                let _ = daemon.drive_container(id, false).await;
            });
        }
    }

    async fn drive_container(
        self: &Arc<Self>,
        id: AgentId,
        create: bool,
    ) -> Result<(), ContainerError> {
        {
            let mut state = lock(&self.state);
            if !state.container_busy.insert(id.clone()) && !create {
                return Ok(());
            }
        }
        let _busy = Busy {
            daemon: self.clone(),
            id: id.clone(),
        };
        let _permit = if create {
            self.container_slots.clone().acquire_owned().await.ok()
        } else {
            self.container_slots.clone().try_acquire_owned().ok()
        };
        if _permit.is_none() {
            return Ok(());
        }
        let result = self.drive_container_inner(&id, create).await;
        if let Err(e) = &result {
            let message: String = e.to_string().chars().take(2048).collect();
            if self
                .container_record(&id)
                .is_some_and(|r| r.container.unwrap().last_error.as_ref() != Some(&message))
            {
                warn!(agent = %id, error = %message, "container state uncertain; protection retained");
            }
            let _ = self.update_container(&id, |r| {
                r.container.as_mut().unwrap().last_error = Some(message)
            });
        }
        result
    }

    async fn drive_container_inner(
        &self,
        id: &AgentId,
        create: bool,
    ) -> Result<(), ContainerError> {
        let record = self.current_container(id)?;
        if !record.status.is_live() {
            return Ok(());
        }
        let inspected = if create {
            // Only the original run request creates. Recovery only discovers an existing
            // owned name, so an ambiguous/lost create response cannot duplicate a run.
            self.engine_call(record, |b, r| b.create(r)).await?
        } else {
            self.engine_call(record, |b, r| b.inspect(r)).await?
        };
        self.observe_container(id, &inspected)?;
        let mut record = self.current_container(id)?;
        if !record.status.is_live() {
            return Ok(());
        }
        let c = record.container.as_ref().unwrap();
        match (c.intent, inspected.state) {
            (ContainerIntent::Run, ContainerState::Created) => {
                if c.start_attempted {
                    return Err(ContainerError(c.last_error.clone().unwrap_or_else(|| "previous start outcome is unknown; stop this container before retrying as a new agent".into())));
                }
                self.update_container(id, |r| {
                    r.container.as_mut().unwrap().start_attempted = true
                })?;
                record = self.current_container(id)?;
                // Stop may have arrived while identity was being committed.
                if record.container.as_ref().unwrap().intent != ContainerIntent::Run {
                    return Ok(());
                }
                self.engine_call(record, |b, r| b.start(r)).await?;
            }
            (ContainerIntent::Stop | ContainerIntent::Kill, ContainerState::Created) => {
                // A stopped never-started container is removed by exact verified ID;
                // a delayed start cannot resurrect it after protection is released.
                self.engine_call(record, |b, r| b.remove_created(r)).await?;
                self.update_container(id, |r| r.status = AgentStatus::Exited { code: None })?;
                return Ok(());
            }
            (ContainerIntent::Stop | ContainerIntent::Kill, ContainerState::Running) => {
                let force = c.intent == ContainerIntent::Kill;
                self.engine_call(record, move |b, r| b.stop(r, force))
                    .await?;
            }
            _ => return Ok(()),
        }
        let record = self.current_container(id)?;
        let inspected = self.engine_call(record, |b, r| b.inspect(r)).await?;
        self.observe_container(id, &inspected)
    }

    fn observe_container(
        &self,
        id: &AgentId,
        inspected: &Inspection,
    ) -> Result<(), ContainerError> {
        self.update_container(id, |record| {
            let c = record.container.as_mut().unwrap();
            c.id = Some(inspected.id.clone());
            if inspected.state != ContainerState::Created || !c.start_attempted {
                c.last_error = None;
            }
            record.status = match inspected.state {
                ContainerState::Exited(code) => AgentStatus::Exited { code: Some(code) },
                ContainerState::Running if c.intent == ContainerIntent::Run => AgentStatus::Running,
                _ if c.intent != ContainerIntent::Run => AgentStatus::Stopping,
                _ => AgentStatus::Created,
            };
        })
    }

    pub async fn container_logs(
        &self,
        record: AgentRecord,
        tail: usize,
    ) -> Result<String, ContainerError> {
        self.engine_call(record, move |b, r| {
            b.logs(r, if tail == 0 { 10000 } else { tail })
        })
        .await
    }
}

impl State {
    fn save_container_transition(&mut self, mut record: AgentRecord) {
        let Some(previous) = self.registry.get(&record.id).cloned() else {
            return;
        };
        let now = Utc::now();
        let exited = previous.status.is_live() && !record.status.is_live();
        if record.status == AgentStatus::Running && record.started_at.is_none() {
            record.started_at = Some(now);
        }
        if exited {
            record.finished_at = Some(now);
        }
        record.last_seen = now;
        let released: Vec<Lease> = if exited {
            self.leases
                .all()
                .into_iter()
                .filter(|l| l.holder == record.id)
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        let mut journal = Vec::new();
        if exited {
            if !released.is_empty() {
                if let Some(entry) =
                    self.release_entry(&record.id, &released, None, SummarySource::Synthesised)
                {
                    journal.push(entry);
                }
            }
            if let Some(entry) = self.plain_entry(
                &record,
                JournalKind::Leave,
                format!("left ({})", record.status),
                SummarySource::Synthesised,
            ) {
                journal.push(entry);
            }
        }
        let mut kinds = vec![EventKind::ContainerUpdated {
            agent: record.id.clone(),
        }];
        if record.status != previous.status {
            match &record.status {
                AgentStatus::Running => kinds.push(EventKind::AgentStarted {
                    agent: record.id.clone(),
                    pid: None,
                }),
                AgentStatus::Stopping => kinds.push(EventKind::AgentStopping {
                    agent: record.id.clone(),
                    force: record.container.as_ref().unwrap().intent == ContainerIntent::Kill,
                }),
                _ if exited => kinds.push(EventKind::AgentExited {
                    agent: record.id.clone(),
                    status: record.status.clone(),
                }),
                _ => {}
            }
        }
        kinds.extend(
            released
                .iter()
                .cloned()
                .map(|lease| EventKind::LeaseReleased { lease }),
        );
        for entry in &mut journal {
            entry.seq = self.next_journal_seq(&entry.project);
            kinds.push(EventKind::JournalAppended {
                entry: entry.clone(),
            });
        }
        let events: Vec<_> = kinds
            .into_iter()
            .enumerate()
            .map(|(i, kind)| {
                let mut e = Event::new(kind, now);
                e.seq = self.next_seq + i as u64;
                e
            })
            .collect();
        let leases: Vec<_> = released.iter().map(|l| l.id.clone()).collect();
        self.persist("container transition", |store| {
            store.container_transition(&record, &leases, &journal, &events)
        });
        if self.storage_error.is_some() {
            return;
        }
        *self.registry.get_mut(&record.id).unwrap() = record.clone();
        if exited {
            self.leases.release_all(&record.id);
        }
        for entry in journal {
            self.cache_journal(entry);
        }
        self.next_seq += events.len() as u64;
        for event in events {
            let _ = self.events.send(event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::{ContainerEngine, ImageBuildSpec};
    use std::sync::atomic::{AtomicBool, Ordering};

    #[derive(Default)]
    struct Fake {
        observed: Mutex<Option<Inspection>>,
        actions: Mutex<Vec<&'static str>>,
        unavailable: AtomicBool,
        lost_create: AtomicBool,
        lost_start: AtomicBool,
        stop_stays_live: AtomicBool,
    }
    impl ContainerBackend for Fake {
        fn create(&self, _: &AgentRecord) -> Result<Inspection, ContainerError> {
            lock(&self.actions).push("create");
            let inspection = Inspection {
                id: "b".repeat(64),
                state: ContainerState::Created,
            };
            *lock(&self.observed) = Some(inspection.clone());
            if self.lost_create.load(Ordering::SeqCst) {
                return Err(ContainerError("lost create response".into()));
            }
            Ok(inspection)
        }
        fn inspect(&self, _: &AgentRecord) -> Result<Inspection, ContainerError> {
            if self.unavailable.load(Ordering::SeqCst) {
                return Err(ContainerError("engine unreachable".into()));
            }
            lock(&self.observed)
                .clone()
                .ok_or_else(|| ContainerError("not found is not confirmed exit".into()))
        }
        fn start(&self, _: &AgentRecord) -> Result<(), ContainerError> {
            lock(&self.actions).push("start");
            if self.lost_start.load(Ordering::SeqCst) {
                return Err(ContainerError("lost start response".into()));
            }
            lock(&self.observed).as_mut().unwrap().state = ContainerState::Running;
            Ok(())
        }
        fn stop(&self, _: &AgentRecord, force: bool) -> Result<(), ContainerError> {
            lock(&self.actions).push(if force { "kill" } else { "stop" });
            if !self.stop_stays_live.load(Ordering::SeqCst) {
                lock(&self.observed).as_mut().unwrap().state =
                    ContainerState::Exited(if force { 137 } else { 0 });
            }
            Ok(())
        }
        fn remove_created(&self, _: &AgentRecord) -> Result<(), ContainerError> {
            lock(&self.actions).push("remove_created");
            *lock(&self.observed) = None;
            Ok(())
        }
        fn logs(&self, _: &AgentRecord, _: usize) -> Result<String, ContainerError> {
            Ok("hello\n".into())
        }
    }
    fn open(dir: &Path, fake: Arc<Fake>) -> Arc<Daemon> {
        let mut daemon = Daemon::open(dir.into(), dir.join("sock")).unwrap();
        daemon.container_backend = fake;
        Arc::new(daemon)
    }
    fn seed(daemon: &Daemon) {
        let build = ImageBuild {
            id: "build".into(),
            spec: ImageBuildSpec {
                engine: ContainerEngine::Docker,
                connection: Some("pinned".into()),
                context: "/inputs".into(),
                recipe: "Containerfile".into(),
                timeout_secs: 60,
            },
            captured_at: Utc::now(),
            finished_at: Utc::now(),
            context_version: "context".into(),
            recipe_version: "recipe".into(),
            image_id: format!("sha256:{}", "a".repeat(64)),
            client_version: "test".into(),
            server_version: None,
            os: "linux".into(),
            architecture: "amd64".into(),
            variant: None,
        };
        lock(&daemon.state)
            .store
            .put_document("image_build", "build", &build)
            .unwrap();
    }
    async fn launch(daemon: &Arc<Daemon>, dir: &Path) -> Response {
        daemon
            .handle(Request::RunContainer {
                spec: AgentSpec {
                    name: "worker".into(),
                    command: vec!["sh".into()],
                    workdir: Some(dir.into()),
                    ..AgentSpec::default()
                },
                build: "build".into(),
            })
            .await
    }
    fn retained(daemon: &Daemon) -> AgentRecord {
        lock(&daemon.state).registry.live().next().unwrap().clone()
    }
    async fn claim(daemon: &Arc<Daemon>, record: &AgentRecord) -> Lease {
        let Response::Lease { lease } = daemon
            .handle(Request::Claim {
                agent: record.id.to_string(),
                resource: "task:protected".into(),
                mode: LeaseMode::Exclusive,
                ttl_secs: 600,
                note: None,
                wait_secs: 0,
            })
            .await
        else {
            panic!("claim failed")
        };
        lease
    }

    #[tokio::test]
    async fn daemon_restart_and_engine_outage_retain_container_and_lease() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = Arc::new(Fake::default());
        let daemon = open(tmp.path(), fake.clone());
        seed(&daemon);
        let Response::Agent { agent } = launch(&daemon, tmp.path()).await else {
            panic!()
        };
        assert_eq!(agent.status, AgentStatus::Running);
        assert_eq!(agent.pid, None);
        let lease = claim(&daemon, &agent).await;
        drop(daemon);
        let daemon = open(tmp.path(), fake.clone());
        daemon.check_liveness();
        fake.unavailable.store(true, Ordering::SeqCst);
        assert!(
            daemon
                .drive_container(agent.id.clone(), false)
                .await
                .is_err()
        );
        assert_eq!(retained(&daemon).status, AgentStatus::Running);
        assert_eq!(lock(&daemon.state).leases.all()[0].id, lease.id);
        assert_eq!(lock(&fake.actions).as_slice(), ["create", "start"]);
        fake.unavailable.store(false, Ordering::SeqCst);
        let Response::Agent { agent: stopped } = daemon.stop_agent(agent.id.as_str(), true).await
        else {
            panic!()
        };
        assert_eq!(stopped.status, AgentStatus::Exited { code: Some(137) });
        assert!(lock(&daemon.state).leases.all().is_empty());
        assert_eq!(lock(&fake.actions).as_slice(), ["create", "start", "kill"]);
        drop(daemon);
        assert!(lock(&open(tmp.path(), fake).state).leases.all().is_empty());
    }

    #[tokio::test]
    async fn lost_create_response_recovers_by_name_without_second_create() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = Arc::new(Fake::default());
        fake.lost_create.store(true, Ordering::SeqCst);
        let daemon = open(tmp.path(), fake.clone());
        seed(&daemon);
        assert!(matches!(
            launch(&daemon, tmp.path()).await,
            Response::Error { .. }
        ));
        let pending = retained(&daemon);
        assert!(pending.container.as_ref().unwrap().id.is_none());
        drop(daemon);
        let daemon = open(tmp.path(), fake.clone());
        assert_eq!(retained(&daemon).status, AgentStatus::Created);
        daemon.drive_container(pending.id, false).await.unwrap();
        assert_eq!(retained(&daemon).status, AgentStatus::Running);
        assert_eq!(lock(&fake.actions).as_slice(), ["create", "start"]);
    }

    #[tokio::test]
    async fn uncertain_start_is_never_retried_and_created_stop_removes_exact_container() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = Arc::new(Fake::default());
        fake.lost_start.store(true, Ordering::SeqCst);
        let daemon = open(tmp.path(), fake.clone());
        seed(&daemon);
        assert!(matches!(
            launch(&daemon, tmp.path()).await,
            Response::Error { .. }
        ));
        let pending = retained(&daemon);
        drop(daemon);
        let daemon = open(tmp.path(), fake.clone());
        for _ in 0..2 {
            assert!(
                daemon
                    .drive_container(pending.id.clone(), false)
                    .await
                    .is_err()
            );
        }
        assert_eq!(lock(&fake.actions).as_slice(), ["create", "start"]);
        let Response::Agent { agent } = daemon.stop_agent(pending.id.as_str(), false).await else {
            panic!()
        };
        assert!(!agent.status.is_live());
        assert_eq!(lock(&fake.actions).last(), Some(&"remove_created"));
        assert!(lock(&daemon.state).leases.all().is_empty());
    }

    #[tokio::test]
    async fn stop_response_is_not_exit_and_restart_uses_new_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = Arc::new(Fake::default());
        let daemon = open(tmp.path(), fake.clone());
        seed(&daemon);
        let Response::Agent { agent } = launch(&daemon, tmp.path()).await else {
            panic!()
        };
        claim(&daemon, &agent).await;
        fake.stop_stays_live.store(true, Ordering::SeqCst);
        let Response::Agent { agent: stopping } = daemon.stop_agent(agent.id.as_str(), false).await
        else {
            panic!()
        };
        assert_eq!(stopping.status, AgentStatus::Stopping);
        assert!(!lock(&daemon.state).leases.all().is_empty());
        assert!(matches!(
            daemon.restart_container(agent.id.as_str()).await,
            Response::Error {
                code: ErrorCode::Conflict,
                ..
            }
        ));
        assert_eq!(
            lock(&fake.actions)
                .iter()
                .filter(|op| **op == "create")
                .count(),
            1
        );
        fake.stop_stays_live.store(false, Ordering::SeqCst);
        let Response::Agent { agent: replacement } =
            daemon.restart_container(agent.id.as_str()).await
        else {
            panic!()
        };
        assert_ne!(replacement.id, agent.id);
        assert_eq!(
            replacement.container.as_ref().unwrap().image_id,
            agent.container.as_ref().unwrap().image_id
        );
        assert_ne!(
            replacement.container.as_ref().unwrap().owner,
            agent.container.as_ref().unwrap().owner
        );
        assert!(lock(&daemon.state).leases.all().is_empty());
    }

    #[tokio::test]
    async fn failed_exit_commit_publishes_nothing_and_retains_protection_after_restart() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = Arc::new(Fake::default());
        let daemon = open(tmp.path(), fake.clone());
        seed(&daemon);
        let Response::Agent { agent } = launch(&daemon, tmp.path()).await else {
            panic!()
        };
        let lease = claim(&daemon, &agent).await;
        let mut events = daemon.subscribe_events();
        lock(&daemon.state)
            .store
            .reject_event_for_test("agent_exited");
        lock(&fake.observed).as_mut().unwrap().state = ContainerState::Exited(0);
        assert!(
            daemon
                .drive_container(agent.id.clone(), false)
                .await
                .is_err()
        );
        assert!(events.try_recv().is_err());
        assert_eq!(retained(&daemon).status, AgentStatus::Running);
        drop(daemon);
        let daemon = open(tmp.path(), fake);
        assert_eq!(retained(&daemon).status, AgentStatus::Running);
        assert_eq!(lock(&daemon.state).leases.all()[0].id, lease.id);
    }

    #[tokio::test]
    async fn stop_intent_must_commit_before_signaling_and_survives_an_outage() {
        for reject in [false, true] {
            let tmp = tempfile::tempdir().unwrap();
            let fake = Arc::new(Fake::default());
            let daemon = open(tmp.path(), fake.clone());
            seed(&daemon);
            let Response::Agent { agent } = launch(&daemon, tmp.path()).await else {
                panic!()
            };
            claim(&daemon, &agent).await;
            if reject {
                lock(&daemon.state)
                    .store
                    .reject_event_for_test("container_updated");
            } else {
                fake.unavailable.store(true, Ordering::SeqCst);
            }
            assert!(matches!(
                daemon.stop_agent(agent.id.as_str(), true).await,
                Response::Error { .. }
            ));
            assert_eq!(lock(&fake.actions).as_slice(), ["create", "start"]);
            drop(daemon);
            let daemon = open(tmp.path(), fake.clone());
            let recovered = retained(&daemon);
            assert_eq!(
                recovered.container.unwrap().intent,
                if reject {
                    ContainerIntent::Run
                } else {
                    ContainerIntent::Kill
                }
            );
            assert!(!lock(&daemon.state).leases.all().is_empty());
            if !reject {
                fake.unavailable.store(false, Ordering::SeqCst);
                daemon
                    .drive_container(agent.id.clone(), false)
                    .await
                    .unwrap();
                assert!(!daemon.is_live(&agent.id));
                assert_eq!(lock(&fake.actions).last(), Some(&"kill"));
            }
        }
    }
}
