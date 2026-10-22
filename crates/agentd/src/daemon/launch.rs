use super::*;

impl Daemon {
    pub(super) async fn run(self: &Arc<Self>, spec: AgentSpec) -> Response {
        // A pane is somebody else's to own, so that path registers what
        // tmux starts rather than supervising a child of ours.
        if spec.in_pane {
            return self.run_in_pane(spec).await;
        }
        if spec.command.first().is_none_or(String::is_empty) {
            return Response::error(ErrorCode::Invalid, "run needs a nonempty command");
        }
        let mut record = AgentRecord::new(spec, true, Utc::now());
        if let Err(response) = self.admit_run(&mut record).await {
            return *response;
        }
        if record.spec.isolate {
            match self.isolate(&record).await {
                Ok(path) => {
                    record.spec.workdir = Some(path.clone());
                }
                Err(response) => return *response,
            }
        }
        let project = self.project_for(record.spec.workdir.clone(), true).await;
        let vcs = Self::vcs_for(record.spec.workdir.clone()).await;
        record.project = project;
        record.vcs = vcs;
        let inserted = lock(&self.state).insert_record(record.clone());
        let record = match inserted {
            Response::Agent { agent } => agent,
            other => {
                self.cleanup_isolate(&record).await;
                return other;
            }
        };
        self.launch(record).await
    }

    /// Bring an ended session back as a process of this daemon's, under
    /// its own record: the same id, queue, aliases and everything else,
    /// now supervised. The launch is checked against the record by the
    /// pure rule ([`agentdocker_core::identity::relaunch_check`]); here
    /// the host's facts are added — the old process must be gone, nothing
    /// may be subscribed on the record — and the record's return is
    /// committed (`session_relaunched`) before anything is spawned, so a
    /// launch that fails leaves an ended record with its queue and no
    /// second live process.
    pub(super) async fn resume_session(
        self: &Arc<Self>,
        reference: &str,
        spec: AgentSpec,
    ) -> Response {
        if spec.command.first().is_none_or(String::is_empty) {
            return Response::error(ErrorCode::Invalid, "a relaunch needs a nonempty command");
        }
        // The checkout as the record spells it: canonical, existing.
        let mut spec = spec;
        if let Some(workdir) = spec.workdir.take() {
            let resolved = tokio::task::spawn_blocking(move || {
                let path = std::fs::canonicalize(&workdir)?;
                if !path.is_dir() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "working directory is not a directory",
                    ));
                }
                Ok(path)
            })
            .await;
            match resolved {
                Ok(Ok(path)) => spec.workdir = Some(path),
                Ok(Err(error)) => {
                    return Response::error(
                        ErrorCode::Invalid,
                        format!("the relaunch's checkout is unusable: {error}"),
                    );
                }
                Err(_) => {
                    return Response::error(
                        ErrorCode::Internal,
                        "checkout resolution did not complete",
                    );
                }
            }
        }
        // What the record must satisfy, checked now and again at the
        // commit: the pure rule, nobody subscribed, and the old process
        // gone in fact, not only in the record — a pid still alive whose
        // start time is the recorded one, or one that cannot be asked, is
        // that process.
        let launch = spec.clone();
        let refusal = |state: &State, record: &AgentRecord| -> Option<Response> {
            if let Err(reason) = agentdocker_core::identity::relaunch_check(record, &launch) {
                return Some(Response::error(ErrorCode::Invalid, reason));
            }
            if state.live_subscribers.get(&record.id).copied().unwrap_or(0) > 0 {
                return Some(Response::error(
                    ErrorCode::Conflict,
                    "the session still has a live subscriber; close it first",
                ));
            }
            if record.pid.is_some_and(|pid| {
                process_exists(pid) && same_process(pid, record.process_started_at)
            }) {
                return Some(Response::error(
                    ErrorCode::Conflict,
                    "the session's process is still running; exit it first",
                ));
            }
            None
        };
        let ended = {
            let mut state = lock(&self.state);
            let id = match state.resolve(reference) {
                Ok(id) => id,
                Err(response) => return *response,
            };
            let Some(record) = state.registry.get(&id).cloned() else {
                return Response::error(ErrorCode::NotFound, "agent vanished");
            };
            if let Some(refused) = refusal(&state, &record) {
                return refused;
            }
            record
        };
        let session = ended
            .spec
            .labels
            .get("session_id")
            .cloned()
            .unwrap_or_default();
        // The record as it returns: the launch's command, environment and
        // terminal, its own name, labels, project and checkout, nothing of
        // the old process, and supervised from here on.
        let mut record = ended.clone();
        let mut labels = ended.spec.labels.clone();
        labels.extend(spec.labels.clone());
        labels.insert("session_id".to_owned(), session.clone());
        // The record keeps its own name, so it keeps what that name is: a
        // launch's generated-name label must not relabel a chosen one.
        if let Some(kind) = ended.spec.labels.get(agentdocker_core::agent::NAME_LABEL) {
            labels.insert(agentdocker_core::agent::NAME_LABEL.to_owned(), kind.clone());
        }
        record.spec = AgentSpec {
            name: ended.spec.name.clone(),
            labels,
            workdir: spec.workdir.clone(),
            ..spec
        };
        record.managed = true;
        record.status = AgentStatus::Created;
        record.pid = None;
        record.process_started_at = None;
        record.process_group = None;
        record.owner = None;
        record.started_at = None;
        record.finished_at = None;
        record.session = None;
        record.terminal = None;
        if let Err(response) = self.admit_run(&mut record).await {
            return *response;
        }
        record.vcs = Self::vcs_for(record.spec.workdir.clone()).await;
        #[cfg(test)]
        {
            let hold = lock(&self.relaunch_hold).take();
            if let Some(hold) = hold {
                hold.wait().await;
                hold.wait().await;
            }
        }
        let committed = {
            let mut state = lock(&self.state);
            // Nothing may have changed underneath: the record is still,
            // field for field, the ended one this launch was checked
            // against and rebuilt from (a role, a label, an availability
            // report that landed while admission and the checkout were
            // looked at would otherwise be written over by the copy), and
            // it still passes every check — a subscriber can have arrived
            // meanwhile too.
            let Some(current) = state.registry.get(&record.id).cloned() else {
                return Response::error(ErrorCode::NotFound, "agent vanished");
            };
            if current != ended {
                return Response::error(ErrorCode::Conflict, "the session changed; look again");
            }
            if let Some(refused) = refusal(&state, &current) {
                return refused;
            }
            let now = Utc::now();
            let event = Event::new(
                EventKind::SessionRelaunched {
                    agent: record.id.clone(),
                    session,
                },
                now,
            );
            let committed = state.transition(
                "session relaunch",
                event,
                |store, event| store.agent_transition(&record, event),
                |state| {
                    *state
                        .registry
                        .get_mut(&record.id)
                        .expect("relaunched record retained") = record.clone();
                },
            );
            match state.write_failure() {
                Some(error) => Err(error),
                None if committed == Persisted::Committed => Ok(()),
                None => Err(Response::error(
                    ErrorCode::Unavailable,
                    "the relaunch was not committed",
                )),
            }
        };
        if let Err(error) = committed {
            return error;
        }
        self.launch(record).await
    }

    /// The part of a launch after its record exists: watched, spawned,
    /// marked running and supervised — or marked failed, with the record
    /// left as it was for a later attempt.
    pub(super) async fn launch(self: &Arc<Self>, record: AgentRecord) -> Response {
        // Watched before the process exists, so its first edit is seen.
        if watchable(&record) {
            if let Err(reason) = self.ensure_watched(&record, WATCHER_LAUNCH_TIMEOUT).await {
                self.mark_exited(
                    &record.id,
                    AgentStatus::Failed {
                        reason: reason.clone(),
                    },
                );
                self.cleanup_isolate(&record).await;
                return Response::error(ErrorCode::Unavailable, reason);
            }
        }
        match supervisor::spawn(self, &record).await {
            Ok(mut spawned) => {
                let pid = spawned.pid;
                let process_started_at = Some(spawned.process_started_at);
                if let Some(session) = spawned.session.clone() {
                    lock(&self.sessions).insert(record.id.clone(), session);
                }
                let mut admission_error = None;
                let updated = {
                    let mut state = lock(&self.state);
                    state
                        .supervised
                        .insert(record.id.clone(), spawned.control.clone());
                    let candidate = state
                        .registry
                        .get(&record.id)
                        .cloned()
                        .filter(|current| current.status == AgentStatus::Created);
                    if let Some(error) = state.run_refusal(&record) {
                        admission_error = Some(error);
                        None
                    } else if let Some(mut running) = candidate {
                        let now = Utc::now();
                        running.pid = Some(pid);
                        running.process_started_at = process_started_at;
                        running.process_group = Some(pid);
                        running.owner = Some(spawned.owner.clone());
                        running.status = AgentStatus::Running;
                        running.started_at = Some(now);
                        running.last_seen = now;
                        let event = Event::new(
                            EventKind::AgentStarted {
                                agent: record.id.clone(),
                                pid: Some(pid),
                            },
                            now,
                        );
                        let committed = state.transition(
                            "launch completion",
                            event,
                            |store, event| store.agent_transition(&running, event),
                            |state| {
                                *state
                                    .registry
                                    .get_mut(&record.id)
                                    .expect("launch identity retained") = running.clone();
                            },
                        );
                        if committed == Persisted::Committed {
                            Some(running)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                };
                let failed = lock(&self.state).write_failure().or(admission_error);
                let activation_error = if failed.is_none() && updated.is_some() {
                    spawned.activate("launched").await.err()
                } else {
                    None
                };
                if failed.is_some() || updated.is_none() || activation_error.is_some() {
                    spawned.control.send_replace(Some(true));
                }
                let supervision = supervisor::supervise(self.clone(), record.id, spawned);
                if let Some(error) = failed.or_else(|| {
                    activation_error.map(|error| {
                        Response::error(
                            ErrorCode::Internal,
                            format!("failed to execute command: {error:#}"),
                        )
                    })
                }) {
                    let _ = tokio::time::timeout(SUPERVISION_STOP_TIMEOUT, supervision).await;
                    return error;
                }
                match updated {
                    Some(agent) => Response::Agent { agent },
                    None => Response::error(ErrorCode::NotFound, "agent vanished"),
                }
            }
            Err(err) => {
                self.mark_exited(
                    &record.id,
                    AgentStatus::Failed {
                        reason: format!("{err:#}"),
                    },
                );
                self.cleanup_isolate(&record).await;
                err.downcast_ref::<policies::LaunchDenied>()
                    .map(|denied| denied.0.clone())
                    .unwrap_or_else(|| Response::error(ErrorCode::Internal, format!("{err:#}")))
            }
        }
    }

    pub(super) async fn register(
        &self,
        spec: AgentSpec,
        pid: Option<u32>,
        reported: Option<agentdocker_core::multiplexer::Session>,
    ) -> Response {
        if pid.is_some_and(|pid| signal_pid(pid).is_none()) {
            return Response::error(
                ErrorCode::Invalid,
                "pid must be a positive process id within i32 range",
            );
        }
        // Resolved here, off the state thread and before the record
        // exists: identity compares the physical checkout, and `/tmp`
        // and `/private/tmp` are one directory spelled two ways. Two
        // halves of a session that spell it differently are still one
        // session, and two worktrees that resolve apart are still two.
        // Touching the filesystem is not something the comparison may
        // do — it runs under the state mutex — so it happens once, now.
        let mut spec = spec;
        if let Some(workdir) = spec.workdir.take() {
            // Identity needs an existing directory. The project helper
            // permits nonexistent suffixes, which is useful for planned
            // paths but cannot establish a registration's checkout.
            let given = workdir.clone();
            let resolved = match tokio::task::spawn_blocking(move || {
                let path = std::fs::canonicalize(&workdir)?;
                if !path.is_dir() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "working directory is not a directory",
                    ));
                }
                Ok(path)
            })
            .await
            {
                Ok(result) => result.ok(),
                Err(_) => {
                    return Response::error(
                        ErrorCode::Internal,
                        "the working-directory resolver failed",
                    );
                }
            };
            let Some(resolved) = resolved else {
                // Neither retain an unverified path nor turn a supplied
                // directory into None: both would lose identity evidence.
                return Response::error(
                    ErrorCode::Invalid,
                    format!(
                        "cannot resolve the working directory {}; register from a directory \
                         that exists, or with none at all",
                        given.display()
                    ),
                );
            };
            spec.workdir = Some(resolved);
        }
        // The provider child is part of its supervised input controller. Older
        // hooks may still register its PID; return the existing owner without
        // changing its spec, PID, session binding or queue consumer.
        if spec.runtime == "codex" {
            if let Some(pid) = pid {
                let owners: Vec<_> = lock(&self.state)
                    .registry
                    .live()
                    .filter(|a| agentdocker_host::provider_input::is_codex_input(a))
                    .cloned()
                    .collect();
                let table = procinfo::processes().unwrap_or_default();
                if let Some(owner) = owners.iter().find(|owner| {
                    spec.workdir == owner.spec.workdir
                        && agentdocker_host::provider_input::owns_codex_process(owner, pid, &table)
                }) {
                    let state = lock(&self.state);
                    if let Some(current) = state.registry.get(&owner.id) {
                        if current.status.is_live()
                            && current.pid == owner.pid
                            && current.process_started_at == owner.process_started_at
                        {
                            return Response::Agent {
                                agent: current.clone(),
                            };
                        }
                    }
                }
                // Refused as `adopt` refuses a helper, by what the process
                // is: an MCP server from before Codex 0.160 was understood
                // registered its parent, the detached app-server, as a
                // session of its own beside the real one.
                if table
                    .iter()
                    .find(|process| process.pid == pid)
                    .is_some_and(|process| procinfo::detached_codex_app_server(&process.argv))
                {
                    return Response::error(
                        ErrorCode::Invalid,
                        format!("pid {pid}: {DETACHED_CODEX_HOST}"),
                    );
                }
            }
        }
        let project = self.project_for(spec.workdir.clone(), true).await;
        let vcs = Self::vcs_for(spec.workdir.clone()).await;
        // The checkout's reflog is read up to now before the newcomer is
        // in it: what is committed there from here on can be its.
        if let Some(project) = &project {
            let _ = self
                .replay_checkout(project.id(), project.dir().to_path_buf())
                .await;
        }
        let mut record = AgentRecord::new(spec, false, Utc::now());
        record.project = project;
        record.vcs = vcs;
        self.refresh_policy_for(record.project.as_ref());
        record.pid = pid;
        record.process_started_at = pid.and_then(procinfo::start_time);
        record.status = AgentStatus::Running;
        record.started_at = Some(Utc::now());
        // An agent that registered itself was started by somebody, and
        // that somebody may have been a multiplexer. Knowing which lets
        // a person reach it with the tool that already owns its
        // terminal.
        record.session = pid.and_then(|pid| Self::session_of(pid, reported.clone()));
        // And the application around that terminal, which is what a
        // person brings to the front to reach it.
        record.terminal = pid.and_then(agentdocker_host::terminal::of_process);
        // A Codex process proven to run a conversation known by its key
        // alone joins that record rather than becoming a second one. The
        // check and the insert share this one lock, so an attach racing
        // this registration cannot slip a second record between them.
        let response = {
            let mut state = lock(&self.state);
            let proven = match record.pid {
                Some(pid)
                    if record.spec.runtime == "codex"
                        && record
                            .spec
                            .labels
                            .contains_key(agentdocker_core::agent::PROFILE_LABEL) =>
                {
                    state.prove_keyed(
                        &record.spec,
                        pid,
                        record.process_started_at,
                        (record.session.clone(), record.terminal.clone()),
                        Utc::now(),
                    )
                }
                _ => None,
            };
            match proven {
                Some(response) => response,
                None => state.insert_record(record),
            }
        };
        // The reply is what a session waits for before its first edit, so
        // the checkout is watched by the time it goes out.
        if let Response::Agent { agent } = &response {
            if watchable(agent) {
                if let Err(reason) = self.ensure_watched(agent, WATCHER_ATTACH_TIMEOUT).await {
                    // An externally started process may already be writing. Keep its
                    // identity and protection, but do not report coverage as successful.
                    return Response::error(
                        ErrorCode::Unavailable,
                        format!(
                            "agent {} registered but checkout coverage is unavailable: {reason}",
                            agent.id
                        ),
                    );
                }
            }
        }
        response
    }

    /// Find or make the one record of a provider session by its key, and
    /// note its contact. Only a Codex conversation attaches this way, and
    /// only with its verified profile: the key is (runtime, profile, root
    /// thread). The record is found or made under the state lock, checked
    /// again after the checkout is resolved, so two first calls make one
    /// record. A record that names the conversation and has a process is
    /// returned as it is; one known by its key alone has its contact noted,
    /// which is its presence and never its lifecycle.
    pub(super) async fn session_attach(
        &self,
        runtime: String,
        profile: String,
        session: String,
        workdir: PathBuf,
        adapter: agentdocker_core::AdapterKind,
    ) -> Response {
        if runtime != "codex" {
            return Response::error(
                ErrorCode::Invalid,
                "only a Codex conversation attaches by its key",
            );
        }
        if !agentdocker_core::identity::plain_session_id(&session) {
            return Response::error(ErrorCode::Invalid, "a session key needs a plain session id");
        }
        if profile.trim().is_empty() || profile.chars().any(char::is_control) {
            return Response::error(
                ErrorCode::Invalid,
                "a session key needs its verified provider profile",
            );
        }
        let now = Utc::now();
        if let Some(found) =
            lock(&self.state).keyed_session(&runtime, &profile, &session, adapter, now)
        {
            return found;
        }
        let resolved = tokio::task::spawn_blocking(move || {
            let path = std::fs::canonicalize(&workdir)?;
            if !path.is_dir() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "working directory is not a directory",
                ));
            }
            Ok(path)
        })
        .await;
        let workdir = match resolved {
            Ok(Ok(path)) => path,
            Ok(Err(error)) => {
                return Response::error(
                    ErrorCode::Invalid,
                    format!("the session's checkout is unusable: {error}"),
                );
            }
            Err(_) => {
                return Response::error(
                    ErrorCode::Internal,
                    "checkout resolution did not complete",
                );
            }
        };
        let project = self.project_for(Some(workdir.clone()), true).await;
        let vcs = Self::vcs_for(Some(workdir.clone())).await;
        let response = {
            let mut state = lock(&self.state);
            // Another first call may have made it meanwhile.
            if let Some(found) = state.keyed_session(&runtime, &profile, &session, adapter, now) {
                return found;
            }
            // Named from the record's own id when inserted: two threads
            // that share a prefix never share a name.
            let mut record = AgentRecord::new(
                AgentSpec {
                    name: String::new(),
                    runtime: runtime.clone(),
                    workdir: Some(workdir),
                    labels: [
                        ("session_id".to_owned(), session.clone()),
                        (
                            agentdocker_core::agent::PROFILE_LABEL.to_owned(),
                            profile.clone(),
                        ),
                        (
                            "via".to_owned(),
                            match adapter {
                                agentdocker_core::AdapterKind::Mcp => "mcp",
                                agentdocker_core::AdapterKind::Hooks => "hook",
                            }
                            .to_owned(),
                        ),
                        (
                            agentdocker_core::agent::NAME_LABEL.to_owned(),
                            agentdocker_core::agent::GENERATED_NAME.to_owned(),
                        ),
                    ]
                    .into(),
                    ..AgentSpec::default()
                },
                false,
                now,
            );
            record.project = project;
            record.vcs = vcs;
            record.status = AgentStatus::Running;
            record.started_at = Some(now);
            record.session_contact = Some(agentdocker_core::SessionContact {
                adapter,
                observed_at: now,
            });
            state.insert_record(record)
        };
        if let Response::Agent { agent } = &response
            && watchable(agent)
            && let Err(reason) = self.ensure_watched(agent, WATCHER_ATTACH_TIMEOUT).await
        {
            // The conversation is known either way; coverage is not claimed.
            warn!(agent = %agent.id, %reason, "a keyed session's checkout is not watched");
        }
        response
    }

    /// Where a process lives, when that is somebody else's terminal.
    /// Best-effort: reading another process's environment is refused on
    /// macOS, so a `None` here means "we could not tell", never "it is
    /// homeless".
    pub(super) fn session_of(
        pid: u32,
        reported: Option<agentdocker_core::multiplexer::Session>,
    ) -> Option<agentdocker_core::multiplexer::Session> {
        let by_pid: BTreeMap<u32, procinfo::Process> = procinfo::processes()
            .map(|table| table.into_iter().map(|p| (p.pid, p)).collect())
            .unwrap_or_default();
        multiplexer::of(pid, &by_pid, reported)
    }

    pub(super) async fn cleanup_isolate(&self, record: &AgentRecord) {
        if !record.spec.isolate {
            return;
        }
        let Some(path) = &record.spec.workdir else {
            return;
        };
        let (worktree_removed, branch_removed, reason) = worktrees::cleanup_unstarted(record).await;
        self.emit(EventKind::WorktreeCleanup {
            agent: record.id.clone(),
            path: path.clone(),
            worktree_removed,
            branch_removed,
            reason,
        });
    }

    /// A linked worktree of the agent's repository, made for it under the
    /// daemon's sibling worktree directory, on a branch named after it: `agent/<name>`, or with
    /// its id appended when that is taken by an earlier run.
    pub(super) async fn isolate(&self, record: &AgentRecord) -> Result<PathBuf, Box<Response>> {
        let invalid = |text: &str| Box::new(Response::error(ErrorCode::Invalid, text));
        let Some(workdir) = record.spec.workdir.clone() else {
            return Err(invalid(
                "isolate needs a working directory inside a git checkout",
            ));
        };
        let base = self
            .project_for(Some(workdir), true)
            .await
            .filter(|p| p.source == ProjectSource::Git)
            .ok_or_else(|| invalid("isolate needs a working directory inside a git checkout"))?;
        let name = if record.spec.name.is_empty() {
            default_name(&record.id)
        } else {
            record.spec.name.clone()
        };
        // The name becomes a path component and part of a git ref: say so
        // before anything is created rather than let git say "invalid".
        if !isolate_name_ok(&name) {
            return Err(invalid(&format!(
                "agent name `{name}` cannot name a worktree and branch: use letters, digits, '-', '_' and '.', not starting with '-' or '.', without '..' or a '.lock' ending"
            )));
        }
        let dir = paths::worktree_dir(&self.home).join(base.id().short());
        if let Err(err) = std::fs::create_dir_all(&dir) {
            return Err(Box::new(Response::error(
                ErrorCode::Internal,
                format!("cannot create {}: {err}", dir.display()),
            )));
        }
        let root = project::canonical(base.dir());
        let dir = project::canonical(&dir);
        let candidates = [
            (dir.join(&name), format!("agent/{name}")),
            (
                dir.join(format!("{name}-{}", record.id.short())),
                format!("agent/{name}-{}", record.id.short()),
            ),
        ];
        let mut last: Option<Box<Response>> = None;
        for (path, branch) in candidates {
            if path.exists() {
                continue;
            }
            match worktrees::add_worktree(root.clone(), &path, &branch, None).await {
                Ok(()) => {
                    let (copied, trouble) =
                        worktrees::copy_includes(root.clone(), path.clone()).await;
                    if let Some(trouble) = trouble {
                        warn!(agent = %record.id.short(), %trouble, "worktree includes copied in part");
                    }
                    info!(agent = %record.id.short(), path = %path.display(), %branch, copied, "isolated in a worktree");
                    lock(&self.state)
                        .worktree_creators
                        .insert(path.clone(), record.id.clone());
                    if let Some(project) = &record.project {
                        let _ = self.replay_checkout(project.id(), path.clone()).await;
                    }
                    self.emit(EventKind::WorktreeCreated {
                        agent: record.id.clone(),
                        path: path.clone(),
                    });
                    return Ok(path);
                }
                Err(response) => last = Some(response),
            }
        }
        Err(last.unwrap_or_else(|| invalid("no free worktree path for the agent")))
    }
}
