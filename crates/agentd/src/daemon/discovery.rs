use super::*;

impl Daemon {
    /// Return the last successful scan only while it is fresh and healthy.
    pub(super) async fn discover(&self) -> Response {
        {
            let state = lock(&self.state);
            let cache = &state.discovered;
            if cache.error.is_none() && cache.at.is_some_and(|at| at.elapsed() < DISCOVERY_FRESH) {
                return Response::Processes {
                    processes: cache.processes.clone(),
                };
            }
        }
        match self.scan_agents().await {
            Ok(processes) => Response::Processes { processes },
            Err(error) => Response::error(ErrorCode::Unavailable, error),
        }
    }

    /// Serialize scans, then reconcile their results with the current registry
    /// in one synchronous transition. Registration may advance while ps runs.
    pub async fn scan_agents(&self) -> Result<Vec<DiscoveredProcess>, String> {
        use std::sync::atomic::Ordering;
        loop {
            // Register before checking the flag so completion between the
            // check and await cannot be missed.
            let finished = self.scan_finished.notified();
            tokio::pin!(finished);
            finished.as_mut().enable();
            if self
                .scanning
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                break;
            }
            finished.await;
            let state = lock(&self.state);
            if let Some(error) = &state.discovered.error {
                return Err(error.clone());
            }
            if state
                .discovered
                .at
                .is_some_and(|at| at.elapsed() < DISCOVERY_FRESH)
            {
                return Ok(state.discovered.processes.clone());
            }
            // The previous owner was cancelled without a fresh result. Try
            // to become the scanner instead of fabricating an empty result.
        }
        let _clear = ScanGuard {
            scanning: &self.scanning,
            finished: &self.scan_finished,
        };
        self.apply_scan(self.scan().await)
    }

    pub(super) fn apply_scan(
        &self,
        result: Result<Vec<DiscoveredProcess>, String>,
    ) -> Result<Vec<DiscoveredProcess>, String> {
        let mut state = lock(&self.state);
        // Background scans may finish after a transfer offer. Keep the prior
        // snapshot so an aborted transfer's next scan still emits its changes.
        if state.fenced() {
            return Err("discovery is paused during coordinator transfer".into());
        }
        let mut found = match result {
            Ok(found) => found,
            Err(reason) => {
                if state.discovered.error.as_ref() != Some(&reason) {
                    state.emit(EventKind::DiscoveryUnavailable {
                        reason: reason.clone(),
                    });
                }
                state.discovered.error = Some(reason.clone());
                return Err(reason);
            }
        };
        let registered: HashMap<_, _> = state
            .registry
            .live()
            .filter_map(|a| a.pid.map(|pid| (pid, a.process_started_at)))
            .collect();
        let is_registered = |p: &DiscoveredProcess| {
            registered.get(&p.pid).is_some_and(|started| {
                started.is_none() || p.started_at.is_none() || *started == p.started_at
            })
        };
        found.retain(|p| !is_registered(p));
        let previous = std::mem::replace(&mut state.discovered.processes, found.clone());
        let identity = |p: &DiscoveredProcess| (p.pid, p.started_at);
        let now_ids: HashSet<_> = found.iter().map(identity).collect();
        // Remove the old PID generation before announcing its replacement.
        for process in previous.iter().filter(|p| !now_ids.contains(&identity(p))) {
            state.emit(EventKind::AgentVanished {
                pid: process.pid,
                started_at: process.started_at,
                runtime: process.runtime.clone(),
                adopted: is_registered(process),
            });
        }
        for process in found
            .iter()
            .filter(|p| !previous.iter().any(|old| old == *p))
        {
            state.emit(EventKind::AgentDiscovered {
                pid: process.pid,
                started_at: process.started_at,
                runtime: process.runtime.clone(),
                project: process.project.as_ref().map(ProjectRef::id),
                cwd: process.cwd.clone(),
            });
        }
        state.discovered.at = Some(Instant::now());
        if state.discovered.error.take().is_some() {
            state.emit(EventKind::DiscoveryAvailable);
        }
        Ok(found)
    }

    /// The agent tools on this machine, with how many unregistered
    /// processes of each the last scan saw.
    pub(super) async fn runtimes(&self) -> Response {
        let roots = agentdocker_host::runtimes::Roots::from_env();
        let inventory = tokio::task::spawn_blocking(move || {
            agentdocker_host::runtimes::inventory(&roots, "agentdocker")
        })
        .await;
        let mut runtimes = match inventory {
            Ok(Ok(runtimes)) => runtimes,
            Ok(Err(err)) => return Response::error(ErrorCode::Unavailable, err.to_string()),
            Err(_) => {
                return Response::error(ErrorCode::Unavailable, "runtime inventory worker failed");
            }
        };
        let processes = match self.discover().await {
            Response::Processes { processes } => processes,
            error => return error,
        };
        for runtime in &mut runtimes {
            runtime.running = processes
                .iter()
                .filter(|p| p.runtime == runtime.name)
                .count();
        }
        Response::Runtimes { runtimes }
    }

    /// The process table, filtered to known agent runtimes that no live
    /// agent claims by pid. Projects come without fingerprints: this runs
    /// on every scan, and a process nobody adopted should not warm the
    /// cache or announce a repository.
    pub(super) async fn scan(&self) -> Result<Vec<DiscoveredProcess>, String> {
        let mine = std::process::id();
        let (owners, controllers): (Vec<_>, HashSet<u32>) = {
            let state = lock(&self.state);
            let owners = state
                .registry
                .live()
                .filter(|a| agentdocker_host::provider_input::is_codex_input(a))
                .cloned()
                .collect();
            let controllers = state
                .registry
                .live()
                .filter_map(|a| a.input_binding.as_ref())
                .flat_map(|b| {
                    std::iter::once(b.controller.pid)
                        .chain(b.restart.launched.as_ref().map(|p| p.pid))
                })
                .collect();
            (owners, controllers)
        };
        tokio::task::spawn_blocking(move || {
            let table = procinfo::processes().map_err(|e| e.to_string())?;
            let launchers = procinfo::codex_launchers(&table);
            let controlled: HashSet<_> = table
                .iter()
                .filter(|p| {
                    owners.iter().any(|owner| {
                        agentdocker_host::provider_input::owns_codex_process(owner, p.pid, &table)
                    })
                })
                .map(|p| p.pid)
                .collect();
            // Ancestry needs the whole table, and only agents are asked
            // about, so it is built once rather than per candidate.
            let by_pid: BTreeMap<u32, procinfo::Process> =
                table.iter().map(|p| (p.pid, p.clone())).collect();
            // A process a bound controller started (a receiver's own
            // helpers) is that controller's, not a session to connect.
            let sidecars: HashSet<u32> = table
                .iter()
                .filter(|p| controllers.contains(&p.ppid))
                .map(|p| p.pid)
                .collect();
            let mut found: Vec<DiscoveredProcess> = table
                .into_iter()
                .filter(|p| p.pid != mine && !launchers.contains(&p.pid))
                .filter(|p| !controlled.contains(&p.pid) && !sidecars.contains(&p.pid))
                .filter_map(|p| {
                    let runtime = procinfo::runtime_of(&p.argv)?;
                    let cwd = procinfo::cwd(p.pid);
                    Some(DiscoveredProcess {
                        pid: p.pid,
                        ppid: p.ppid,
                        runtime: runtime.to_owned(),
                        command: p.argv.join(" "),
                        project: cwd.as_deref().map(project::discover),
                        cwd,
                        started_at: procinfo::start_time(p.pid),
                        session: multiplexer::of(p.pid, &by_pid, None),
                    })
                })
                .collect();
            found.sort_by(|a, b| {
                let key = |p: &DiscoveredProcess| {
                    (
                        p.project.is_none(),
                        p.project.as_ref().map(ProjectRef::name),
                        p.pid,
                    )
                };
                key(a).cmp(&key(b))
            });
            Ok(found)
        })
        .await
        .map_err(|e| format!("process scan worker failed: {e}"))?
    }

    /// Register a running process by pid: runtime from the known table
    /// unless given, working directory from the process, so it lands in
    /// its project. Adopted agents run no hooks, so they hold no leases
    /// and report nothing, but they are visible, messageable, and counted.
    /// `authorized` is what the socket's check let the pid through on, as
    /// for a registration ([`acting::Authorized::recheck`]).
    pub(super) async fn adopt(
        &self,
        pid: u32,
        name: Option<String>,
        runtime: Option<String>,
        authorized: Option<acting::Authorized>,
    ) -> Response {
        let already = lock(&self.state)
            .registry
            .live()
            .find(|a| a.pid == Some(pid))
            .map(|a| a.spec.name.clone());
        if let Some(name) = already {
            return Response::error(
                ErrorCode::Invalid,
                format!("pid {pid} is already agent `{name}`"),
            );
        }
        let found = tokio::task::spawn_blocking(move || {
            procinfo::inspect(pid).map(|p| (p, procinfo::cwd(pid)))
        })
        .await
        .ok()
        .flatten();
        let Some((process, cwd)) = found else {
            return Response::error(ErrorCode::NotFound, format!("no process with pid {pid}"));
        };
        // A known runtime's helper is not a session, whatever runtime the
        // caller names: registering it would show an agent nobody can
        // reach, in a project named after the helper's directory.
        if let Some(helper) = procinfo::helper_of(&process.argv) {
            return Response::error(
                ErrorCode::Invalid,
                format!(
                    "pid {pid} is {}, not a session; there is nothing to adopt",
                    helper.role
                ),
            );
        }
        let runtime = runtime
            .or_else(|| procinfo::runtime_of(&process.argv).map(str::to_owned))
            .unwrap_or_else(|| "custom".to_owned());
        let spec = AgentSpec {
            name: name.clone().unwrap_or_else(|| format!("{runtime}-{pid}")),
            runtime: runtime.clone(),
            workdir: cwd,
            labels: {
                let mut labels = BTreeMap::from([("adopted".to_owned(), "true".to_owned())]);
                if name.is_none() {
                    labels.insert(
                        agentdocker_core::agent::NAME_LABEL.to_owned(),
                        agentdocker_core::agent::GENERATED_NAME.to_owned(),
                    );
                }
                labels
            },
            ..AgentSpec::default()
        };
        let response = self.register(spec, Some(pid), None, authorized).await;
        if matches!(response, Response::Agent { .. }) {
            let mut state = lock(&self.state);
            if let Some(index) = state.discovered.processes.iter().position(|p| p.pid == pid) {
                let process = state.discovered.processes.remove(index);
                state.emit(EventKind::AgentVanished {
                    pid,
                    started_at: process.started_at,
                    runtime,
                    adopted: true,
                });
            }
        }
        response
    }
}
