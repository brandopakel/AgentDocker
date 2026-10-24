//! Durable discovery frontier and bounded pending-file manifest. Page contents,
//! the frontier, coverage and event commit together; readers cannot skip jobs
//! after a crash between enumeration and accounting.
use super::*;
use agentdocker_host::usage::discovery::{Checkpoint, MAX_FILES, PASS_ENTRIES, Root, Source};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Progress {
    pub roots: Vec<Root>,
    pub generation: u64,
    pub frontier: Checkpoint,
    pub jobs: usize,
    pub failures: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Job {
    pub id: usize,
    pub source: Source,
    pub captured: reader::Cursor,
    pub priority: bool,
}

pub(crate) enum Change<'a> {
    Start(&'a Progress),
    Page(&'a Progress, &'a [Job]),
    RefreshJob(usize, &'a reader::Cursor),
    FinishedJob(usize),
    /// Several jobs finished with nothing to record, removed together.
    FinishedJobs(&'a [usize]),
    Complete,
}

fn saved_job(id: i64, json: &str) -> Result<Job> {
    let job: Job = serde_json::from_str(json)?;
    anyhow::ensure!(
        job.id < MAX_FILES
            && i64::try_from(job.id)? == id
            && job.source.path.is_absolute()
            && job.source.path.to_str().is_some_and(|p| p.len() <= 8192),
        "invalid saved discovery job"
    );
    Ok(job)
}

impl Store {
    pub(crate) fn usage_discovery(&self) -> Result<Option<Progress>> {
        // This is restartable enumeration metadata, not accepted accounting.
        // A malformed document starts a new generation; actual SQLite failures
        // must still fence storage. Change::Start clears its stale manifest.
        let progress: Option<Progress> = match self.document("usage", "discovery") {
            Ok(progress) => progress,
            Err(error) if error.is::<serde_json::Error>() => return Ok(None),
            Err(error) => return Err(error),
        };
        Ok(progress.filter(|progress| {
            progress.generation > 0
                && progress
                    .jobs
                    .checked_add(progress.failures)
                    .is_some_and(|n| n <= MAX_FILES)
        }))
    }

    #[cfg(test)]
    pub(crate) fn usage_next_job(&self) -> Result<Option<Job>> {
        self.usage_job_after(None)
    }

    /// A deferred growing file is visited at most once per collector pass, so
    /// it cannot starve later files. Restart starts again at the first job.
    pub(crate) fn usage_job_after(&self, after: Option<(bool, usize)>) -> Result<Option<Job>> {
        let (priority, id) = match after {
            Some((priority, id)) => (i64::from(priority), i64::try_from(id)?),
            None => (2, -1),
        };
        self.conn
            .query_row(
                "SELECT id,json FROM usage_discovery_jobs
                 WHERE priority < ?1 OR (priority = ?1 AND id > ?2)
                 ORDER BY priority DESC,id LIMIT 1",
                params![priority, id],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?
            .map(|(id, json)| saved_job(id, &json))
            .transpose()
    }

    /// One bounded page of the pending manifest in id order, after `after`,
    /// each job with its file's saved progress: what the collector reads to
    /// choose a pass's scheduled whole proofs.
    pub(crate) fn usage_jobs_with_progress(
        &self,
        after: Option<usize>,
        limit: usize,
    ) -> Result<Vec<(Job, Option<FileProgress>)>> {
        let after = match after {
            Some(id) => i64::try_from(id)?,
            None => -1,
        };
        let rows = self
            .conn
            .prepare("SELECT id,json FROM usage_discovery_jobs WHERE id > ?1 ORDER BY id LIMIT ?2")?
            .query_map(params![after, i64::try_from(limit)?], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter()
            .map(|(id, json)| {
                let job = saved_job(id, &json)?;
                let progress = self.usage_file(&file_key(&job.source))?;
                Ok((job, progress))
            })
            .collect()
    }

    /// Called only within an accounting or coverage transaction.
    pub(super) fn usage_discovery_change(&self, change: Change<'_>) -> Result<()> {
        let progress = match change {
            Change::Start(progress) => {
                anyhow::ensure!(
                    progress.jobs == 0 && progress.failures == 0,
                    "new discovery is not empty"
                );
                self.conn.execute("DELETE FROM usage_discovery_jobs", [])?;
                Some(progress)
            }
            Change::Page(progress, jobs) => {
                anyhow::ensure!(
                    jobs.len() <= PASS_ENTRIES && jobs.len() <= progress.jobs,
                    "discovery page exceeds bounds"
                );
                let first = progress.jobs - jobs.len();
                for (offset, job) in jobs.iter().enumerate() {
                    anyhow::ensure!(
                        job.id == first + offset && job.id < MAX_FILES,
                        "invalid discovery job order"
                    );
                    let value = serde_json::to_string(job)?;
                    anyhow::ensure!(value.len() <= 128 * 1024, "discovery job exceeds bounds");
                    self.conn.execute(
                        "INSERT INTO usage_discovery_jobs VALUES (?1,?2,?3)",
                        params![i64::try_from(job.id)?, job.priority, value],
                    )?;
                }
                Some(progress)
            }
            Change::RefreshJob(id, captured) => {
                let value: String = self.conn.query_row(
                    "SELECT json FROM usage_discovery_jobs WHERE id=?1",
                    [i64::try_from(id)?],
                    |row| row.get(0),
                )?;
                let mut job: Job = serde_json::from_str(&value)?;
                anyhow::ensure!(
                    job.id == id && id < MAX_FILES,
                    "invalid discovery job identity"
                );
                job.captured = captured.clone();
                let value = serde_json::to_string(&job)?;
                anyhow::ensure!(value.len() <= 128 * 1024, "discovery job exceeds bounds");
                anyhow::ensure!(
                    self.conn.execute(
                        "UPDATE usage_discovery_jobs SET json=?1 WHERE id=?2",
                        params![value, i64::try_from(id)?],
                    )? == 1,
                    "discovery job is no longer pending"
                );
                None
            }
            Change::FinishedJob(id) => {
                anyhow::ensure!(
                    self.conn.execute(
                        "DELETE FROM usage_discovery_jobs WHERE id=?1",
                        [i64::try_from(id)?]
                    )? == 1,
                    "discovery job is no longer pending"
                );
                None
            }
            Change::FinishedJobs(ids) => {
                anyhow::ensure!(ids.len() <= PASS_ENTRIES, "finished jobs exceed bounds");
                for id in ids {
                    anyhow::ensure!(
                        self.conn.execute(
                            "DELETE FROM usage_discovery_jobs WHERE id=?1",
                            [i64::try_from(*id)?]
                        )? == 1,
                        "discovery job is no longer pending"
                    );
                }
                None
            }
            Change::Complete => {
                let count: i64 =
                    self.conn
                        .query_row("SELECT COUNT(*) FROM usage_discovery_jobs", [], |r| {
                            r.get(0)
                        })?;
                anyhow::ensure!(count == 0, "discovery still has pending jobs");
                self.delete_document("usage", "discovery")?;
                None
            }
        };
        if let Some(progress) = progress {
            anyhow::ensure!(
                progress
                    .jobs
                    .checked_add(progress.failures)
                    .is_some_and(|n| n <= MAX_FILES),
                "discovery manifest exceeds bounds"
            );
            anyhow::ensure!(
                serde_json::to_vec(progress)?.len() <= 4 * 1024 * 1024,
                "discovery frontier exceeds bounds"
            );
            self.put_document("usage", "discovery", progress)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_host::usage::{discovery::Walk, reader::Runtime};

    #[test]
    fn corrupt_discovery_metadata_is_restartable_but_job_identity_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("source.jsonl");
        std::fs::write(&path, "").unwrap();
        let store = Store::open(&temp.path().join("state.db")).unwrap();
        let roots = vec![Root {
            runtime: Runtime::Claude,
            path: temp.path().to_owned(),
        }];
        let progress = Progress {
            frontier: Walk::new(roots.clone()).unwrap().checkpoint(),
            roots,
            generation: 1,
            jobs: usize::MAX,
            failures: 1,
        };
        store.put_document("usage", "discovery", &progress).unwrap();
        assert!(store.usage_discovery().unwrap().is_none());
        let job = Job {
            id: 1,
            source: Source {
                runtime: Runtime::Claude,
                path: path.clone(),
            },
            captured: reader::Cursor::capture(&path, Runtime::Claude).unwrap(),
            priority: false,
        };
        store
            .conn
            .execute(
                "INSERT INTO usage_discovery_jobs VALUES (0,0,?1)",
                [serde_json::to_string(&job).unwrap()],
            )
            .unwrap();
        assert!(store.usage_next_job().is_err());
        store
            .put_document("usage", "discovery", &"malformed")
            .unwrap();
        assert!(store.usage_discovery().unwrap().is_none());
        store.conn.execute("DROP TABLE documents", []).unwrap();
        assert!(
            store.usage_discovery().is_err(),
            "SQL errors must not be hidden"
        );
    }

    #[test]
    fn refreshing_pending_snapshot_rolls_back_with_its_event() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("source.jsonl");
        std::fs::write(&path, "initial\n").unwrap();
        let store = Store::open(&temp.path().join("state.db")).unwrap();
        let roots = vec![Root {
            runtime: Runtime::Claude,
            path: temp.path().to_owned(),
        }];
        let mut progress = Progress {
            frontier: Walk::new(roots.clone()).unwrap().checkpoint(),
            roots,
            generation: 1,
            jobs: 0,
            failures: 0,
        };
        let now = Utc::now();
        let collection = Collection {
            discovery_generation: Some(1),
            pending_files: Some(1),
            ..Collection::default()
        };
        store
            .usage_snapshot(&collection, &[], now, 1, Some(Change::Start(&progress)))
            .unwrap();
        let job = Job {
            id: 0,
            source: Source {
                runtime: Runtime::Claude,
                path: path.clone(),
            },
            captured: reader::Cursor::capture(&path, Runtime::Claude).unwrap(),
            priority: true,
        };
        progress.jobs = 1;
        store
            .usage_snapshot(
                &collection,
                &[],
                now,
                2,
                Some(Change::Page(&progress, std::slice::from_ref(&job))),
            )
            .unwrap();
        std::fs::write(&path, "initial\nappended\n").unwrap();
        let refreshed = reader::Cursor::capture(&path, Runtime::Claude).unwrap();
        let refresh = || {
            store.usage_snapshot(
                &collection,
                &[],
                now,
                3,
                Some(Change::RefreshJob(0, &refreshed)),
            )
        };
        store.conn.execute_batch("CREATE TRIGGER fail_refresh BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT,'event refused'); END;").unwrap();
        assert!(refresh().is_err());
        assert_eq!(
            store.usage_next_job().unwrap().unwrap().captured,
            job.captured
        );
        store
            .conn
            .execute_batch("DROP TRIGGER fail_refresh")
            .unwrap();
        refresh().unwrap();
        let saved = store.usage_next_job().unwrap().unwrap();
        assert_eq!(saved.captured, refreshed);
        assert_eq!(saved.source.path, job.source.path);
        assert!(saved.priority);
        assert!(store.usage_job_after(Some((true, 0))).unwrap().is_none());
        assert_eq!(
            store.usage_collection().unwrap().unwrap().pending_files,
            Some(1)
        );
    }

    #[test]
    fn discovery_pages_and_finished_jobs_rollback_with_their_event() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("source.jsonl");
        let now = Utc::now();
        let line = serde_json::json!({"type":"assistant","version":"2.1.270",
            "sessionId":"fixture","timestamp":now.to_rfc3339(),
            "message":{"id":"response","model":"fixture","content":[{"text":"PRIVATE_TRANSCRIPT"}],
                "usage":{"input_tokens":7,"output_tokens":2}}});
        std::fs::write(&path, format!("{line}\n")).unwrap();
        let roots = vec![Root {
            runtime: Runtime::Claude,
            path: temp.path().to_owned(),
        }];
        let mut walk = Walk::new(roots.clone()).unwrap();
        let mut progress = Progress {
            roots,
            generation: 1,
            frontier: walk.checkpoint(),
            jobs: 0,
            failures: 0,
        };
        let store = Store::open(&temp.path().join("state.db")).unwrap();
        let mut collection = Collection {
            discovery_generation: Some(1),
            ..Collection::default()
        };
        store
            .usage_snapshot(&collection, &[], now, 1, Some(Change::Start(&progress)))
            .unwrap();
        let page = walk.next_page();
        assert!(page.complete);
        assert_eq!(page.sources.len(), 1);
        progress.frontier = walk.checkpoint();
        progress.jobs = 1;
        collection.pending_files = Some(1);
        let jobs = [Job {
            id: 0,
            source: page.sources[0].clone(),
            captured: reader::Cursor::capture(&path, Runtime::Claude).unwrap(),
            priority: false,
        }];
        assert!(
            !serde_json::to_string(&jobs)
                .unwrap()
                .contains("PRIVATE_TRANSCRIPT")
        );
        let fail = "CREATE TRIGGER fail_discovery BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT,'event refused'); END;";
        store.conn.execute_batch(fail).unwrap();
        assert!(
            store
                .usage_snapshot(
                    &collection,
                    &[],
                    now,
                    2,
                    Some(Change::Page(&progress, &jobs))
                )
                .is_err()
        );
        assert_eq!(store.usage_discovery().unwrap().unwrap().jobs, 0);
        assert!(store.usage_next_job().unwrap().is_none());
        assert_eq!(
            store.usage_collection().unwrap().unwrap().pending_files,
            None
        );
        store
            .conn
            .execute_batch("DROP TRIGGER fail_discovery")
            .unwrap();
        store
            .usage_snapshot(
                &collection,
                &[],
                now,
                2,
                Some(Change::Page(&progress, &jobs)),
            )
            .unwrap();

        let scanned =
            reader::scan(&path, Runtime::Claude, None, reader::Budget::default()).unwrap();
        let file = FileProgress {
            cursor: scanned.cursor,
            stop: scanned.stop,
            verified_at: None,
        };
        let samples: Vec<_> = scanned
            .samples
            .into_iter()
            .map(|s| (s, Attribution::default()))
            .collect();
        assert_eq!(samples.len(), 1);
        collection.pending_files = Some(0);
        let ingest = || {
            store.usage_ingest(Ingest {
                key: "source",
                progress: &file,
                samples: &samples,
                gaps: &[],
                collection: &collection,
                retained_since: usage::hour(now - chrono::Duration::days(1)),
                now,
                event_seq: 3,
                finished_job: Some(0),
            })
        };
        store.conn.execute_batch(fail).unwrap();
        assert!(ingest().is_err());
        assert!(store.usage_next_job().unwrap().is_some());
        assert!(store.usage_file("source").unwrap().is_none());
        assert_eq!(
            store.usage_collection().unwrap().unwrap().pending_files,
            Some(1)
        );
        let count: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM usage_samples", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
        store
            .conn
            .execute_batch("DROP TRIGGER fail_discovery")
            .unwrap();
        ingest().unwrap();
        assert!(store.usage_next_job().unwrap().is_none());
        assert!(store.usage_file("source").unwrap().is_some());
        let count: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM usage_samples", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn quiet_jobs_finish_together_or_not_at_all() {
        let temp = tempfile::tempdir().unwrap();
        let roots = vec![Root {
            runtime: Runtime::Claude,
            path: temp.path().to_owned(),
        }];
        let mut progress = Progress {
            frontier: Walk::new(roots.clone()).unwrap().checkpoint(),
            roots,
            generation: 1,
            jobs: 0,
            failures: 0,
        };
        let store = Store::open(&temp.path().join("state.db")).unwrap();
        let now = Utc::now();
        let collection = Collection {
            discovery_generation: Some(1),
            ..Collection::default()
        };
        store
            .usage_snapshot(&collection, &[], now, 1, Some(Change::Start(&progress)))
            .unwrap();
        let jobs: Vec<_> = (0..3)
            .map(|id| {
                let path = temp.path().join(format!("source-{id}.jsonl"));
                std::fs::write(&path, "").unwrap();
                Job {
                    id,
                    source: Source {
                        runtime: Runtime::Claude,
                        path: path.clone(),
                    },
                    captured: reader::Cursor::capture(&path, Runtime::Claude).unwrap(),
                    priority: false,
                }
            })
            .collect();
        progress.jobs = 3;
        store
            .usage_snapshot(
                &collection,
                &[],
                now,
                2,
                Some(Change::Page(&progress, &jobs)),
            )
            .unwrap();
        let pending = || {
            let mut ids = Vec::new();
            let mut after = None;
            while let Some(job) = store.usage_job_after(after).unwrap() {
                after = Some((job.priority, job.id));
                ids.push(job.id);
            }
            ids
        };
        // The scheduler reads the manifest in bounded id-ordered pages.
        let page_ids = |page: Vec<(Job, Option<FileProgress>)>| {
            assert!(page.iter().all(|(_, progress)| progress.is_none()));
            page.into_iter().map(|(job, _)| job.id).collect::<Vec<_>>()
        };
        assert_eq!(
            page_ids(store.usage_jobs_with_progress(None, 2).unwrap()),
            [0, 1]
        );
        assert_eq!(
            page_ids(store.usage_jobs_with_progress(Some(1), 2).unwrap()),
            [2]
        );
        assert!(
            store
                .usage_jobs_with_progress(Some(2), 2)
                .unwrap()
                .is_empty()
        );
        let finish = |ids: &[usize], seq| {
            store.usage_snapshot(&collection, &[], now, seq, Some(Change::FinishedJobs(ids)))
        };
        store.conn.execute_batch("CREATE TRIGGER fail_quiet BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT,'event refused'); END;").unwrap();
        assert!(finish(&[0, 2], 3).is_err());
        assert_eq!(pending(), [0, 1, 2]);
        store.conn.execute_batch("DROP TRIGGER fail_quiet").unwrap();
        finish(&[0, 2], 3).unwrap();
        assert_eq!(pending(), [1]);
        // A job that is no longer pending refuses the whole change.
        assert!(finish(&[1, 0], 4).is_err());
        assert_eq!(pending(), [1]);
        let too_many = vec![1; PASS_ENTRIES + 1];
        assert!(finish(&too_many, 4).is_err());
        finish(&[1], 4).unwrap();
        assert!(pending().is_empty());
    }
}
