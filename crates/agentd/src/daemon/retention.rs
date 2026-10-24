use super::*;

impl Daemon {
    pub fn evict_journal_rings(&self) {
        lock(&self.state).evict_journal_rings();
    }

    /// Trim the ledger. Called occasionally from the reaper.
    pub fn prune_changes(&self) {
        if let Some(removed) = lock(&self.state).store_op("ledger retention", |store| {
            store.prune_changes(CHANGE_HISTORY)
        }) && removed > 0
        {
            info!(removed, "pruned the ledger");
        }
    }

    /// A read set belongs to a record. One whose agent is no longer
    /// known — forgotten by an earlier daemon that left its read set
    /// behind — goes, [`ORPHAN_READS_BATCH`] a tick, so what read sets
    /// take is bounded by the records kept.
    pub fn prune_orphan_reads(&self) {
        if let Some(removed) = lock(&self.state).store_op("read-set retention", |store| {
            store.prune_orphan_reads(ORPHAN_READS_BATCH)
        }) && removed > 0
        {
            info!(removed, "forgot the read sets of forgotten agents");
        }
    }

    /// Captured output belongs to a record. A log whose agent is no longer
    /// known, forgotten while the daemon was down or left by an earlier
    /// daemon, goes once it is an hour old, so `logs/` holds nothing
    /// `ps --all` cannot show: the session log and its rotated half, and
    /// the controller log of a relaunched input receiver. Validation logs
    /// are named for their validation record and stay with it.
    pub fn prune_logs(&self) {
        let Ok(entries) = std::fs::read_dir(self.home.join("logs")) else {
            return;
        };
        let known: HashSet<String> = lock(&self.state)
            .registry
            .all()
            .map(|a| a.id.to_string())
            .collect();
        let mut removed = 0;
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let Some(owner) = log_owner(&name) else {
                continue;
            };
            if owner.starts_with("validation-") || known.contains(owner) {
                continue;
            }
            let old = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|m| m.elapsed().ok())
                .is_some_and(|age| age > std::time::Duration::from_secs(60 * 60));
            if old && std::fs::remove_file(entry.path()).is_ok() {
                removed += 1;
            }
        }
        if removed > 0 {
            info!(removed, "pruned logs of forgotten agents");
        }
    }

    /// The daemon's own output goes to `agentd.log`, and a relaunched
    /// input receiver's to `logs/<id>.controller.log`, through inherited
    /// descriptors in append mode that nobody coordinates, so neither can
    /// be renamed away under its writer. Past `DAEMON_LOG_CAP` bytes each
    /// is copied to `<name>.1` and truncated in place, and the next line
    /// lands at the start. A line written in the instant between the copy
    /// and the truncation is lost: these are diagnostic logs, bounded, not
    /// evidence, and a session's captured output is not trimmed this way.
    pub fn trim_diagnostic_logs(&self) {
        trim_in_place(&paths::daemon_log(&self.home), "the daemon log");
        if let Ok(entries) = std::fs::read_dir(self.home.join("logs")) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().ends_with(".controller.log"))
                {
                    trim_in_place(&path, "a controller log");
                }
            }
        }
    }

    /// Prune a project's journal on request: below a sequence number, older
    /// than an age, or both (whichever keeps more).
    pub(super) async fn journal_prune_request(
        &self,
        project: &str,
        before_seq: Option<u64>,
        older_than_secs: Option<u64>,
    ) -> Response {
        if before_seq.is_none() && older_than_secs.is_none() {
            return Response::error(
                ErrorCode::Invalid,
                "say what to prune: before_seq, older_than_secs, or both",
            );
        }
        let id = match self.resolve_project(project).await {
            Ok(id) => id,
            Err(response) => return *response,
        };
        let mut state = lock(&self.state);
        let by_age = match older_than_secs {
            None => None,
            Some(secs) => {
                let Some(cutoff) = cutoff_before(secs) else {
                    return Response::Pruned { removed: 0 };
                };
                match state.store_op("journal", |store| {
                    store.journal_retention_boundary(&id, cutoff, usize::MAX)
                }) {
                    Some(Some(seq)) => Some(seq),
                    Some(None) => return Response::Pruned { removed: 0 },
                    None => {
                        return state.write_failure().unwrap_or_else(|| {
                            Response::error(ErrorCode::Internal, "journal prune failed")
                        });
                    }
                }
            }
        };
        let before = match (before_seq, by_age) {
            (Some(seq), Some(age)) => seq.min(age),
            (Some(seq), None) => seq,
            (None, age) => age.unwrap_or(0),
        };
        state.journal_prune(&id, before, "request")
    }

    /// Give freed pages back to the filesystem. Holds the state lock for the
    /// duration, so nothing else is answered while SQLite rewrites the file:
    /// a live provider bridge waiting on the daemon could time out. Refused
    /// while sessions are live unless the caller accepts that with `force`.
    pub(super) fn vacuum(&self, force: bool) -> Response {
        let state = lock(&self.state);
        let live = state
            .registry
            .live()
            .filter(|a| a.spec.runtime != agentdocker_core::HUMAN_RUNTIME)
            .count();
        if live > 0 && !force {
            return Response::error(
                ErrorCode::Conflict,
                format!(
                    "{live} live session(s) would be paused while the database is rewritten; wait for them to finish, or pass force to accept that"
                ),
            );
        }
        match state.store.vacuum() {
            Ok((before_bytes, after_bytes)) => {
                info!(before_bytes, after_bytes, "vacuumed the state database");
                Response::Vacuumed {
                    before_bytes,
                    after_bytes,
                }
            }
            Err(err) => Response::error(ErrorCode::Internal, format!("vacuum failed: {err}")),
        }
    }

    /// Apply `[journal] retention` from `agentd.toml`, one batch per project
    /// per call, so a long backlog drains over several ticks instead of
    /// holding the state lock for one long delete. The file is read outside
    /// the lock; absence means keep everything.
    pub fn apply_journal_retention(&self) {
        use agentdocker_core::config::{FILE_NAME, RETENTION_BATCH};
        let path = self.home.join(FILE_NAME);
        let read = crate::config::read(&self.home).and_then(|config| {
            config
                .journal_retention()
                .map_err(|error| format!("{}: {error}", path.display()))
        });
        let mut state = lock(&self.state);
        let window = match read {
            Ok(window) => {
                if state.config_notice.take().is_some() {
                    info!(path = %path.display(), "daemon configuration is readable again");
                }
                window
            }
            Err(notice) => {
                if state.config_notice.as_ref() != Some(&notice) {
                    warn!(%notice, "daemon configuration ignored");
                    state.config_notice = Some(notice);
                }
                return;
            }
        };
        let Some(cutoff) = window.and_then(|window| Utc::now().checked_sub_signed(window)) else {
            return;
        };
        let projects = match state.store_read("journal", |store| store.journal_projects()) {
            Some(projects) => projects,
            None => return,
        };
        for project in projects {
            let boundary = state.store_op("journal", |store| {
                store.journal_retention_boundary(&project, cutoff, RETENTION_BATCH)
            });
            match boundary {
                Some(Some(before_seq)) => {
                    state.journal_prune(&project, before_seq, "retention");
                }
                Some(None) => {}
                None => return,
            }
        }
    }
}

/// Read sets of forgotten agents removed per tick at most.
const ORPHAN_READS_BATCH: usize = 100;
