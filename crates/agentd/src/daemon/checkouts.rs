use super::*;

impl Daemon {
    pub(super) async fn vcs_for(workdir: Option<PathBuf>) -> Option<VcsState> {
        let dir = workdir?;
        tokio::task::spawn_blocking(move || vcs::state(&dir))
            .await
            .ok()
            .flatten()
    }

    /// Read every live agent's checkout and record what moved. Called on a
    /// timer, so branch and head stay right for agents that never report —
    /// adopted ones, and anything started with `run`.
    pub async fn refresh_vcs(&self, dir: Option<&Path>) {
        let targets: Vec<(AgentId, PathBuf, Option<VcsState>)> = lock(&self.state)
            .registry
            .live()
            .filter(|a| dir.is_none_or(|dir| a.project.as_ref().is_some_and(|p| p.dir() == dir)))
            .filter_map(|a| {
                a.spec
                    .workdir
                    .clone()
                    .map(|workdir| (a.id.clone(), workdir, a.vcs.clone()))
            })
            .collect();
        if targets.is_empty() {
            return;
        }
        // A moved HEAD is named while we are off the lock anyway.
        let observed = tokio::task::spawn_blocking(move || {
            targets
                .into_iter()
                .filter_map(|(id, dir, old)| {
                    let state = vcs::state(&dir)?;
                    let moved = state.head.is_some()
                        && old.as_ref().and_then(|o| o.head.clone()) != state.head;
                    let subject = if moved {
                        state
                            .head
                            .as_deref()
                            .and_then(|sha| vcs::subject(&dir, sha, SUBJECT_TIMEOUT))
                    } else {
                        None
                    };
                    Some((id, state, old, moved, subject))
                })
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();
        for (id, state, old, moved, subject) in observed {
            self.apply_vcs(&id, state.clone());
            if moved {
                lock(&self.state).note_head_move(&id, old.as_ref(), &state, subject);
            }
        }
    }

    /// The checkouts the watcher should cover: every distinct directory a
    /// live agent works in whose project is a repository or an Agentfile
    /// root. Plain directories are left alone — a recursive watch on a
    /// home directory is what inotify cannot afford.
    /// Every directory the watcher should be watching.
    ///
    /// Not merely "where each agent is". A project is the repository,
    /// and the repository is every worktree of it: an agent can commit
    /// in a checkout nobody registered — its own `--isolate` worktree,
    /// or one a person made by hand — and if that directory is not
    /// watched, the commit reaches neither the journal nor the ledger,
    /// and `overlap` answers "nothing collides" from a single checkout.
    /// That last one is the worst of the three, because it is a
    /// confident wrong answer to the question the feature exists for.
    pub fn watch_targets(&self) -> Vec<Checkout> {
        let mut seen: HashSet<PathBuf> = HashSet::new();
        let state = lock(&self.state);
        let mut targets: Vec<Checkout> = state
            .registry
            .live()
            .filter_map(|a| a.project.as_ref())
            .filter(|p| matches!(p.source, ProjectSource::Git | ProjectSource::Agentfile))
            .filter(|p| seen.insert(p.dir().to_path_buf()))
            .map(|p| Checkout {
                dir: p.dir().to_path_buf(),
                project: p.id(),
                worktree: p.worktree.clone(),
            })
            .collect();
        // Then the rest of each project's checkouts, from the cache the
        // VCS sweep keeps. Nested ones are skipped: a worktree inside a
        // watched directory is already covered, and watching it twice
        // would record every change in it twice.
        let known: Vec<(ProjectId, Vec<PathBuf>)> = state
            .project_checkouts
            .iter()
            .map(|(id, dirs)| (id.clone(), dirs.clone()))
            .collect();
        drop(state);
        for (project, dirs) in known {
            if !targets.iter().any(|c| c.project == project) {
                continue; // nothing live in it; nothing to watch for
            }
            let mut added = 0usize;
            let mut skipped = 0usize;
            for dir in dirs {
                if seen.contains(&dir) || seen.iter().any(|w| dir.starts_with(w)) {
                    continue;
                }
                if added >= MAX_EXTRA_CHECKOUTS {
                    skipped += 1;
                    continue;
                }
                seen.insert(dir.clone());
                added += 1;
                targets.push(Checkout {
                    dir: dir.clone(),
                    project: project.clone(),
                    // Anything that is not where an agent lives is a
                    // linked worktree of the same repository.
                    worktree: Some(dir),
                });
            }
            if skipped > 0 {
                // Said out loud rather than silently dropped: an
                // unwatched checkout is a hole in the ledger, and a hole
                // nobody is told about is the failure mode this whole
                // change exists to remove.
                self.emit(EventKind::WatcherGap {
                    reason: format!(
                        "{skipped} more checkout{} of project {} than the {MAX_EXTRA_CHECKOUTS} \
                         this watches; changes in them are not recorded",
                        if skipped == 1 { "" } else { "s" },
                        project.short()
                    ),
                });
            }
        }
        targets
    }

    /// Read one checkout's HEAD and journal it if it moved.
    ///
    /// The agent-driven sweep only looks where agents are. This looks at
    /// a checkout as a checkout, so a commit in a worktree nobody
    /// registered still reaches the journal.
    pub(crate) async fn note_checkout(&self, checkout: &Checkout) {
        let dir = checkout.dir.clone();
        let known = lock(&self.state).last_head.get(&dir).cloned();
        let dir_for_read = dir.clone();
        let known_for_read = known.clone();
        let observed = tokio::task::spawn_blocking(move || {
            let state = vcs::state(&dir_for_read)?;
            // Naming the commit runs `git log`, so it is only done when
            // there is something new to name. This is called for every
            // checkout of every project on every sweep.
            let subject = match &state.head {
                Some(head) if Some(head) != known_for_read.as_ref() => {
                    vcs::subject(&dir_for_read, head, SUBJECT_TIMEOUT)
                }
                _ => None,
            };
            Some((state, subject))
        })
        .await
        .ok()
        .flatten();
        let Some((state, subject)) = observed else {
            return;
        };
        // A checkout seen for the first time is recorded, not announced:
        // its history did not happen while we were watching. The branch
        // goes down with the head, or the next commit here would read as
        // a switch onto a branch it was already on.
        let Some(before) = known else {
            if let Some(head) = state.head {
                let mut daemon = lock(&self.state);
                daemon.last_head.insert(dir.clone(), head);
                daemon.last_branch.insert(dir, state.branch);
            }
            return;
        };
        if state.head.as_deref() == Some(before.as_str()) {
            return;
        }
        // Only the head is known to have been different; the branch it
        // was on is remembered per checkout, and `note_checkout_move`
        // falls back to that.
        let old = VcsState {
            head: Some(before),
            branch: None,
            ..state.clone()
        };
        lock(&self.state).note_checkout_move(
            checkout.project.clone(),
            dir,
            checkout.worktree.clone(),
            Some(&old),
            &state,
            subject,
        );
    }

    /// Re-read where each live project's checkouts are.
    ///
    /// Runs git, so it happens off the lock and on the same five-second
    /// tick as the VCS sweep. A project whose enumeration fails keeps
    /// the checkouts it had: an empty answer means "ask again", never
    /// "there is only one", and treating it as the latter would quietly
    /// stop watching real work.
    pub async fn refresh_project_checkouts(&self) {
        let roots: Vec<(ProjectId, PathBuf)> = {
            let mut seen: HashSet<ProjectId> = HashSet::new();
            lock(&self.state)
                .registry
                .live()
                .filter_map(|a| a.project.as_ref())
                .filter(|p| matches!(p.source, ProjectSource::Git | ProjectSource::Agentfile))
                .filter(|p| seen.insert(p.id()))
                .map(|p| (p.id(), p.dir().to_path_buf()))
                .collect()
        };
        if roots.is_empty() {
            return;
        }
        let found = tokio::task::spawn_blocking(move || {
            roots
                .into_iter()
                .map(|(id, dir)| {
                    let dirs = vcs::worktrees(&dir, SUBJECT_TIMEOUT);
                    (id, dirs)
                })
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();
        {
            let mut state = lock(&self.state);
            for (id, dirs) in found {
                if dirs.is_empty() {
                    continue;
                }
                state.project_checkouts.insert(id, dirs);
            }
        }
        // Take each checkout's head now rather than waiting for a
        // filesystem event. Two reasons: a checkout first seen at the
        // moment of a commit would have that commit read as its
        // starting point and swallowed, and this is the polling net
        // under the watcher for the days it misses something.
        for checkout in self.watch_targets() {
            self.note_checkout(&checkout).await;
        }
    }

    /// Let the watcher hand us its flush channel.
    pub fn set_watcher_flush(&self, sender: mpsc::Sender<oneshot::Sender<()>>) {
        *lock(&self.watcher_flush) = Some(sender);
    }

    /// The watcher now records every change in `dir`, or no longer does.
    pub fn ledger_covers(&self, dir: &Path, covered: bool) {
        let mut state = lock(&self.state);
        if covered {
            state.ledger_since.insert(dir.to_path_buf(), Utc::now());
        } else {
            state.ledger_since.remove(dir);
        }
    }

    /// The watcher lost events: no checkout's ledger accounts for anything
    /// before now.
    pub fn ledger_gap(&self) {
        let now = Utc::now();
        for since in lock(&self.state).ledger_since.values_mut() {
            *since = now;
        }
    }

    /// A watcher is about to be spawned: registrations arriving before it
    /// has handed over its channel wait for it, within the same bound,
    /// instead of taking startup for no watcher at all.
    pub fn expect_watcher(&self) {
        self.watcher_attach.send_replace(WatcherLink::Starting);
        self.emit(EventKind::WatcherStarting);
    }

    /// Let the watcher hand us its reconcile channel.
    pub(crate) fn set_watcher_attach(&self, sender: mpsc::Sender<WatcherAttachment>) {
        self.watcher_attach.send_replace(WatcherLink::On(sender));
        self.emit(EventKind::WatcherStarted);
    }

    /// The watcher could not start after all; stop waiting for it.
    pub fn watcher_off(&self, reason: String) {
        self.watcher_attach
            .send_replace(WatcherLink::Unavailable(reason.clone()));
        self.emit(EventKind::WatcherUnavailable { reason });
    }
}

impl Daemon {
    /// Have the watcher cover every checkout a live agent works in, now:
    /// registration awaits this so the agent's first edit is never made in
    /// the gap before the next reconcile tick. Bounded, and a no-op when no
    /// watcher runs.
    pub(super) async fn ensure_watched(
        &self,
        record: &AgentRecord,
        within: std::time::Duration,
    ) -> Result<(), String> {
        let checkout = record
            .project
            .as_ref()
            .ok_or("agent has no watchable checkout")?
            .dir()
            .to_path_buf();
        tokio::time::timeout(within, async {
            let mut link = self.watcher_attach.subscribe();
            let sender = match link
                .wait_for(|link| !matches!(link, WatcherLink::Starting))
                .await
            {
                Ok(link) => match &*link {
                    WatcherLink::On(sender) => sender.clone(),
                    WatcherLink::Off => return Ok(()),
                    WatcherLink::Unavailable(reason) => return Err(reason.clone()),
                    WatcherLink::Starting => unreachable!(),
                },
                Err(_) => return Err("watcher stopped during startup".into()),
            };
            let (ack, done) = oneshot::channel();
            sender
                .send(WatcherAttachment { checkout, ack })
                .await
                .map_err(|_| "watcher stopped before attachment".to_string())?;
            done.await
                .map_err(|_| "watcher stopped before confirming coverage".to_string())?
        })
        .await
        .map_err(|_| {
            format!(
                "watcher did not confirm checkout coverage within {} ms",
                within.as_millis()
            )
        })?
    }

    /// Only path leases need pending filesystem observations for their summary.
    /// Select under the state lock, then await the watcher with no guard held.
    pub(super) async fn flush_release_watcher(&self, reference: &str, lease: Option<&LeaseId>) {
        let needs_flush = {
            let state = lock(&self.state);
            state
                .registry
                .resolve(reference)
                .ok()
                .is_some_and(|holder| {
                    state.leases.all().into_iter().any(|held| {
                        held.holder == holder
                            && held.resource.kind() == "path"
                            && lease.is_none_or(|id| held.id == *id)
                    })
                })
        };
        if !needs_flush {
            return;
        }
        self.flush_ledger().await;
    }

    /// Ask the watcher to record what it has pending and wait for it to
    /// say so. False when there is no watcher, or it did not confirm in
    /// time: changes made before this call may then be missing from the
    /// ledger.
    pub(super) async fn flush_ledger(&self) -> bool {
        let sender = lock(&self.watcher_flush).clone();
        let Some(sender) = sender else {
            return false;
        };
        // Bound both queue admission and acknowledgement: a full channel must
        // never leave a caller waiting indefinitely for the watcher.
        tokio::time::timeout(WATCHER_FLUSH_TIMEOUT, async {
            let (ack, done) = oneshot::channel();
            sender.send(ack).await.is_ok() && done.await.is_ok()
        })
        .await
        .unwrap_or(false)
    }

    /// An absolute path made relative to the checkout containing it;
    /// anything else passes through.
    pub(super) async fn relative_path(&self, raw: String) -> String {
        if !Path::new(&raw).is_absolute() {
            return raw;
        }
        let given = PathBuf::from(&raw);
        let (absolute, discovery_dir) = tokio::task::spawn_blocking(move || {
            let absolute = project::canonical(&given);
            let discovery_dir = if absolute.is_dir() {
                Some(absolute.clone())
            } else {
                absolute.parent().map(Path::to_path_buf)
            };
            (absolute, discovery_dir)
        })
        .await
        .unwrap_or_else(|_| (PathBuf::from(&raw), None));
        match self.project_for(discovery_dir, false).await {
            Some(found) => match absolute.strip_prefix(found.dir()) {
                Ok(relative) => relative.to_string_lossy().into_owned(),
                Err(_) => raw,
            },
            None => raw,
        }
    }
}
