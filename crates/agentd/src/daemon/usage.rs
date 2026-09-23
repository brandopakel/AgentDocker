//! Local log collection runs off the coordination mutex. Only bounded accounting
//! proposals enter a fenced SQLite transaction; rejected proposals move nothing.
use super::*;
use crate::store::usage::discovery::{Change, Job, Progress};
use crate::store::usage::{Attribution as UsageAttribution, FileProgress, Ingest};
use agentdocker_core::config::{DaemonConfig, UsageConfig};
use agentdocker_core::usage::{self, Range, report::*};
use agentdocker_host::usage::{
    discovery::{Root, Walk},
    reader,
};
use std::sync::Weak;

fn configuration(home: &Path) -> Result<UsageConfig, String> {
    use agentdocker_host::policy_file::{self, ReadPolicy};
    let config =
        match policy_file::read_changed(&home.join(agentdocker_core::config::FILE_NAME), None)
            .map_err(|_| "usage configuration is unreadable")?
        {
            ReadPolicy::Text { text, .. } => {
                toml::from_str::<DaemonConfig>(&text)
                    .map_err(|_| "usage configuration is invalid")?
                    .usage
            }
            ReadPolicy::Absent | ReadPolicy::Unchanged => UsageConfig::default(),
        };
    config.check()?;
    if config.enabled {
        roots(&config)?;
    }
    Ok(config)
}

fn roots(config: &UsageConfig) -> Result<Vec<Root>, String> {
    let mut codex = config.codex_roots.clone();
    let mut claude = config.claude_roots.clone();
    if let Some(home) = std::env::home_dir() {
        if codex.is_empty() {
            let base = std::env::var_os("CODEX_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".codex"));
            codex.extend([base.join("sessions"), base.join("archived_sessions")]);
        }
        if claude.is_empty() {
            let base = std::env::var_os("CLAUDE_CONFIG_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".claude"));
            claude.push(base.join("projects"));
        }
    }
    let roots: Vec<_> = codex
        .into_iter()
        .map(|path| Root {
            runtime: reader::Runtime::Codex,
            path,
        })
        .chain(claude.into_iter().map(|path| Root {
            runtime: reader::Runtime::Claude,
            path,
        }))
        .collect();
    // Use the walk's own root validation here so configuration/query errors
    // explain every constraint before the worker attempts discovery.
    Walk::new(roots.clone()).map_err(str::to_owned)?;
    Ok(roots)
}

fn retained(now: DateTime<Utc>, config: &UsageConfig) -> DateTime<Utc> {
    usage::hour(now - Duration::days(i64::from(config.retention_days)))
}

impl Daemon {
    fn reconcile_usage(&self, config: &UsageConfig) {
        let mut state = lock(&self.state);
        let mut sessions: BTreeMap<(String, String), ()> = BTreeMap::new();
        for record in state.registry.all() {
            if let Some(session) = record.spec.labels.get("session_id") {
                sessions.insert((record.spec.runtime.clone(), session.clone()), ());
            }
        }
        // One bounded reconciliation page, selected by a persisted rotating
        // session key so unresolved old sessions cannot starve later ones.
        let Some(after) = state.store_read("usage reconciliation cursor", |store| {
            store.document::<(String, String)>("usage", "reconcile_after")
        }) else {
            return;
        };
        let mut entries: Vec<_> = sessions.into_keys().collect();
        if let Some(after) = after {
            let split = entries.partition_point(|key| key <= &after);
            entries.rotate_left(split);
        }
        for (runtime, session) in entries.into_iter().take(8) {
            // A session resumed under a new agent has several registrations;
            // each takes the samples made while it was the current one.
            let windows = usage::registration_windows(&registrations(&state, &runtime, &session));
            // With nobody to attribute to, the cursor still advances.
            let windows = if windows.is_empty() {
                vec![(UsageAttribution::default(), None, None)]
            } else {
                windows
            };
            for (attribution, from, until) in windows {
                let mut event = None;
                let seq = state.next_seq;
                let now = Utc::now();
                let result = state.persist("usage attribution", |store| {
                    event = store.usage_reconcile(
                        &runtime,
                        &session,
                        Some(&attribution),
                        (from, until),
                        retained(now, config),
                        now,
                        seq,
                    )?;
                    Ok(())
                });
                if result != Persisted::Committed {
                    return;
                }
                if let Some(event) = event {
                    state.next_seq += 1;
                    let _ = state.events.send(event);
                }
            }
        }
    }

    pub(super) async fn usage(&self, query: Query) -> Response {
        let config = match configuration(&self.home) {
            Ok(config) => config,
            Err(error) => return Response::error(ErrorCode::Invalid, error),
        };
        let now = Utc::now();
        let since = match query
            .since
            .as_deref()
            .map(|s| usage::since(s, now))
            .transpose()
        {
            Ok(since) => since,
            Err(error) => return Response::error(ErrorCode::Invalid, error),
        };
        let project = match query.project {
            Some(reference) => match self.resolve_project(&reference).await {
                Ok(project) => Some(project.to_string()),
                Err(error) => return *error,
            },
            None => None,
        };
        let mut state = lock(&self.state);
        let Some(retained_since) = state.store_read("usage retention", |store| {
            store.usage_retained_since(retained(now, &config))
        }) else {
            return state.storage_failure().unwrap_or_else(|| {
                Response::error(
                    ErrorCode::StorageUnavailable,
                    "usage retention could not be read",
                )
            });
        };
        let range = match Range::new(since, query.until, now, retained_since) {
            Ok(range) => range,
            Err(error) => return Response::error(ErrorCode::Invalid, error),
        };
        let agent = match query.agent {
            Some(reference) => match state.resolve(&reference) {
                Ok(agent) => Some(agent.to_string()),
                Err(error) => return *error,
            },
            None => None,
        };
        match state.store_read("usage query", |store| {
            store.usage_report(range, query.by, project.as_deref(), agent.as_deref())
        }) {
            Some(Some(mut report)) => {
                let effective_roots = match roots(&config) {
                    Ok(roots) => roots,
                    Err(error) => return Response::error(ErrorCode::Invalid, error),
                };
                let expected_roots: Vec<_> = effective_roots
                    .iter()
                    .map(|r| r.path.display().to_string())
                    .collect();
                if !config.enabled || report.coverage.collection.scope.roots != expected_roots {
                    report.coverage.collection = Collection::default();
                    for row in &mut report.rows {
                        for counter in [
                            &mut row.counters.input_tokens,
                            &mut row.counters.cache_read_input_tokens,
                            &mut row.counters.cache_write_input_tokens,
                            &mut row.counters.output_tokens,
                            &mut row.counters.reasoning_output_tokens,
                        ] {
                            if counter.coverage == usage::Coverage::Complete {
                                counter.coverage = usage::Coverage::Partial;
                            }
                        }
                    }
                }
                report.coverage.collection.enabled = Some(config.enabled);
                Response::Usage { report }
            }
            Some(None) => Response::error(
                ErrorCode::Invalid,
                "usage query exceeds its bucket or counter limit; narrow the time range",
            ),
            None => state.storage_failure().unwrap_or_else(|| {
                Response::error(ErrorCode::StorageUnavailable, "usage could not be read")
            }),
        }
    }

    /// Only the server starts this worker. Private library/test daemons do not
    /// scan host transcripts, and absent configuration leaves collection off.
    pub fn collect_usage(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        std::thread::spawn(move || worker(weak));
    }

    fn usage_snapshot(
        &self,
        collection: &Collection,
        gaps: &[(&str, &str)],
        change: Option<Change<'_>>,
    ) -> bool {
        let mut state = lock(&self.state);
        let seq = state.next_seq;
        let mut event = None;
        let result = state.persist("usage coverage", |store| {
            event = Some(store.usage_snapshot(collection, gaps, Utc::now(), seq, change)?);
            Ok(())
        });
        if result != Persisted::Committed {
            return false;
        }
        state.next_seq += 1;
        let _ = state.events.send(event.expect("committed usage event"));
        true
    }

    fn usage_batch(
        &self,
        key: &str,
        batch: &reader::Batch,
        collection: &Collection,
        config: &UsageConfig,
        finished_job: Option<usize>,
    ) -> bool {
        let mut state = lock(&self.state);
        let now = Utc::now();
        let samples: Vec<_> = batch
            .samples
            .iter()
            .map(|sample| {
                let attribution = usage::registration_at(
                    &registrations(&state, &sample.runtime, &sample.session_id),
                    sample.at,
                )
                .unwrap_or_default();
                (sample.clone(), attribution)
            })
            .collect();
        let progress = FileProgress {
            cursor: batch.cursor.clone(),
            stop: batch.stop,
        };
        let mut event = None;
        let seq = state.next_seq;
        let result = state.persist("usage accounting", |store| {
            event = Some(store.usage_ingest(Ingest {
                key,
                progress: &progress,
                samples: &samples,
                gaps: &batch.gaps,
                collection,
                retained_since: retained(now, config),
                now,
                event_seq: seq,
                finished_job,
            })?);
            Ok(())
        });
        if result != Persisted::Committed {
            return false;
        }
        state.next_seq += 1;
        let _ = state.events.send(event.expect("committed usage event"));
        true
    }
}

fn alive_wait(weak: &Weak<Daemon>, ticks: usize) -> bool {
    for _ in 0..ticks {
        if weak.strong_count() == 0 {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    true
}

fn worker(weak: Weak<Daemon>) {
    loop {
        let Some(daemon) = weak.upgrade() else {
            return;
        };
        let config = configuration(&daemon.home);
        drop(daemon);
        if let Ok(config) = config
            && config.enabled
        {
            collect_generation(&weak, &config);
            if let Some(daemon) = weak.upgrade() {
                daemon.reconcile_usage(&config);
            }
        }
        if !alive_wait(&weak, 300) {
            return;
        }
    }
}

fn snapshot(weak: &Weak<Daemon>, collection: &Collection, gaps: &[(&str, &str)]) -> bool {
    weak.upgrade()
        .is_some_and(|daemon| daemon.usage_snapshot(collection, gaps, None))
}

fn checkpoint(
    weak: &Weak<Daemon>,
    collection: &Collection,
    gaps: &[(&str, &str)],
    change: Change<'_>,
) -> bool {
    weak.upgrade()
        .is_some_and(|daemon| daemon.usage_snapshot(collection, gaps, Some(change)))
}

fn finish_failed_job(
    weak: &Weak<Daemon>,
    collection: &mut Collection,
    job: usize,
    gaps: &[(&str, &str)],
) -> bool {
    let mut next = collection.clone();
    if let Some(pending) = &mut next.pending_files {
        *pending = pending.saturating_sub(1);
    }
    if !checkpoint(weak, &next, gaps, Change::FinishedJob(job)) {
        return false;
    }
    *collection = next;
    true
}

fn collect_generation(weak: &Weak<Daemon>, config: &UsageConfig) {
    collect_generation_bounded(weak, config, usize::MAX);
}

// A bounded page allowance also lets restart trials stop at a committed frontier
// without killing a process while it owns the coordination mutex.
fn collect_generation_bounded(weak: &Weak<Daemon>, config: &UsageConfig, mut pages: usize) {
    let Ok(sources) = roots(config) else {
        return;
    };
    let Some(daemon) = weak.upgrade() else {
        return;
    };
    let previous = lock(&daemon.state).store_read("usage discovery", |store| {
        Ok((store.usage_collection()?, store.usage_discovery()?))
    });
    let live_sessions: Vec<String> = lock(&daemon.state)
        .registry
        .all()
        .filter(|a| a.status.is_live())
        .filter_map(|a| a.spec.labels.get("session_id").cloned())
        .collect();
    drop(daemon);
    let Some((previous_collection, previous_discovery)) = previous else {
        return;
    };
    let restored = previous_discovery
        .zip(previous_collection.clone())
        .filter(|(progress, collection)| {
            progress.roots == sources
                && collection.discovery_generation == Some(progress.generation)
        })
        .and_then(|(progress, collection)| {
            Walk::resume(progress.frontier.clone(), &sources)
                .ok()
                .map(|walk| (progress, collection, walk))
        });
    let (mut progress, mut collection, mut walk) = if let Some(restored) = restored {
        restored
    } else {
        let Some(generation) = previous_collection
            .and_then(|c| c.discovery_generation)
            .unwrap_or(0)
            .checked_add(1)
        else {
            return;
        };
        let Ok(walk) = Walk::new(sources.clone()) else {
            return;
        };
        let progress = Progress {
            roots: sources.clone(),
            generation,
            frontier: walk.checkpoint(),
            jobs: 0,
            failures: 0,
        };
        let collection = Collection {
            discovery_generation: Some(generation),
            snapshot_at: Some(Utc::now()),
            scope: Scope {
                roots: sources
                    .iter()
                    .map(|s| s.path.display().to_string())
                    .collect(),
                formats: vec![
                    "codex-rollout-0.153.4-0.154.0-v1".into(),
                    "codex-rollout-0.155.1-v1".into(),
                    "claude-transcript-2.1.268-270-v1".into(),
                    "claude-transcript-2.1.271-276-v1".into(),
                    "claude-transcript-2.1.277-278-280-v1".into(),
                ],
            },
            ..Collection::default()
        };
        if !checkpoint(weak, &collection, &[], Change::Start(&progress)) {
            return;
        }
        (progress, collection, walk)
    };
    let generation = progress.generation;
    // Once the frontier is exhausted, the saved manifest is already complete:
    // do not enumerate again or reset pending counts when resuming accounting.
    while !walk.finished() {
        if pages == 0 {
            return;
        }
        pages -= 1;
        let page = walk.next_page();
        let mut reasons: Vec<(String, &str)> = page
            .gaps
            .iter()
            .map(|reason| (format!("discovery:{generation}:{reason}"), *reason))
            .collect();
        let mut jobs = Vec::new();
        for source in page.sources {
            match reader::Cursor::capture(&source.path, source.runtime) {
                Ok(captured) => {
                    let priority = live_sessions.iter().any(|id| {
                        source
                            .path
                            .file_name()
                            .is_some_and(|f| f.to_string_lossy().contains(id))
                    });
                    jobs.push(Job {
                        id: progress.jobs,
                        source,
                        captured,
                        priority,
                    });
                    progress.jobs += 1;
                }
                Err(_) => {
                    progress.failures += 1;
                    reasons.push((
                        format!("capture:{generation}:{}", source.path.display()),
                        "usage file could not be captured",
                    ));
                }
            }
        }
        let frontier = walk.checkpoint();
        let advanced = frontier != progress.frontier;
        progress.frontier = frontier;
        if page.finished {
            collection.discovery_complete = page.complete;
            // Capture failures already have durable source gaps and no jobs.
            // Gaps keep reported token coverage partial, not work pending.
            collection.pending_files = page.complete.then_some(progress.jobs as u64);
            collection.pending_tail_files = page.complete.then_some(0);
            if page.complete {
                collection.state = CollectionState::Scanning;
            }
        }
        let gaps: Vec<_> = reasons
            .iter()
            .map(|(key, reason)| (key.as_str(), *reason))
            .collect();
        if (advanced || !jobs.is_empty() || !gaps.is_empty())
            && !checkpoint(weak, &collection, &gaps, Change::Page(&progress, &jobs))
        {
            return;
        }
        if !page.finished && !alive_wait(weak, 1) {
            return;
        }
    }
    loop {
        if pages == 0 {
            return;
        }
        let Some(daemon) = weak.upgrade() else {
            return;
        };
        let job =
            lock(&daemon.state).store_read("usage pending file", |store| store.usage_next_job());
        drop(daemon);
        let Some(job) = job else {
            return;
        };
        let Some(Job {
            id: job_id,
            source,
            captured,
            ..
        }) = job
        else {
            break;
        };
        let key = serde_json::to_string(&(source.runtime, &source.path))
            .expect("serializable source path");
        let Some(daemon) = weak.upgrade() else {
            return;
        };
        let old = lock(&daemon.state).store_read("usage cursor", |store| store.usage_file(&key));
        drop(daemon);
        let Some(old) = old else {
            return;
        };
        let previous = old.as_ref().map(|old| &old.cursor);
        let prepare =
            |previous: Option<&reader::Cursor>| -> Result<reader::Session, reader::Error> {
                let mut session = reader::Session::at_snapshot(captured.clone(), previous)?;
                loop {
                    let progress = session.prepare_next(
                        &source.path,
                        4 * 1024 * 1024,
                        std::time::Duration::from_millis(100),
                    )?;
                    if progress.ready {
                        return Ok(session);
                    }
                    if !alive_wait(weak, 1) {
                        return Err(reader::Error::ValidationIncomplete);
                    }
                }
            };
        let (mut session, mut previous_offset) = match prepare(previous) {
            Ok(session) => {
                let offset = session.offset();
                (session, offset)
            }
            Err(_) => {
                let gap_key = format!("generation:{generation}:{key}");
                if !snapshot(
                    weak,
                    &collection,
                    &[(
                        &gap_key,
                        "usage source changed or prefix verification failed; replay deduplicates accepted sources",
                    )],
                ) {
                    return;
                }
                // Retry once from the captured generation. If the file changed
                // after discovery, even the empty prefix refuses it this pass.
                let Ok(session) = prepare(None) else {
                    if !finish_failed_job(weak, &mut collection, job_id, &[]) {
                        return;
                    }
                    continue;
                };
                (session, 0)
            }
        };
        // Parsing is off-lock, but samples, gaps and the cursor commit together
        // under the coordination mutex. A full 4,096-record reader batch can
        // exceed the entire hook deadline on small hosts; keep these writes
        // short and yield between them without dropping or skipping records.
        let mut budget = reader::Budget {
            records: 128,
            ..reader::Budget::default()
        };
        loop {
            let batch = match session.scan(&source.path, budget) {
                Ok(batch) if session.validate(&source.path, &batch.cursor).is_ok() => batch,
                Err(reader::Error::Oversized { .. }) if budget.bytes < 16 * 1024 * 1024 => {
                    budget.bytes = 16 * 1024 * 1024;
                    continue;
                }
                _ => {
                    let gap_key = format!("scan:{generation}:{key}");
                    if !finish_failed_job(
                        weak,
                        &mut collection,
                        job_id,
                        &[(
                            &gap_key,
                            "usage source scan or prefix validation is incomplete",
                        )],
                    ) {
                        return;
                    }
                    break;
                }
            };
            let mut next = collection.clone();
            if batch.stop == reader::Stop::Budget && batch.cursor.offset() == previous_offset {
                let gap_key = format!("budget:{generation}:{key}");
                if !finish_failed_job(
                    weak,
                    &mut collection,
                    job_id,
                    &[(&gap_key, "usage scan budget made no progress")],
                ) {
                    return;
                }
                break;
            }
            let finished = matches!(
                batch.stop,
                reader::Stop::Complete | reader::Stop::PendingTail
            ) || (batch.stop == reader::Stop::Quarantined
                && budget.bytes == 16 * 1024 * 1024);
            if finished {
                if let Some(pending) = &mut next.pending_files {
                    *pending = pending.saturating_sub(1);
                }
                if batch.stop == reader::Stop::PendingTail
                    && let Some(tails) = &mut next.pending_tail_files
                {
                    *tails += 1;
                }
            }
            let Some(daemon) = weak.upgrade() else {
                return;
            };
            let committed =
                daemon.usage_batch(&key, &batch, &next, config, finished.then_some(job_id));
            drop(daemon);
            if !committed {
                return;
            }
            collection = next;
            pages -= 1;
            if pages == 0 {
                return;
            }
            previous_offset = batch.cursor.offset();
            match batch.stop {
                reader::Stop::Complete | reader::Stop::PendingTail => break,
                reader::Stop::Quarantined if budget.bytes < 16 * 1024 * 1024 => {
                    budget.bytes = 16 * 1024 * 1024;
                }
                reader::Stop::Quarantined => break,
                reader::Stop::Budget => {}
            }
            if !alive_wait(weak, 1) {
                return;
            }
        }
        if !alive_wait(weak, 1) {
            return;
        }
    }
    if collection.discovery_complete
        && collection.pending_files == Some(0)
        && collection.pending_tail_files == Some(0)
    {
        collection.state = CollectionState::CaughtUp;
        collection.completed_at = Some(Utc::now());
    }
    let _ = checkpoint(weak, &collection, &[], Change::Complete);
}

/// Every registration of one provider session: the agents (by canonical
/// ID) whose `session_id` label names it, each with when it was first
/// registered, as the attribution its samples would take.
fn registrations(
    state: &State,
    runtime: &str,
    session: &str,
) -> Vec<(UsageAttribution, DateTime<Utc>)> {
    let mut first: BTreeMap<AgentId, DateTime<Utc>> = BTreeMap::new();
    for record in state.registry.all().filter(|a| {
        a.spec.runtime == runtime
            && a.spec.labels.get("session_id").map(String::as_str) == Some(session)
    }) {
        let id = state.registry.canonical_id(&record.id).clone();
        let created = first.entry(id).or_insert(record.created_at);
        *created = (*created).min(record.created_at);
    }
    first
        .into_iter()
        .filter_map(|(id, created)| {
            let agent = state.registry.get(&id)?;
            Some((
                UsageAttribution {
                    agent: Some(id.to_string()),
                    project: agent.project.as_ref().map(|p| p.id().to_string()),
                },
                created,
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn usage_configuration_counts_default_roots_against_the_limit() {
        let home = tempfile::tempdir().unwrap();
        // Fifteen explicit Claude roots leave only one slot. The two default
        // Codex roots must not silently push discovery over its sixteen-root cap.
        let config = UsageConfig {
            enabled: true,
            claude_roots: (0..15)
                .map(|i| home.path().join(format!("claude-{i}")))
                .collect(),
            ..UsageConfig::default()
        };
        assert!(config.check().is_ok());
        if std::env::home_dir().is_some() {
            assert!(roots(&config).is_err());
        }
        let mut config = config;
        config.codex_roots = vec![home.path().join("codex")];
        assert_eq!(roots(&config).unwrap().len(), 16);
    }

    fn fixture() -> (tempfile::TempDir, Arc<Daemon>, UsageConfig, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("logs");
        std::fs::create_dir(&root).unwrap();
        let daemon =
            Arc::new(Daemon::open(temp.path().to_owned(), temp.path().join("sock")).unwrap());
        let config = UsageConfig {
            enabled: true,
            claude_roots: vec![root.clone()],
            codex_roots: vec![temp.path().join("no-codex")],
            ..UsageConfig::default()
        };
        let config_text = format!(
            "[usage]\nenabled=true\nclaude_roots=[{}]\ncodex_roots=[{}]\n",
            serde_json::to_string(&root).unwrap(),
            serde_json::to_string(&config.codex_roots[0]).unwrap()
        );
        std::fs::write(temp.path().join("agentd.toml"), config_text).unwrap();
        (temp, daemon, config, root)
    }

    fn record(id: &str, input: u64) -> String {
        serde_json::json!({"type":"assistant","version":"2.1.270","sessionId":"fixture-session",
            "timestamp":(Utc::now()-Duration::hours(2)).to_rfc3339(),
            "message":{"id":id,"model":"fixture-model","content":[{"text":"TRANSCRIPT_SECRET_MUST_NOT_PERSIST"}],
                "usage":{"input_tokens":input,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"output_tokens":2}}}).to_string()+"\n"
    }

    async fn query(daemon: &Arc<Daemon>) -> Report {
        let response = daemon
            .handle(Request::Usage {
                project: None,
                agent: None,
                since: Some("24h".into()),
                until: None,
                by: Group::Agent,
            })
            .await;
        match response {
            Response::Usage { report } => report,
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn usage_query_explains_a_parent_component_in_configured_roots() {
        let (temp, daemon, _, _) = fixture();
        let root = temp.path().join("logs/../logs");
        let config = format!(
            "[usage]\nenabled=true\nclaude_roots=[{}]\n",
            serde_json::to_string(&root).unwrap()
        );
        std::fs::write(temp.path().join("agentd.toml"), config).unwrap();
        let response = daemon.usage(Query::default()).await;
        let Response::Error { code, message, .. } = response else {
            panic!("invalid roots must not silently report empty usage");
        };
        assert_eq!(code, ErrorCode::Invalid);
        assert_eq!(
            message,
            "usage discovery roots must not contain parent ('..') components; use a direct absolute path"
        );
    }

    #[tokio::test]
    async fn usage_configuration_is_not_inferred_from_unstarted_or_old_collection() {
        let (temp, daemon, config, root) = fixture();
        let starting = query(&daemon).await;
        assert_eq!(starting.coverage.collection.enabled, Some(true));
        assert_eq!(starting.coverage.collection.discovery_generation, None);
        std::fs::write(root.join("source.jsonl"), record("first", 4)).unwrap();
        collect_generation(&Arc::downgrade(&daemon), &config);
        let caught_up = query(&daemon).await;
        assert_eq!(caught_up.coverage.collection.enabled, Some(true));
        assert_eq!(
            caught_up.coverage.collection.state,
            CollectionState::CaughtUp
        );
        let path = temp.path().join("agentd.toml");
        let original = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, original.replace("enabled=true", "enabled=false")).unwrap();
        let off = query(&daemon).await;
        assert_eq!(off.coverage.collection.enabled, Some(false));
        assert_eq!(
            off.rows[0].samples, 1,
            "disabling retains available history"
        );
        assert_eq!(
            off.rows[0].counters.input_tokens.coverage,
            usage::Coverage::Partial
        );
        std::fs::write(&path, original.replace("logs", "other-logs")).unwrap();
        let waiting = query(&daemon).await;
        assert_eq!(waiting.coverage.collection.enabled, Some(true));
        assert_eq!(waiting.coverage.collection.discovery_generation, None);
    }

    #[tokio::test]
    async fn usage_collection_keeps_partial_tail_visible_and_restart_deduplicates_it() {
        let (temp, daemon, config, root) = fixture();
        let path = root.join("session.jsonl");
        let first = record("first", 11);
        let second = record("second", 7);
        let partial = second.trim_end_matches('\n');
        std::fs::write(&path, format!("{first}{partial}")).unwrap();
        collect_generation(&Arc::downgrade(&daemon), &config);
        let report = query(&daemon).await;
        assert_eq!(report.rows[0].counters.input_tokens.sum, Some(11));
        assert_eq!(report.coverage.collection.state, CollectionState::Scanning);
        assert_eq!(report.coverage.collection.pending_tail_files, Some(1));
        assert_eq!(report.rows[0].key, None);
        drop(daemon);
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"\n")
            .unwrap();
        let daemon =
            Arc::new(Daemon::open(temp.path().to_owned(), temp.path().join("sock")).unwrap());
        collect_generation(&Arc::downgrade(&daemon), &config);
        let report = query(&daemon).await;
        assert_eq!(report.rows[0].samples, 2);
        assert_eq!(report.rows[0].counters.input_tokens.sum, Some(18));
        assert_eq!(report.coverage.collection.state, CollectionState::CaughtUp);
        assert_eq!(report.coverage.collection.pending_tail_files, Some(0));
        assert_eq!(
            report.coverage.source_gaps, 0,
            "a fully reverified appended prefix is not missing history"
        );
        std::fs::copy(&path, root.join("copy.jsonl")).unwrap();
        collect_generation(&Arc::downgrade(&daemon), &config);
        assert_eq!(query(&daemon).await.rows[0].samples, 2);
        let conn = crate::sqlite_fixture::open(temp.path().join("state.db")).unwrap();
        for table in [
            "usage_samples",
            "usage_buckets",
            "usage_baselines",
            "usage_files",
            "usage_gaps",
            "usage_discovery_jobs",
        ] {
            let mut statement = conn.prepare(&format!("SELECT * FROM {table}")).unwrap();
            let columns = statement.column_count();
            let mut rows = statement.query([]).unwrap();
            while let Some(row) = rows.next().unwrap() {
                for i in 0..columns {
                    if let Ok(text) = row.get::<_, String>(i) {
                        assert!(!text.contains("TRANSCRIPT_SECRET_MUST_NOT_PERSIST"));
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn usage_parser_upgrade_replays_skipped_patch_records_without_recounting() {
        let (temp, daemon, config, root) = fixture();
        let path = root.join("session.jsonl");
        let first = record("first", 11);
        std::fs::write(&path, &first).unwrap();
        collect_generation(&Arc::downgrade(&daemon), &config);
        assert_eq!(query(&daemon).await.rows[0].samples, 1);
        let newer = record("newer", 7).replace("2.1.270", "2.1.280");
        std::fs::write(&path, format!("{first}{newer}")).unwrap();
        // A v3 collector reached EOF but skipped the then-unsupported patch.
        // Preserve the accepted first response while restoring that old cursor.
        let scanned = reader::scan(
            &path,
            reader::Runtime::Claude,
            None,
            reader::Budget::default(),
        )
        .unwrap();
        let mut old_cursor = serde_json::to_value(scanned.cursor).unwrap();
        old_cursor["version"] = serde_json::json!(3);
        drop(daemon);
        let conn = crate::sqlite_fixture::open(temp.path().join("state.db")).unwrap();
        let saved: String = conn
            .query_row("SELECT json FROM usage_files", [], |row| row.get(0))
            .unwrap();
        let mut progress: serde_json::Value = serde_json::from_str(&saved).unwrap();
        progress["cursor"] = old_cursor;
        conn.execute("UPDATE usage_files SET json=?1", [progress.to_string()])
            .unwrap();
        drop(conn);
        let daemon =
            Arc::new(Daemon::open(temp.path().to_owned(), temp.path().join("sock")).unwrap());
        for _ in 0..2 {
            collect_generation(&Arc::downgrade(&daemon), &config);
            let report = query(&daemon).await;
            assert_eq!(report.rows[0].samples, 2);
            assert_eq!(report.rows[0].counters.input_tokens.sum, Some(18));
            assert_eq!(report.coverage.collection.state, CollectionState::CaughtUp);
            assert_eq!(report.coverage.source_gaps, 0);
            assert!(
                report
                    .coverage
                    .collection
                    .scope
                    .formats
                    .iter()
                    .any(|format| { format == "claude-transcript-2.1.277-278-280-v1" })
            );
        }
    }

    #[tokio::test]
    async fn usage_transfer_fences_accounting_but_keeps_queries_available() {
        let (_temp, daemon, config, root) = fixture();
        std::fs::write(root.join("source.jsonl"), record("first", 4)).unwrap();
        daemon.offer_transfer(4242).unwrap();
        let before = lock(&daemon.state).next_seq;
        collect_generation(&Arc::downgrade(&daemon), &config);
        assert!(query(&daemon).await.rows.is_empty());
        assert_eq!(lock(&daemon.state).next_seq, before);
        assert!(
            lock(&daemon.state)
                .store
                .usage_collection()
                .unwrap()
                .is_none()
        );
        assert!(daemon.abort_transfer("usage fixture"));
        collect_generation(&Arc::downgrade(&daemon), &config);
        assert_eq!(
            query(&daemon).await.rows[0].counters.input_tokens.sum,
            Some(4)
        );
    }

    #[tokio::test]
    async fn usage_discovery_restarts_corrupt_metadata_without_fencing_accounting() {
        for malformed in [false, true] {
            let (temp, daemon, config, root) = fixture();
            std::fs::write(root.join("source.jsonl"), record("source", 7)).unwrap();
            collect_generation_bounded(&Arc::downgrade(&daemon), &config, 1);
            {
                let state = lock(&daemon.state);
                assert!(state.store.usage_next_job().unwrap().is_some());
                let mut value =
                    serde_json::to_value(state.store.usage_discovery().unwrap().unwrap()).unwrap();
                if malformed {
                    value = serde_json::json!({"incompatible": true});
                } else {
                    value["jobs"] = serde_json::json!(usize::MAX);
                    value["failures"] = serde_json::json!(1);
                }
                state
                    .store
                    .put_document("usage", "discovery", &value)
                    .unwrap();
            }
            drop(daemon);
            let daemon =
                Arc::new(Daemon::open(temp.path().to_owned(), temp.path().join("sock")).unwrap());
            collect_generation(&Arc::downgrade(&daemon), &config);
            let report = query(&daemon).await;
            assert_eq!(report.coverage.collection.discovery_generation, Some(2));
            assert_eq!(report.coverage.collection.state, CollectionState::CaughtUp);
            assert_eq!(report.rows[0].samples, 1);
            assert_eq!(report.rows[0].counters.input_tokens.sum, Some(7));
            let state = lock(&daemon.state);
            assert!(state.storage_error.is_none());
            assert!(state.store.usage_next_job().unwrap().is_none());
            assert!(state.store.usage_discovery().unwrap().is_none());
        }
    }

    #[tokio::test]
    async fn usage_capture_gaps_are_not_pending_jobs_after_restart() {
        let (temp, daemon, config, root) = fixture();
        std::fs::write(root.join("source.jsonl"), record("source", 7)).unwrap();
        // Model a committed page that recorded a capture failure before the
        // remaining roots. Its gap and failure count survive the restart.
        let sources = roots(&config).unwrap();
        let mut progress = Progress {
            frontier: Walk::new(sources.clone()).unwrap().checkpoint(),
            roots: sources,
            generation: 1,
            jobs: 0,
            failures: 0,
        };
        let collection = Collection {
            discovery_generation: Some(1),
            snapshot_at: Some(Utc::now()),
            scope: Scope {
                roots: progress
                    .roots
                    .iter()
                    .map(|root| root.path.display().to_string())
                    .collect(),
                ..Scope::default()
            },
            ..Collection::default()
        };
        assert!(checkpoint(
            &Arc::downgrade(&daemon),
            &collection,
            &[],
            Change::Start(&progress)
        ));
        progress.failures = 1;
        assert!(checkpoint(
            &Arc::downgrade(&daemon),
            &collection,
            &[("capture:fixture", "usage file could not be captured")],
            Change::Page(&progress, &[])
        ));
        drop(daemon);
        let daemon =
            Arc::new(Daemon::open(temp.path().to_owned(), temp.path().join("sock")).unwrap());
        collect_generation(&Arc::downgrade(&daemon), &config);
        let report = query(&daemon).await;
        assert_eq!(report.coverage.collection.pending_files, Some(0));
        assert_eq!(report.coverage.collection.state, CollectionState::CaughtUp);
        assert_eq!(report.coverage.source_gaps, 1);
        assert_eq!(
            report.rows[0].counters.input_tokens.coverage,
            usage::Coverage::Partial
        );
    }

    #[tokio::test]
    async fn usage_removed_manifest_source_finishes_with_a_gap_not_pending_work() {
        let (temp, daemon, config, root) = fixture();
        let path = root.join("source.jsonl");
        std::fs::write(&path, record("source", 7)).unwrap();
        collect_generation_bounded(&Arc::downgrade(&daemon), &config, 1);
        assert_eq!(
            query(&daemon).await.coverage.collection.pending_files,
            Some(1)
        );
        std::fs::remove_file(path).unwrap();
        drop(daemon);
        let daemon =
            Arc::new(Daemon::open(temp.path().to_owned(), temp.path().join("sock")).unwrap());
        collect_generation(&Arc::downgrade(&daemon), &config);
        let report = query(&daemon).await;
        assert_eq!(report.coverage.collection.pending_files, Some(0));
        assert_eq!(report.coverage.collection.state, CollectionState::CaughtUp);
        assert_eq!(report.coverage.source_gaps, 1);
        assert!(report.rows.is_empty());
    }

    #[tokio::test]
    async fn usage_discovery_resumes_its_persisted_generation_after_reopen() {
        let (temp, daemon, config, root) = fixture();
        for n in 0..650 {
            std::fs::write(root.join(format!("ignored-{n}.txt")), "").unwrap();
        }
        for n in 0..8 {
            std::fs::write(
                root.join(format!("source-{n}.jsonl")),
                record(&format!("message-{n}"), 1),
            )
            .unwrap();
        }
        collect_generation_bounded(&Arc::downgrade(&daemon), &config, 1);
        let first = query(&daemon).await;
        assert_eq!(first.coverage.collection.discovery_generation, Some(1));
        assert!(!first.coverage.collection.discovery_complete);
        assert!(first.rows.is_empty());
        assert_eq!(
            lock(&daemon.state)
                .store
                .usage_discovery()
                .unwrap()
                .unwrap()
                .generation,
            1
        );
        drop(daemon);
        let daemon =
            Arc::new(Daemon::open(temp.path().to_owned(), temp.path().join("sock")).unwrap());
        collect_generation(&Arc::downgrade(&daemon), &config);
        let report = query(&daemon).await;
        assert_eq!(report.coverage.collection.discovery_generation, Some(1));
        assert_eq!(
            report.coverage.collection.snapshot_at,
            first.coverage.collection.snapshot_at
        );
        assert_eq!(report.coverage.collection.state, CollectionState::CaughtUp);
        assert_eq!(report.rows[0].samples, 8);
        assert_eq!(report.rows[0].counters.input_tokens.sum, Some(8));
        assert!(
            lock(&daemon.state)
                .store
                .usage_discovery()
                .unwrap()
                .is_none()
        );
        assert!(
            lock(&daemon.state)
                .store
                .usage_next_job()
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn short_accounting_transactions_resume_exactly_after_reopen() {
        let (temp, daemon, config, root) = fixture();
        let source: String = (0..300)
            .map(|n| record(&format!("message-{n}"), 2))
            .collect();
        std::fs::write(root.join("source.jsonl"), source).unwrap();
        // One discovery page followed by one short accounting transaction.
        collect_generation_bounded(&Arc::downgrade(&daemon), &config, 2);
        let first = query(&daemon).await;
        assert_eq!(first.coverage.collection.state, CollectionState::Scanning);
        assert_eq!(first.coverage.collection.pending_files, Some(1));
        assert_eq!(first.rows[0].samples, 128);
        assert_eq!(first.rows[0].counters.input_tokens.sum, Some(256));
        drop(daemon);
        let daemon =
            Arc::new(Daemon::open(temp.path().to_owned(), temp.path().join("sock")).unwrap());
        collect_generation(&Arc::downgrade(&daemon), &config);
        let resumed = query(&daemon).await;
        assert_eq!(resumed.coverage.collection.discovery_generation, Some(1));
        assert_eq!(resumed.coverage.collection.state, CollectionState::CaughtUp);
        assert_eq!(resumed.rows[0].samples, 300);
        assert_eq!(resumed.rows[0].counters.input_tokens.sum, Some(600));
        collect_generation(&Arc::downgrade(&daemon), &config);
        let replay = query(&daemon).await;
        assert_eq!(replay.coverage.collection.discovery_generation, Some(2));
        assert_eq!(replay.rows[0].samples, 300);
        assert_eq!(replay.rows[0].counters.input_tokens.sum, Some(600));
    }

    #[tokio::test]
    async fn usage_manifest_resumes_remaining_jobs_without_rediscovery_or_double_counting() {
        let (temp, daemon, config, root) = fixture();
        for n in 0..3 {
            std::fs::write(
                root.join(format!("source-{n}.jsonl")),
                record(&format!("message-{n}"), 2),
            )
            .unwrap();
        }
        collect_generation_bounded(&Arc::downgrade(&daemon), &config, 2);
        let first = query(&daemon).await;
        assert_eq!(first.rows[0].samples, 1);
        assert_eq!(first.coverage.collection.pending_files, Some(2));
        // A newly added log belongs to the next generation, not the retained snapshot.
        std::fs::write(root.join("later.jsonl"), record("later", 100)).unwrap();
        drop(daemon);
        let daemon =
            Arc::new(Daemon::open(temp.path().to_owned(), temp.path().join("sock")).unwrap());
        collect_generation(&Arc::downgrade(&daemon), &config);
        let resumed = query(&daemon).await;
        assert_eq!(resumed.coverage.collection.discovery_generation, Some(1));
        assert_eq!(resumed.rows[0].samples, 3);
        assert_eq!(resumed.rows[0].counters.input_tokens.sum, Some(6));
        collect_generation(&Arc::downgrade(&daemon), &config);
        let next = query(&daemon).await;
        assert_eq!(next.coverage.collection.discovery_generation, Some(2));
        assert_eq!(next.rows[0].samples, 4);
        assert_eq!(next.rows[0].counters.input_tokens.sum, Some(106));
    }

    #[tokio::test]
    async fn usage_manifest_retains_a_partly_scanned_file_until_its_last_batch() {
        let (temp, daemon, config, root) = fixture();
        let content: String = (0..5000)
            .map(|n| record(&format!("response-{n}"), 1))
            .collect();
        std::fs::write(root.join("large.jsonl"), content).unwrap();
        collect_generation_bounded(&Arc::downgrade(&daemon), &config, 2);
        let first = query(&daemon).await;
        assert!(first.rows[0].samples > 0 && first.rows[0].samples < 5000);
        assert_eq!(first.coverage.collection.pending_files, Some(1));
        assert!(
            lock(&daemon.state)
                .store
                .usage_next_job()
                .unwrap()
                .is_some()
        );
        drop(daemon);
        let daemon =
            Arc::new(Daemon::open(temp.path().to_owned(), temp.path().join("sock")).unwrap());
        collect_generation(&Arc::downgrade(&daemon), &config);
        let report = query(&daemon).await;
        assert_eq!(report.coverage.collection.discovery_generation, Some(1));
        assert_eq!(report.coverage.collection.pending_files, Some(0));
        assert_eq!(report.coverage.collection.state, CollectionState::CaughtUp);
        assert_eq!(report.rows[0].samples, 5000);
        assert_eq!(report.rows[0].counters.input_tokens.sum, Some(5000));
        assert_eq!(report.coverage.source_gaps, 0);
        assert!(
            lock(&daemon.state)
                .store
                .usage_next_job()
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn usage_matches_only_unambiguous_sessions_and_reconciles_retained_samples() {
        let (_temp, daemon, config, root) = fixture();
        std::fs::write(root.join("source.jsonl"), record("first", 4)).unwrap();
        collect_generation(&Arc::downgrade(&daemon), &config);
        assert_eq!(query(&daemon).await.rows[0].key, None);
        let response = daemon
            .handle(Request::Register {
                spec: AgentSpec {
                    name: "usage-agent".into(),
                    runtime: "claude-code".into(),
                    labels: BTreeMap::from([("session_id".into(), "fixture-session".into())]),
                    ..AgentSpec::default()
                },
                pid: None,
                session: None,
            })
            .await;
        let Response::Agent { agent } = response else {
            panic!("{response:?}")
        };
        daemon.reconcile_usage(&config);
        let report = query(&daemon).await;
        assert_eq!(report.rows[0].key.as_deref(), Some(agent.id.as_str()));
        assert_eq!(report.rows[0].counters.input_tokens.sum, Some(4));
        let events = daemon
            .recent_events(100)
            .iter()
            .filter(|e| matches!(e.kind, EventKind::UsageReconciled { .. }))
            .count();
        assert_eq!(events, 1);
        daemon.reconcile_usage(&config);
        assert_eq!(
            daemon
                .recent_events(100)
                .iter()
                .filter(|e| matches!(e.kind, EventKind::UsageReconciled { .. }))
                .count(),
            events
        );
    }
}
