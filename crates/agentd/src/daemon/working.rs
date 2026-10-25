//! Content observations are captured outside the state lock, then persisted
//! under it. Host timestamps describe the beginning of the observation.
use super::*;
use agentdocker_core::{ReadMark, StalePath};
use agentdocker_host::content;

impl Daemon {
    /// Resolve a running writer and its registered working directory under one
    /// guard. Unlike a read, a commit must not fall back to another directory
    /// when the agent registered no workdir. Policy keys use the checkout root
    /// separately, so registering from a subdirectory does not change them.
    pub(super) fn writer_checkout(
        &self,
        reference: &str,
    ) -> Result<(AgentId, PathBuf, Option<String>), Box<Response>> {
        let mut state = lock(&self.state);
        let id = state.resolve(reference)?;
        let record = state.registry.get(&id).unwrap();
        if record.status != AgentStatus::Running {
            return Err(Box::new(Response::error(
                ErrorCode::Forbidden,
                "commits require a running agent",
            )));
        }
        let workdir = record.spec.workdir.clone().ok_or_else(|| {
            Box::new(Response::error(
                ErrorCode::Invalid,
                "this agent registered no working directory, so there is no checkout of its own to commit; commit with git, or re-register from the checkout you are in",
            ))
        })?;
        Ok((
            id,
            project::canonical(&workdir),
            record.vcs.as_ref().and_then(|v| v.head.clone()),
        ))
    }

    pub(super) fn reader_checkout(
        &self,
        reference: &str,
    ) -> Result<(AgentId, PathBuf, Option<String>), Box<Response>> {
        let mut state = lock(&self.state);
        let id = state.resolve(reference)?;
        let record = state.registry.get(&id).unwrap();
        if record.status != AgentStatus::Running {
            return Err(Box::new(Response::error(
                ErrorCode::Forbidden,
                "observations require a running agent",
            )));
        }
        let root = record
            .project
            .as_ref()
            .map(|p| p.dir().to_path_buf())
            .or_else(|| record.spec.workdir.clone())
            .ok_or_else(|| {
                Box::new(Response::error(ErrorCode::Invalid, "agent has no checkout"))
            })?;
        Ok((
            id,
            project::canonical(&root),
            record.vcs.as_ref().and_then(|v| v.head.clone()),
        ))
    }

    pub(super) fn reads(&self, reference: &str) -> Response {
        let mut state = lock(&self.state);
        let id = match state.resolve(reference) {
            Ok(id) => id,
            Err(e) => return *e,
        };
        match state.store.document::<Vec<ReadMark>>("reads", id.as_str()) {
            Ok(reads) => Response::Reads {
                reads: reads.unwrap_or_default(),
            },
            Err(e) => Response::error(ErrorCode::Internal, e.to_string()),
        }
    }

    /// Apply `[read_set] capacity` from `agentd.toml`: at start and once a
    /// minute, so an edit takes effect without a restart. A value out of
    /// range, or a file that does not read, leaves the default in force
    /// and is said once, and again when it is fixed.
    pub fn apply_read_set_capacity(&self) {
        use agentdocker_core::config::{FILE_NAME, READ_SET_CAPACITY};
        let path = self.home.join(FILE_NAME);
        let read = crate::config::read(&self.home).and_then(|config| {
            config
                .read_set_capacity()
                .map_err(|error| format!("{}: {error}", path.display()))
        });
        let mut state = lock(&self.state);
        let capacity = match read {
            Ok(capacity) => {
                if state.read_set_notice.take().is_some() {
                    info!(capacity, "read-set capacity applied again");
                }
                capacity
            }
            Err(notice) => {
                if state.read_set_notice.as_ref() != Some(&notice) {
                    warn!(%notice, capacity = READ_SET_CAPACITY, "read-set capacity left at its default");
                    state.read_set_notice = Some(notice);
                }
                READ_SET_CAPACITY
            }
        };
        if state.store.read_capacity() != capacity {
            info!(capacity, "read-set capacity");
            state.store.set_read_capacity(capacity);
        }
    }

    pub(super) async fn observe(&self, reference: &str, paths: Vec<String>) -> Response {
        if paths.is_empty() || paths.len() > OBSERVE_PATHS {
            return Response::error(
                ErrorCode::Invalid,
                format!("observe requires 1–{OBSERVE_PATHS} paths"),
            );
        }
        let (id, root, head) = match self.reader_checkout(reference) {
            Ok(v) => v,
            Err(e) => return *e,
        };
        let observed_root = root.clone();
        let captured = tokio::task::spawn_blocking(
            move || -> anyhow::Result<Vec<(ReadMark, Option<Arc<content::Snapshot>>)>> {
                let mut marks = Vec::new();
                let mut remaining = super::read_content::CACHE_BYTES;
                for raw in paths {
                    let path = checkout_path(&root, &raw)?;
                    let at = Utc::now();
                    let snapshot = Arc::new(content::snapshot(&path)?);
                    let version = snapshot.version.clone();
                    let bytes = snapshot.retained_bytes();
                    let proof = if bytes <= remaining {
                        remaining -= bytes;
                        Some(snapshot)
                    } else {
                        None
                    };
                    marks.push((
                        ReadMark {
                            path,
                            at,
                            version,
                            head: head.clone(),
                        },
                        proof,
                    ));
                }
                Ok(marks)
            },
        )
        .await;
        let marks = match captured {
            Ok(Ok(marks)) => marks,
            Ok(Err(e)) => return Response::error(ErrorCode::Invalid, e.to_string()),
            Err(e) => return Response::error(ErrorCode::Internal, e.to_string()),
        };
        let mut state = lock(&self.state);
        if !state.registry.get(&id).is_some_and(|r| {
            r.status == AgentStatus::Running
                && r.project
                    .as_ref()
                    .is_some_and(|p| project::canonical(p.dir()) == observed_root)
        }) {
            return Response::error(
                ErrorCode::Forbidden,
                "agent stopped or changed checkout during observation",
            );
        }
        let existing = match state.store.document::<Vec<ReadMark>>("reads", id.as_str()) {
            Ok(existing) => existing.unwrap_or_default(),
            Err(e) => return Response::error(ErrorCode::Internal, e.to_string()),
        };
        let held = existing.len();
        let mut by_path: BTreeMap<PathBuf, ReadMark> =
            existing.into_iter().map(|m| (m.path.clone(), m)).collect();
        let paths = marks.iter().map(|(m, _)| m.path.clone()).collect();
        let mut accepted = Vec::new();
        for (mark, proof) in marks {
            if by_path.get(&mark.path).is_none_or(|old| old.at <= mark.at) {
                accepted.push((mark.clone(), proof));
                by_path.insert(mark.path.clone(), mark);
            }
        }
        // Only growth is refused: paths already held are always observed
        // again, even after the capacity was lowered below what is held,
        // so a full set never keeps an agent from rereading.
        let capacity = state.store.read_capacity();
        if by_path.len() > capacity && by_path.len() > held {
            return Response::Error {
                code: ErrorCode::Invalid,
                message: format!(
                    "read set full: it holds {held} of {capacity} observations ([read_set] capacity in agentd.toml) and this would add {}; checkpoint and continue in a replacement session",
                    by_path.len() - held
                ),
                details: Some(json!({ "capacity": capacity, "held": held })),
            };
        }
        let reads: Vec<_> = by_path.into_values().collect();
        if let Err(e) = state.store.put_document("reads", id.as_str(), &reads) {
            return Response::error(ErrorCode::Internal, e.to_string());
        }
        for (mark, proof) in accepted {
            if let Some(snapshot) = proof {
                state.content_proofs.observe(&id, mark, snapshot);
            } else {
                state.content_proofs.forget_read(&id, &mark.path);
            }
        }
        state.emit(EventKind::ReadsObserved { agent: id, paths });
        Response::Reads { reads }
    }

    pub(super) async fn stale(&self, reference: &str, paths: Vec<String>) -> Response {
        let (id, root, _) = match self.reader_checkout(reference) {
            Ok(v) => v,
            Err(e) => return *e,
        };
        let reads = match self.reads(reference) {
            Response::Reads { reads } => reads,
            e => return e,
        };
        let checkout = root.clone();
        let baselines: HashMap<_, _> = {
            let state = lock(&self.state);
            reads
                .iter()
                .filter_map(|mark| {
                    state
                        .content_proofs
                        .baseline(&id, mark)
                        .map(|snapshot| (mark.path.clone(), snapshot))
                })
                .collect()
        };
        let candidates = match current_reads(
            root.clone(),
            reads.clone(),
            baselines.clone(),
            paths.clone(),
        )
        .await
        {
            Ok(candidates) => candidates,
            Err(error) => return *error,
        };
        if candidates.is_empty() {
            return Response::Stale { stale: Vec::new() };
        }
        // A flush only acknowledges the application's pending queue; it does
        // not prove that the OS has delivered every earlier event. Capture
        // current bytes after the wait, and require matching content proofs.
        let flushed = self.flush_ledger().await;
        let candidates = match current_reads(root, reads, baselines, paths).await {
            Ok(candidates) => candidates,
            Err(error) => return *error,
        };
        let mut stale = Vec::new();
        let state = lock(&self.state);
        let (project, watched) = state
            .registry
            .get(&id)
            .and_then(|r| r.project.as_ref())
            .map(|p| (p.id(), p.dir().to_path_buf()))
            .unzip();
        let since = watched.and_then(|dir| state.ledger_since.get(&dir).copied());
        for ReadCandidate {
            found,
            mark,
            target,
            current,
            differences,
        } in candidates
        {
            let explained = match (&current, &differences) {
                (Some(current), Some(differences)) => {
                    state
                        .content_proofs
                        .explains(&id, &mark, differences, current)
                }
                _ => false,
            };
            if !explained {
                stale.push(found);
                continue;
            }
            let covered = flushed && since.is_some_and(|since| since <= mark.at);
            let (true, Some(project), Some(_), Ok(mark_rel), Ok(target_rel)) = (
                covered,
                project.clone(),
                found.current.as_ref(),
                mark.path.strip_prefix(&checkout),
                target.strip_prefix(&checkout),
            ) else {
                stale.push(StalePath {
                    path: target,
                    ..found
                });
                continue;
            };
            let query = ChangesQuery {
                project,
                since_seq: None,
                path: Some(mark_rel.to_string_lossy().into_owned()),
                agent: None,
                limit: STALE_LEDGER_SCAN,
                after: Some(mark.at),
                before_seq: None,
            };
            // Whether the page is whole is a property of the page, before
            // other checkouts' entries are dropped from it.
            let (changes, complete): (Vec<Change>, bool) = match state.store.changes(&query) {
                Ok(page) => {
                    let complete = page.len() < STALE_LEDGER_SCAN;
                    let changes = page
                        .into_iter()
                        // SQLite julianday's coarse precision may include rows
                        // just before the read. Recheck their exact stored time.
                        .filter(|c| c.at >= mark.at)
                        .filter(|c| c.checkout.as_deref().is_none_or(|c| c == checkout))
                        .collect();
                    (changes, complete)
                }
                Err(_) => {
                    stale.push(StalePath {
                        path: target,
                        ..found
                    });
                    continue;
                }
            };
            if agentdocker_core::working_set::stale_by_ledger(
                &id, mark_rel, target_rel, &changes, complete,
            ) {
                stale.push(StalePath {
                    path: target,
                    ..found
                });
            }
        }
        Response::Stale { stale }
    }
}

struct ReadCandidate {
    found: StalePath,
    mark: ReadMark,
    target: PathBuf,
    current: Option<Arc<content::Snapshot>>,
    differences: Option<Vec<(PathBuf, String)>>,
}

async fn current_reads(
    root: PathBuf,
    reads: Vec<ReadMark>,
    baselines: HashMap<PathBuf, Arc<content::Snapshot>>,
    paths: Vec<String>,
) -> Result<Vec<ReadCandidate>, Box<Response>> {
    match tokio::task::spawn_blocking(move || snapshot_candidates(&root, &reads, &baselines, paths))
        .await
    {
        Ok(Ok(candidates)) => Ok(candidates),
        Ok(Err(error)) => Err(Box::new(Response::error(
            ErrorCode::Invalid,
            error.to_string(),
        ))),
        Err(error) => Err(Box::new(Response::error(
            ErrorCode::Internal,
            error.to_string(),
        ))),
    }
}

fn snapshot_candidates(
    root: &Path,
    reads: &[ReadMark],
    baselines: &HashMap<PathBuf, Arc<content::Snapshot>>,
    paths: Vec<String>,
) -> anyhow::Result<Vec<ReadCandidate>> {
    let mut asked: BTreeMap<(PathBuf, PathBuf), ReadMark> = BTreeMap::new();
    if paths.is_empty() {
        for mark in reads {
            asked.insert((mark.path.clone(), mark.path.clone()), mark.clone());
        }
    } else {
        for raw in paths {
            let path = checkout_path(root, &raw)?;
            // Missing is not synonymous with never observed: a covering
            // directory snapshot can remember a child that was removed.
            if let Some(mark) = reads
                .iter()
                .filter(|mark| path.starts_with(&mark.path))
                .max_by_key(|mark| (mark.at, mark.path.components().count()))
            {
                asked.insert((mark.path.clone(), path.clone()), mark.clone());
            }
            for mark in reads.iter().filter(|mark| mark.path.starts_with(&path)) {
                asked.insert((mark.path.clone(), mark.path.clone()), mark.clone());
            }
        }
    }
    // Asked paths are ordered by their covering mark. Keep only that group's
    // temporary snapshot; candidates may retain snapshots and differences up to
    // a separate per-request payload budget. Refuse exemptions past either cap.
    let mut captured_group: Option<(PathBuf, Result<Arc<content::Snapshot>, String>, bool)> = None;
    let mut snapshot_budget = super::read_content::CACHE_BYTES;
    let mut difference_budget = super::read_content::CACHE_BYTES;
    let mut candidates = Vec::new();
    for ((mark_path, target), mark) in asked {
        if captured_group
            .as_ref()
            .is_none_or(|(path, _, _)| path != &mark_path)
        {
            let captured = content::snapshot(&mark.path)
                .map(Arc::new)
                .map_err(|error| error.to_string());
            let retain = captured.as_ref().is_ok_and(|snapshot| {
                let bytes = snapshot.retained_bytes();
                if bytes > snapshot_budget {
                    return false;
                }
                snapshot_budget -= bytes;
                true
            });
            captured_group = Some((mark_path, captured, retain));
        }
        let (_, captured, retain_snapshot) = captured_group.as_ref().unwrap();
        let (current, mut differences, reason) = match captured {
            Ok(current) if current.version == mark.version => continue,
            Ok(current) => {
                let relative = target.strip_prefix(&mark.path)?;
                let differences = baselines
                    .get(&mark.path)
                    .map(|baseline| baseline.differences(current, relative));
                if differences.as_ref().is_some_and(Vec::is_empty) {
                    continue;
                }
                let reason = if current.entry(relative) == content::MISSING {
                    let existed = (relative.as_os_str().is_empty()
                        && mark.version != content::MISSING)
                        || baselines
                            .get(&mark.path)
                            .is_some_and(|baseline| baseline.entry(relative) != content::MISSING);
                    if existed {
                        "removed since observation; look again before writing it back"
                    } else {
                        "path is absent and its previous content is unknown; observe this path again before writing"
                    }
                } else {
                    "content changed since observation; reread before editing"
                };
                (Some(current.clone()), differences, reason.to_string())
            }
            Err(error) => (
                None,
                None,
                format!("cannot verify current content: {error}"),
            ),
        };
        let found_current = current.as_ref().map(|snapshot| snapshot.version.clone());
        let retain = *retain_snapshot
            && differences.as_ref().is_some_and(|changes| {
                let bytes = changes
                    .iter()
                    .map(|(path, version)| 128 + path.as_os_str().len() * 2 + version.len() * 2)
                    .sum::<usize>();
                if bytes > difference_budget {
                    return false;
                }
                difference_budget -= bytes;
                true
            });
        let current = if retain {
            current
        } else {
            differences = None;
            None
        };
        candidates.push(ReadCandidate {
            found: StalePath {
                path: target.clone(),
                observed: mark.version.clone(),
                current: found_current,
                reason,
            },
            mark,
            target,
            current,
            differences,
        });
    }
    Ok(candidates)
}

/// At most this many ledger entries are read to explain one stale mark;
/// past it, the mark's targets stay stale.
const STALE_LEDGER_SCAN: usize = 500;

/// Paths one `observe` names at most, whatever the read-set capacity: a
/// bound on the host work of one request, which snapshots every path.
const OBSERVE_PATHS: usize = 1000;

pub(super) fn checkout_path(root: &Path, raw: &str) -> anyhow::Result<PathBuf> {
    let raw = Path::new(raw);
    let path = project::canonical(&if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        root.join(raw)
    });
    anyhow::ensure!(
        path.starts_with(root),
        "observation is outside the agent's physical checkout"
    );
    Ok(path)
}

pub(super) fn check_reads(reads: &[ReadMark]) -> Vec<StalePath> {
    reads
        .iter()
        .filter_map(|mark| match content::fingerprint(&mark.path) {
            Ok(current) if current == mark.version => None,
            Ok(current) => Some(StalePath {
                path: mark.path.clone(),
                observed: mark.version.clone(),
                reason: if current == content::MISSING {
                    "removed since observation; look again before writing it back".into()
                } else {
                    "content changed since observation; reread before editing".into()
                },
                current: Some(current),
            }),
            Err(e) => Some(StalePath {
                path: mark.path.clone(),
                observed: mark.version.clone(),
                current: None,
                reason: format!("cannot verify current content: {e}"),
            }),
        })
        .collect()
}

impl State {
    pub(super) fn warn_readers(&mut self, change: &Change, physical: Option<&Path>) {
        let Some(checkout) = &change.checkout else {
            return;
        };
        let absolute = physical
            .map(Path::to_path_buf)
            .unwrap_or_else(|| checkout.join(&change.path));
        let agents: Vec<_> = self
            .registry
            .list(false)
            .iter()
            .filter(|r| {
                r.project.as_ref().is_some_and(|p| p.dir() == checkout)
                    && change.by.agent() != Some(&r.id)
            })
            .map(|r| r.id.clone())
            .collect();
        for agent in agents {
            let reads = match self
                .store
                .document::<Vec<ReadMark>>("reads", agent.as_str())
            {
                Ok(Some(reads)) => reads,
                _ => continue,
            };
            if !reads
                .iter()
                .any(|m| absolute.starts_with(&m.path) && m.at < change.at)
            {
                continue;
            }
            // The live event says so at once; the inbox notice waits for
            // the tick, so one switch of a branch is one message.
            self.emit(EventKind::AgentStale {
                agent: agent.clone(),
                paths: vec![absolute.clone()],
            });
            let pending = self.pending_stale.entry(agent).or_default();
            if pending.len() < PENDING_STALE_PATHS || pending.contains_key(&absolute) {
                pending.insert(absolute.clone(), change.clone());
            }
        }
    }

    /// One `stale` message per reader for everything that changed since the
    /// last tick, unless the reader still has the last one queued: then
    /// the paths wait, and the notice sent once that one has been
    /// acknowledged names them all (a queued envelope is never changed).
    /// The notice is kept under [`NOTICE_BYTES`], naming fewer paths and
    /// changes when they do not fit, down to the count alone. A message
    /// that still cannot be queued (a full inbox, a storage failure) is
    /// dropped with its paths; `check_stale` reads content and hooks
    /// refuse a stale edit, whether or not a notice arrived.
    pub(super) fn flush_notices(&mut self) {
        if self.fenced() {
            // Nothing can be queued while fenced. What waits is in memory
            // only: it is sent by this daemon's next tick if the transfer
            // is aborted, and lost with this daemon if it is not; a reader
            // of the successor finds the change by `stale`, and hooks deny
            // a stale edit either way.
            return;
        }
        self.stale_outstanding.retain(|agent, message| {
            self.inboxes
                .get(agent)
                .is_some_and(|queue| queue.iter().any(|m| m.id == *message))
        });
        let ready: Vec<AgentId> = self
            .pending_stale
            .keys()
            .filter(|agent| !self.stale_outstanding.contains_key(*agent))
            .cloned()
            .collect();
        for agent in ready {
            let Some(pending) = self.pending_stale.remove(&agent) else {
                continue;
            };
            if self
                .registry
                .get(&agent)
                .is_none_or(|r| !r.status.is_live())
            {
                continue;
            }
            let count = pending.len();
            let mut changes: Vec<&Change> = pending.values().collect();
            changes.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| b.seq.cmp(&a.seq)));
            let mut list_paths = LISTED_STALE_PATHS;
            let mut list_changes = LISTED_STALE_CHANGES;
            let payload = loop {
                let paths: Vec<&PathBuf> = pending.keys().take(list_paths).collect();
                let listed = paths
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                let text = if count == 1 && !paths.is_empty() {
                    format!(
                        "{listed} changed after your observation. Check current content and reread before editing. Attribution is best-effort."
                    )
                } else if paths.is_empty() {
                    format!(
                        "{count} paths changed after your observation. Check current content and reread before editing (`stale` names them). Attribution is best-effort."
                    )
                } else {
                    format!(
                        "{count} paths changed after your observation: {listed}{}. Check current content and reread before editing. Attribution is best-effort.",
                        if count > paths.len() {
                            format!(" (+{} more)", count - paths.len())
                        } else {
                            String::new()
                        }
                    )
                };
                let payload = json!({
                    "text": text, "paths": paths, "count": count,
                    "changes": &changes[..list_changes.min(changes.len())],
                });
                let fits = serde_json::to_vec(&payload).is_ok_and(|b| b.len() <= NOTICE_BYTES);
                if fits || (list_paths == 0 && list_changes == 0) {
                    break payload;
                }
                list_changes /= 2;
                list_paths /= 2;
            };
            let response = self.send(
                "agentd".into(),
                Destination::Agent(agent.clone()),
                "stale".into(),
                payload,
                None,
            );
            if let Response::Sent { message, .. } = response {
                self.stale_outstanding.insert(agent, message);
            }
        }
        self.flush_contested();
        self.sweep_channel_ghosts();
    }
}

/// Distinct paths kept per reader between ticks; beyond this nothing more
/// is tracked, the notice's `count` is what was, and a reader that never
/// drains its inbox finds the rest by `check_stale`.
const PENDING_STALE_PATHS: usize = 10_000;
/// Paths named in one notice, and changes carried with it, at most; a
/// notice is kept under this many bytes by naming fewer.
const LISTED_STALE_PATHS: usize = 200;
const LISTED_STALE_CHANGES: usize = 50;
const NOTICE_BYTES: usize = 64 * 1024;

#[cfg(test)]
mod tests {
    use super::*;
    async fn setup(home: &Path, checkout: &Path, name: &str) -> Arc<Daemon> {
        let daemon = Arc::new(Daemon::open(home.into(), home.join("sock")).unwrap());
        daemon
            .handle(Request::Register {
                spec: AgentSpec {
                    name: name.into(),
                    workdir: Some(checkout.into()),
                    ..AgentSpec::default()
                },
                pid: None,
                session: None,
            })
            .await;
        daemon
    }
    fn configure(daemon: &Daemon, text: &str) {
        std::fs::write(daemon.home.join(agentdocker_core::config::FILE_NAME), text).unwrap();
        daemon.apply_read_set_capacity();
    }

    fn held(daemon: &Daemon, agent: &str) -> usize {
        match daemon.reads(agent) {
            Response::Reads { reads } => reads.len(),
            other => panic!("{other:?}"),
        }
    }

    /// The capacity in `agentd.toml` is the one observations keep to: an
    /// observation that would grow the set past it is refused whole and
    /// records nothing, while rereading what is held never is, even after
    /// the capacity was lowered below it.
    #[tokio::test]
    async fn observations_keep_to_the_configured_capacity_and_rereads_are_never_refused() {
        use agentdocker_core::config::READ_SET_CAPACITY;
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("state");
        let root = tmp.path().join("checkout");
        std::fs::create_dir(&root).unwrap();
        for i in 0..160 {
            std::fs::write(root.join(format!("f{i}")), format!("{i}")).unwrap();
        }
        let daemon = setup(&home, &root, "reader").await;
        let files = |range: std::ops::Range<usize>| -> Vec<String> {
            range.map(|i| format!("f{i}")).collect()
        };
        daemon.apply_read_set_capacity();
        assert_eq!(lock(&daemon.state).store.read_capacity(), READ_SET_CAPACITY);

        configure(&daemon, "[read_set]\ncapacity = 100\n");
        assert_eq!(lock(&daemon.state).store.read_capacity(), 100);
        assert!(matches!(
            daemon.observe("reader", files(0..100)).await,
            Response::Reads { reads } if reads.len() == 100
        ));
        let refused = daemon.observe("reader", files(99..101)).await;
        let Response::Error {
            code: ErrorCode::Invalid,
            message,
            details: Some(details),
        } = refused
        else {
            panic!("{refused:?}")
        };
        assert!(
            message.contains("holds 100 of 100") && message.contains("[read_set] capacity"),
            "{message}"
        );
        assert_eq!(details, json!({"capacity": 100, "held": 100}));
        assert_eq!(held(&daemon, "reader"), 100, "a refusal records nothing");
        assert!(matches!(
            daemon.observe("reader", files(0..100)).await,
            Response::Reads { reads } if reads.len() == 100
        ));

        configure(&daemon, "[read_set]\ncapacity = 200\n");
        assert!(matches!(
            daemon.observe("reader", files(100..150)).await,
            Response::Reads { reads } if reads.len() == 150
        ));
        // Lowered below what is held: rereads go on, growth does not.
        configure(&daemon, "[read_set]\ncapacity = 100\n");
        assert!(matches!(
            daemon.observe("reader", files(0..150)).await,
            Response::Reads { reads } if reads.len() == 150
        ));
        assert!(matches!(
            daemon.observe("reader", files(150..151)).await,
            Response::Error { code: ErrorCode::Invalid, details: Some(details), .. }
                if details == json!({"capacity": 100, "held": 150})
        ));
        assert_eq!(held(&daemon, "reader"), 150);
    }

    /// A capacity out of range, or a file that does not read, leaves the
    /// default in force and is said once until it changes; a good value
    /// clears the notice. One request names at most 1,000 paths whatever
    /// the capacity.
    #[tokio::test]
    async fn an_unusable_capacity_leaves_the_default_and_requests_stay_bounded() {
        use agentdocker_core::config::{READ_SET_CAPACITY, READ_SET_CAPACITY_MAX};
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("state");
        let root = tmp.path().join("checkout");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("file"), "one").unwrap();
        let daemon = setup(&home, &root, "reader").await;
        for text in [
            "[read_set]\ncapacity = 99\n".to_owned(),
            format!("[read_set]\ncapacity = {}\n", READ_SET_CAPACITY_MAX + 1),
            "[read_set]\ncapacity = \"lots\"\n".to_owned(),
        ] {
            configure(&daemon, "[read_set]\ncapacity = 300\n");
            assert_eq!(lock(&daemon.state).store.read_capacity(), 300);
            assert!(lock(&daemon.state).read_set_notice.is_none());
            configure(&daemon, &text);
            let notice = lock(&daemon.state).read_set_notice.clone();
            assert!(notice.is_some(), "{text}");
            assert_eq!(
                lock(&daemon.state).store.read_capacity(),
                READ_SET_CAPACITY,
                "{text}"
            );
            daemon.apply_read_set_capacity();
            assert_eq!(lock(&daemon.state).read_set_notice, notice, "said once");
        }
        configure(&daemon, "[read_set]\ncapacity = 10000\n");
        assert!(lock(&daemon.state).read_set_notice.is_none());
        assert_eq!(lock(&daemon.state).store.read_capacity(), 10_000);
        std::fs::remove_file(home.join(agentdocker_core::config::FILE_NAME)).unwrap();
        daemon.apply_read_set_capacity();
        assert_eq!(lock(&daemon.state).store.read_capacity(), READ_SET_CAPACITY);

        let many: Vec<String> = (0..=OBSERVE_PATHS).map(|_| "file".to_owned()).collect();
        assert!(matches!(
            daemon.observe("reader", many).await,
            Response::Error { code: ErrorCode::Invalid, message, .. } if message == "observe requires 1–1000 paths"
        ));
        assert!(matches!(
            daemon.observe("reader", Vec::new()).await,
            Response::Error {
                code: ErrorCode::Invalid,
                ..
            }
        ));
    }

    /// `rm` forgets an agent's read set with its record, and the tick
    /// sweeps read sets left for records an earlier daemon forgot.
    #[tokio::test]
    async fn a_forgotten_agent_leaves_no_read_set() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("state");
        let root = tmp.path().join("checkout");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("file"), "one").unwrap();
        let daemon = setup(&home, &root, "reader").await;
        let id = lock(&daemon.state).registry.resolve("reader").unwrap();
        daemon.observe("reader", vec!["file".into()]).await;
        let stored = |id: &str| {
            lock(&daemon.state)
                .store
                .document::<Vec<ReadMark>>("reads", id)
                .unwrap()
        };
        assert!(stored(id.as_str()).is_some());
        let deregistered = daemon
            .handle(Request::Deregister {
                agent: "reader".into(),
            })
            .await;
        assert!(
            !matches!(deregistered, Response::Error { .. }),
            "{deregistered:?}"
        );
        assert_eq!(
            daemon
                .handle(Request::Remove {
                    agent: id.to_string()
                })
                .await,
            Response::Ok
        );
        assert!(stored(id.as_str()).is_none());
        lock(&daemon.state)
            .store
            .put_document("reads", "left-behind", &Vec::<ReadMark>::new())
            .unwrap();
        daemon.prune_orphan_reads();
        assert!(stored("left-behind").is_none());
    }

    #[tokio::test]
    async fn content_check_catches_unwatched_changes_retries_and_restart() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("state");
        let root = tmp.path().join("checkout");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("file"), "one").unwrap();
        let daemon = setup(&home, &root, "reader").await;
        assert!(matches!(
            daemon.observe("reader", vec!["file".into()]).await,
            Response::Reads { .. }
        ));
        std::fs::write(root.join("file"), "two").unwrap();
        // No watcher event is needed, and repeated checks cannot bypass it.
        for _ in 0..2 {
            assert!(
                matches!(daemon.stale("reader", vec!["file".into()]).await, Response::Stale { stale } if stale.len() == 1)
            );
        }
        drop(daemon);
        let daemon = Arc::new(Daemon::open(home.clone(), home.join("sock")).unwrap());
        assert!(
            matches!(daemon.stale("reader", vec![]).await, Response::Stale { stale } if stale.len() == 1)
        );
        daemon.observe("reader", vec!["file".into()]).await;
        assert!(
            matches!(daemon.stale("reader", vec![]).await, Response::Stale { stale } if stale.is_empty())
        );
        assert!(matches!(
            daemon.observe("reader", vec!["../outside".into()]).await,
            Response::Error {
                code: ErrorCode::Invalid,
                ..
            }
        ));
    }
    #[tokio::test]
    async fn rereading_specific_file_shadows_old_directory_for_that_target() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("checkout");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("file"), "one").unwrap();
        let daemon = setup(&tmp.path().join("state"), &root, "reader").await;
        daemon.observe("reader", vec![".".into()]).await;
        std::fs::write(root.join("file"), "two").unwrap();
        daemon.observe("reader", vec!["file".into()]).await;
        assert!(
            matches!(daemon.stale("reader", vec!["file".into()]).await, Response::Stale { stale } if stale.is_empty())
        );
        assert!(
            matches!(daemon.stale("reader", vec![".".into()]).await, Response::Stale { stale } if !stale.is_empty())
        );
    }

    #[tokio::test]
    async fn separate_checkouts_do_not_stale_each_other() {
        let tmp = tempfile::tempdir().unwrap();
        let first = tmp.path().join("first");
        let second = tmp.path().join("second");
        std::fs::create_dir(&first).unwrap();
        std::fs::create_dir(&second).unwrap();
        std::fs::write(first.join("file"), "one").unwrap();
        std::fs::write(second.join("file"), "one").unwrap();
        let daemon = setup(&tmp.path().join("state"), &first, "reader").await;
        daemon.observe("reader", vec!["file".into()]).await;
        std::fs::write(second.join("file"), "two").unwrap();
        assert!(
            matches!(daemon.stale("reader", vec![]).await, Response::Stale { stale } if stale.is_empty())
        );
    }
}

#[cfg(test)]
mod warning_tests {
    use super::*;
    #[tokio::test]
    async fn watcher_warns_only_readers_of_the_changed_physical_checkout() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("checkout");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("file"), "one").unwrap();
        let root = project::canonical(&root);
        let daemon =
            Arc::new(Daemon::open(tmp.path().join("state"), tmp.path().join("sock")).unwrap());
        let reader = match daemon
            .handle(Request::Register {
                spec: AgentSpec {
                    name: "reader".into(),
                    workdir: Some(root.clone()),
                    ..AgentSpec::default()
                },
                pid: None,
                session: None,
            })
            .await
        {
            Response::Agent { agent } => agent,
            other => panic!("{other:?}"),
        };
        daemon.observe("reader", vec!["file".into()]).await;
        daemon
            .record_fs_changes(
                vec![Observed {
                    checkout: Checkout {
                        dir: root,
                        project: reader.project.unwrap().id(),
                        worktree: None,
                    },
                    path: "file".into(),
                    kind: agentdocker_core::ChangeKind::Modified,
                }],
                vec![],
            )
            .await;
        daemon.flush_notices();
        let Response::Messages { messages } = daemon
            .handle(Request::Inbox {
                agent: "reader".into(),
                drain: false,
            })
            .await
        else {
            panic!()
        };
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].kind, "stale");
    }

    /// A branch switch is hundreds of changes in a moment. The reader gets
    /// one notice naming the paths, and nothing more until it has consumed
    /// that one; what changes in the meantime waits and is named by the
    /// next notice, so an unread notice is superseded, never followed.
    #[tokio::test]
    async fn stale_notices_are_one_per_tick_and_wait_for_the_last_to_be_read() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("checkout");
        std::fs::create_dir(&root).unwrap();
        for name in ["a", "b", "c", "d"] {
            std::fs::write(root.join(name), "one").unwrap();
        }
        let root = project::canonical(&root);
        let daemon =
            Arc::new(Daemon::open(tmp.path().join("state"), tmp.path().join("sock")).unwrap());
        let Response::Agent { agent: reader } = daemon
            .handle(Request::Register {
                spec: AgentSpec {
                    name: "reader".into(),
                    workdir: Some(root.clone()),
                    ..AgentSpec::default()
                },
                pid: None,
                session: None,
            })
            .await
        else {
            panic!("register failed");
        };
        let project = reader.project.unwrap().id();
        let checkout = Checkout {
            dir: root.clone(),
            project,
            worktree: None,
        };
        daemon.observe("reader", vec![".".into()]).await;
        let change = |path: &str, kind| Observed {
            checkout: checkout.clone(),
            path: path.into(),
            kind,
        };
        use agentdocker_core::ChangeKind::{Created, Modified, Removed};
        // Created, removed and modified for one path within the switch, and
        // two more paths: one notice.
        daemon
            .record_fs_changes(
                vec![
                    change("a", Created),
                    change("a", Removed),
                    change("a", Modified),
                    change("b", Modified),
                    change("c", Modified),
                ],
                vec![],
            )
            .await;
        let inbox = |drain: bool| {
            let daemon = daemon.clone();
            async move {
                match daemon
                    .handle(Request::Inbox {
                        agent: "reader".into(),
                        drain,
                    })
                    .await
                {
                    Response::Messages { messages } => messages,
                    other => panic!("{other:?}"),
                }
            }
        };
        assert!(inbox(false).await.is_empty(), "nothing before the tick");
        daemon.flush_notices();
        let first = inbox(false).await;
        assert_eq!(first.len(), 1, "{first:?}");
        assert_eq!(first[0].kind, "stale");
        let canonical = |name: &str| agentdocker_host::project::canonical(&root.join(name));
        assert_eq!(
            first[0].payload["paths"],
            json!([canonical("a"), canonical("b"), canonical("c")])
        );
        assert_eq!(first[0].payload["count"], json!(3));
        assert_eq!(
            first[0].payload["changes"].as_array().unwrap().len(),
            3,
            "the last change per path travels with the notice"
        );
        assert!(
            first[0].payload["text"]
                .as_str()
                .unwrap()
                .starts_with("3 paths changed after your observation: ")
        );
        // Unread: a further change waits rather than queueing another.
        daemon
            .record_fs_changes(vec![change("d", Modified)], vec![])
            .await;
        daemon.flush_notices();
        daemon.flush_notices();
        assert_eq!(inbox(false).await.len(), 1, "still the one notice");
        // Consumed: the next tick names what waited.
        let drained = inbox(true).await;
        assert_eq!(drained.len(), 1);
        daemon.flush_notices();
        let second = inbox(true).await;
        assert_eq!(second.len(), 1, "{second:?}");
        assert_eq!(second[0].payload["paths"], json!([canonical("d")]));
        assert_eq!(
            second[0].payload["text"],
            json!(format!(
                "{} changed after your observation. Check current content and reread before editing. Attribution is best-effort.",
                canonical("d").display()
            ))
        );
        daemon.flush_notices();
        assert!(
            inbox(false).await.is_empty(),
            "nothing pending, nothing sent"
        );
        // Fenced: the tick sends nothing and forgets nothing; the notice
        // arrives once authority is back.
        daemon
            .record_fs_changes(vec![change("b", Modified)], vec![])
            .await;
        daemon.offer_transfer(1).unwrap();
        daemon.flush_notices();
        {
            let state = lock(&daemon.state);
            assert!(
                state.pending_stale.values().any(|p| !p.is_empty()),
                "still pending while fenced"
            );
            assert!(
                state.stale_outstanding.is_empty(),
                "nothing sent while fenced"
            );
        }
        assert!(daemon.abort_transfer("cleanup"));
        daemon.flush_notices();
        let after = inbox(true).await;
        assert_eq!(after.len(), 1, "{after:?}");
        assert_eq!(after[0].payload["paths"], json!([canonical("b")]));
        // A change to very many paths with long names still fits one
        // notice: fewer are named, the count stays exact.
        // Names near the filesystem's limit, in nested directories.
        let long: Vec<String> = (0..400)
            .map(|n| format!("{0}/{0}/{0}-{n}", "d".repeat(200)))
            .collect();
        for name in &long {
            std::fs::create_dir_all(root.join(name).parent().unwrap()).unwrap();
            std::fs::write(root.join(name), "one").unwrap();
        }
        daemon.observe("reader", vec![".".into()]).await;
        daemon
            .record_fs_changes(
                long.iter().map(|name| change(name, Modified)).collect(),
                vec![],
            )
            .await;
        daemon.flush_notices();
        let big = inbox(true).await;
        assert_eq!(big.len(), 1, "{}", big.len());
        assert_eq!(big[0].payload["count"], json!(400));
        let named = big[0].payload["paths"].as_array().unwrap().len();
        assert!(named > 0 && named < 200, "named {named}");
        assert!(serde_json::to_vec(&big[0].payload).unwrap().len() <= NOTICE_BYTES);
    }
}
