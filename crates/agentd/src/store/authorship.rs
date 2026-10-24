//! What agents have committed, apart from the journal: one row per commit
//! the daemon made for an agent or a worktree's hook reported, never
//! pruned with the journal; and the worktrees whose hook may report, each
//! with the hash of its witness token and who it was created for.
use super::*;
use agentdocker_core::gate::Authorship;

/// A worktree the daemon created and gave a post-commit hook: where it is,
/// whose project, who it was made for, and the hash of the token only its
/// hook's reports carry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WitnessedWorktree {
    pub path: PathBuf,
    pub project: ProjectId,
    pub agent: AgentId,
    pub token_hash: String,
    pub created_at: DateTime<Utc>,
}

impl Store {
    /// Keep `authorship` unless that commit is already kept on that branch,
    /// and with it, only when it is new, an `authorship_recorded` event at
    /// sequence `first`: the events written, none for a commit already kept.
    pub(crate) fn record_authorship(
        &self,
        authorship: &Authorship,
        first: u64,
    ) -> Result<(Vec<Event>, bool)> {
        let tx = write_transaction(&self.conn)?;
        let added = self.conn.execute(
            "INSERT OR IGNORE INTO authorship (project, commit_id, branch, agent, witness, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                authorship.project.as_str(),
                authorship.commit,
                authorship.branch,
                authorship.agent.as_str(),
                authorship.witness.as_str(),
                authorship.at.to_rfc3339(),
            ],
        )? == 1;
        let mut events = Vec::new();
        if added {
            let mut event = Event::new(
                EventKind::AuthorshipRecorded {
                    project: authorship.project.clone(),
                    agent: authorship.agent.clone(),
                    branch: authorship.branch.clone(),
                    commit: authorship.commit.clone(),
                    witness: authorship.witness,
                },
                authorship.at,
            );
            event.seq = first;
            self.append_event(&event)?;
            events.push(event);
        }
        tx.commit()?;
        Ok((events, added))
    }

    /// Whether any of `agents` made a commit on `branch` (a full ref) in
    /// `project`.
    pub(crate) fn authored(
        &self,
        project: &ProjectId,
        branch: &str,
        agents: &[AgentId],
    ) -> Result<bool> {
        let mut statement = self.conn.prepare_cached(
            "SELECT EXISTS(SELECT 1 FROM authorship WHERE project = ?1 AND branch = ?2 AND agent = ?3)",
        )?;
        for agent in agents {
            let found: bool = statement
                .query_row(params![project.as_str(), branch, agent.as_str()], |row| {
                    row.get(0)
                })?;
            if found {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// What `agent` is kept as having committed, oldest first.
    #[cfg(test)]
    pub(crate) fn authorship_of(&self, agent: &AgentId) -> Result<Vec<Authorship>> {
        let mut statement = self.conn.prepare(
            "SELECT project, commit_id, branch, witness, at FROM authorship
             WHERE agent = ?1 ORDER BY at, commit_id",
        )?;
        let rows = statement
            .query_map(params![agent.as_str()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter()
            .map(|(project, commit, branch, witness, at)| {
                Ok(Authorship {
                    project: ProjectId::from(project.as_str()),
                    agent: agent.clone(),
                    branch,
                    commit,
                    witness: agentdocker_core::gate::Witness::parse(&witness)
                        .with_context(|| format!("unknown authorship witness {witness:?}"))?,
                    at: DateTime::parse_from_rfc3339(&at)?.with_timezone(&Utc),
                })
            })
            .collect()
    }

    /// Keep a worktree whose hook may report, replacing what was kept for
    /// the same path, with its `worktree_witnessed` event.
    pub(crate) fn witness_worktree(
        &self,
        worktree: &WitnessedWorktree,
        event: &Event,
    ) -> Result<()> {
        let tx = write_transaction(&self.conn)?;
        self.conn.execute(
            "INSERT OR REPLACE INTO witnessed_worktrees (path, project, agent, token_hash, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                worktree.path.to_string_lossy(),
                worktree.project.as_str(),
                worktree.agent.as_str(),
                worktree.token_hash,
                worktree.created_at.to_rfc3339(),
            ],
        )?;
        self.append_event(event)?;
        tx.commit()?;
        Ok(())
    }

    /// The worktree kept for `path`, if its hook may report.
    pub(crate) fn witnessed_worktree(&self, path: &Path) -> Result<Option<WitnessedWorktree>> {
        let row = self
            .conn
            .query_row(
                "SELECT project, agent, token_hash, created_at FROM witnessed_worktrees WHERE path = ?1",
                params![path.to_string_lossy()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?;
        row.map(|(project, agent, token_hash, created_at)| {
            Ok(WitnessedWorktree {
                path: path.to_path_buf(),
                project: ProjectId::from(project.as_str()),
                agent: AgentId::from(agent.as_str()),
                token_hash,
                created_at: DateTime::parse_from_rfc3339(&created_at)?.with_timezone(&Utc),
            })
        })
        .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::gate::Witness;

    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn authored(agent: &str, branch: &str, commit: &str, witness: Witness) -> Authorship {
        Authorship {
            project: ProjectId::from("p"),
            agent: AgentId::from(agent),
            branch: branch.to_owned(),
            commit: commit.to_owned(),
            witness,
            at: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
        }
    }

    /// A commit is kept once per branch, with one event; asking is by the
    /// full ref and by any of an agent's ids.
    #[test]
    fn a_commit_is_kept_once_and_asked_for_by_its_full_ref() {
        let store = Store::in_memory().unwrap();
        let first = authored("a", "refs/heads/feature/mine", A, Witness::Hook);
        let (events, added) = store.record_authorship(&first, 7).unwrap();
        assert!(added);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].seq, 7);
        assert_eq!(store.recent_events(1).unwrap()[0].seq, 7);
        // The same commit again, by either way of seeing it: nothing new.
        let again = authored("a", "refs/heads/feature/mine", A, Witness::Commit);
        let (events, added) = store.record_authorship(&again, 8).unwrap();
        assert!(!added && events.is_empty());
        assert_eq!(store.max_event_seq().unwrap(), 7);
        store
            .record_authorship(
                &authored("a", "refs/heads/feature/mine", B, Witness::Commit),
                8,
            )
            .unwrap();
        assert_eq!(store.authorship_of(&AgentId::from("a")).unwrap().len(), 2);

        let p = ProjectId::from("p");
        let a = [AgentId::from("a")];
        assert!(store.authored(&p, "refs/heads/feature/mine", &a).unwrap());
        // A short name or a tag's ref is not the branch.
        assert!(!store.authored(&p, "feature/mine", &a).unwrap());
        assert!(!store.authored(&p, "refs/tags/feature/mine", &a).unwrap());
        assert!(
            !store
                .authored(&ProjectId::from("q"), "refs/heads/feature/mine", &a)
                .unwrap()
        );
        let both = [AgentId::from("retired"), AgentId::from("a")];
        assert!(
            store
                .authored(&p, "refs/heads/feature/mine", &both)
                .unwrap()
        );
        assert!(
            !store
                .authored(&p, "refs/heads/feature/mine", &[AgentId::from("b")])
                .unwrap()
        );
    }

    /// A forgotten record's authorship goes with it, its former ids' too,
    /// and so do the worktrees made for it; nobody else's.
    #[test]
    fn forgetting_an_agent_forgets_what_it_committed() {
        let store = Store::in_memory().unwrap();
        for (agent, branch, commit) in [
            ("a", "refs/heads/x", A),
            ("old", "refs/heads/x", B),
            ("b", "refs/heads/y", A),
        ] {
            store
                .record_authorship(&authored(agent, branch, commit, Witness::Hook), 0)
                .unwrap();
        }
        store
            .put_document(
                "identity_alias",
                "old",
                &serde_json::json!({"retired": "old", "canonical": "a"}),
            )
            .unwrap();
        for (path, agent) in [("/w/a", "a"), ("/w/b", "b")] {
            let worktree = WitnessedWorktree {
                path: PathBuf::from(path),
                project: ProjectId::from("p"),
                agent: AgentId::from(agent),
                token_hash: "h".into(),
                created_at: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            };
            let event = Event::new(
                EventKind::WorktreeWitnessed {
                    agent: worktree.agent.clone(),
                    path: worktree.path.clone(),
                },
                worktree.created_at,
            );
            store.witness_worktree(&worktree, &event).unwrap();
        }
        let a = AgentId::from("a");
        let mut removed = Event::new(EventKind::AgentRemoved { agent: a.clone() }, Utc::now());
        removed.seq = store.max_event_seq().unwrap() + 1;
        store.delete_agent(&a, &removed).unwrap();
        assert!(store.authorship_of(&a).unwrap().is_empty());
        assert!(
            store
                .authorship_of(&AgentId::from("old"))
                .unwrap()
                .is_empty()
        );
        assert_eq!(store.authorship_of(&AgentId::from("b")).unwrap().len(), 1);
        assert_eq!(store.witnessed_worktree(Path::new("/w/a")).unwrap(), None);
        assert!(
            store
                .witnessed_worktree(Path::new("/w/b"))
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn a_witnessed_worktree_is_replaced_by_its_path() {
        let store = Store::in_memory().unwrap();
        let path = PathBuf::from("/w/one");
        assert_eq!(store.witnessed_worktree(&path).unwrap(), None);
        let made = |agent: &str, hash: &str, seq: u64| {
            let worktree = WitnessedWorktree {
                path: path.clone(),
                project: ProjectId::from("p"),
                agent: AgentId::from(agent),
                token_hash: hash.to_owned(),
                created_at: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            };
            let mut event = Event::new(
                EventKind::WorktreeWitnessed {
                    agent: worktree.agent.clone(),
                    path: path.clone(),
                },
                worktree.created_at,
            );
            event.seq = seq;
            store.witness_worktree(&worktree, &event).unwrap();
            worktree
        };
        made("a", "one", 1);
        let second = made("b", "two", 2);
        assert_eq!(store.witnessed_worktree(&path).unwrap(), Some(second));
        assert_eq!(
            store.witnessed_worktree(Path::new("/w/other")).unwrap(),
            None
        );
    }
}
