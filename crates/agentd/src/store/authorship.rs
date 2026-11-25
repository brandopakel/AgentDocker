//! What agents have committed, apart from the journal: one row per commit
//! the daemon made for an agent or a worktree's hook reported, never
//! pruned with the journal; and the worktrees whose hook may report, each
//! with the hash of its witness token and who it was created for.
//!
//! Beside each new row, a receipt of it (`authorship_history`): its place
//! in the project's order, how its agent was named and who that agent was,
//! a project keeping its newest `RECEIPTS_PER_PROJECT`. The review gate
//! reads only the rows, which no bound prunes; a receipt outlives its
//! agent's record, which takes its rows with it when it is forgotten.
use super::*;
use agentdocker_core::gate::{Authorship, AuthorshipReceipt, Basis, RECEIPTS_PER_PROJECT};

/// A page of a ref's receipts and the project's bounds beside it.
pub(crate) struct ReceiptPage {
    pub receipts: Vec<AuthorshipReceipt>,
    pub next: Option<u64>,
    pub capture_since: Option<DateTime<Utc>>,
    pub pruned_through: Option<u64>,
    pub oldest_retained: Option<DateTime<Utc>>,
    pub seed_skipped: u64,
}

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
                    basis: authorship.basis,
                },
                authorship.at,
            );
            event.seq = first;
            self.append_event(&event)?;
            events.push(event);
            self.keep_receipt(authorship)?;
        }
        tx.commit()?;
        Ok((events, added))
    }

    /// The receipts a project keeps.
    pub(crate) fn receipt_cap(&self) -> usize {
        #[cfg(test)]
        if let Some(cap) = self.receipt_cap.get() {
            return cap;
        }
        RECEIPTS_PER_PROJECT
    }

    /// A receipt of a commit newly kept, pruning the project to its bound,
    /// in the caller's transaction. A receipt still kept for the same
    /// project, ref and commit (its agent's record forgotten since, and
    /// the commit reported again) stands as it was, and the bound is not
    /// moved; any other failure is the write's.
    fn keep_receipt(&self, authorship: &Authorship) -> Result<()> {
        let author = authorship
            .author
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let kept = self.conn.execute(
            "INSERT INTO authorship_history (project, branch, commit_id, agent, witness, basis, author, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT (project, branch, commit_id) DO NOTHING",
            params![
                authorship.project.as_str(),
                authorship.branch,
                authorship.commit,
                authorship.agent.as_str(),
                authorship.witness.as_str(),
                authorship.basis.as_str(),
                author,
                authorship.at.to_rfc3339(),
            ],
        )? == 1;
        if !kept {
            return Ok(());
        }
        let cut: Option<i64> = self
            .conn
            .query_row(
                "SELECT seq FROM authorship_history WHERE project = ?1
                 ORDER BY seq DESC LIMIT 1 OFFSET ?2",
                params![
                    authorship.project.as_str(),
                    i64::try_from(self.receipt_cap())?
                ],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(cut) = cut {
            self.conn.execute(
                "DELETE FROM authorship_history WHERE project = ?1 AND seq <= ?2",
                params![authorship.project.as_str(), cut],
            )?;
            self.conn.execute(
                "INSERT INTO authorship_history_bounds (project, pruned_through) VALUES (?1, ?2)
                 ON CONFLICT (project) DO UPDATE SET
                 pruned_through = max(pruned_through, excluded.pruned_through)",
                params![authorship.project.as_str(), cut],
            )?;
        }
        Ok(())
    }

    /// Once, atomically with the time capture began and the mark that it
    /// was done: a receipt of each row kept before receipts were, newest
    /// `cap` per project in the order (time, commit, ref), with basis
    /// `unknown` and no snapshot, and how many rows each project had past
    /// the bound. Nothing later seeds again, repopulates a pruned receipt,
    /// or fills one in from the registry as it is now.
    pub(super) fn seed_receipts(conn: &Connection, cap: usize, now: DateTime<Utc>) -> Result<()> {
        let tx = write_transaction(conn)?;
        let seeded: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM meta WHERE key = 'authorship_receipts_seeded')",
            [],
            |row| row.get(0),
        )?;
        if seeded {
            return Ok(());
        }
        let projects: Vec<(String, i64)> = conn
            .prepare("SELECT project, COUNT(*) FROM authorship GROUP BY project")?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let cap = i64::try_from(cap)?;
        for (project, rows) in projects {
            let skipped = (rows - cap).max(0);
            conn.execute(
                "INSERT INTO authorship_history (project, branch, commit_id, agent, witness, basis, author, at)
                 SELECT project, branch, commit_id, agent, witness, 'unknown', NULL, at
                 FROM authorship WHERE project = ?1
                 ORDER BY at, commit_id, branch LIMIT -1 OFFSET ?2",
                params![project, skipped],
            )?;
            if skipped > 0 {
                conn.execute(
                    "INSERT INTO authorship_history_bounds (project, seed_skipped) VALUES (?1, ?2)
                     ON CONFLICT (project) DO UPDATE SET seed_skipped = excluded.seed_skipped",
                    params![project, skipped],
                )?;
            }
        }
        conn.execute(
            "INSERT INTO meta (key, value) VALUES ('authorship_receipts_since', ?1)",
            [now.to_rfc3339()],
        )?;
        conn.execute(
            "INSERT INTO meta (key, value) VALUES ('authorship_receipts_seeded', '1')",
            [],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// `branch`'s receipts in `project` after `after` in their order, at
    /// most `limit`, with where the next page starts and the project's
    /// bounds. A cursor past every order SQLite can hold reads as the end,
    /// never as a fault.
    pub(crate) fn receipts(
        &self,
        project: &ProjectId,
        branch: &str,
        after: u64,
        limit: usize,
    ) -> Result<ReceiptPage> {
        let time = |text: &str| -> Result<DateTime<Utc>> {
            Ok(DateTime::parse_from_rfc3339(text)?.with_timezone(&Utc))
        };
        let mut statement = self.conn.prepare_cached(
            "SELECT seq, commit_id, agent, witness, basis, author, at FROM authorship_history
             WHERE project = ?1 AND branch = ?2 AND seq > ?3 ORDER BY seq LIMIT ?4",
        )?;
        let rows = statement
            .query_map(
                params![
                    project.as_str(),
                    branch,
                    i64::try_from(after).unwrap_or(i64::MAX),
                    i64::try_from(limit.saturating_add(1)).unwrap_or(i64::MAX)
                ],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, String>(6)?,
                    ))
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let more = rows.len() > limit;
        let mut receipts = Vec::with_capacity(rows.len().min(limit));
        for (seq, commit, agent, witness, basis, author, at) in rows.into_iter().take(limit) {
            receipts.push(AuthorshipReceipt {
                seq: u64::try_from(seq)?,
                commit,
                agent: AgentId::from(agent.as_str()),
                witness,
                basis: Basis::parse(&basis),
                author: author.map(|text| serde_json::from_str(&text)).transpose()?,
                at: time(&at)?,
            });
        }
        let next = more.then(|| receipts.last().map(|r| r.seq)).flatten();
        let (pruned_through, seed_skipped) = self
            .conn
            .query_row(
                "SELECT pruned_through, seed_skipped FROM authorship_history_bounds WHERE project = ?1",
                [project.as_str()],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?
            .unwrap_or((0, 0));
        let capture_since = self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'authorship_receipts_since'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|text| time(&text))
            .transpose()?;
        let oldest_retained = self
            .conn
            .query_row(
                "SELECT at FROM authorship_history WHERE project = ?1 ORDER BY seq LIMIT 1",
                [project.as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|text| time(&text))
            .transpose()?;
        Ok(ReceiptPage {
            receipts,
            next,
            capture_since,
            oldest_retained,
            pruned_through: (pruned_through > 0)
                .then(|| u64::try_from(pruned_through))
                .transpose()?,
            seed_skipped: u64::try_from(seed_skipped)?,
        })
    }

    /// The projects receipts are kept for (which outlive the records that
    /// named them) that `selector` may name: itself when receipts are kept
    /// for exactly it, else at most two it is a prefix of, which is as many
    /// as tell one from ambiguous. Each by a seek in the project index,
    /// never a scan of the receipts.
    pub(crate) fn receipt_projects(&self, selector: &str) -> Result<Vec<ProjectId>> {
        let exact: bool = self
            .conn
            .prepare_cached("SELECT EXISTS(SELECT 1 FROM authorship_history WHERE project = ?1)")?
            .query_row([selector], |row| row.get(0))?;
        if exact {
            return Ok(vec![ProjectId::from(selector)]);
        }
        let mut next = self.conn.prepare_cached(
            "SELECT project FROM authorship_history WHERE project > ?1 ORDER BY project LIMIT 1",
        )?;
        let mut found = Vec::new();
        let mut after = selector.to_owned();
        while found.len() < 2 {
            let Some(project) = next
                .query_row([&after], |row| row.get::<_, String>(0))
                .optional()?
                .filter(|project| project.starts_with(selector))
            else {
                break;
            };
            found.push(ProjectId::from(project.as_str()));
            after = project;
        }
        Ok(found)
    }

    /// How many of `branch`'s rows in `project` have no receipt, counted to
    /// `limit` at most.
    pub(crate) fn unreceipted(&self, project: &ProjectId, branch: &str, limit: u64) -> Result<u64> {
        Ok(u64::try_from(self.conn.query_row(
            "SELECT COUNT(*) FROM (SELECT 1 FROM authorship AS g
             WHERE g.project = ?1 AND g.branch = ?2 AND NOT EXISTS (
                 SELECT 1 FROM authorship_history AS h
                 WHERE h.project = g.project AND h.branch = g.branch AND h.commit_id = g.commit_id)
             LIMIT ?3)",
            params![project.as_str(), branch, i64::try_from(limit)?],
            |row| row.get::<_, i64>(0),
        )?)?)
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
                    // The rows keep neither: they are the receipts'.
                    basis: Basis::Unknown,
                    author: None,
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
            basis: agentdocker_core::gate::Basis::Unknown,
            author: None,
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

    /// A full commit id from a number.
    fn commit(n: u32) -> String {
        format!("{n:040x}")
    }

    fn page(store: &Store, branch: &str, after: u64, limit: usize) -> ReceiptPage {
        store
            .receipts(&ProjectId::from("p"), branch, after, limit)
            .unwrap()
    }

    fn seqs(page: &ReceiptPage) -> Vec<u64> {
        page.receipts.iter().map(|r| r.seq).collect()
    }

    fn missing(store: &Store, branch: &str) -> u64 {
        store
            .unreceipted(&ProjectId::from("p"), branch, 1_000)
            .unwrap()
    }

    fn forget(store: &Store, agent: &str) {
        let id = AgentId::from(agent);
        let mut removed = Event::new(EventKind::AgentRemoved { agent: id.clone() }, Utc::now());
        removed.seq = store.max_event_seq().unwrap() + 1;
        store.delete_agent(&id, &removed).unwrap();
    }

    /// A new row gets a receipt with its basis and who its agent was; the
    /// same commit reported again changes nothing: no second event, no
    /// second receipt, the bound not moved.
    #[test]
    fn a_receipt_is_kept_beside_a_new_row_and_a_repeat_changes_nothing() {
        let store = Store::in_memory().unwrap();
        let mut first = authored("a", "refs/heads/x", A, Witness::Hook);
        first.basis = Basis::HookSession;
        first.author = Some(agentdocker_core::gate::AuthorSnapshot {
            name: "writer".into(),
            runtime: "codex".into(),
            configured: None,
            session: Some("s1".into()),
        });
        let (events, added) = store.record_authorship(&first, 1).unwrap();
        assert!(added && events.len() == 1);
        let mut again = first.clone();
        again.agent = AgentId::from("b");
        again.basis = Basis::HookOwner;
        let (events, added) = store.record_authorship(&again, 2).unwrap();
        assert!(!added && events.is_empty());
        let read = page(&store, "refs/heads/x", 0, 10);
        assert_eq!(read.receipts.len(), 1);
        let receipt = &read.receipts[0];
        assert_eq!(
            (
                receipt.agent.as_str(),
                receipt.basis,
                receipt.witness.as_str()
            ),
            ("a", Basis::HookSession, "hook")
        );
        assert_eq!(receipt.author, first.author);
        assert_eq!(read.pruned_through, None);
        assert!(read.capture_since.is_some());
    }

    /// The bound prunes receipts, oldest first, and says how far; it never
    /// touches the rows the review gate reads, through an alias included,
    /// and the pruned commit is counted as having no receipt.
    #[test]
    fn the_bound_prunes_receipts_and_never_the_rows() {
        let store = Store::in_memory().unwrap();
        store.receipt_cap.set(Some(2));
        store
            .put_document(
                "identity_alias",
                "old",
                &serde_json::json!({"retired": "old", "canonical": "a"}),
            )
            .unwrap();
        for (n, agent) in [(1, "old"), (2, "b"), (3, "b")] {
            store
                .record_authorship(
                    &authored(agent, "refs/heads/x", &commit(n), Witness::Hook),
                    0,
                )
                .unwrap();
        }
        let read = page(&store, "refs/heads/x", 0, 10);
        assert_eq!(seqs(&read), vec![2, 3]);
        assert_eq!(read.pruned_through, Some(1));
        assert!(
            store
                .authored(
                    &ProjectId::from("p"),
                    "refs/heads/x",
                    &[AgentId::from("a"), AgentId::from("old")]
                )
                .unwrap(),
            "the gate still reads the pruned commit's row"
        );
        assert_eq!(missing(&store, "refs/heads/x"), 1);
    }

    /// A receipt outlives its agent's record: forgotten, its row goes, and
    /// the same commit reported for another owner is a new row and event
    /// while the first receipt stands as it was. Once that receipt is
    /// pruned, the commit reported again starts a new one.
    #[test]
    fn a_receipt_outlives_a_forget_and_the_next_owner() {
        let store = Store::in_memory().unwrap();
        let a = authored("a", "refs/heads/x", A, Witness::Hook);
        store.record_authorship(&a, 0).unwrap();
        forget(&store, "a");
        let mut b = authored("b", "refs/heads/x", A, Witness::Hook);
        b.basis = Basis::HookOwner;
        let (events, added) = store.record_authorship(&b, 0).unwrap();
        assert!(added && events.len() == 1, "the gate row and event are new");
        let read = page(&store, "refs/heads/x", 0, 10);
        assert_eq!(read.receipts.len(), 1);
        assert_eq!(
            (read.receipts[0].agent.as_str(), read.receipts[0].seq),
            ("a", 1),
            "the first receipt, as it was"
        );
        assert_eq!(read.pruned_through, None, "the bound did not move");
        // Pruned, then forgotten and reported again: a new receipt.
        store.receipt_cap.set(Some(1));
        store
            .record_authorship(&authored("c", "refs/heads/y", B, Witness::Hook), 0)
            .unwrap();
        assert!(page(&store, "refs/heads/x", 0, 10).receipts.is_empty());
        forget(&store, "b");
        store
            .record_authorship(&authored("d", "refs/heads/x", A, Witness::Hook), 0)
            .unwrap();
        // A new place in the order: later than every one before, gaps
        // allowed (a report that met a kept receipt used one up).
        let read = page(&store, "refs/heads/x", 0, 10);
        assert_eq!(
            read.receipts
                .iter()
                .map(|r| r.agent.as_str())
                .collect::<Vec<_>>(),
            vec!["d"]
        );
        assert!(read.receipts[0].seq > 2, "{}", read.receipts[0].seq);
    }

    /// Which commit lost its receipt to the bound and which got none from
    /// an older daemon cannot be told apart: both are counted alike, as
    /// having no receipt. An older daemon's row whose agent is forgotten
    /// leaves nothing to count, so a zero proves no continuity.
    #[test]
    fn a_missing_receipt_is_counted_and_its_cause_never_assigned() {
        let older = |store: &Store, n: u32, branch: &str| {
            store
                .conn
                .execute(
                    "INSERT INTO authorship (project, commit_id, branch, agent, witness, at)
                     VALUES ('p', ?1, ?2, 'old', 'hook', '2023-11-14T22:13:20+00:00')",
                    params![commit(n), branch],
                )
                .unwrap();
        };
        let capture = |store: &Store, n: u32, branch: &str| {
            store
                .record_authorship(&authored("a", branch, &commit(n), Witness::Hook), 0)
                .unwrap();
        };
        let mut seen = Vec::new();
        for exchanged in [false, true] {
            let store = Store::in_memory().unwrap();
            store.receipt_cap.set(Some(1));
            let (x, y) = if exchanged {
                ("refs/heads/y", "refs/heads/x")
            } else {
                ("refs/heads/x", "refs/heads/y")
            };
            capture(&store, 1, x);
            older(&store, 2, y);
            capture(&store, 3, "refs/heads/z");
            seen.push((
                missing(&store, "refs/heads/x"),
                missing(&store, "refs/heads/y"),
                page(&store, "refs/heads/z", 0, 10).pruned_through,
            ));
        }
        assert_eq!(seen[0], seen[1]);
        assert_eq!(seen[0], (1, 1, Some(1)));
        let store = Store::in_memory().unwrap();
        older(&store, 4, "refs/heads/w");
        assert_eq!(missing(&store, "refs/heads/w"), 1);
        forget(&store, "old");
        assert_eq!(missing(&store, "refs/heads/w"), 0, "nothing left to count");
    }

    /// The seed runs once, with the time capture began and its mark in one
    /// transaction: an interrupted seed leaves nothing, and a seed that
    /// completes takes the newest by (time, commit, ref), says how many it
    /// left out, and is never run again.
    #[test]
    fn the_seed_runs_once_with_its_ties_broken_and_never_half() {
        let store = Store::in_memory().unwrap();
        store
            .conn
            .execute_batch(
                "DELETE FROM meta WHERE key IN ('authorship_receipts_seeded', 'authorship_receipts_since');
                 INSERT INTO authorship (project, commit_id, branch, agent, witness, at) VALUES
                  ('p', '1111111111111111111111111111111111111111', 'refs/heads/a', 'x', 'hook', '2023-11-14T22:13:20+00:00'),
                  ('p', '2222222222222222222222222222222222222222', 'refs/heads/a', 'x', 'hook', '2023-11-14T22:13:21+00:00'),
                  ('p', '2222222222222222222222222222222222222222', 'refs/heads/b', 'x', 'hook', '2023-11-14T22:13:21+00:00');
                 CREATE TRIGGER fail BEFORE INSERT ON authorship_history
                   WHEN NEW.branch = 'refs/heads/b' BEGIN SELECT RAISE(ABORT, 'seed fault'); END;",
            )
            .unwrap();
        let now = Utc::now();
        assert!(Store::seed_receipts(&store.conn, 2, now).is_err());
        let count = |sql: &str| -> i64 { store.conn.query_row(sql, [], |row| row.get(0)).unwrap() };
        assert_eq!(count("SELECT COUNT(*) FROM authorship_history"), 0);
        assert_eq!(
            count("SELECT COUNT(*) FROM meta WHERE key LIKE 'authorship_receipts_%'"),
            0,
            "nothing of an interrupted seed is kept"
        );
        store.conn.execute_batch("DROP TRIGGER fail").unwrap();
        Store::seed_receipts(&store.conn, 2, now).unwrap();
        let a = page(&store, "refs/heads/a", 0, 10);
        let b = page(&store, "refs/heads/b", 0, 10);
        // At the cut, the tie between one commit at one time on two refs
        // goes by ref: both of the newest kept, the older one left out.
        assert_eq!(
            (a.receipts.len(), b.receipts.len()),
            (1, 1),
            "{:?} {:?}",
            seqs(&a),
            seqs(&b)
        );
        assert_eq!(a.receipts[0].commit, "2".repeat(40));
        assert!(a.receipts[0].seq < b.receipts[0].seq);
        assert_eq!(a.receipts[0].basis, Basis::Unknown);
        assert_eq!(a.receipts[0].author, None);
        assert_eq!(a.seed_skipped, 1);
        assert_eq!(
            a.capture_since,
            Some(
                DateTime::parse_from_rfc3339(&now.to_rfc3339())
                    .unwrap()
                    .with_timezone(&Utc)
            )
        );
        // One slot over a tie at the cut (one commit at one time on two
        // refs): the later ref is kept, the other left out and counted.
        let tied = Store::in_memory().unwrap();
        tied.conn
            .execute_batch(
                "DELETE FROM meta WHERE key IN ('authorship_receipts_seeded', 'authorship_receipts_since');
                 INSERT INTO authorship (project, commit_id, branch, agent, witness, at) VALUES
                  ('p', '4444444444444444444444444444444444444444', 'refs/heads/a', 'x', 'hook', '2023-11-14T22:13:20+00:00'),
                  ('p', '4444444444444444444444444444444444444444', 'refs/heads/b', 'x', 'hook', '2023-11-14T22:13:20+00:00');",
            )
            .unwrap();
        Store::seed_receipts(&tied.conn, 1, now).unwrap();
        let (a, b) = (
            page(&tied, "refs/heads/a", 0, 10),
            page(&tied, "refs/heads/b", 0, 10),
        );
        assert_eq!(
            (a.receipts.len(), b.receipts.len(), b.seed_skipped),
            (0, 1, 1)
        );
        // Never again: a row added since stays without a receipt.
        store
            .conn
            .execute_batch(
                "INSERT INTO authorship (project, commit_id, branch, agent, witness, at) VALUES
                 ('p', '3333333333333333333333333333333333333333', 'refs/heads/a', 'x', 'hook', '2023-11-14T22:13:22+00:00')",
            )
            .unwrap();
        Store::seed_receipts(&store.conn, 2, now).unwrap();
        assert_eq!(count("SELECT COUNT(*) FROM authorship_history"), 2);
    }

    /// Which projects a selector names among the receipts is found by
    /// seeks, the work the same however many receipts unrelated projects
    /// hold: exactly, by a unique prefix, or ambiguous between two.
    #[test]
    fn a_project_is_found_among_receipts_by_seeks_not_scans() {
        let store = Store::in_memory().unwrap();
        let keep = |project: &str, n: u32| {
            let mut kept = authored("a", "refs/heads/x", &commit(n), Witness::Hook);
            kept.project = ProjectId::from(project);
            store.record_authorship(&kept, 0).unwrap();
        };
        keep("abc111", 1);
        keep("abc222", 2);
        keep("def333", 3);
        let work = |selector: &str| {
            let steps = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
            let counted = std::sync::Arc::clone(&steps);
            store
                .conn
                .progress_handler(
                    1,
                    Some(move || {
                        counted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        false
                    }),
                )
                .unwrap();
            let found = store.receipt_projects(selector).unwrap();
            store
                .conn
                .progress_handler(0, None::<fn() -> bool>)
                .unwrap();
            (found, steps.load(std::sync::atomic::Ordering::Relaxed))
        };
        let names = |found: Vec<ProjectId>| -> Vec<String> {
            found.iter().map(|p| p.as_str().to_owned()).collect()
        };
        assert_eq!(names(work("abc111").0), vec!["abc111"]);
        assert_eq!(names(work("de").0), vec!["def333"]);
        assert_eq!(names(work("abc").0), vec!["abc111", "abc222"]);
        assert!(work("zzz").0.is_empty());
        let (_, before) = work("de");
        for n in 0..3_000 {
            keep(&format!("abc{n:04}x"), 10 + n);
        }
        let (found, after) = work("de");
        assert_eq!(names(found), vec!["def333"]);
        assert!(after <= before + 20, "{before} steps, then {after}");
    }

    /// Pages go by `seq`, a total order whatever the times (every receipt
    /// here has the same one); two changes under one reused ref are listed
    /// alike; and a page whose start the bound has pruned past may miss
    /// receipts.
    #[test]
    fn receipts_page_in_order_and_say_when_the_bound_overtook_the_cursor() {
        let store = Store::in_memory().unwrap();
        for (n, agent) in [(1, "a"), (2, "a"), (3, "b"), (4, "b"), (5, "b")] {
            store
                .record_authorship(
                    &authored(agent, "refs/heads/x", &commit(n), Witness::Hook),
                    0,
                )
                .unwrap();
        }
        let first = page(&store, "refs/heads/x", 0, 2);
        assert_eq!((seqs(&first), first.next), (vec![1, 2], Some(2)));
        let second = page(&store, "refs/heads/x", 2, 2);
        assert_eq!((seqs(&second), second.next), (vec![3, 4], Some(4)));
        let last = page(&store, "refs/heads/x", 4, 2);
        assert_eq!((seqs(&last), last.next), (vec![5], None));
        store.receipt_cap.set(Some(3));
        store
            .record_authorship(&authored("c", "refs/heads/y", &commit(6), Witness::Hook), 0)
            .unwrap();
        let resumed = page(&store, "refs/heads/x", 2, 2);
        assert_eq!(seqs(&resumed), vec![4, 5]);
        assert_eq!(resumed.pruned_through, Some(3), "past the cursor at 2");
        // An empty ref still says when the project's oldest retained
        // receipt was kept, though another ref holds it.
        let empty = page(&store, "refs/heads/z", 0, 2);
        assert!(empty.receipts.is_empty());
        assert_eq!(empty.oldest_retained, Some(resumed.receipts[0].at));
        assert_eq!(
            page(&store, "refs/heads/x", u64::MAX, 2).receipts.len(),
            0,
            "a cursor past every order is the end"
        );
        let none = Store::in_memory().unwrap();
        assert_eq!(page(&none, "refs/heads/x", 0, 2).oldest_retained, None);
    }
}
