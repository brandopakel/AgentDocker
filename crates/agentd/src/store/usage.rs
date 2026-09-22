//! Atomic token accounting. File bytes never enter these tables.
use super::*;
use agentdocker_core::usage::{self, Aggregate, Baseline, Counters, Range, report::*};
use agentdocker_host::usage::{Sample, Semantics, reader};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub(crate) mod discovery;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Attribution {
    pub agent: Option<String>,
    pub project: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Bucket {
    attribution: Attribution,
    runtime: String,
    provider: Option<String>,
    model: Option<String>,
    hour: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Contribution {
    bucket: Bucket,
    session: String,
    counters: Counters,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct FileProgress {
    pub cursor: reader::Cursor,
    pub stop: reader::Stop,
}

pub(crate) struct Ingest<'a> {
    pub key: &'a str,
    pub progress: &'a FileProgress,
    pub samples: &'a [(Sample, Attribution)],
    pub gaps: &'a [reader::Gap],
    pub collection: &'a Collection,
    pub retained_since: DateTime<Utc>,
    pub now: DateTime<Utc>,
    pub event_seq: u64,
    pub finished_job: Option<usize>,
}

fn fingerprint<T: Serialize>(value: &T) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(value)?)))
}

fn text(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)
}

impl Store {
    pub(crate) fn usage_snapshot(
        &self,
        collection: &Collection,
        gaps: &[(&str, &str)],
        now: DateTime<Utc>,
        seq: u64,
        discovery: Option<discovery::Change<'_>>,
    ) -> Result<Event> {
        let tx = self.conn.unchecked_transaction()?;
        if let Some(change) = discovery {
            self.usage_discovery_change(change)?;
        }
        let mut count = 0;
        for (key, reason) in gaps {
            count += self.usage_gap(key, None, now, None, reason)?;
        }
        self.put_document("usage", "collection", collection)?;
        let mut event = Event::new(
            EventKind::UsageRecorded {
                generation: collection.discovery_generation.unwrap_or(0),
                samples: 0,
                gaps: count,
            },
            now,
        );
        event.seq = seq;
        self.append_event(&event)?;
        tx.commit()?;
        Ok(event)
    }

    pub(crate) fn usage_file(&self, key: &str) -> Result<Option<FileProgress>> {
        self.conn
            .query_row("SELECT json FROM usage_files WHERE key=?1", [key], |r| {
                r.get::<_, String>(0)
            })
            .optional()?
            .map(|value| serde_json::from_str(&value).map_err(Into::into))
            .transpose()
    }

    pub(crate) fn usage_collection(&self) -> Result<Option<Collection>> {
        self.document("usage", "collection")
    }

    /// Increasing the configured retention cannot recover already discarded
    /// history. Keep the committed cutoff across restarts and configuration edits.
    pub(crate) fn usage_retained_since(&self, requested: DateTime<Utc>) -> Result<DateTime<Utc>> {
        Ok(self
            .document::<DateTime<Utc>>("usage", "retained_since")?
            .map_or(requested, |committed| committed.max(requested)))
    }

    /// Move at most 256 retained contributions per transaction. Already
    /// attributed samples are immutable history, even if the live agent moves.
    /// The caller resolves an unambiguous runtime/session before this operation.
    pub(crate) fn usage_reconcile(
        &self,
        runtime: &str,
        session: &str,
        attribution: Option<&Attribution>,
        retained_since: DateTime<Utc>,
        now: DateTime<Utc>,
        seq: u64,
    ) -> Result<Option<Event>> {
        let tx = self.conn.unchecked_transaction()?;
        // The scheduling cursor commits with the accounting/event, including
        // a no-op. A failed cursor write must roll the entire operation back.
        self.put_document("usage", "reconcile_after", &(runtime, session))?;
        let Some(attribution) = attribution.filter(|a| a.agent.is_some()) else {
            tx.commit()?;
            return Ok(None);
        };
        let agent = attribution.agent.as_ref().expect("attributed agent");
        let retained_since = self.usage_retained_since(retained_since)?;
        let mut statement = self.conn.prepare("SELECT source_id,contribution FROM usage_samples WHERE at>=?1 AND contribution IS NOT NULL AND json_extract(contribution,'$.bucket.attribution.agent') IS NULL AND json_extract(contribution,'$.bucket.runtime')=?2 AND json_extract(contribution,'$.session')=?3 ORDER BY source_id LIMIT 256")?;
        let records = statement
            .query_map(params![text(retained_since), runtime, session], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut count = 0;
        for (id, value) in records {
            let mut contribution: Contribution = serde_json::from_str(&value)?;
            let mut destination = contribution.bucket.clone();
            destination.attribution = attribution.clone();
            let Ok(next) = self.usage_bucket(&destination)?.add(&contribution.counters) else {
                // An unrepresentable merge stays visibly unattributed. It must
                // not disable unrelated coordination or lose the old total.
                continue;
            };
            let previous = self
                .usage_bucket(&contribution.bucket)?
                .remove(&contribution.counters)
                .map_err(anyhow::Error::msg)?;
            self.write_usage_bucket(&contribution.bucket, &previous)?;
            contribution.bucket = destination;
            self.write_usage_bucket(&contribution.bucket, &next)?;
            self.conn.execute(
                "UPDATE usage_samples SET contribution=?1 WHERE source_id=?2",
                params![serde_json::to_string(&contribution)?, id],
            )?;
            count += 1;
        }
        drop(statement);
        let event = if count > 0 {
            let mut event = Event::new(
                EventKind::UsageReconciled {
                    agent: AgentId::from(agent.clone()),
                    samples: count,
                },
                now,
            );
            event.seq = seq;
            self.append_event(&event)?;
            Some(event)
        } else {
            None
        };
        tx.commit()?;
        Ok(event)
    }

    pub(crate) fn usage_ingest(&self, batch: Ingest<'_>) -> Result<Event> {
        anyhow::ensure!(
            batch.samples.len() <= 4096 && batch.gaps.len() <= 4096,
            "usage batch exceeds bounds"
        );
        let tx = self.conn.unchecked_transaction()?;
        let retained_since = self.usage_retained_since(batch.retained_since)?;
        let mut accepted = 0;
        let mut gaps = 0;
        // Reader records normally arrive in source order. Sorting this bounded
        // batch also handles per-response logs whose records were interleaved.
        let mut samples: Vec<_> = batch.samples.iter().collect();
        samples.sort_by_key(|(sample, _)| sample.at);
        for (sample, attribution) in samples {
            // The same provider record can be encountered with or without
            // proof of the preceding file prefix. That evidence controls its
            // first accounting decision; it is not different accounting on
            // a replay. Keep compatibility with draft stores that hashed it.
            let mut accounting = sample.clone();
            accounting.proves_zero_baseline = false;
            if sample.semantics == Semantics::Response {
                // Claude emits several content records for one response, each
                // with its own observation time. The first accepted record
                // fixes the hour; later fragments must not move or recount it.
                accounting.at = DateTime::<Utc>::UNIX_EPOCH;
            }
            let hash = fingerprint(&accounting)?;
            let old: Option<(String, String)> = self
                .conn
                .query_row(
                    "SELECT fingerprint,at FROM usage_samples WHERE source_id=?1",
                    [&sample.source_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if let Some((old, first_at)) = old {
                if old != hash {
                    let mut legacy = sample.clone();
                    if sample.semantics == Semantics::Response {
                        legacy.at = DateTime::parse_from_rfc3339(&first_at)?.with_timezone(&Utc);
                    }
                    legacy.proves_zero_baseline = false;
                    let without_proof = fingerprint(&legacy)?;
                    legacy.proves_zero_baseline = true;
                    if old != without_proof && old != fingerprint(&legacy)? {
                        gaps += self.usage_gap(
                            &format!("conflict:{}", sample.source_id),
                            None,
                            sample.at,
                            Some((&sample.runtime, &sample.session_id)),
                            "source identity has conflicting accounting",
                        )?;
                    }
                }
                continue;
            }
            let contribution = match sample.semantics {
                Semantics::Response => Some(sample.counters.clone()),
                Semantics::Cumulative => {
                    let key = serde_json::to_string(&(&sample.runtime, &sample.session_id))?;
                    let previous: Option<Baseline> = self
                        .conn
                        .query_row(
                            "SELECT json FROM usage_baselines WHERE key=?1",
                            [&key],
                            |r| r.get::<_, String>(0),
                        )
                        .optional()?
                        .map(|s| serde_json::from_str(&s))
                        .transpose()?;
                    match usage::observe_cumulative(
                        previous.as_ref(),
                        sample.at,
                        sample.counters.clone(),
                        sample.proves_zero_baseline,
                    ) {
                        Ok(Some(observed)) => {
                            if let Some(gap) = observed.gap {
                                gaps += self.usage_gap(
                                    &format!("baseline:{}", sample.source_id),
                                    gap.since,
                                    gap.until,
                                    Some((&sample.runtime, &sample.session_id)),
                                    "unknown initial history or counter reset",
                                )?;
                            }
                            self.conn.execute("INSERT INTO usage_baselines VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET json=excluded.json",
                                params![key, serde_json::to_string(&observed.baseline)?])?;
                            observed.contribution
                        }
                        Ok(None) => None,
                        Err(_) => {
                            gaps += self.usage_gap(
                                &format!("order:{}", sample.source_id),
                                None,
                                sample.at,
                                Some((&sample.runtime, &sample.session_id)),
                                "cumulative source is out of order or inconsistent",
                            )?;
                            None
                        }
                    }
                }
            };
            let mut contribution = contribution
                .filter(|_| usage::hour(sample.at) >= retained_since)
                .map(|counters| Contribution {
                    bucket: Bucket {
                        attribution: attribution.clone(),
                        runtime: sample.runtime.clone(),
                        provider: sample.provider.clone(),
                        model: sample.model.clone(),
                        hour: usage::hour(sample.at),
                    },
                    session: sample.session_id.clone(),
                    counters,
                });
            let counted = if let Some(contribution) = &contribution {
                match self
                    .usage_bucket(&contribution.bucket)?
                    .add(&contribution.counters)
                {
                    Ok(aggregate) => {
                        self.write_usage_bucket(&contribution.bucket, &aggregate)?;
                        true
                    }
                    Err(_) => {
                        gaps += self.usage_gap(
                            &format!("overflow:{}", sample.source_id),
                            None,
                            sample.at,
                            Some((&sample.runtime, &sample.session_id)),
                            "usage bucket arithmetic exceeds its range",
                        )?;
                        false
                    }
                }
            } else {
                false
            };
            if !counted {
                contribution = None;
            }
            self.conn.execute(
                "INSERT INTO usage_samples VALUES (?1,?2,?3,?4)",
                params![
                    sample.source_id,
                    hash,
                    text(sample.at),
                    contribution
                        .as_ref()
                        .map(serde_json::to_string)
                        .transpose()?,
                ],
            )?;
            accepted += 1;
        }
        for gap in batch.gaps {
            let key = fingerprint(&(batch.key, &batch.progress.cursor, gap.offset, &gap.reason))?;
            gaps += self.usage_gap(&key, None, batch.now, None, &gap.reason)?;
        }
        self.conn.execute("INSERT INTO usage_files VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET json=excluded.json",
            params![batch.key, serde_json::to_string(batch.progress)?])?;
        self.put_document("usage", "collection", batch.collection)?;
        if let Some(id) = batch.finished_job {
            self.usage_discovery_change(discovery::Change::FinishedJob(id))?;
        }
        self.prune_usage(batch.retained_since)?;
        let mut event = Event::new(
            EventKind::UsageRecorded {
                generation: batch.collection.discovery_generation.unwrap_or(0),
                samples: accepted,
                gaps,
            },
            batch.now,
        );
        event.seq = batch.event_seq;
        self.append_event(&event)?;
        tx.commit()?;
        Ok(event)
    }

    fn usage_gap(
        &self,
        key: &str,
        since: Option<DateTime<Utc>>,
        until: DateTime<Utc>,
        session: Option<(&str, &str)>,
        reason: &str,
    ) -> Result<u64> {
        Ok(self.conn.execute(
            "INSERT OR IGNORE INTO usage_gaps VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                key,
                since.map(text),
                text(until),
                session.map(|s| s.0),
                session.map(|s| s.1),
                reason,
            ],
        )? as u64)
    }

    fn usage_bucket(&self, key: &Bucket) -> Result<Aggregate> {
        self.conn
            .query_row(
                "SELECT json FROM usage_buckets WHERE key=?1",
                [serde_json::to_string(key)?],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .map(|s| serde_json::from_str(&s).map_err(Into::into))
            .transpose()
            .map(|v| v.unwrap_or_default())
    }

    fn write_usage_bucket(&self, key: &Bucket, value: &Aggregate) -> Result<()> {
        let id = serde_json::to_string(key)?;
        if value.samples() == 0 {
            self.conn
                .execute("DELETE FROM usage_buckets WHERE key=?1", [id])?;
        } else {
            self.conn.execute("INSERT INTO usage_buckets VALUES (?1,?2,?3,?4,?5) ON CONFLICT(key) DO UPDATE SET json=excluded.json", params![
                id, text(key.hour), key.attribution.agent, key.attribution.project, serde_json::to_string(value)?,
            ])?;
        }
        Ok(())
    }

    /// Dedupe fingerprints and baselines outlive retained contributions. Old
    /// logs cannot resurrect expired totals after their file cursor changes.
    fn prune_usage(&self, since: DateTime<Utc>) -> Result<()> {
        let since = self.usage_retained_since(since)?;
        self.put_document("usage", "retained_since", &since)?;
        let since = text(since);
        self.conn.execute("DELETE FROM usage_buckets WHERE key IN (SELECT key FROM usage_buckets WHERE hour<?1 LIMIT 1000)", [&since])?;
        self.conn.execute("UPDATE usage_samples SET contribution=NULL WHERE source_id IN (SELECT source_id FROM usage_samples WHERE at<?1 AND contribution IS NOT NULL LIMIT 1000)", [&since])?;
        self.conn.execute("DELETE FROM usage_gaps WHERE key IN (SELECT key FROM usage_gaps WHERE until<?1 LIMIT 1000)", [&since])?;
        Ok(())
    }

    pub(crate) fn usage_report(
        &self,
        mut range: Range,
        by: Group,
        project: Option<&str>,
        agent: Option<&str>,
    ) -> Result<Option<Report>> {
        let retained_since = self.usage_retained_since(range.retained_since)?;
        range.history_truncated |= range.effective_since < retained_since;
        range.retained_since = retained_since;
        range.effective_since = range.effective_since.max(retained_since);
        range.effective_until = range.effective_until.max(retained_since);
        let mut statement = self.conn.prepare("SELECT key,json FROM usage_buckets WHERE hour>=?1 AND hour<?2 AND (?3 IS NULL OR project=?3) AND (?4 IS NULL OR agent=?4) ORDER BY key LIMIT 10001")?;
        let records = statement.query_map(
            params![
                text(range.effective_since),
                text(range.effective_until),
                project,
                agent
            ],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )?;
        let mut grouped: BTreeMap<Option<String>, Aggregate> = BTreeMap::new();
        for (index, record) in records.enumerate() {
            if index >= 10_000 {
                return Ok(None);
            }
            let (key, value) = record?;
            let key: Bucket = serde_json::from_str(&key)?;
            let value: Aggregate = serde_json::from_str(&value)?;
            let group = match by {
                Group::Agent => key.attribution.agent,
                Group::Project => key.attribution.project,
                Group::Model => key.model,
                Group::Provider => key.provider,
                Group::Hour => Some(text(key.hour)),
            };
            let aggregate = grouped.entry(group).or_default();
            let Ok(merged) = aggregate.merge(&value) else {
                return Ok(None);
            };
            *aggregate = merged;
        }
        // A parser gap can lack session metadata. Conservatively keep it in
        // filtered reports too; it cannot be assumed to belong elsewhere.
        let source_gaps: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM usage_gaps WHERE until>=?1 AND (since IS NULL OR since<?2)",
            params![text(range.effective_since), text(range.effective_until)],
            |r| r.get(0),
        )?;
        let source_gaps = u64::try_from(source_gaps).context("negative usage gap count")?;
        let collection = self.usage_collection()?.unwrap_or_default();
        let complete = collection.state == CollectionState::CaughtUp
            && source_gaps == 0
            && collection
                .snapshot_at
                .is_some_and(|at| range.effective_until <= at);
        let rows = grouped
            .into_iter()
            .map(|(key, value)| Row {
                key,
                samples: value.samples(),
                counters: CounterReports::new(&value, complete),
            })
            .collect();
        Ok(Some(Report {
            rows,
            by,
            as_of: range.as_of,
            effective_since: range.effective_since,
            effective_until: range.effective_until,
            coverage: ReportCoverage {
                retained_since: range.retained_since,
                history_truncated: range.history_truncated,
                future_until_clamped: range.future_until_clamped,
                includes_current_hour: range.includes_current_hour,
                source_gaps,
                collection,
            },
            overhead: Overhead::default(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(hour: u32) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(&format!("2026-09-17T{hour:02}:00:00Z"))
            .unwrap()
            .with_timezone(&Utc)
    }

    fn sample(id: &str, hour: u32, input: u64, cumulative: bool) -> Sample {
        Sample {
            source_id: id.into(),
            runtime: "codex".into(),
            format: "fixture".into(),
            session_id: "session".into(),
            at: at(hour),
            provider: None,
            model: Some("model".into()),
            counters: Counters {
                input_tokens: Some(input),
                output_tokens: Some(0),
                ..Counters::default()
            },
            semantics: if cumulative {
                Semantics::Cumulative
            } else {
                Semantics::Response
            },
            proves_zero_baseline: false,
        }
    }

    fn progress(root: &Path) -> FileProgress {
        let path = root.join("empty.jsonl");
        std::fs::write(&path, "").unwrap();
        FileProgress {
            cursor: reader::Cursor::capture(&path, reader::Runtime::Codex).unwrap(),
            stop: reader::Stop::Complete,
        }
    }

    fn ingest(
        store: &Store,
        progress: &FileProgress,
        key: &str,
        samples: Vec<(Sample, Attribution)>,
        cutoff: DateTime<Utc>,
    ) -> Result<Event> {
        store.usage_ingest(Ingest {
            key,
            progress,
            samples: &samples,
            gaps: &[],
            collection: &Collection {
                discovery_generation: Some(1),
                ..Collection::default()
            },
            retained_since: cutoff,
            now: at(20),
            event_seq: store.max_event_seq()? + 1,
            finished_job: None,
        })
    }

    fn report(store: &Store, since: u32, until: u32) -> Report {
        store
            .usage_report(
                Range::new(Some(at(since)), Some(at(until)), at(23), at(0)).unwrap(),
                Group::Agent,
                None,
                None,
            )
            .unwrap()
            .unwrap()
    }

    #[test]
    fn usage_replay_restart_retention_and_unknown_counters_keep_their_meaning() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("state.db");
        let store = Store::open(&db).unwrap();
        let progress = progress(temp.path());
        let value = sample("response", 2, 10, false);
        ingest(
            &store,
            &progress,
            "first",
            vec![(value.clone(), Attribution::default())],
            at(0),
        )
        .unwrap();
        ingest(
            &store,
            &progress,
            "copied",
            vec![(value.clone(), Attribution::default())],
            at(0),
        )
        .unwrap();
        drop(store);
        let store = Store::open(&db).unwrap();
        let rows = report(&store, 0, 21).rows;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].samples, 1);
        assert_eq!(rows[0].counters.input_tokens.sum, Some(10));
        assert_eq!(rows[0].counters.output_tokens.sum, Some(0));
        assert_eq!(rows[0].counters.cache_read_input_tokens.sum, None);
        assert_eq!(
            rows[0].counters.input_tokens.coverage,
            usage::Coverage::Partial
        );
        assert!(store.usage_file("copied").unwrap().is_some());
        ingest(&store, &progress, "retention", vec![], at(3)).unwrap();
        ingest(
            &store,
            &progress,
            "replay",
            vec![(value, Attribution::default())],
            at(3),
        )
        .unwrap();
        assert!(report(&store, 0, 21).rows.is_empty());
        let count: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM usage_samples", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "fingerprint survives aggregate retention");
    }

    #[test]
    fn usage_response_fragments_keep_the_first_hour_without_false_conflicts() {
        for legacy_hash in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let db = temp.path().join("state.db");
            let store = Store::open(&db).unwrap();
            let progress = progress(temp.path());
            let mut value = sample("one-response", 2, 10, false);
            ingest(
                &store,
                &progress,
                "first-fragment",
                vec![(value.clone(), Attribution::default())],
                at(0),
            )
            .unwrap();
            if legacy_hash {
                store
                    .conn
                    .execute(
                        "UPDATE usage_samples SET fingerprint=?1 WHERE source_id=?2",
                        params![fingerprint(&value).unwrap(), value.source_id],
                    )
                    .unwrap();
            }
            drop(store);
            let store = Store::open(&db).unwrap();
            value.at = at(3);
            ingest(
                &store,
                &progress,
                "later-fragment",
                vec![(value.clone(), Attribution::default())],
                at(0),
            )
            .unwrap();
            let observed = report(&store, 0, 21);
            assert_eq!(observed.rows.len(), 1);
            assert_eq!(observed.rows[0].samples, 1);
            assert_eq!(observed.rows[0].counters.input_tokens.sum, Some(10));
            assert_eq!(observed.coverage.source_gaps, 0);
            let hours = store
                .usage_report(
                    Range::new(Some(at(0)), Some(at(21)), at(23), at(0)).unwrap(),
                    Group::Hour,
                    None,
                    None,
                )
                .unwrap()
                .unwrap();
            assert_eq!(hours.rows.len(), 1);
            assert_eq!(
                hours.rows[0].key.as_deref(),
                Some("2026-09-17T02:00:00.000000000Z")
            );
            value.counters.input_tokens = Some(11);
            ingest(
                &store,
                &progress,
                "real-conflict",
                vec![(value, Attribution::default())],
                at(0),
            )
            .unwrap();
            assert_eq!(report(&store, 0, 21).coverage.source_gaps, 1);
        }
    }

    #[test]
    fn usage_replayed_prefix_proof_is_not_a_conflicting_provider_record() {
        for initially_proven in [true, false] {
            let temp = tempfile::tempdir().unwrap();
            let db = temp.path().join("state.db");
            let store = Store::open(&db).unwrap();
            let progress = progress(temp.path());
            let mut value = sample("first-snapshot", 2, 10, true);
            value.proves_zero_baseline = initially_proven;
            ingest(
                &store,
                &progress,
                "original",
                vec![(value.clone(), Attribution::default())],
                at(0),
            )
            .unwrap();
            let before = report(&store, 0, 21);
            // A persisted hash from the earlier draft remains replayable too.
            store
                .conn
                .execute(
                    "UPDATE usage_samples SET fingerprint=?1 WHERE source_id=?2",
                    params![fingerprint(&value).unwrap(), value.source_id],
                )
                .unwrap();
            drop(store);
            let store = Store::open(&db).unwrap();
            value.proves_zero_baseline = !initially_proven;
            ingest(
                &store,
                &progress,
                "copy",
                vec![(value.clone(), Attribution::default())],
                at(0),
            )
            .unwrap();
            let after = report(&store, 0, 21);
            assert_eq!(after.rows, before.rows);
            assert_eq!(after.coverage.source_gaps, before.coverage.source_gaps);
            // Removing contextual proof must not hide an actual counter change.
            value.counters.input_tokens = Some(11);
            ingest(
                &store,
                &progress,
                "conflict",
                vec![(value, Attribution::default())],
                at(0),
            )
            .unwrap();
            let conflict = report(&store, 0, 21);
            assert_eq!(conflict.rows, before.rows);
            assert_eq!(
                conflict.coverage.source_gaps,
                before.coverage.source_gaps + 1
            );
        }
    }

    #[test]
    fn usage_retention_cannot_claim_discarded_history_after_configuration_expands() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("state.db");
        let store = Store::open(&db).unwrap();
        let progress = progress(temp.path());
        ingest(
            &store,
            &progress,
            "first",
            vec![(sample("old", 1, 7, false), Attribution::default())],
            at(0),
        )
        .unwrap();
        ingest(&store, &progress, "prune", vec![], at(3)).unwrap();
        drop(store);
        let store = Store::open(&db).unwrap();
        // Newly discovered old logs cannot fill only part of discarded history
        // and make the wider configured range appear complete.
        ingest(
            &store,
            &progress,
            "expanded",
            vec![(
                sample("late-discovered", 2, 9, false),
                Attribution::default(),
            )],
            at(0),
        )
        .unwrap();
        let report = report(&store, 0, 21);
        assert!(report.rows.is_empty());
        assert_eq!(report.coverage.retained_since, at(3));
        assert_eq!(report.effective_since, at(3));
        assert!(report.coverage.history_truncated);
        assert_eq!(store.usage_retained_since(at(0)).unwrap(), at(3));
    }

    #[test]
    fn usage_reconciliation_failure_rolls_back_cursor_buckets_and_event_together() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().join("state.db")).unwrap();
        let progress = progress(temp.path());
        ingest(
            &store,
            &progress,
            "source",
            vec![(sample("a", 1, 7, false), Attribution::default())],
            at(0),
        )
        .unwrap();
        let attribution = Attribution {
            agent: Some("agent".into()),
            project: Some("project".into()),
        };
        let before = store.max_event_seq().unwrap();
        store.conn.execute_batch("CREATE TRIGGER fail_usage_reconcile BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT,'event refused'); END;").unwrap();
        assert!(
            store
                .usage_reconcile(
                    "codex",
                    "session",
                    Some(&attribution),
                    at(0),
                    at(20),
                    before + 1
                )
                .is_err()
        );
        assert!(
            store
                .document::<(String, String)>("usage", "reconcile_after")
                .unwrap()
                .is_none()
        );
        let rows = report(&store, 0, 21).rows;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].key, None);
        assert_eq!(rows[0].counters.input_tokens.sum, Some(7));
        assert_eq!(store.max_event_seq().unwrap(), before);
        store
            .conn
            .execute_batch("DROP TRIGGER fail_usage_reconcile;")
            .unwrap();
        store.conn.execute_batch("CREATE TRIGGER fail_usage_cursor BEFORE INSERT ON documents WHEN NEW.kind='usage' AND NEW.id='reconcile_after' BEGIN SELECT RAISE(ABORT,'cursor refused'); END;").unwrap();
        assert!(
            store
                .usage_reconcile(
                    "codex",
                    "session",
                    Some(&attribution),
                    at(0),
                    at(20),
                    before + 1
                )
                .is_err()
        );
        assert_eq!(store.max_event_seq().unwrap(), before);
        assert_eq!(report(&store, 0, 21).rows[0].key, None);
        assert!(
            store
                .document::<(String, String)>("usage", "reconcile_after")
                .unwrap()
                .is_none()
        );
        store
            .conn
            .execute_batch("DROP TRIGGER fail_usage_cursor;")
            .unwrap();
        store
            .usage_reconcile(
                "codex",
                "session",
                Some(&attribution),
                at(0),
                at(20),
                before + 1,
            )
            .unwrap();
        assert_eq!(
            store
                .document::<(String, String)>("usage", "reconcile_after")
                .unwrap(),
            Some(("codex".into(), "session".into()))
        );
        assert_eq!(report(&store, 0, 21).rows[0].key.as_deref(), Some("agent"));
    }

    #[test]
    fn usage_transaction_failure_does_not_advance_cursor_baseline_or_dedupe() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().join("state.db")).unwrap();
        let progress = progress(temp.path());
        store.conn.execute_batch("CREATE TRIGGER fail_usage BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT,'usage event refused'); END;").unwrap();
        assert!(
            ingest(
                &store,
                &progress,
                "source",
                vec![(sample("first", 1, 50, true), Attribution::default())],
                at(0)
            )
            .is_err()
        );
        assert!(store.usage_file("source").unwrap().is_none());
        assert!(store.usage_collection().unwrap().is_none());
        for table in ["usage_samples", "usage_baselines", "usage_gaps"] {
            let count: i64 = store
                .conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 0);
        }
        store
            .conn
            .execute_batch("DROP TRIGGER fail_usage;")
            .unwrap();
        ingest(
            &store,
            &progress,
            "source",
            vec![
                (sample("first", 1, 50, true), Attribution::default()),
                (sample("second", 2, 65, true), Attribution::default()),
            ],
            at(0),
        )
        .unwrap();
        assert_eq!(
            report(&store, 0, 21).rows[0].counters.input_tokens.sum,
            Some(15)
        );
        assert_eq!(report(&store, 0, 21).coverage.source_gaps, 1);
        assert_eq!(report(&store, 2, 21).coverage.source_gaps, 0);
    }

    #[test]
    fn usage_resets_and_out_of_order_snapshots_cannot_add_or_subtract_twice() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().join("state.db")).unwrap();
        let progress = progress(temp.path());
        for value in [
            sample("baseline", 1, 100, true),
            sample("delta", 2, 130, true),
            sample("reset", 3, 5, true),
            sample("after", 4, 9, true),
            sample("late", 1, 50, true),
        ] {
            ingest(
                &store,
                &progress,
                "source",
                vec![(value, Attribution::default())],
                at(0),
            )
            .unwrap();
        }
        let report = report(&store, 0, 21);
        assert_eq!(report.rows[0].counters.input_tokens.sum, Some(34));
        assert_eq!(report.rows[0].samples, 2);
        assert_eq!(report.coverage.source_gaps, 3);
    }

    #[test]
    fn usage_reconciliation_moves_counters_and_coverage_once_without_moving_history_again() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("state.db");
        let store = Store::open(&db).unwrap();
        let progress = progress(temp.path());
        let a = sample("a", 1, 7, false);
        let mut b = sample("b", 2, 8, false);
        b.counters.output_tokens = None;
        ingest(
            &store,
            &progress,
            "source",
            vec![
                (a.clone(), Attribution::default()),
                (b.clone(), Attribution::default()),
            ],
            at(0),
        )
        .unwrap();
        let attribution = Attribution {
            agent: Some("agent".into()),
            project: Some("project".into()),
        };
        let event = store
            .usage_reconcile(
                "codex",
                "session",
                Some(&attribution),
                at(0),
                at(20),
                store.max_event_seq().unwrap() + 1,
            )
            .unwrap()
            .unwrap();
        assert!(matches!(
            event.kind,
            EventKind::UsageReconciled { samples: 2, .. }
        ));
        assert!(
            store
                .usage_reconcile(
                    "codex",
                    "session",
                    Some(&attribution),
                    at(0),
                    at(20),
                    store.max_event_seq().unwrap() + 1
                )
                .unwrap()
                .is_none()
        );
        drop(store);
        let store = Store::open(&db).unwrap();
        let moved = Attribution {
            project: Some("other-project".into()),
            ..attribution.clone()
        };
        ingest(
            &store,
            &progress,
            "copied",
            vec![(a, moved.clone()), (b, moved.clone())],
            at(0),
        )
        .unwrap();
        assert!(
            store
                .usage_reconcile(
                    "codex",
                    "session",
                    Some(&moved),
                    at(0),
                    at(20),
                    store.max_event_seq().unwrap() + 1
                )
                .unwrap()
                .is_none()
        );
        let query = Range::new(Some(at(0)), Some(at(21)), at(23), at(0)).unwrap();
        let report = store
            .usage_report(query.clone(), Group::Agent, Some("project"), None)
            .unwrap()
            .unwrap();
        assert_eq!(report.rows.len(), 1);
        assert_eq!(report.rows[0].key.as_deref(), Some("agent"));
        assert_eq!(report.rows[0].samples, 2);
        assert_eq!(report.rows[0].counters.input_tokens.sum, Some(15));
        assert_eq!(report.rows[0].counters.output_tokens.known_samples, 1);
        assert!(
            store
                .usage_report(query, Group::Agent, Some("other-project"), None)
                .unwrap()
                .unwrap()
                .rows
                .is_empty()
        );
    }
}
