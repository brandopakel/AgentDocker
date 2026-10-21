use super::*;

impl Daemon {
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn journal(
        &self,
        project: &str,
        since_seq: Option<u64>,
        until_seq: Option<u64>,
        agent: Option<String>,
        branch: Option<String>,
        kind: Option<String>,
        path: Option<String>,
        grep: Option<String>,
        limit: usize,
        digest: Option<DigestRequest>,
    ) -> Response {
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
    pub async fn record_fs_changes(&self, observed: Vec<Observed>, vcs_touched: Vec<Checkout>) {
        let dirs: Vec<PathBuf> = {
            let mut dirs: Vec<PathBuf> = observed.iter().map(|o| o.checkout.dir.clone()).collect();
            dirs.sort();
            dirs.dedup();
            dirs
        };
        let heads: HashMap<PathBuf, Option<String>> = tokio::task::spawn_blocking(move || {
            dirs.into_iter()
                .map(|dir| {
                    let head = vcs::state(&dir).and_then(|s| s.head);
                    (dir, head)
                })
                .collect()
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
                    // same work: give them a room.
                    state.note_contested(&change);
                    debug!(project = %change.project.short(), path = %change.path.display(), kind = %change.kind, "file changed");
                    let _ = state
                        .events
                        .send(Event::new(EventKind::FileChanged { change }, now));
                }
            }
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
