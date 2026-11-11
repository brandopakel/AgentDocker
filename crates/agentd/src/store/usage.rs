//! Atomic token accounting. File bytes never enter these tables.
use super::*;
use agentdocker_core::LeaseMode;
use agentdocker_core::usage::{self, Aggregate, Baseline, Counters, Range, report::*};
use agentdocker_host::usage::{Sample, Semantics, reader};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub(crate) mod discovery;
pub(crate) mod emission;
mod tracking;
pub(super) use tracking::init as tracking_init;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Attribution {
    pub agent: Option<String>,
    pub project: Option<String>,
    /// The card (`task:<id>` lease) the agent held when the sample was
    /// recorded, by task id. Left out of the bucket key when absent, so
    /// buckets written before cards were stamped keep their keys.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card: Option<String>,
    /// The agent's roles then, as its `role` label (`reviewer,tester`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
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
    /// Whether the sample is the provider's own count for one response
    /// (`Semantics::Response`) rather than a difference between running
    /// totals. Absent on a contribution written before this was kept, which
    /// leaves whatever bucket it moves to unable to say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    exact: Option<bool>,
    /// The thread that spawned this sample's session, kept while that
    /// session has no record of its own: the usage is the parent's record's
    /// (once it has one), until the session's own record takes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent: Option<String>,
    /// The price the sample carried, in micro-dollars, once known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cost_usd_micros: Option<u64>,
}

/// The fingerprint of a `usage_samples` row that is another row's name
/// for the same response: `alias:` and the row it stands for. Such a row
/// has no contribution of its own; a sample met under its id is a replay.
const ALIAS: &str = "alias:";

/// `usage_holdings` kinds: a card held (`task:` lease on a board card)
/// and the roles held, each over the interval it was held.
const HELD_CARD: &str = "card";
pub(super) const HELD_ROLE: &str = "role";

/// What ingesting samples did, for the events that say so.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Ingested {
    /// New samples counted.
    accepted: u64,
    /// New source gaps kept.
    gaps: u64,
    /// Counted responses that learnt their provider-reported cost.
    priced: u64,
    /// Responses counted once from each source that now count once.
    folded: u64,
}

impl std::ops::AddAssign for Ingested {
    fn add_assign(&mut self, other: Self) {
        self.accepted += other.accepted;
        self.gaps += other.gaps;
        self.priced += other.priced;
        self.folded += other.folded;
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct FileProgress {
    pub cursor: reader::Cursor,
    pub stop: reader::Stop,
    /// When the collector last rehashed this cursor's whole accepted prefix.
    /// Absent before the first such proof, and in progress written before
    /// it was kept: either way the next pass schedules a whole proof.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_at: Option<DateTime<Utc>>,
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

/// The `usage_files` key of one discovered source: its runtime and path.
pub(crate) fn file_key(source: &agentdocker_host::usage::discovery::Source) -> String {
    serde_json::to_string(&(source.runtime, &source.path)).expect("serializable source path")
}

fn fingerprint<T: Serialize>(value: &T) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(value)?)))
}

pub(super) fn text(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)
}

/// Which retained samples one attribution pass takes: a runtime session's,
/// to `attribution` (none attributes nothing and only moves the cursor),
/// within `window` and no older than `retained_since`.
pub(crate) struct UsageReconcile<'a> {
    pub runtime: &'a str,
    pub session: &'a str,
    pub attribution: Option<&'a Attribution>,
    pub window: (Option<DateTime<Utc>>, Option<DateTime<Utc>>),
    pub retained_since: DateTime<Utc>,
}

/// One report row from a merged bucket: its counters, its price and the
/// cache share the counters give.
fn row(key: Option<String>, value: &Aggregate, complete: bool) -> Row {
    let counters = CounterReports::new(value, complete);
    Row {
        key,
        samples: value.samples(),
        usage_source: value.source(),
        cache_read_share_percent: Row::cache_read_share(value.samples(), &counters),
        counters,
        cost_usd_micros: value.cost(complete),
    }
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
        let tx = write_transaction(&self.conn)?;
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
    /// The caller resolves an unambiguous runtime/session before this operation,
    /// and `window` bounds the sample times this attribution takes.
    pub(crate) fn usage_reconcile(
        &self,
        reconcile: UsageReconcile<'_>,
        now: DateTime<Utc>,
        seq: u64,
    ) -> Result<Option<Event>> {
        let UsageReconcile {
            runtime,
            session,
            attribution,
            window,
            retained_since,
        } = reconcile;
        let tx = write_transaction(&self.conn)?;
        // The scheduling cursor commits with the accounting/event, including
        // a no-op. A failed cursor write must roll the entire operation back.
        self.put_document("usage", "reconcile_after", &(runtime, session))?;
        let Some(attribution) = attribution.filter(|a| a.agent.is_some()) else {
            tx.commit()?;
            return Ok(None);
        };
        let agent = attribution.agent.as_ref().expect("attributed agent");
        let retained_since = self.usage_retained_since(retained_since)?;
        // `session`'s own samples that are nobody's, or that were counted
        // through its parent while it had no record (its own record comes
        // first); and the samples of threads `session` spawned that are
        // nobody's, counted while those threads had no record.
        let mut statement = self.conn.prepare("SELECT source_id,contribution FROM usage_samples WHERE at>=?1 AND contribution IS NOT NULL AND json_extract(contribution,'$.bucket.runtime')=?2 AND ((json_extract(contribution,'$.session')=?3 AND (json_extract(contribution,'$.bucket.attribution.agent') IS NULL OR json_extract(contribution,'$.parent') IS NOT NULL)) OR (json_extract(contribution,'$.parent')=?3 AND json_extract(contribution,'$.bucket.attribution.agent') IS NULL)) AND (?4 IS NULL OR at>=?4) AND (?5 IS NULL OR at<?5) ORDER BY source_id LIMIT 256")?;
        let (from, until) = window;
        let records = statement
            .query_map(
                params![
                    text(retained_since),
                    runtime,
                    session,
                    from.map(text),
                    until.map(text)
                ],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut count = 0;
        let mut tracking_gaps = 0;
        for (id, value) in records {
            let mut contribution: Contribution = serde_json::from_str(&value)?;
            // A sample of the session's own is the session's from now on;
            // a spawned thread's keeps its parent, so the thread's own
            // record can still take it.
            let own = contribution.session == session;
            // Reconciliation moves whose the sample is, agent and project;
            // the card and roles stamped on it when it was counted (to the
            // parent's record, say) are history it keeps.
            let mut destination = contribution.bucket.clone();
            destination.attribution.agent = attribution.agent.clone();
            destination.attribution.project = attribution.project.clone();
            if destination == contribution.bucket {
                // Already this record's: only how it got here changes.
                if own && contribution.parent.take().is_some() {
                    self.usage_bounded_write(|| {
                        Ok(self.conn.execute(
                            "UPDATE usage_samples SET contribution=?1 WHERE source_id=?2",
                            params![serde_json::to_string(&contribution)?, id],
                        )?)
                    })?;
                }
                continue;
            }
            let Ok(next) = self.usage_bucket(&destination)?.add_priced(
                &contribution.counters,
                contribution.exact,
                contribution.cost_usd_micros,
            ) else {
                // An unrepresentable merge stays visibly unattributed. It must
                // not disable unrelated coordination or lose the old total.
                continue;
            };
            let previous = self
                .usage_bucket(&contribution.bucket)?
                .remove_priced(
                    &contribution.counters,
                    contribution.exact,
                    contribution.cost_usd_micros,
                )
                .map_err(anyhow::Error::msg)?;
            if self
                .usage_bounded_write(|| {
                    self.write_usage_bucket(&contribution.bucket, &previous)?;
                    contribution.bucket = destination;
                    if own {
                        contribution.parent = None;
                    }
                    self.write_usage_bucket(&contribution.bucket, &next)?;
                    self.conn.execute(
                        "UPDATE usage_samples SET contribution=?1 WHERE source_id=?2",
                        params![serde_json::to_string(&contribution)?, id],
                    )?;
                    Ok(())
                })?
                .is_some()
            {
                count += 1;
            } else {
                tracking_gaps += self.usage_tracking_gap(now)?;
            }
        }
        drop(statement);
        // A retained capacity marker already explains an unchanged refusal.
        // Reconciliation must not append empty events on every full-store tick.
        let event = if count > 0 || tracking_gaps > 0 {
            let kind = if count > 0 {
                EventKind::UsageReconciled {
                    agent: AgentId::from(agent.clone()),
                    samples: count,
                }
            } else {
                EventKind::UsageRecorded {
                    generation: self
                        .usage_collection()?
                        .and_then(|c| c.discovery_generation)
                        .unwrap_or(0),
                    samples: 0,
                    gaps: tracking_gaps,
                }
            };
            let mut event = Event::new(kind, now);
            event.seq = seq;
            self.append_event(&event)?;
            Some(event)
        } else {
            None
        };
        tx.commit()?;
        Ok(event)
    }

    pub(crate) fn usage_ingest(&self, batch: Ingest<'_>) -> Result<Vec<Event>> {
        anyhow::ensure!(
            batch.samples.len() <= 4096 && batch.gaps.len() <= 4096,
            "usage batch exceeds bounds"
        );
        let tx = write_transaction(&self.conn)?;
        let retained_since = self.usage_retained_since(batch.retained_since)?;
        let mut done = Ingested::default();
        // Reader records normally arrive in source order. Sorting this bounded
        // batch also handles per-response logs whose records were interleaved.
        let mut samples: Vec<_> = batch.samples.iter().collect();
        samples.sort_by_key(|(sample, _)| sample.at);
        for (sample, attribution) in samples {
            match self.usage_bounded_write(|| {
                self.usage_ingest_sample(sample, attribution, retained_since)
            })? {
                Some(sample) => done += sample,
                None => done.gaps += self.usage_tracking_gap(batch.now.max(sample.at))?,
            }
        }
        for gap in batch.gaps {
            let key = fingerprint(&(batch.key, &batch.progress.cursor, gap.offset, &gap.reason))?;
            done.gaps += self.usage_gap(&key, None, batch.now, None, &gap.reason)?;
        }
        if self.usage_bounded_write(|| {
            Ok(self.conn.execute("INSERT INTO usage_files VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET json=excluded.json",
                params![batch.key, serde_json::to_string(batch.progress)?])?)
        })?.is_none() {
            done.gaps += self.usage_tracking_gap(batch.now)?;
        }
        self.put_document("usage", "collection", batch.collection)?;
        if let Some(id) = batch.finished_job {
            self.usage_discovery_change(discovery::Change::FinishedJob(id))?;
        }
        self.prune_usage(batch.retained_since)?;
        let events = self.usage_events(
            true,
            batch.collection.discovery_generation.unwrap_or(0),
            done,
            batch.now,
            batch.event_seq,
        )?;
        tx.commit()?;
        Ok(events)
    }

    /// The events one ingestion commits, numbered from `seq`:
    /// `usage_recorded` when `recorded` (a collected batch always says it,
    /// since it carries scan progress), and `usage_joined` when counted
    /// responses learnt their cost or were folded into one.
    fn usage_events(
        &self,
        recorded: bool,
        generation: u64,
        done: Ingested,
        now: DateTime<Utc>,
        seq: u64,
    ) -> Result<Vec<Event>> {
        let mut events = Vec::new();
        if recorded {
            events.push(Event::new(
                EventKind::UsageRecorded {
                    generation,
                    samples: done.accepted,
                    gaps: done.gaps,
                },
                now,
            ));
        }
        if done.priced > 0 || done.folded > 0 {
            events.push(Event::new(
                EventKind::UsageJoined {
                    priced: done.priced,
                    folded: done.folded,
                },
                now,
            ));
        }
        for (offset, event) in events.iter_mut().enumerate() {
            event.seq = seq + offset as u64;
            self.append_event(event)?;
        }
        Ok(events)
    }

    /// Normalize sorted standalone metadata with the same dedupe and baseline
    /// rules as collection. This is used only by a fresh temporary Store and
    /// never writes a live daemon cursor or imports attribution by display name.
    pub(crate) fn usage_scan_ingest(
        &self,
        samples: &[(Sample, Attribution)],
        retained_since: DateTime<Utc>,
        now: DateTime<Utc>,
        seq: u64,
    ) -> Result<u64> {
        anyhow::ensure!(
            samples.len() <= 256,
            "standalone ingestion batch exceeds bounds"
        );
        let tx = write_transaction(&self.conn)?;
        let mut done = Ingested::default();
        for (sample, attribution) in samples {
            done += self
                .usage_bounded_write(|| {
                    self.usage_ingest_sample(sample, attribution, retained_since)
                })?
                .context("standalone accounting capacity exceeded; select fewer inputs")?;
        }
        self.usage_events(true, 1, done, now, seq)?;
        tx.commit()?;
        Ok(done.accepted)
    }

    /// One received telemetry export: its samples through the same dedupe,
    /// baseline and bucket rules as a collected batch, under the tracking
    /// budget, and its accounting events that could not be read (`rejected`,
    /// each a reason) as source gaps, so coverage never claims what was
    /// lost. One `usage_recorded` event when anything was counted or a new
    /// gap kept; an export that changes nothing (a replay, a rejection
    /// already on record this hour) appends none, so a noisy exporter does
    /// not crowd the event history. No file cursor moves: the events came
    /// over the wire, not from a file.
    pub(crate) fn usage_ingest_telemetry(
        &self,
        samples: &[(Sample, Attribution)],
        rejected: &[String],
        retained_since: DateTime<Utc>,
        now: DateTime<Utc>,
        seq: u64,
    ) -> Result<Vec<Event>> {
        anyhow::ensure!(
            samples.len() + rejected.len() <= agentdocker_host::usage::otlp::MAX_RECORDS,
            "telemetry batch exceeds bounds"
        );
        let tx = write_transaction(&self.conn)?;
        let retained_since = self.usage_retained_since(retained_since)?;
        let mut done = Ingested::default();
        let mut samples: Vec<_> = samples.iter().collect();
        samples.sort_by_key(|(sample, _)| sample.at);
        for (sample, attribution) in samples {
            match self.usage_bounded_write(|| {
                self.usage_ingest_sample(sample, attribution, retained_since)
            })? {
                Some(sample) => done += sample,
                None => done.gaps += self.usage_tracking_gap(now.max(sample.at))?,
            }
        }
        // One gap per reason per hour, however many events give it: the
        // reasons are the decoder's, a short fixed set.
        let hour = usage::hour(now);
        let until = hour + chrono::Duration::hours(1) - chrono::Duration::nanoseconds(1);
        let mut reasons: Vec<&str> = rejected.iter().map(String::as_str).collect();
        reasons.sort_unstable();
        reasons.dedup();
        for reason in reasons.into_iter().take(16) {
            let reason: String = reason.chars().take(200).collect();
            done.gaps += self.usage_gap(
                &format!("telemetry:{}:{reason}", text(hour)),
                Some(hour),
                until,
                None,
                &format!("telemetry event not accounted: {reason}"),
            )?;
        }
        self.prune_usage(retained_since)?;
        let generation = self
            .usage_collection()?
            .and_then(|c| c.discovery_generation)
            .unwrap_or(0);
        let events = self.usage_events(
            done.accepted > 0 || done.gaps > 0,
            generation,
            done,
            now,
            seq,
        )?;
        tx.commit()?;
        Ok(events)
    }

    /// A response already counted under another of its names: the row
    /// the alias stands for, with its contribution when still retained.
    fn usage_aliased(&self, ids: &[String]) -> Result<Option<(String, Option<String>)>> {
        for id in ids {
            if let Some(found) = self.usage_resolve(id)? {
                return Ok(Some(found));
            }
        }
        Ok(None)
    }

    /// The row that holds the accounting for `id`: its own, or the one its
    /// alias row names, followed a few steps (a row folded into another
    /// after something already named it). None when no row has the name.
    fn usage_resolve(&self, id: &str) -> Result<Option<(String, Option<String>)>> {
        let mut id = id.to_owned();
        for _ in 0..4 {
            let found: Option<(String, Option<String>)> = self
                .conn
                .query_row(
                    "SELECT fingerprint,contribution FROM usage_samples WHERE source_id=?1",
                    [&id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let Some((fingerprint, contribution)) = found else {
                return Ok(None);
            };
            match fingerprint.strip_prefix(ALIAS) {
                Some(target) => id = target.to_owned(),
                None => return Ok(Some((id, contribution))),
            }
        }
        Ok(None)
    }

    /// A response counted under its own name, met again with names it did
    /// not carry when first counted (a transcript record read before
    /// request ids were kept, read again): each becomes its alias, so the
    /// other source's record is its replay; and a response already counted
    /// under one of them (its telemetry event, which nothing tied to it
    /// then) is folded into it, so it counts once.
    fn usage_learn_aliases(
        &self,
        source_id: &str,
        aliases: &[String],
        at: DateTime<Utc>,
    ) -> Result<u64> {
        let mut folded = 0;
        for alias in aliases.iter().filter(|alias| alias.as_str() != source_id) {
            let found: Option<(String, Option<String>)> = self
                .conn
                .query_row(
                    "SELECT fingerprint,contribution FROM usage_samples WHERE source_id=?1",
                    [alias],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            match found {
                None => {
                    self.conn.execute(
                        "INSERT INTO usage_samples VALUES (?1,?2,?3,NULL)",
                        params![alias, format!("{ALIAS}{source_id}"), text(at)],
                    )?;
                }
                Some((fingerprint, _)) if fingerprint.starts_with(ALIAS) => {}
                Some((_, contribution)) => {
                    folded +=
                        u64::from(self.usage_fold(alias, contribution.as_deref(), source_id)?);
                }
            }
        }
        Ok(folded)
    }

    /// One response counted twice, under `duplicate` and under `keep`
    /// (each source's record, counted before anything tied them):
    /// `duplicate`'s contribution leaves its bucket, its price joins
    /// `keep` when `keep` has none, and `duplicate` becomes `keep`'s alias.
    /// Sums, sample counts and price move together; a bucket that cannot
    /// give the contribution back leaves both as they were, visible, rather
    /// than inventing a total.
    fn usage_fold(&self, duplicate: &str, contribution: Option<&str>, keep: &str) -> Result<bool> {
        let mut cost = None;
        if let Some(text) = contribution {
            let contribution: Contribution = serde_json::from_str(text)?;
            let Ok(remaining) = self.usage_bucket(&contribution.bucket)?.remove_priced(
                &contribution.counters,
                contribution.exact,
                contribution.cost_usd_micros,
            ) else {
                return Ok(false);
            };
            self.write_usage_bucket(&contribution.bucket, &remaining)?;
            cost = contribution.cost_usd_micros;
        }
        self.conn.execute(
            "UPDATE usage_samples SET fingerprint=?1, contribution=NULL WHERE source_id=?2",
            params![format!("{ALIAS}{keep}"), duplicate],
        )?;
        if let Some(cost) = cost
            && let Some((keep, kept)) = self.usage_resolve(keep)?
        {
            self.usage_price(&keep, kept.as_deref(), cost)?;
        }
        Ok(true)
    }

    /// A price learnt for a response counted without one (its telemetry
    /// event arrived after its transcript record): the bucket takes it,
    /// the contribution remembers it, tokens unchanged.
    fn usage_price(&self, source_id: &str, contribution: Option<&str>, cost: u64) -> Result<bool> {
        let Some(text) = contribution else {
            return Ok(false);
        };
        let mut contribution: Contribution = serde_json::from_str(text)?;
        if contribution.cost_usd_micros.is_some() {
            return Ok(false);
        }
        let Ok(priced) = self.usage_bucket(&contribution.bucket)?.price(cost) else {
            return Ok(false);
        };
        self.write_usage_bucket(&contribution.bucket, &priced)?;
        contribution.cost_usd_micros = Some(cost);
        self.conn.execute(
            "UPDATE usage_samples SET contribution=?1 WHERE source_id=?2",
            params![serde_json::to_string(&contribution)?, source_id],
        )?;
        Ok(true)
    }

    fn usage_ingest_sample(
        &self,
        sample: &Sample,
        attribution: &Attribution,
        retained_since: DateTime<Utc>,
    ) -> Result<Ingested> {
        let mut done = Ingested::default();
        // The same provider record can be encountered with or without
        // proof of the preceding file prefix. That evidence controls its
        // first accounting decision; it is not different accounting on
        // a replay. Keep compatibility with draft stores that hashed it.
        let mut accounting = sample.clone();
        accounting.proves_zero_baseline = false;
        // Whose the usage is when its session has no agent is attribution,
        // not accounting: the same provider record hashes as older builds
        // hashed it, so a copy or replay of it is never a conflict.
        accounting.parent_session = None;
        // The other names a response goes by are dedupe evidence, not
        // accounting: a record first met without them is the same record.
        accounting.aliases = Vec::new();
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
            // Met before under another source's name: a replay of that,
            // whatever this source says, except that a price it brings
            // joins the counted response.
            if let Some(target) = old.strip_prefix(ALIAS) {
                if let Some(cost) = sample.cost_usd_micros
                    && let Some((target, contribution)) =
                        self.usage_aliased(&[target.to_owned()])?
                {
                    done.priced +=
                        u64::from(self.usage_price(&target, contribution.as_deref(), cost)?);
                }
                return Ok(done);
            }
            // Its own row: a replay that may carry names it did not before.
            done.folded +=
                self.usage_learn_aliases(&sample.source_id, &sample.aliases, sample.at)?;
            if old != hash {
                let mut legacy = sample.clone();
                legacy.parent_session = None;
                if sample.semantics == Semantics::Response {
                    legacy.at = DateTime::parse_from_rfc3339(&first_at)?.with_timezone(&Utc);
                }
                legacy.proves_zero_baseline = false;
                let without_proof = fingerprint(&legacy)?;
                legacy.proves_zero_baseline = true;
                if old != without_proof && old != fingerprint(&legacy)? {
                    done.gaps += self.usage_gap(
                        &format!("conflict:{}", sample.source_id),
                        None,
                        sample.at,
                        Some((&sample.runtime, &sample.session_id)),
                        "source identity has conflicting accounting",
                    )?;
                }
            }
            return Ok(done);
        }
        // The same response already counted by another of its names: this
        // record is its replay from the other source. Remember this name
        // for it, take a price the first source did not have, count nothing.
        if let Some((target, contribution)) = self.usage_aliased(&sample.aliases)? {
            self.conn.execute(
                "INSERT INTO usage_samples VALUES (?1,?2,?3,NULL)",
                params![
                    sample.source_id,
                    format!("{ALIAS}{target}"),
                    text(sample.at)
                ],
            )?;
            if let Some(cost) = sample.cost_usd_micros {
                done.priced +=
                    u64::from(self.usage_price(&target, contribution.as_deref(), cost)?);
            }
            return Ok(done);
        }
        // A source may account for the same tokens twice (a Codex rollout:
        // the provider's count per response, and its running totals after
        // each). One session is counted by one account, the first one it is
        // seen by; a session counted by totals before accounts were kept
        // has a baseline and stays on totals, however its files move. The
        // other account's samples of that session are not counted and not
        // remembered: the same rule skips them again.
        let account = match sample.semantics {
            Semantics::Response => "responses",
            Semantics::Cumulative => "totals",
        };
        let session_key = serde_json::to_string(&(&sample.runtime, &sample.session_id))?;
        let counted_by: Option<String> = self
            .conn
            .query_row(
                "SELECT account FROM usage_accounts WHERE key=?1",
                [&session_key],
                |r| r.get(0),
            )
            .optional()?;
        let counted_by = match counted_by {
            Some(by) => Some(by),
            None => self
                .conn
                .query_row(
                    "SELECT 1 FROM usage_baselines WHERE key=?1",
                    [&session_key],
                    |_| Ok(()),
                )
                .optional()?
                .map(|()| "totals".to_owned()),
        };
        match counted_by {
            Some(by) if by != account => return Ok(Ingested::default()),
            Some(_) => {}
            None => {
                self.conn.execute(
                    "INSERT INTO usage_accounts VALUES (?1,?2)",
                    params![session_key, account],
                )?;
            }
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
                            done.gaps += self.usage_gap(
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
                        done.gaps += self.usage_gap(
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
                exact: Some(sample.semantics == Semantics::Response),
                parent: sample.parent_session.clone(),
                cost_usd_micros: sample.cost_usd_micros,
            });
        let counted = if let Some(contribution) = &contribution {
            match self.usage_bucket(&contribution.bucket)?.add_priced(
                &contribution.counters,
                contribution.exact,
                contribution.cost_usd_micros,
            ) {
                Ok(aggregate) => {
                    self.write_usage_bucket(&contribution.bucket, &aggregate)?;
                    true
                }
                Err(_) => {
                    done.gaps += self.usage_gap(
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
        // Its other names, so the other source's record is its replay.
        for alias in &sample.aliases {
            self.conn.execute(
                "INSERT OR IGNORE INTO usage_samples VALUES (?1,?2,?3,NULL)",
                params![
                    alias,
                    format!("{ALIAS}{}", sample.source_id),
                    text(sample.at)
                ],
            )?;
        }
        done.accepted = 1;
        Ok(done)
    }

    /// Everything stamped with one card over the retained window, as one
    /// report row keyed by the card: what it has cost to date. None when
    /// no retained sample was made while the card was held.
    pub(crate) fn usage_card_row(&self, card: &str) -> Result<Option<Row>> {
        let retained_since = self.usage_retained_since(DateTime::<Utc>::UNIX_EPOCH)?;
        let mut statement = self.conn.prepare(
            "SELECT json FROM usage_buckets WHERE hour>=?1 AND json_extract(key,'$.attribution.card')=?2 LIMIT 10001",
        )?;
        let mut total = Aggregate::default();
        let mut any = false;
        for (index, record) in statement
            .query_map(params![text(retained_since), card], |r| {
                r.get::<_, String>(0)
            })?
            .enumerate()
        {
            if index >= 10_000 {
                return Ok(None);
            }
            let value: Aggregate = serde_json::from_str(&record?)?;
            total = total.merge(&value).map_err(anyhow::Error::msg)?;
            any = true;
        }
        if !any {
            return Ok(None);
        }
        Ok(Some(row(Some(card.to_owned()), &total, false)))
    }

    fn usage_gap(
        &self,
        key: &str,
        since: Option<DateTime<Utc>>,
        until: DateTime<Utc>,
        session: Option<(&str, &str)>,
        reason: &str,
    ) -> Result<u64> {
        match self.usage_bounded_write(|| {
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
        })? {
            Some(count) => Ok(count),
            None => self.usage_tracking_gap(until),
        }
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

    /// A lease ends for its holder (released, lapsed, its agent gone, taken
    /// over or passed to another): when it was an exclusive hold on a board
    /// card, the interval that holder held it is kept — from the moment it
    /// took it (`Lease::held_since`, the hand-over for a lease passed on),
    /// never from the sender's claim — so usage made then is charged to the
    /// card however late its record arrives. A lapsed lease ends at its
    /// expiry, not at the sweep that removes it.
    pub(crate) fn usage_holding_ended(&self, lease: &Lease, now: DateTime<Utc>) -> Result<()> {
        if lease.mode != LeaseMode::Exclusive {
            return Ok(());
        }
        let Some(card) = lease.resource.as_str().strip_prefix("task:") else {
            return Ok(());
        };
        if !self.usage_is_card(card)? {
            return Ok(());
        }
        let since = lease.held_since();
        let until = now.min(lease.expires_at).max(since);
        self.conn.execute(
            "INSERT OR REPLACE INTO usage_holdings (agent,kind,since,until,value) VALUES (?1,?2,?3,?4,?5)",
            params![lease.holder.as_str(), HELD_CARD, text(since), text(until), card],
        )?;
        Ok(())
    }

    /// An agent's roles changed from `before` to `after` at `now`: the
    /// interval it held `before` closes and one for `after` opens. The
    /// first change kept for an agent also keeps what it held since it was
    /// registered. No role is kept as the empty label.
    pub(crate) fn usage_role_changed(
        &self,
        agent: &str,
        registered: DateTime<Utc>,
        before: Option<&str>,
        after: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let closed = self.conn.execute(
            "UPDATE usage_holdings SET until=?3 WHERE agent=?1 AND kind=?2 AND until IS NULL",
            params![agent, HELD_ROLE, text(now)],
        )?;
        if closed == 0 && registered < now {
            self.conn.execute(
                "INSERT OR REPLACE INTO usage_holdings (agent,kind,since,until,value) VALUES (?1,?2,?3,?4,?5)",
                params![
                    agent,
                    HELD_ROLE,
                    text(registered),
                    text(now),
                    before.unwrap_or_default()
                ],
            )?;
        }
        self.conn.execute(
            "INSERT OR REPLACE INTO usage_holdings (agent,kind,since,until,value) VALUES (?1,?2,?3,NULL,?4)",
            params![agent, HELD_ROLE, text(now), after.unwrap_or_default()],
        )?;
        Ok(())
    }

    /// A card `agent` held at `at` and holds no longer, with when it took
    /// it; the latest taken when it held several.
    pub(crate) fn usage_card_held(
        &self,
        agent: &str,
        at: DateTime<Utc>,
    ) -> Result<Option<(DateTime<Utc>, String)>> {
        self.conn
            .query_row(
                "SELECT since,value FROM usage_holdings WHERE agent=?1 AND kind=?2 AND since<=?3 AND until>?3 ORDER BY since DESC LIMIT 1",
                params![agent, HELD_CARD, text(at)],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?
            .map(|(since, card)| {
                Ok((
                    DateTime::parse_from_rfc3339(&since)?.with_timezone(&Utc),
                    card,
                ))
            })
            .transpose()
    }

    /// The roles `agent` held at `at` by its kept history: `Some(label)`
    /// (`None` inside for no role) once a change has been kept, `None` when
    /// its roles have never changed here, so the current ones are the ones
    /// it has always held. A time before the history begins (a transcript
    /// older than the registration) takes the first roles kept.
    pub(crate) fn usage_role_held(
        &self,
        agent: &str,
        at: DateTime<Utc>,
    ) -> Result<Option<Option<String>>> {
        let label = |value: String| (!value.is_empty()).then_some(value);
        let covering = self
            .conn
            .query_row(
                "SELECT value FROM usage_holdings WHERE agent=?1 AND kind=?2 AND since<=?3 AND (until IS NULL OR until>?3) ORDER BY since DESC LIMIT 1",
                params![agent, HELD_ROLE, text(at)],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        if let Some(value) = covering {
            return Ok(Some(label(value)));
        }
        Ok(self
            .conn
            .query_row(
                "SELECT value FROM usage_holdings WHERE agent=?1 AND kind=?2 ORDER BY since ASC LIMIT 1",
                params![agent, HELD_ROLE],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .map(label))
    }

    /// Whether `id` names a card on a board: a `task:` lease on anything
    /// else (a build campaign, say) is not a card held.
    pub(crate) fn usage_is_card(&self, id: &str) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM documents WHERE kind=?1 AND id=?2)",
            params![super::TASK_DOCUMENT, id],
            |r| r.get(0),
        )?)
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
        // Holdings that ended before the retained window can charge nothing.
        self.conn.execute("DELETE FROM usage_holdings WHERE rowid IN (SELECT rowid FROM usage_holdings WHERE until IS NOT NULL AND until<?1 LIMIT 1000)", [&since])?;
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
                Group::Card => key.attribution.card,
                Group::Role => key.attribution.role,
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
            .map(|(key, value)| row(key, &value, complete))
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
                tracking: Some(
                    self.usage_tracking_report(range.effective_since, range.effective_until)?,
                ),
            },
            overhead: self.emission_report(
                range.effective_since,
                range.effective_until,
                project,
                agent,
            )?,
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
            parent_session: None,
            cost_usd_micros: None,
            aliases: Vec::new(),
        }
    }

    fn progress(root: &Path) -> FileProgress {
        let path = root.join("empty.jsonl");
        std::fs::write(&path, "").unwrap();
        FileProgress {
            cursor: reader::Cursor::capture(&path, reader::Runtime::Codex).unwrap(),
            stop: reader::Stop::Complete,
            verified_at: None,
        }
    }

    fn ingest(
        store: &Store,
        progress: &FileProgress,
        key: &str,
        samples: Vec<(Sample, Attribution)>,
        cutoff: DateTime<Utc>,
    ) -> Result<Event> {
        store
            .usage_ingest(Ingest {
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
            .map(|events| {
                events
                    .into_iter()
                    .next()
                    .expect("a collected batch records")
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

    fn tracking_bytes(store: &Store) -> i64 {
        store
            .conn
            .query_row("SELECT bytes FROM usage_tracking", [], |r| r.get(0))
            .unwrap()
    }

    fn set_tracking_capacity(store: &Store, capacity: i64) {
        store
            .conn
            .execute("UPDATE usage_tracking SET capacity=?1", [capacity])
            .unwrap();
    }

    /// One response, two sources: the transcript record (by message id,
    /// with the request id as an alias) and the telemetry event (by request
    /// id, with the price). Whichever comes first counts it; the other adds
    /// only a price the first lacked; replays of either add nothing; and
    /// the card and role stamped at ingestion group the usage.
    #[test]
    fn a_response_reported_by_transcript_and_telemetry_counts_once_with_its_price() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().join("state.db")).unwrap();
        let progress = progress(temp.path());
        let held = Attribution {
            agent: Some("agent-a".into()),
            project: Some("project-p".into()),
            card: Some("card-1".into()),
            role: Some("reviewer".into()),
        };
        let transcript = |message: &str, request: &str| {
            let mut sample = sample(message, 3, 100, false);
            sample.runtime = "claude-code".into();
            sample.aliases = vec![request.to_owned()];
            sample
        };
        let telemetry = |request: &str, cost: u64| {
            let mut sample = sample(request, 3, 100, false);
            sample.runtime = "claude-code".into();
            sample.cost_usd_micros = Some(cost);
            sample
        };
        // Transcript first, then the event: the price joins the counted response.
        ingest(
            &store,
            &progress,
            "transcript",
            vec![(transcript("msg-1", "req-1"), held.clone())],
            at(0),
        )
        .unwrap();
        let events = store
            .usage_ingest_telemetry(
                &[(telemetry("req-1", 2_500), held.clone())],
                &[],
                at(0),
                at(20),
                store.max_event_seq().unwrap() + 1,
            )
            .unwrap();
        assert_eq!(
            events.iter().map(|e| e.kind.clone()).collect::<Vec<_>>(),
            [EventKind::UsageJoined {
                priced: 1,
                folded: 0
            }],
            "nothing new is counted; the counted response takes its cost"
        );
        let rows = report(&store, 0, 20).rows;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].samples, 1, "one response, not two");
        assert_eq!(rows[0].counters.input_tokens.sum, Some(100));
        assert_eq!(rows[0].cost_usd_micros.sum, Some(2_500));
        assert_eq!(rows[0].cost_usd_micros.known_samples, 1);
        // The event first, then the transcript record: counted once, priced.
        store
            .usage_ingest_telemetry(
                &[(telemetry("req-2", 1_000), held.clone())],
                &[],
                at(0),
                at(20),
                store.max_event_seq().unwrap() + 1,
            )
            .unwrap();
        ingest(
            &store,
            &progress,
            "transcript",
            vec![(transcript("msg-2", "req-2"), held.clone())],
            at(0),
        )
        .unwrap();
        let rows = report(&store, 0, 20).rows;
        assert_eq!(rows[0].samples, 2);
        assert_eq!(rows[0].cost_usd_micros.sum, Some(3_500));
        assert_eq!(
            rows[0].cost_usd_micros.coverage,
            usage::Coverage::Partial,
            "collection unfinished"
        );
        // Replays from both sources, in both orders, change nothing.
        for _ in 0..2 {
            ingest(
                &store,
                &progress,
                "transcript",
                vec![
                    (transcript("msg-1", "req-1"), held.clone()),
                    (transcript("msg-2", "req-2"), held.clone()),
                ],
                at(0),
            )
            .unwrap();
            let replayed = store
                .usage_ingest_telemetry(
                    &[
                        (telemetry("req-1", 2_500), held.clone()),
                        (telemetry("req-2", 1_000), held.clone()),
                    ],
                    &[],
                    at(0),
                    at(20),
                    store.max_event_seq().unwrap() + 1,
                )
                .unwrap();
            assert!(replayed.is_empty(), "a replay changes nothing to say");
        }
        let after = report(&store, 0, 20);
        assert_eq!(after.rows[0].samples, 2);
        assert_eq!(after.rows[0].cost_usd_micros.sum, Some(3_500));
        assert_eq!(after.coverage.source_gaps, 0, "a replay is not a conflict");
        // An unpriced response beside them keeps the price partial, and the
        // card and role group the same usage.
        ingest(
            &store,
            &progress,
            "transcript",
            vec![(
                transcript("msg-3", "req-3"),
                Attribution {
                    card: None,
                    ..held.clone()
                },
            )],
            at(0),
        )
        .unwrap();
        let by_card = store
            .usage_report(
                Range::new(Some(at(0)), Some(at(20)), at(23), at(0)).unwrap(),
                Group::Card,
                None,
                None,
            )
            .unwrap()
            .unwrap();
        let keys: Vec<_> = by_card.rows.iter().map(|r| r.key.clone()).collect();
        assert_eq!(keys, [None, Some("card-1".to_owned())]);
        assert_eq!(by_card.rows[1].samples, 2);
        assert_eq!(by_card.rows[1].cost_usd_micros.sum, Some(3_500));
        let by_role = store
            .usage_report(
                Range::new(Some(at(0)), Some(at(20)), at(23), at(0)).unwrap(),
                Group::Role,
                None,
                None,
            )
            .unwrap()
            .unwrap();
        assert_eq!(by_role.rows[0].key.as_deref(), Some("reviewer"));
        assert_eq!(by_role.rows[0].samples, 3);
        assert_eq!(by_role.rows[0].cost_usd_micros.known_samples, 2);
        let card = store.usage_card_row("card-1").unwrap().unwrap();
        assert_eq!(card.key.as_deref(), Some("card-1"));
        assert_eq!(card.samples, 2);
        assert_eq!(card.cost_usd_micros.sum, Some(3_500));
        assert_eq!(card.counters.input_tokens.sum, Some(200));
        assert_eq!(
            card.cache_read_share_percent, None,
            "no cache counter reported"
        );
        assert!(store.usage_card_row("card-9").unwrap().is_none());
    }

    /// A transcript record counted before it carried the request id learns
    /// it when it is read again, in either order: read again before the
    /// telemetry event, the event is its replay and brings only its cost;
    /// read again after the event was counted apart (nothing tied them
    /// then), the two counts fold into one that keeps the event's cost.
    /// Replays of everything afterwards change nothing.
    #[test]
    fn a_transcript_read_again_learns_its_request_id_and_counts_once() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().join("state.db")).unwrap();
        let progress = progress(temp.path());
        let held = Attribution {
            agent: Some("agent-a".into()),
            project: Some("project-p".into()),
            ..Attribution::default()
        };
        let transcript = |message: &str, request: Option<&str>| {
            let mut sample = sample(message, 3, 100, false);
            sample.runtime = "claude-code".into();
            sample.aliases = request.map(|r| vec![r.to_owned()]).unwrap_or_default();
            sample
        };
        let telemetry = |request: &str, cost: u64| {
            let mut sample = sample(request, 3, 100, false);
            sample.runtime = "claude-code".into();
            sample.cost_usd_micros = Some(cost);
            sample
        };
        let read = |samples: Vec<Sample>| {
            ingest(
                &store,
                &progress,
                "transcript",
                samples.into_iter().map(|s| (s, held.clone())).collect(),
                at(0),
            )
            .unwrap();
        };
        let receive = |samples: Vec<Sample>| {
            store
                .usage_ingest_telemetry(
                    &samples
                        .into_iter()
                        .map(|s| (s, held.clone()))
                        .collect::<Vec<_>>(),
                    &[],
                    at(0),
                    at(20),
                    store.max_event_seq().unwrap() + 1,
                )
                .unwrap()
        };
        let totals = || {
            let rows = report(&store, 0, 20).rows;
            assert_eq!(rows.len(), 1);
            (
                rows[0].samples,
                rows[0].counters.input_tokens.sum,
                rows[0].cost_usd_micros.sum,
            )
        };

        let kinds = |events: Vec<Event>| events.into_iter().map(|e| e.kind).collect::<Vec<_>>();

        // Read without the id, read again with it, then the event.
        read(vec![transcript("msg-1", None)]);
        read(vec![transcript("msg-1", Some("req-1"))]);
        assert_eq!(
            kinds(receive(vec![telemetry("req-1", 700)])),
            [EventKind::UsageJoined {
                priced: 1,
                folded: 0
            }],
            "the event is the record's replay and brings only its cost"
        );
        assert_eq!(totals(), (1, Some(100), Some(700)));

        // Read without the id, the event counted apart, then read again.
        read(vec![transcript("msg-2", None)]);
        assert!(matches!(
            kinds(receive(vec![telemetry("req-2", 300)])).as_slice(),
            [EventKind::UsageRecorded { samples: 1, .. }]
        ));
        assert_eq!(
            totals(),
            (3, Some(300), Some(1_000)),
            "counted twice while nothing ties them"
        );
        let refolded = store
            .usage_ingest(Ingest {
                key: "transcript",
                progress: &progress,
                samples: &[(transcript("msg-2", Some("req-2")), held.clone())],
                gaps: &[],
                collection: &Collection {
                    discovery_generation: Some(1),
                    ..Collection::default()
                },
                retained_since: at(0),
                now: at(20),
                event_seq: store.max_event_seq().unwrap() + 1,
                finished_job: None,
            })
            .unwrap();
        assert!(
            kinds(refolded).contains(&EventKind::UsageJoined {
                priced: 0,
                folded: 1
            }),
            "the fold says so"
        );
        assert_eq!(
            totals(),
            (2, Some(200), Some(1_000)),
            "folded into one, keeping the event's cost"
        );

        // Replays of every record from both sources change nothing.
        for _ in 0..2 {
            read(vec![
                transcript("msg-1", Some("req-1")),
                transcript("msg-2", Some("req-2")),
                transcript("msg-1", None),
            ]);
            assert!(receive(vec![telemetry("req-1", 700), telemetry("req-2", 300)]).is_empty());
        }
        assert_eq!(totals(), (2, Some(200), Some(1_000)));
        assert_eq!(report(&store, 0, 20).coverage.source_gaps, 0);
    }

    /// An accounting event that cannot be read is a source gap for its
    /// hour, one per reason however many events give it, and an export
    /// that changes nothing appends no event.
    #[test]
    fn rejected_telemetry_is_a_bounded_gap_and_changes_nothing_else() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().join("state.db")).unwrap();
        let reasons = vec![
            "api_request without request_id".to_owned(),
            "api_request without request_id".to_owned(),
            "api_request without session.id".to_owned(),
        ];
        let first = store
            .usage_ingest_telemetry(
                &[],
                &reasons,
                at(0),
                at(5),
                store.max_event_seq().unwrap() + 1,
            )
            .unwrap();
        assert!(matches!(
            first.as_slice(),
            [Event {
                kind: EventKind::UsageRecorded {
                    samples: 0,
                    gaps: 2,
                    ..
                },
                ..
            }]
        ));
        let again = store
            .usage_ingest_telemetry(
                &[],
                &reasons,
                at(0),
                at(5),
                store.max_event_seq().unwrap() + 1,
            )
            .unwrap();
        assert!(again.is_empty(), "the same reasons this hour add nothing");
        assert_eq!(report(&store, 0, 20).coverage.source_gaps, 2);
        assert_eq!(
            report(&store, 6, 20).coverage.source_gaps,
            0,
            "only its hour"
        );
        assert_eq!(report(&store, 5, 6).coverage.source_gaps, 2);
    }

    #[test]
    fn a_spawned_threads_samples_go_to_its_parent_until_its_own_record_takes_them() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().join("state.db")).unwrap();
        let progress = progress(temp.path());
        let to = |agent: &str| Attribution {
            agent: Some(agent.into()),
            project: None,
            ..Attribution::default()
        };
        let child = |id: &str, input: u64, attribution: Attribution| {
            let mut sample = sample(id, 10, input, false);
            sample.session_id = "child".into();
            sample.parent_session = Some("parent".into());
            (sample, attribution)
        };
        let rows = |store: &Store| -> BTreeMap<Option<String>, Option<u64>> {
            report(store, 9, 12)
                .rows
                .into_iter()
                .map(|row| (row.key, row.counters.input_tokens.sum))
                .collect()
        };
        let grouped = |store: &Store, group: Group| -> BTreeMap<Option<String>, Option<u64>> {
            store
                .usage_report(
                    Range::new(Some(at(9)), Some(at(12)), at(23), at(0)).unwrap(),
                    group,
                    None,
                    None,
                )
                .unwrap()
                .unwrap()
                .rows
                .into_iter()
                .map(|row| (row.key, row.counters.input_tokens.sum))
                .collect()
        };
        let reconcile = |session: &str, agent: &str| {
            let seq = store.max_event_seq().unwrap() + 1;
            store
                .usage_reconcile(
                    UsageReconcile {
                        runtime: "codex",
                        session,
                        attribution: Some(&to(agent)),
                        window: (None, None),
                        retained_since: at(0),
                    },
                    at(21),
                    seq,
                )
                .unwrap()
        };
        // Counted while neither thread had a record, then while only the
        // parent had one (the collector attributes those to the parent).
        ingest(
            &store,
            &progress,
            "k",
            vec![
                child("r1", 5, Attribution::default()),
                child(
                    "r2",
                    7,
                    Attribution {
                        card: Some("card-a".into()),
                        role: Some("reviewer".into()),
                        ..to("parent-agent")
                    },
                ),
            ],
            at(0),
        )
        .unwrap();
        assert_eq!(
            rows(&store),
            BTreeMap::from([(None, Some(5)), (Some("parent-agent".into()), Some(7))])
        );
        // The parent's record takes what was nobody's, once.
        assert!(reconcile("parent", "parent-agent").is_some());
        assert_eq!(
            rows(&store),
            BTreeMap::from([(Some("parent-agent".into()), Some(12))])
        );
        assert!(reconcile("parent", "parent-agent").is_none());
        // The thread's own record takes all of it from the parent's; what
        // was stamped on a sample when it was counted to the parent (the
        // card and roles held then) stays with it.
        assert!(reconcile("child", "child-agent").is_some());
        assert_eq!(
            rows(&store),
            BTreeMap::from([(Some("child-agent".into()), Some(12))])
        );
        assert_eq!(
            grouped(&store, Group::Card),
            BTreeMap::from([(None, Some(5)), (Some("card-a".into()), Some(7))])
        );
        assert_eq!(
            grouped(&store, Group::Role),
            BTreeMap::from([(None, Some(5)), (Some("reviewer".into()), Some(7))])
        );
        // ... and keeps it: neither reconciles it again.
        assert!(reconcile("parent", "parent-agent").is_none());
        assert!(reconcile("child", "child-agent").is_none());
        assert_eq!(
            rows(&store),
            BTreeMap::from([(Some("child-agent".into()), Some(12))])
        );
        // A thread's sample counted to its own record never goes to a parent.
        let mut own = sample("r3", 10, 1, false);
        own.session_id = "other-child".into();
        ingest(
            &store,
            &progress,
            "k",
            vec![(own, to("other-agent"))],
            at(0),
        )
        .unwrap();
        assert!(reconcile("parent", "parent-agent").is_none());
        assert_eq!(rows(&store)[&Some("other-agent".into())], Some(1));
    }

    #[test]
    fn a_samples_parent_thread_is_not_part_of_its_identity() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().join("state.db")).unwrap();
        let progress = progress(temp.path());
        // As an older build stored it, without the parent thread.
        let older = sample("c1", 10, 5, true);
        ingest(
            &store,
            &progress,
            "k",
            vec![(older.clone(), Attribution::default())],
            at(0),
        )
        .unwrap();
        // The same provider record read again (an archived copy), now
        // naming the thread that spawned it: a duplicate, not a conflict.
        let mut again = older;
        again.parent_session = Some("parent-thread".into());
        let event = ingest(
            &store,
            &progress,
            "k",
            vec![(again, Attribution::default())],
            at(0),
        )
        .unwrap();
        assert!(
            matches!(
                event.kind,
                EventKind::UsageRecorded {
                    samples: 0,
                    gaps: 0,
                    ..
                }
            ),
            "{:?}",
            event.kind
        );
    }

    #[test]
    fn one_session_is_counted_by_one_account_whichever_file_carries_it() {
        use agentdocker_core::usage::UsageSource;
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().join("state.db")).unwrap();
        let progress = progress(temp.path());
        let in_session = |mut sample: Sample, session: &str| {
            sample.session_id = session.into();
            sample.source_id = format!("{session}:{}", sample.source_id);
            (sample, Attribution::default())
        };
        // A new session: the response's count first, then its totals, as a
        // rollout writes them. The totals are the same tokens, not counted.
        let event = ingest(
            &store,
            &progress,
            "k",
            vec![
                in_session(sample("r1", 10, 5, false), "fresh"),
                in_session(sample("c1", 10, 5, true), "fresh"),
                in_session(sample("r2", 10, 7, false), "fresh"),
                in_session(sample("c2", 11, 12, true), "fresh"),
            ],
            at(0),
        )
        .unwrap();
        assert!(matches!(
            event.kind,
            EventKind::UsageRecorded {
                samples: 2,
                gaps: 0,
                ..
            }
        ));
        let rows = report(&store, 9, 12).rows;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].counters.input_tokens.sum, Some(12));
        assert_eq!(rows[0].usage_source, Some(UsageSource::Exact));
        // A session counted by totals before accounts were kept: its
        // baseline says so, and the response account of the same tokens,
        // arriving later from a moved or copied file, is not counted.
        store
            .conn
            .execute("DELETE FROM usage_accounts", [])
            .unwrap();
        ingest(
            &store,
            &progress,
            "k",
            vec![
                in_session(sample("c3", 10, 5, true), "older"),
                in_session(sample("c4", 11, 9, true), "older"),
            ],
            at(0),
        )
        .unwrap();
        store
            .conn
            .execute("DELETE FROM usage_accounts", [])
            .unwrap();
        ingest(
            &store,
            &progress,
            "k",
            vec![
                in_session(sample("r3", 10, 5, false), "older"),
                in_session(sample("r4", 10, 4, false), "older"),
                in_session(sample("c5", 12, 15, true), "older"),
            ],
            at(0),
        )
        .unwrap();
        let rows = report(&store, 9, 13).rows;
        assert_eq!(rows.len(), 1);
        // fresh: 12 exact; older: 4 (9-5) then 6 (15-9), estimated.
        assert_eq!(rows[0].counters.input_tokens.sum, Some(22));
        assert_eq!(rows[0].usage_source, Some(UsageSource::Estimated));
        let accounts: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM usage_accounts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            accounts, 0,
            "the older session is known by its baseline alone; no row is written for it"
        );
    }

    #[test]
    fn usage_rows_say_whether_their_figures_are_exact() {
        use agentdocker_core::usage::UsageSource;
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().join("state.db")).unwrap();
        let progress = progress(temp.path());
        let exact = Attribution {
            agent: Some("exact-agent".into()),
            project: None,
            ..Attribution::default()
        };
        let totals = Attribution {
            agent: Some("totals-agent".into()),
            project: None,
            ..Attribution::default()
        };
        // Totals of another session: one session is counted by one account.
        let of_totals = |mut sample: Sample| {
            sample.session_id = "totals-session".into();
            sample
        };
        ingest(
            &store,
            &progress,
            "k",
            vec![
                (sample("r1", 10, 5, false), exact.clone()),
                (sample("r2", 10, 7, false), exact.clone()),
                (of_totals(sample("c1", 10, 5, true)), totals.clone()),
                (of_totals(sample("c2", 11, 9, true)), totals.clone()),
            ],
            at(0),
        )
        .unwrap();
        let first = report(&store, 9, 12);
        let row = |key: &str| {
            first
                .rows
                .iter()
                .find(|row| row.key.as_deref() == Some(key))
                .unwrap()
                .clone()
        };
        assert_eq!(row("exact-agent").usage_source, Some(UsageSource::Exact));
        assert_eq!(row("exact-agent").counters.input_tokens.sum, Some(12));
        // A cumulative source's first snapshot proves nothing; the second
        // contributes the difference, an estimate.
        assert_eq!(
            row("totals-agent").usage_source,
            Some(UsageSource::Estimated)
        );
        assert_eq!(row("totals-agent").counters.input_tokens.sum, Some(4));
        // Moving an exact contribution to its agent keeps it exact, and a
        // bucket written before exactness was counted cannot say.
        ingest(
            &store,
            &progress,
            "k",
            vec![(sample("r3", 11, 1, false), Attribution::default())],
            at(0),
        )
        .unwrap();
        assert_eq!(report(&store, 9, 12).rows.len(), 3);
        let moved = Attribution {
            agent: Some("late-agent".into()),
            project: None,
            ..Attribution::default()
        };
        let seq = store.max_event_seq().unwrap() + 1;
        store
            .usage_reconcile(
                UsageReconcile {
                    runtime: "codex",
                    session: "session",
                    attribution: Some(&moved),
                    window: (None, None),
                    retained_since: at(0),
                },
                at(21),
                seq,
            )
            .unwrap();
        let moved_report = report(&store, 9, 12);
        let late = moved_report
            .rows
            .iter()
            .find(|row| row.key.as_deref() == Some("late-agent"))
            .unwrap();
        assert_eq!(late.usage_source, Some(UsageSource::Exact));
        store
            .conn
            .execute(
                "UPDATE usage_buckets SET json=json_remove(json,'$.exact') WHERE agent='late-agent'",
                [],
            )
            .unwrap();
        let older = report(&store, 9, 12);
        let late = older
            .rows
            .iter()
            .find(|row| row.key.as_deref() == Some("late-agent"))
            .unwrap();
        assert_eq!(late.usage_source, None);
        assert_eq!(late.counters.input_tokens.sum, Some(1));
    }

    #[test]
    fn usage_capacity_keeps_dedupe_and_baselines_with_one_durable_gap() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("state.db");
        let store = Store::open(&db).unwrap();
        let progress = progress(temp.path());
        let mut first = sample("first", 2, 10, true);
        first.proves_zero_baseline = true;
        ingest(
            &store,
            &progress,
            "file",
            vec![(first.clone(), Attribution::default())],
            at(0),
        )
        .unwrap();
        let bytes = tracking_bytes(&store);
        set_tracking_capacity(&store, bytes);
        for index in 0..100 {
            ingest(
                &store,
                &progress,
                &format!("copy-{index}"),
                vec![
                    (
                        sample(&format!("new-{index}"), 3, 20, true),
                        Attribution::default(),
                    ),
                    (first.clone(), Attribution::default()),
                ],
                at(0),
            )
            .unwrap();
        }
        assert_eq!(tracking_bytes(&store), bytes);
        assert_eq!(report(&store, 0, 20).coverage.source_gaps, 1);
        let tracking = report(&store, 0, 20).coverage.tracking.unwrap();
        assert_eq!(tracking.logical_bytes, bytes as u64);
        assert_eq!(tracking.capacity_bytes, bytes as u64);
        assert!(tracking.capacity_gap);
        assert_eq!(
            report(&store, 0, 20).rows[0].counters.input_tokens.sum,
            Some(10)
        );
        for table in [
            "usage_samples",
            "usage_baselines",
            "usage_files",
            "usage_gaps",
        ] {
            let count: i64 = store
                .conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 1, "{table}");
        }
        drop(store);
        let store = Store::open(&db).unwrap();
        assert_eq!(tracking_bytes(&store), bytes);
        set_tracking_capacity(&store, 1_000_000);
        ingest(
            &store,
            &progress,
            "file",
            vec![(sample("later", 4, 30, true), Attribution::default())],
            at(0),
        )
        .unwrap();
        assert_eq!(
            report(&store, 0, 20).rows[0].counters.input_tokens.sum,
            Some(30)
        );
        // The rejected cumulative record advanced neither its baseline nor
        // dedupe identity. The next accepted delta includes it exactly once.
        assert_eq!(report(&store, 0, 20).coverage.source_gaps, 1);
    }

    #[test]
    fn usage_capacity_reconciliation_preserves_unattributed_totals_and_event() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::in_memory().unwrap();
        let progress = progress(temp.path());
        ingest(
            &store,
            &progress,
            "file",
            vec![(sample("one", 2, 10, false), Attribution::default())],
            at(0),
        )
        .unwrap();
        let bytes = tracking_bytes(&store);
        set_tracking_capacity(&store, bytes);
        let attribution = Attribution {
            agent: Some("new-agent".repeat(100)),
            project: Some("project".into()),
            ..Attribution::default()
        };
        let event = store
            .usage_reconcile(
                UsageReconcile {
                    runtime: "codex",
                    session: "session",
                    attribution: Some(&attribution),
                    window: (None, None),
                    retained_since: at(0),
                },
                at(20),
                store.max_event_seq().unwrap() + 1,
            )
            .unwrap()
            .unwrap();
        assert!(matches!(
            event.kind,
            EventKind::UsageRecorded {
                samples: 0,
                gaps: 1,
                ..
            }
        ));
        assert_eq!(tracking_bytes(&store), bytes);
        let result = report(&store, 0, 20);
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0].key, None);
        assert_eq!(result.rows[0].counters.input_tokens.sum, Some(10));
        let last_event = store.max_event_seq().unwrap();
        for tick in 1..=100 {
            assert!(
                store
                    .usage_reconcile(
                        UsageReconcile {
                            runtime: "codex",
                            session: "session",
                            attribution: Some(&attribution),
                            window: (None, None),
                            retained_since: at(0),
                        },
                        at(20) + chrono::Duration::seconds(tick),
                        last_event + 1,
                    )
                    .unwrap()
                    .is_none()
            );
        }
        assert_eq!(store.max_event_seq().unwrap(), last_event);
        assert_eq!(tracking_bytes(&store), bytes);
        assert_eq!(report(&store, 0, 21).coverage.source_gaps, 1);
        set_tracking_capacity(&store, 1_000_000);
        let recovered = store
            .usage_reconcile(
                UsageReconcile {
                    runtime: "codex",
                    session: "session",
                    attribution: Some(&attribution),
                    window: (None, None),
                    retained_since: at(0),
                },
                at(21),
                store.max_event_seq().unwrap() + 1,
            )
            .unwrap()
            .unwrap();
        assert!(matches!(
            recovered.kind,
            EventKind::UsageReconciled { samples: 1, .. }
        ));
        let result = report(&store, 0, 20);
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0].key, attribution.agent);
        assert_eq!(result.rows[0].counters.input_tokens.sum, Some(10));
    }

    #[test]
    fn usage_zero_capacity_advances_no_accounting_and_never_fences_coordination() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::in_memory().unwrap();
        let progress = progress(temp.path());
        set_tracking_capacity(&store, 0);
        let event = ingest(
            &store,
            &progress,
            "file",
            vec![(sample("one", 2, 10, false), Attribution::default())],
            at(0),
        )
        .unwrap();
        assert!(matches!(
            event.kind,
            EventKind::UsageRecorded {
                samples: 0,
                gaps: 1,
                ..
            }
        ));
        assert!(store.usage_file("file").unwrap().is_none());
        assert_eq!(tracking_bytes(&store), 0);
        assert!(report(&store, 0, 20).rows.is_empty());
        assert_eq!(report(&store, 0, 20).coverage.source_gaps, 1);
        store
            .put_document("fixture", "unrelated", &"coordination still writable")
            .unwrap();
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

    /// A resumed session's samples split at the resume: each registration
    /// takes only the samples inside its window.
    #[test]
    fn usage_reconciliation_takes_only_the_samples_inside_its_window() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().join("state.db")).unwrap();
        let progress = progress(temp.path());
        ingest(
            &store,
            &progress,
            "source",
            vec![
                (sample("before", 1, 7, false), Attribution::default()),
                (sample("after", 5, 11, false), Attribution::default()),
            ],
            at(0),
        )
        .unwrap();
        let to = |agent: &str| Attribution {
            agent: Some(agent.into()),
            project: Some("project".into()),
            ..Attribution::default()
        };
        for (agent, window) in [
            ("ended", (None, Some(at(3)))),
            ("resumed", (Some(at(3)), None)),
        ] {
            store
                .usage_reconcile(
                    UsageReconcile {
                        runtime: "codex",
                        session: "session",
                        attribution: Some(&to(agent)),
                        window,
                        retained_since: at(0),
                    },
                    at(20),
                    store.max_event_seq().unwrap() + 1,
                )
                .unwrap()
                .expect("one sample moved");
        }
        let rows = report(&store, 0, 21).rows;
        let input = |agent: &str| {
            rows.iter()
                .find(|row| row.key.as_deref() == Some(agent))
                .and_then(|row| row.counters.input_tokens.sum)
        };
        assert_eq!(input("ended"), Some(7));
        assert_eq!(input("resumed"), Some(11));
        assert!(rows.iter().all(|row| row.key.is_some()), "{rows:?}");
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
            ..Attribution::default()
        };
        let before = store.max_event_seq().unwrap();
        store.conn.execute_batch("CREATE TRIGGER fail_usage_reconcile BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT,'event refused'); END;").unwrap();
        assert!(
            store
                .usage_reconcile(
                    UsageReconcile {
                        runtime: "codex",
                        session: "session",
                        attribution: Some(&attribution),
                        window: (None, None),
                        retained_since: at(0),
                    },
                    at(20),
                    before + 1,
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
                    UsageReconcile {
                        runtime: "codex",
                        session: "session",
                        attribution: Some(&attribution),
                        window: (None, None),
                        retained_since: at(0),
                    },
                    at(20),
                    before + 1,
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
                UsageReconcile {
                    runtime: "codex",
                    session: "session",
                    attribution: Some(&attribution),
                    window: (None, None),
                    retained_since: at(0),
                },
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
            ..Attribution::default()
        };
        let event = store
            .usage_reconcile(
                UsageReconcile {
                    runtime: "codex",
                    session: "session",
                    attribution: Some(&attribution),
                    window: (None, None),
                    retained_since: at(0),
                },
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
                    UsageReconcile {
                        runtime: "codex",
                        session: "session",
                        attribution: Some(&attribution),
                        window: (None, None),
                        retained_since: at(0),
                    },
                    at(20),
                    store.max_event_seq().unwrap() + 1,
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
                    UsageReconcile {
                        runtime: "codex",
                        session: "session",
                        attribution: Some(&moved),
                        window: (None, None),
                        retained_since: at(0),
                    },
                    at(20),
                    store.max_event_seq().unwrap() + 1,
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
