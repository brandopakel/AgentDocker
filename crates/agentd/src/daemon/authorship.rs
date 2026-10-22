//! What each agent has committed to, kept apart from the journal (which
//! pruning and retention may empty) so that no agent's word is taken on a
//! branch it wrote, wherever its checkout stands now.
//!
//! Two sources, both exact about the commit: a commit the daemon makes for
//! an agent (`commit`), recorded as soon as git has made it; and every
//! `git commit` a checkout's own HEAD reflog records, replayed entry by
//! entry from where the last reading stopped and given to every agent the
//! daemon knows works in that checkout. The reading is a durable checkpoint
//! per checkout, moved in the same transaction as the authorship it found.
use super::*;
use agentdocker_core::gate::{self, Authorship};
use agentdocker_host::vcs::reflog::{self, ReflogCheckpoint, Replayed};

/// One document per project, agent and branch.
pub(super) const AUTHORSHIP: &str = "authorship";
/// One document per checkout, by its path: how far its HEAD reflog is read.
const CHECKPOINT: &str = "reflog_checkpoint";
/// Readings a verdict makes of one checkout while sweeps move its
/// checkpoint under it, before it gives up.
const VERDICT_READINGS: usize = 3;
/// Daemon-made commits remembered for the replay to pass over; past this
/// many (a reflog that is never written), all are forgotten.
const DAEMON_COMMITS: usize = 4096;

/// Why a checkout's reflog was not brought up to date.
pub(super) enum Unreplayed {
    /// Git's record could not be read; the checkpoint stays where it was.
    Unreadable(String),
    /// The store could not say or write.
    Storage(Box<Response>),
}

fn json<T: serde::Serialize>(
    kind: &'static str,
    id: String,
    value: &T,
) -> (&'static str, String, String) {
    (
        kind,
        id,
        serde_json::to_string(value).expect("authorship documents serialize"),
    )
}

fn storage(state: &State, what: &str) -> Box<Response> {
    Box::new(
        state
            .storage_failure()
            .unwrap_or_else(|| Response::error(ErrorCode::StorageUnavailable, what.to_owned())),
    )
}

impl State {
    /// Where `dir`'s reflog was last read, if it ever was.
    fn reflog_checkpoint(&mut self, dir: &Path) -> Result<Option<ReflogCheckpoint>, Box<Response>> {
        if let Some(checkpoint) = self.reflogs.get(dir) {
            return Ok(Some(checkpoint.clone()));
        }
        let key = dir.to_string_lossy().into_owned();
        let stored = self
            .store_read("reflog checkpoint", |store| {
                store.document::<ReflogCheckpoint>(CHECKPOINT, &key)
            })
            .ok_or_else(|| storage(self, "a checkout's reading could not be read"))?;
        if let Some(checkpoint) = &stored {
            self.reflogs.insert(dir.to_path_buf(), checkpoint.clone());
        }
        Ok(stored)
    }

    /// The live agents the daemon knows work in the checkout `dir`: those
    /// registered there, the one it created the worktree for, and the
    /// holder of an exclusive `path:` lease over it. Never the person.
    fn workers_of(&self, dir: &Path) -> Vec<AgentId> {
        let agent = |id: &AgentId| {
            self.registry
                .get(id)
                .filter(|r| r.status.is_live() && !humans::is_human(r))
                .map(|r| r.id.clone())
        };
        let mut ids: Vec<AgentId> = self
            .registry
            .live()
            .filter(|r| !humans::is_human(r) && r.project.as_ref().is_some_and(|p| p.dir() == dir))
            .map(|r| r.id.clone())
            .collect();
        ids.extend(self.worktree_creators.get(dir).and_then(agent));
        if let Attribution::Agent { agent: holder, .. } = self.attribute(dir) {
            ids.extend(agent(&holder));
        }
        let mut seen = HashSet::new();
        ids.retain(|id| seen.insert(id.clone()));
        ids
    }

    /// The checkouts the daemon knows `agent` works in, the same rule as
    /// [`State::workers_of`] read the other way, with their project.
    fn checkouts_of(&self, agent: &AgentId) -> Vec<(ProjectId, PathBuf)> {
        let Some(project) = self.registry.get(agent).and_then(|r| r.project.clone()) else {
            return Vec::new();
        };
        let id = project.id();
        let mut dirs = vec![project.dir().to_path_buf()];
        dirs.extend(
            self.worktree_creators
                .iter()
                .filter(|(_, creator)| *creator == agent)
                .map(|(dir, _)| dir.clone()),
        );
        if let Some(known) = self.project_checkouts.get(&id) {
            dirs.extend(known.iter().filter(|dir| {
                matches!(self.attribute(dir), Attribution::Agent { agent: holder, .. } if &holder == agent)
            }).cloned());
        }
        let mut seen = HashSet::new();
        dirs.retain(|dir| seen.insert(dir.clone()));
        dirs.into_iter().map(|dir| (id.clone(), dir)).collect()
    }

    /// The authorship of `agent` on `branch` in `project`, as stored.
    fn authorship(
        &mut self,
        project: &ProjectId,
        agent: &AgentId,
        branch: &str,
    ) -> Result<Option<Authorship>, Box<Response>> {
        let key = gate::authorship_id(project, agent, branch);
        let stored = self
            .store_read("authorship", |store| {
                store.document::<Authorship>(AUTHORSHIP, &key)
            })
            .ok_or_else(|| storage(self, "what an agent has committed to could not be read"))?;
        Ok(stored.filter(|a| a.names(project, agent, branch)))
    }

    /// Whether `agent`, or a record folded into it, has committed to
    /// `branch` in `project`.
    pub(super) fn has_committed_to(
        &mut self,
        project: &ProjectId,
        agent: &AgentId,
        branch: &str,
    ) -> Result<bool, Box<Response>> {
        for id in self.registry.identity_ids(agent) {
            if self.authorship(project, &id, branch)?.is_some() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Whether the daemon is making a commit for `agent` right now.
    pub(super) fn is_committing(&self, agent: &AgentId) -> bool {
        self.committing_agents.get(agent).is_some_and(|n| *n > 0)
    }

    /// Record that `agent` made `commit` on `branch` through the daemon,
    /// as soon as git has made it: its authorship, announced, and the
    /// commit remembered so the reflog replay does not give it to anybody
    /// else working in the checkout. The person, a detached HEAD or an
    /// agent in no project records nothing. A refusal when it could not be
    /// written.
    pub(super) fn record_commit(
        &mut self,
        agent: &AgentId,
        branch: Option<&str>,
        commit: &str,
    ) -> Option<Response> {
        let branch = branch.filter(|b| !b.is_empty())?;
        let record = self.registry.get(agent).filter(|r| !humans::is_human(r))?;
        let project = record.project.clone()?;
        let id = project.id();
        let previous = match self.authorship(&id, agent, branch) {
            Ok(previous) => previous,
            Err(response) => return Some(*response),
        };
        let authored = Authorship::after(previous.as_ref(), &id, agent, branch, commit, Utc::now());
        let kind = EventKind::AuthorshipRecorded {
            project: id.clone(),
            agent: agent.clone(),
            branch: branch.to_owned(),
            commit: commit.to_owned(),
        };
        let documents = vec![json(
            AUTHORSHIP,
            gate::authorship_id(&id, agent, branch),
            &authored,
        )];
        let now = Utc::now();
        let committed = self.commit(
            "authorship",
            vec![Event::new(kind, now)],
            |store, events| store.documents_with_events(&documents, events),
            |_, _, ()| {},
        );
        if committed != Persisted::Committed {
            return Some(self.write_failure().unwrap_or_else(|| {
                Response::error(ErrorCode::Internal, "the commit's author was not recorded")
            }));
        }
        if self.daemon_commits.len() >= DAEMON_COMMITS {
            self.daemon_commits.clear();
        }
        self.daemon_commits
            .insert((project.dir().to_path_buf(), commit.to_owned()));
        None
    }

    /// Apply a replay of `dir`'s reflog read from `from`, if `from` is still
    /// where its reading stands: each commit it found — other than one the
    /// daemon made and recorded itself — is the authorship of every agent
    /// working in the checkout now, its newest commit on each branch the
    /// one kept, and the checkpoint moves with them in one transaction.
    /// `Ok(false)` when another reading moved the checkpoint meanwhile.
    fn apply_replay(
        &mut self,
        project: &ProjectId,
        dir: &Path,
        from: Option<&ReflogCheckpoint>,
        replayed: Replayed,
    ) -> Result<bool, Box<Response>> {
        if self.reflog_checkpoint(dir)?.as_ref() != from {
            return Ok(false);
        }
        let Replayed {
            checkpoint,
            commits,
        } = replayed;
        let unchanged = match from {
            Some(from) => *from == checkpoint,
            // Nothing there to read from: not a checkout, or nothing yet.
            None => checkpoint == ReflogCheckpoint::default(),
        };
        if unchanged && commits.is_empty() {
            return Ok(true);
        }
        let workers = self.workers_of(dir);
        let mut authored: BTreeMap<(AgentId, String), Authorship> = BTreeMap::new();
        for commit in commits {
            if self
                .daemon_commits
                .remove(&(dir.to_path_buf(), commit.commit.clone()))
            {
                continue;
            }
            let at = DateTime::from_timestamp(commit.at, 0).unwrap_or_else(Utc::now);
            for agent in &workers {
                let key = (agent.clone(), commit.branch.clone());
                let previous = match authored.get(&key) {
                    Some(record) => Some(record.clone()),
                    None => self.authorship(project, agent, &commit.branch)?,
                };
                let next = Authorship::after(
                    previous.as_ref(),
                    project,
                    agent,
                    &commit.branch,
                    &commit.commit,
                    at,
                );
                authored.insert(key, next);
            }
        }
        let path = dir.to_string_lossy().into_owned();
        let mut documents: Vec<(&'static str, String, String)> = authored
            .values()
            .map(|a| {
                json(
                    AUTHORSHIP,
                    gate::authorship_id(project, &a.agent, &a.branch),
                    a,
                )
            })
            .collect();
        documents.push(json(CHECKPOINT, path, &checkpoint));
        let mut kinds: Vec<EventKind> = authored
            .into_values()
            .map(|a| EventKind::AuthorshipRecorded {
                project: a.project,
                agent: a.agent,
                branch: a.branch,
                commit: a.last_commit,
            })
            .collect();
        kinds.push(EventKind::ReflogRead {
            checkout: dir.to_path_buf(),
            entries: checkpoint.entries,
        });
        let now = Utc::now();
        let events = kinds
            .into_iter()
            .map(|kind| Event::new(kind, now))
            .collect();
        let committed = self.commit(
            "reflog",
            events,
            |store, events| store.documents_with_events(&documents, events),
            |state, _, ()| {
                state.reflogs.insert(dir.to_path_buf(), checkpoint.clone());
            },
        );
        if committed != Persisted::Committed {
            return Err(Box::new(self.write_failure().unwrap_or_else(|| {
                Response::error(ErrorCode::Internal, "a checkout's reading was not recorded")
            })));
        }
        Ok(true)
    }
}

impl Daemon {
    /// Read `dir`'s HEAD reflog on from where it was last read, off the
    /// lock, and apply what it says. `Ok(false)` when another reading moved
    /// the checkpoint first; a failed read leaves the checkpoint as it was.
    pub(super) async fn replay_checkout(
        &self,
        project: ProjectId,
        dir: PathBuf,
    ) -> Result<bool, Unreplayed> {
        let from = lock(&self.state)
            .reflog_checkpoint(&dir)
            .map_err(Unreplayed::Storage)?;
        let (read_dir, read_from) = (dir.clone(), from.clone());
        let replayed =
            tokio::task::spawn_blocking(move || reflog::replay(&read_dir, read_from.as_ref()))
                .await
                .map_err(|_| Unreplayed::Unreadable("the read did not complete".to_owned()))?
                .map_err(Unreplayed::Unreadable)?;
        lock(&self.state)
            .apply_replay(&project, &dir, from.as_ref(), replayed)
            .map_err(Unreplayed::Storage)
    }

    /// Bring what `agent` has committed to up to date before its word is
    /// weighed: every checkout the daemon knows it works in, read to the
    /// end. A refusal when one cannot be read, or keeps moving.
    pub(super) async fn replay_agent(&self, agent: &AgentId) -> Result<(), Box<Response>> {
        let checkouts = lock(&self.state).checkouts_of(agent);
        for (project, dir) in checkouts {
            let mut readings = 0;
            loop {
                readings += 1;
                match self.replay_checkout(project.clone(), dir.clone()).await {
                    Ok(true) => break,
                    Ok(false) if readings < VERDICT_READINGS => {}
                    Ok(false) => {
                        return Err(Box::new(Response::error(
                            ErrorCode::Unavailable,
                            "the reviewer's history kept moving while it was read; give it again",
                        )));
                    }
                    Err(Unreplayed::Unreadable(reason)) => {
                        return Err(Box::new(Response::error(
                            ErrorCode::Unavailable,
                            format!("the reviewer's history could not be read: {reason}"),
                        )));
                    }
                    Err(Unreplayed::Storage(response)) => return Err(response),
                }
            }
        }
        Ok(())
    }
}
