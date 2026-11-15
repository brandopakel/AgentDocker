use super::*;

/// A journal read as the request spelt it: the filters by name, resolved
/// to ids, kinds and relative paths by [`Daemon::journal`].
pub(super) struct JournalArgs {
    pub since_seq: Option<u64>,
    pub until_seq: Option<u64>,
    pub agent: Option<String>,
    pub branch: Option<String>,
    pub kind: Option<String>,
    pub path: Option<String>,
    pub grep: Option<String>,
    pub limit: usize,
    pub digest: Option<DigestRequest>,
}

/// How long git has to say what a checkout has uncommitted.
const UNCOMMITTED_READ: std::time::Duration = std::time::Duration::from_secs(2);
/// How often a checkout is looked at while git may still be writing it.
const SETTLE_LOOK: std::time::Duration = std::time::Duration::from_millis(250);
/// How long a checkout is waited for at most before the changes seen in
/// it are judged as it then stands.
const SETTLE_LIMIT: std::time::Duration = std::time::Duration::from_secs(60);
/// At most this many changes of one checkout wait to be judged.
pub(super) const CONTEST_WAITING: usize = 2_000;

/// The paths of the checkout `dir` that differ from its HEAD: what tells
/// a change somebody made from one git made. `None` when that cannot be
/// known, and every change there then counts as somebody's: `dir` is not
/// a git checkout's own root (git names paths from the root, so a watched
/// folder inside one would be compared with nothing), or git did not
/// answer in time.
fn uncommitted(dir: &Path) -> Option<HashSet<PathBuf>> {
    if !dir.join(".git").exists() {
        return None;
    }
    let deadline = std::time::Instant::now() + UNCOMMITTED_READ;
    vcs::dirty_paths(dir, deadline)
        .ok()
        .map(|paths| paths.into_iter().map(PathBuf::from).collect())
}

/// What a look at the checkout `dir` finds: whether git is in the middle
/// of changing it, and where its HEAD is.
///
/// Git is in the middle of it while it holds a lock or has an operation
/// open. The locks: the index's, from before a pull, merge, rebase,
/// branch switch or reset writes its first file until the index is
/// written; then HEAD's and the branch's, while the reference is moved
/// (a `reference-transaction` hook can hold git there, the files and
/// the index new and HEAD still old); and the packed references'. An
/// open operation: a merge, a cherry-pick or a revert that has not made
/// its commit, a rebase or a sequence under way. Those last also stand
/// while git waits for a person to settle a conflict, which is why a
/// checkout is not waited for without end ([`SETTLE_LIMIT`]).
fn look(dir: &Path) -> (bool, Option<String>) {
    const HELD_OR_OPEN: [&str; 8] = [
        "index.lock",
        "HEAD.lock",
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "rebase-merge",
        "rebase-apply",
        "sequencer",
    ];
    let state = vcs::state(dir);
    let writing = vcs::git_dirs(dir).is_some_and(|(gitdir, common)| {
        HELD_OR_OPEN.iter().any(|name| gitdir.join(name).exists())
            || common.join("packed-refs.lock").exists()
            || state
                .as_ref()
                .and_then(|state| state.branch.as_ref())
                .is_some_and(|branch| {
                    common
                        .join("refs/heads")
                        .join(format!("{branch}.lock"))
                        .exists()
                })
    });
    (writing, state.and_then(|state| state.head))
}

/// Count toward a contested room the changes waiting for the checkout
/// `dir`, once git has finished with it.
///
/// What differs from HEAD at the moment a change is seen proves nothing
/// yet: in the middle of a merge the files git has already written
/// differ from a HEAD it has not moved. So the checkout is looked at
/// ([`look`]) until git is not in the middle of anything and HEAD is
/// where it was at the look before; git is then asked what differs; and
/// the answer is used only if a look after it finds the same: nothing
/// held or open, HEAD unmoved. An answer read while git was at work, or
/// across a move of HEAD, is thrown away and the wait goes on. A change
/// whose path still differs is somebody's, and one git brought in is by
/// then equal to HEAD.
///
/// A change that arrives while git is being asked is left for the next
/// round, never judged by an answer older than itself. After
/// [`SETTLE_LIMIT`] the checkout is judged as it stands: a lock left
/// behind by a git that died, or a conflict nobody settles, does not
/// keep real edits from ever counting.
///
/// It holds the daemon weakly: a wait must not be what keeps one alive.
async fn settle_contest(daemon: std::sync::Weak<Daemon>, dir: PathBuf) {
    let look_at = |dir: &Path| {
        let dir = dir.to_path_buf();
        tokio::task::spawn_blocking(move || look(&dir))
    };
    // A look that cannot be made ends the wait, and what waited with it: a
    // list left behind would keep a later change from starting a wait.
    let give_up = |daemon: &std::sync::Weak<Daemon>, dir: &Path| {
        if let Some(daemon) = daemon.upgrade() {
            lock(&daemon.state).contest_waiting.remove(dir);
        }
    };
    let started = std::time::Instant::now();
    let mut seen: Option<Option<String>> = None;
    loop {
        tokio::time::sleep(SETTLE_LOOK).await;
        let Ok((writing, head)) = look_at(&dir).await else {
            return give_up(&daemon, &dir);
        };
        let overdue = started.elapsed() >= SETTLE_LIMIT;
        let moved = seen.as_ref() != Some(&head);
        seen = Some(head.clone());
        if (writing || moved) && !overdue {
            continue;
        }
        // The changes there are now; a later one waits for the next round.
        let Some(upto) = daemon.upgrade().and_then(|daemon| {
            lock(&daemon.state)
                .contest_waiting
                .get(&dir)
                .and_then(|waiting| waiting.iter().map(|change| change.seq).max())
        }) else {
            return;
        };
        let read = dir.clone();
        let differing = tokio::task::spawn_blocking(move || uncommitted(&read))
            .await
            .ok()
            .flatten();
        // The answer counts only if git was at rest around it.
        let Ok((writing, after)) = look_at(&dir).await else {
            return give_up(&daemon, &dir);
        };
        if (writing || after != head) && !overdue {
            seen = Some(after);
            continue;
        }
        let Some(daemon) = daemon.upgrade() else {
            return;
        };
        let mut state = lock(&daemon.state);
        let waiting = state.contest_waiting.remove(&dir).unwrap_or_default();
        let (judged, later): (Vec<Change>, Vec<Change>) =
            waiting.into_iter().partition(|change| change.seq <= upto);
        for change in judged {
            if differing
                .as_ref()
                .is_none_or(|differs| differs.contains(&change.path))
            {
                state.note_contested(&change);
            }
        }
        if later.is_empty() {
            return;
        }
        state.contest_waiting.insert(dir.clone(), later);
    }
}

impl Daemon {
    pub(super) async fn journal(&self, project: &str, args: JournalArgs) -> Response {
        let JournalArgs {
            since_seq,
            until_seq,
            agent,
            branch,
            kind,
            path,
            grep,
            limit,
            digest,
        } = args;
        if let Some(request) = digest {
            return self.journal_digest(project, since_seq, request).await;
        }
        let project = match self.resolve_project(project).await {
            Ok(id) => id,
            Err(response) => return *response,
        };
        let agent = match agent.map(|reference| self.resolve(&reference)).transpose() {
            Ok(agent) => agent,
            Err(response) => return *response,
        };
        let kind = match kind.as_deref().map(JournalKind::parse) {
            None => None,
            Some(Some(kind)) => Some(kind),
            Some(None) => {
                return Response::error(
                    ErrorCode::Invalid,
                    "kind must be release, note, commit, join, leave, or handoff",
                );
            }
        };
        let path = match path {
            Some(raw) => Some(self.relative_path(raw).await),
            None => None,
        };
        let query = JournalQuery {
            project,
            since_seq,
            until_seq,
            agent,
            branch,
            kind,
            path,
            grep,
            limit: limit.clamp(1, 10_000),
        };
        lock(&self.state).journal_query(query)
    }

    /// The digest form of `journal`: who is reading decides the cursor and
    /// the branch filter. An empty project means the reader's own.
    pub(super) async fn journal_digest(
        &self,
        project: &str,
        since_seq: Option<u64>,
        request: DigestRequest,
    ) -> Response {
        let (key, name, branch, own_project) = if request.reader == USER_READER {
            (USER_READER.to_owned(), USER_READER.to_owned(), None, None)
        } else {
            let id = match self.resolve(&request.reader) {
                Ok(id) => id,
                Err(response) => return *response,
            };
            let Some(record) = lock(&self.state).registry.get(&id).cloned() else {
                return Response::error(ErrorCode::NotFound, "agent vanished");
            };
            (
                id.as_str().to_owned(),
                record.spec.name.clone(),
                record.vcs.as_ref().and_then(|v| v.branch.clone()),
                record.project.as_ref().map(ProjectRef::id),
            )
        };
        let project = if project.is_empty() {
            match own_project {
                Some(id) => id,
                None => {
                    return Response::error(
                        ErrorCode::Invalid,
                        "the reader is in no project; name one",
                    );
                }
            }
        } else {
            match self.resolve_project(project).await {
                Ok(id) => id,
                Err(response) => return *response,
            }
        };
        lock(&self.state).journal_digest(
            project,
            &key,
            &name,
            branch.as_deref(),
            since_seq,
            &request,
        )
    }

    /// What the watcher saw in one debounced batch: file changes become
    /// ledger entries (persisted in `changes`, announced live as
    /// `file_changed`), and checkouts whose HEAD moved get their agents'
    /// branch and head re-read.
    pub async fn record_fs_changes(
        self: &Arc<Self>,
        observed: Vec<Observed>,
        vcs_touched: Vec<Checkout>,
    ) {
        let dirs: Vec<PathBuf> = {
            let mut dirs: Vec<PathBuf> = observed.iter().map(|o| o.checkout.dir.clone()).collect();
            dirs.sort();
            dirs.dedup();
            dirs
        };
        // Where each touched checkout's HEAD is, and what it has that
        // differs from it: read once per checkout for the batch, off the
        // lock.
        let (heads, differing): (HashMap<PathBuf, Option<String>>, HashMap<PathBuf, _>) =
            tokio::task::spawn_blocking(move || {
                dirs.into_iter()
                    .map(|dir| {
                        let head = vcs::state(&dir).and_then(|s| s.head);
                        let differs = uncommitted(&dir);
                        ((dir.clone(), head), (dir, differs))
                    })
                    .unzip()
            })
            .await
            .unwrap_or_default();
        let observed = tokio::task::spawn_blocking(move || {
            observed
                .into_iter()
                .map(|entry| {
                    let physical =
                        project::try_canonical(&entry.checkout.dir.join(&entry.path)).ok();
                    (entry, physical)
                })
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();
        let observed: Vec<_> = {
            let state = lock(&self.state);
            observed
                .into_iter()
                .map(|(entry, physical)| {
                    let expected = physical
                        .as_deref()
                        .map_or(Attribution::External, |path| state.attribute(path));
                    let capture = expected.agent().is_some_and(|agent| {
                        state
                            .content_proofs
                            .interested(agent, &entry.checkout.dir.join(&entry.path))
                    });
                    (entry, physical, expected, capture)
                })
                .collect()
        };
        let observed = tokio::task::spawn_blocking(move || {
            // Supplementary guard proofs have a batch I/O budget too.
            let mut proof_budget = agentdocker_host::content::MAX_HASH_BYTES;
            observed
                .into_iter()
                .map(|(entry, physical, expected, capture)| {
                    let version = if capture {
                        agentdocker_host::content::entry_proof(
                            &entry.checkout.dir.join(&entry.path),
                            &mut proof_budget,
                        )
                        .ok()
                        .flatten()
                    } else {
                        None
                    };
                    (entry, physical, expected, version)
                })
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();
        let now = Utc::now();
        // The checkouts that gained a first change to judge once git has
        // settled in them.
        let mut settling: Vec<PathBuf> = Vec::new();
        {
            // One hold of the lock and one durable transaction for the
            // batch: a branch switch is thousands of paths, and a sync per
            // path, each taking the coordination lock, held every claim and
            // release up for as long as it lasted.
            let mut state = lock(&self.state);
            let mut changes = Vec::with_capacity(observed.len());
            let mut physicals = Vec::with_capacity(observed.len());
            let mut proofs = Vec::with_capacity(observed.len());
            for (
                Observed {
                    checkout,
                    path,
                    kind,
                },
                physical,
                expected,
                version,
            ) in observed
            {
                let by = physical
                    .as_deref()
                    .map_or(Attribution::External, |path| state.attribute(path));
                proofs.push((
                    checkout.dir.join(&path),
                    if by == expected { version } else { None },
                ));
                changes.push(Change {
                    seq: 0,
                    project: checkout.project.clone(),
                    checkout: Some(checkout.dir.clone()),
                    worktree: checkout.worktree.clone(),
                    path,
                    kind,
                    at: now,
                    by,
                    head: heads.get(&checkout.dir).cloned().flatten(),
                });
                physicals.push(physical);
            }
            let seqs = if changes.is_empty() {
                Some(Vec::new())
            } else {
                state.store_op("change", |store| store.append_changes(&changes))
            };
            if let Some(seqs) = seqs {
                for (((mut change, physical), seq), (path, version)) in
                    changes.into_iter().zip(physicals).zip(seqs).zip(proofs)
                {
                    change.seq = seq;
                    state.content_proofs.record(path, &change.by, version, now);
                    // The strongest "working" signal there is: a file changed
                    // under a lease this agent holds. Recording it keeps derived
                    // activity honest for a runtime with no hooks at all.
                    if let Attribution::Agent { agent, .. } = &change.by {
                        let agent = agent.clone();
                        state.registry.touch(&agent, now);
                    }
                    state.warn_readers(&change, physical.as_deref());
                    // A second checkout on this path means two agents are in the
                    // same work: give them a room. Only for a change somebody
                    // made, though. A pull, a merge or a branch switch writes
                    // every file of the work it brings in, and each of them
                    // equals the checkout's HEAD once git is done: git put it
                    // there, and whoever made that change made it elsewhere.
                    match change
                        .checkout
                        .as_ref()
                        .and_then(|dir| differing.get(dir).zip(Some(dir)))
                    {
                        // Git cannot say for this folder: somebody's, now.
                        None | Some((None, _)) => state.note_contested(&change),
                        // Equal to HEAD already: brought by git, and done.
                        Some((Some(differs), _)) if !differs.contains(&change.path) => {}
                        // It differs now. Whether it still does once git has
                        // finished with the checkout is what decides.
                        Some((Some(_), dir)) => {
                            let waiting = state.contest_waiting.entry(dir.clone()).or_default();
                            if waiting.is_empty() {
                                settling.push(dir.clone());
                            }
                            if waiting.len() < CONTEST_WAITING {
                                waiting.push(change.clone());
                            }
                        }
                    }
                    debug!(project = %change.project.short(), path = %change.path.display(), kind = %change.kind, "file changed");
                    let _ = state
                        .events
                        .send(Event::new(EventKind::FileChanged { change }, now));
                }
            }
        }
        for dir in settling {
            tokio::spawn(settle_contest(Arc::downgrade(self), dir));
        }
        // A repository's refs are shared between its worktrees: a commit
        // made in a linked one writes `refs/heads/<branch>` under the
        // *main* checkout's git directory. So the touched checkout says
        // which repository moved, not which checkout of it — and every
        // checkout of that repository has to be looked at.
        let touched: HashSet<ProjectId> = vcs_touched.into_iter().map(|c| c.project).collect();
        if touched.is_empty() {
            return;
        }
        for checkout in self
            .watch_targets()
            .into_iter()
            .filter(|c| touched.contains(&c.project))
        {
            // Both, and in this order: the agents in that checkout get
            // their branch and head refreshed, and the checkout itself
            // is journaled even when no agent lives in it. A worktree
            // nobody registered is where most of the work happens.
            self.refresh_vcs(Some(&checkout.dir)).await;
            self.note_checkout(&checkout).await;
        }
    }

    pub(super) async fn changes(
        &self,
        project: &str,
        since_seq: Option<u64>,
        path: Option<String>,
        agent: Option<String>,
        limit: usize,
    ) -> Response {
        let project = match self.resolve_project(project).await {
            Ok(id) => id,
            Err(response) => return *response,
        };
        let agent = match agent.map(|reference| self.resolve(&reference)).transpose() {
            Ok(agent) => agent,
            Err(response) => return *response,
        };
        // An absolute path is made relative to the checkout containing it.
        let path = match path {
            Some(raw) => Some(self.relative_path(raw).await),
            None => None,
        };
        let query = ChangesQuery {
            project,
            since_seq,
            path,
            agent,
            limit: limit.clamp(1, 10_000),
            after: None,
            before_seq: None,
        };
        match lock(&self.state).store.changes(&query) {
            Ok(changes) => Response::Changes { changes },
            Err(err) => Response::error(
                ErrorCode::StorageUnavailable,
                format!("ledger query failed: {err}"),
            ),
        }
    }
}
