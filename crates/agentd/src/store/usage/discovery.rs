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
    FinishedJob(usize),
    Complete,
}

impl Store {
    pub(crate) fn usage_discovery(&self) -> Result<Option<Progress>> {
        self.document("usage", "discovery")
    }

    pub(crate) fn usage_next_job(&self) -> Result<Option<Job>> {
        self.conn
            .query_row(
                "SELECT json FROM usage_discovery_jobs ORDER BY priority DESC,id LIMIT 1",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .map(|s| serde_json::from_str(&s).map_err(Into::into))
            .transpose()
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
}
