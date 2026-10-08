//! Explicit, bounded transcript scans. Only normalized metadata is retained;
//! daemon homes, cursors, providers and authentication are never opened.
use crate::store::{Store, usage::Attribution};
use agentdocker_core::usage::{
    self, Coverage, Range,
    report::{Collection, CollectionState, Group, Overhead, Query, Row},
};
use agentdocker_host::usage::{
    Sample,
    discovery::{MAX_FILES, MAX_ROOTS, Root, Source, Walk},
    reader::{self, Cursor, Session, Stop},
};
use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Utc};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    path::PathBuf,
    time::{Duration, Instant},
};

#[cfg(test)]
use std::path::Path;

const MAX_INPUT_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_SAMPLES: usize = 100_000;
const MAX_METADATA_BYTES: usize = 64 * 1024 * 1024;
const MAX_GAPS: usize = 1024;
const DEADLINE: Duration = Duration::from_secs(300);

#[derive(Debug, Serialize)]
pub struct FileReport {
    pub path: PathBuf,
    pub runtime: reader::Runtime,
    pub captured_bytes: u64,
    pub complete_record_bytes: u64,
    pub stop: Stop,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub rows: Vec<Row>,
    pub by: Group,
    pub as_of: DateTime<Utc>,
    pub effective_since: DateTime<Utc>,
    pub effective_until: DateTime<Utc>,
    pub includes_current_hour: bool,
    pub future_until_clamped: bool,
    /// Coverage is limited to the explicitly selected snapshots and formats.
    pub coverage: Coverage,
    pub source_gaps: u64,
    pub files: Vec<FileReport>,
    pub scan_complete: bool,
    pub parsed_samples: usize,
    pub unique_source_records: u64,
    pub captured_bytes: u64,
    pub metadata_accounted_bytes: usize,
    pub metadata_capacity_bytes: usize,
    pub supported_formats: Vec<String>,
    pub elapsed_ms: u128,
    pub overhead: Overhead,
}

fn within(started: Instant, deadline: Duration) -> Result<()> {
    ensure!(
        started.elapsed() < deadline,
        "standalone scan exceeded its time limit; select fewer inputs"
    );
    Ok(())
}

/// Scan explicit files/directories into temporary private accounting state.
/// Missing/changing files and exhausted resource limits refuse the whole report.
/// Parser gaps and partial tails remain explicit partial coverage. Deadlines
/// are cooperative between bounded operations, not hard OS I/O cancellation.
pub fn scan(roots: Vec<Root>, query: Query) -> Result<Report> {
    scan_bounded(roots, query, MAX_INPUT_BYTES, MAX_METADATA_BYTES, DEADLINE)
}

fn scan_bounded(
    roots: Vec<Root>,
    query: Query,
    max_input: u64,
    max_metadata: usize,
    deadline: Duration,
) -> Result<Report> {
    let started = Instant::now();
    let as_of = Utc::now();
    ensure!(
        !roots.is_empty() && roots.len() <= MAX_ROOTS,
        "select one to sixteen transcript files or directories"
    );
    ensure!(
        query.agent.is_none() && query.project.is_none(),
        "standalone scans have no daemon agent or project attribution"
    );
    let since = query
        .since
        .as_deref()
        .map(|s| usage::since(s, as_of).map_err(anyhow::Error::msg))
        .transpose()?;
    Range::new(
        since,
        query.until,
        as_of,
        usage::hour(DateTime::<Utc>::MIN_UTC),
    )
    .map_err(anyhow::Error::msg)?;
    let mut sources = Vec::new();
    let mut directories = Vec::new();
    for root in roots {
        within(started, deadline)?;
        let path = std::path::absolute(&root.path)?;
        let meta =
            std::fs::symlink_metadata(&path).context("selected transcript input is unavailable")?;
        ensure!(
            !meta.file_type().is_symlink(),
            "selected transcript input must not be a symlink"
        );
        if meta.is_file() {
            sources.push(Source {
                runtime: root.runtime,
                path,
            });
        } else if meta.is_dir() {
            directories.push(Root {
                runtime: root.runtime,
                path,
            });
        } else {
            anyhow::bail!("selected transcript input must be a regular file or directory");
        }
    }
    let mut walk = Walk::new(directories).map_err(anyhow::Error::msg)?;
    while !walk.finished() {
        within(started, deadline)?;
        let page = walk.next_page();
        ensure!(
            page.gaps.is_empty(),
            "standalone input discovery is incomplete: {:?}",
            page.gaps
        );
        sources.extend(page.sources);
        ensure!(
            sources.len() <= MAX_FILES,
            "standalone scan exceeds its file limit"
        );
        ensure!(
            !page.finished || page.complete,
            "standalone input discovery is incomplete"
        );
    }
    sources.sort_by(|a, b| {
        a.path
            .cmp(&b.path)
            .then_with(|| format!("{:?}", a.runtime).cmp(&format!("{:?}", b.runtime)))
    });
    sources.dedup_by(|a, b| a.path == b.path && a.runtime == b.runtime);
    let mut captured_bytes = 0u64;
    let mut captured = Vec::new();
    for source in sources {
        within(started, deadline)?;
        let cursor = Cursor::capture(&source.path, source.runtime)?;
        captured_bytes = captured_bytes
            .checked_add(cursor.captured_length())
            .context("scan byte count overflow")?;
        ensure!(
            captured_bytes <= max_input,
            "standalone scan exceeds its input byte limit; select fewer inputs"
        );
        captured.push((source, cursor));
    }
    let mut files = Vec::new();
    let mut witnesses = Vec::new();
    let mut samples = Vec::<(Sample, [u8; 32])>::new();
    let mut metadata_bytes = 0usize;
    let mut gaps = Vec::<(String, String)>::new();
    let mut formats = BTreeSet::new();
    for (index, (source, cursor)) in captured.into_iter().enumerate() {
        let length = cursor.captured_length();
        let file_sample_start = samples.len();
        let file_gap_start = gaps.len();
        let mut session = Session::at_snapshot(cursor, None)?;
        ensure!(
            session
                .prepare_next(&source.path, 4 * 1024 * 1024, Duration::from_millis(100))?
                .ready,
            "new scan prefix was unexpectedly incomplete"
        );
        loop {
            within(started, deadline)?;
            let batch = session.scan(&source.path, reader::Budget::default())?;
            session.validate(&source.path, &batch.cursor)?;
            for sample in batch.samples {
                if sample.at > as_of {
                    gaps.push((
                        format!("future:{index}:{}", sample.source_id),
                        "source accounting timestamp is in the future".into(),
                    ));
                    ensure!(
                        gaps.len() <= MAX_GAPS,
                        "standalone scan exceeds its gap limit"
                    );
                    continue;
                }
                let encoded = serde_json::to_vec(&sample)?;
                metadata_bytes = metadata_bytes
                    .checked_add(encoded.len().saturating_mul(2).saturating_add(512))
                    .context("scan metadata size overflow")?;
                ensure!(
                    samples.len() < MAX_SAMPLES && metadata_bytes <= max_metadata,
                    "standalone scan exceeds its metadata limit; select fewer inputs"
                );
                formats.insert(sample.format.clone());
                samples.push((sample, Sha256::digest(&encoded).into()));
            }
            for gap in batch.gaps {
                gaps.push((format!("parse:{index}:{}", gap.offset), gap.reason));
                ensure!(
                    gaps.len() <= MAX_GAPS,
                    "standalone scan exceeds its gap limit"
                );
            }
            if batch.stop == Stop::Budget {
                continue;
            }
            if batch.stop != Stop::Complete {
                gaps.push((
                    format!("unfinished:{index}"),
                    format!("source stopped with {:?}", batch.stop),
                ));
                ensure!(
                    gaps.len() <= MAX_GAPS,
                    "standalone scan exceeds its gap limit"
                );
            }
            if length > 0 && samples.len() == file_sample_start && gaps.len() == file_gap_start {
                gaps.push((
                    format!("unrecognized:{index}"),
                    "no supported accounting records were observed in this nonempty input".into(),
                ));
                ensure!(
                    gaps.len() <= MAX_GAPS,
                    "standalone scan exceeds its gap limit"
                );
            }
            files.push(FileReport {
                path: source.path.clone(),
                runtime: source.runtime,
                captured_bytes: length,
                complete_record_bytes: batch.cursor.offset(),
                stop: batch.stop,
            });
            witnesses.push((source.path, session, batch.cursor));
            break;
        }
    }
    // Global ordering is necessary when copied/rotated files are supplied in
    // reverse order. At equal source identity, prefer verified start evidence.
    samples.sort_by(|(a, ah), (b, bh)| {
        a.at.cmp(&b.at)
            .then(a.source_id.cmp(&b.source_id))
            .then(b.proves_zero_baseline.cmp(&a.proves_zero_baseline))
            .then(ah.cmp(bh))
    });
    within(started, deadline)?;
    let earliest = samples
        .first()
        .map_or(as_of - chrono::Duration::days(1), |(s, _)| s.at);
    let requested_since = since.unwrap_or(earliest);
    let retained_since = usage::hour(earliest.min(requested_since).min(as_of));
    let range = Range::new(Some(requested_since), query.until, as_of, retained_since)
        .map_err(anyhow::Error::msg)?;
    let scratch = tempfile::Builder::new()
        .prefix("agentdocker-usage-scan-")
        .tempdir()?;
    let result = (|| -> Result<Report> {
        let home = scratch.path().join("state");
        agentdocker_host::dirs::ensure_private_dir(&home)?;
        let store = Store::open(&home.join("state.db"))?;
        let mut unique = 0u64;
        let mut seq = 1u64;
        for batch in samples.chunks(256) {
            within(started, deadline)?;
            let batch: Vec<_> = batch
                .iter()
                .map(|(sample, _)| (sample.clone(), Attribution::default()))
                .collect();
            unique += store.usage_scan_ingest(&batch, retained_since, as_of, seq)?;
            seq += 1;
        }
        let scan_complete = files.iter().all(|f| f.stop == Stop::Complete);
        let supported_formats: Vec<_> = formats.into_iter().collect();
        let collection = Collection {
            state: if scan_complete {
                CollectionState::CaughtUp
            } else {
                CollectionState::Scanning
            },
            discovery_generation: Some(1),
            snapshot_at: Some(as_of),
            completed_at: Some(Utc::now()),
            discovery_complete: true,
            pending_files: Some(0),
            pending_tail_files: Some(
                files.iter().filter(|f| f.stop == Stop::PendingTail).count() as u64
            ),
            scope: usage::report::Scope {
                roots: Vec::new(),
                formats: supported_formats.clone(),
            },
            ..Collection::default()
        };
        let gap_refs: Vec<_> = gaps
            .iter()
            .map(|(key, reason)| (key.as_str(), reason.as_str()))
            .collect();
        store.usage_snapshot(&collection, &gap_refs, as_of, seq, None)?;
        let mut report = store.usage_report(range, query.by, None, None)?.context(
            "standalone report exceeds its bucket or counter limit; narrow its time range",
        )?;
        ensure!(
            !report
                .coverage
                .tracking
                .as_ref()
                .is_some_and(|t| t.capacity_gap),
            "standalone accounting storage reached capacity; select fewer inputs"
        );
        let complete = scan_complete && report.coverage.source_gaps == 0 && !samples.is_empty();
        // This coverage refers to the selected file snapshots, not all provider
        // accounts or the duration remaining in the current hour.
        for row in &mut report.rows {
            for counter in [
                &mut row.counters.input_tokens,
                &mut row.counters.cache_read_input_tokens,
                &mut row.counters.cache_write_input_tokens,
                &mut row.counters.output_tokens,
                &mut row.counters.reasoning_output_tokens,
            ] {
                counter.coverage = if counter.known_samples == 0 {
                    Coverage::Unknown
                } else if complete && counter.known_samples == row.samples {
                    Coverage::Complete
                } else {
                    Coverage::Partial
                };
            }
        }
        for (path, session, cursor) in &witnesses {
            within(started, deadline)?;
            session.validate(path, cursor)?;
        }
        Ok(Report {
            rows: report.rows,
            by: report.by,
            as_of,
            effective_since: report.effective_since,
            effective_until: report.effective_until,
            includes_current_hour: report.coverage.includes_current_hour,
            future_until_clamped: report.coverage.future_until_clamped,
            coverage: if samples.is_empty() {
                Coverage::Unknown
            } else if complete {
                Coverage::Complete
            } else {
                Coverage::Partial
            },
            source_gaps: report.coverage.source_gaps,
            files,
            scan_complete,
            parsed_samples: samples.len(),
            unique_source_records: unique,
            captured_bytes,
            metadata_accounted_bytes: metadata_bytes,
            metadata_capacity_bytes: max_metadata,
            supported_formats,
            elapsed_ms: started.elapsed().as_millis(),
            overhead: Overhead::default(),
        })
    })();
    // Closing the store above before explicit removal matters on Windows too.
    // A failed cleanup must never be presented as a completed private scan.
    scratch
        .close()
        .context("standalone scan temporary-state cleanup failed")?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    fn query() -> Query {
        Query {
            by: Group::Model,
            ..Query::default()
        }
    }
    fn root(path: &Path, runtime: reader::Runtime) -> Root {
        Root {
            path: path.to_owned(),
            runtime,
        }
    }
    fn claude(id: usize) -> String {
        serde_json::json!({"type":"assistant","version":"2.1.280","sessionId":"synthetic",
        "timestamp":"2026-01-01T01:00:00Z","message":{"id":format!("response-{id}"),"model":"fixture",
        "content":[{"text":"PRIVATE-TRANSCRIPT".repeat(4000)}],"usage":{"input_tokens":2,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"output_tokens":3}}}).to_string()+"\n"
    }
    #[test]
    fn reverse_order_and_copied_codex_logs_share_one_normalized_history() {
        let temp = tempfile::tempdir().unwrap();
        let full = temp.path().join("full.jsonl");
        let later = temp.path().join("later.jsonl");
        let text = include_str!("../../host/src/usage/fixtures/codex-0.160.0.jsonl");
        std::fs::write(&full, text).unwrap();
        let rows: Vec<_> = text.lines().collect();
        std::fs::write(&later, format!("{}\n{}\n", rows[0], rows[2])).unwrap();
        let report = scan(
            vec![
                root(&later, reader::Runtime::Codex),
                root(&full, reader::Runtime::Codex),
            ],
            query(),
        )
        .unwrap();
        assert_eq!(report.unique_source_records, 2);
        assert_eq!(report.rows[0].counters.input_tokens.sum, Some(10));
        assert_eq!(report.source_gaps, 0);
        assert_eq!(report.coverage, Coverage::Complete);
        assert_eq!(std::fs::read_to_string(&full).unwrap(), text);
    }
    #[test]
    fn large_transcript_uses_bounded_passes_without_retaining_prompt_text() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("large.jsonl");
        let mut file = std::fs::File::create(&path).unwrap();
        for id in 0..270 {
            file.write_all(claude(id).as_bytes()).unwrap();
        }
        drop(file);
        let report = scan(vec![root(&path, reader::Runtime::Claude)], query()).unwrap();
        assert!(report.captured_bytes > 16 * 1024 * 1024);
        assert!(report.scan_complete);
        assert_eq!(report.rows[0].counters.input_tokens.sum, Some(540));
        assert!(report.metadata_accounted_bytes < 1024 * 1024);
        assert!(
            !serde_json::to_string(&report)
                .unwrap()
                .contains("PRIVATE-TRANSCRIPT")
        );
    }
    #[test]
    fn partial_tail_and_conflicting_response_remain_visible() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("partial.jsonl");
        let first = claude(0);
        let changed = first.replace("\"input_tokens\":2", "\"input_tokens\":4");
        std::fs::write(&path, format!("{first}{changed}{{\"unfinished\":")).unwrap();
        let report = scan(vec![root(&path, reader::Runtime::Claude)], query()).unwrap();
        assert!(!report.scan_complete);
        assert_eq!(report.unique_source_records, 1);
        assert!(report.source_gaps >= 2);
        assert_eq!(report.coverage, Coverage::Partial);
        assert_eq!(
            report.rows[0].counters.input_tokens.coverage,
            Coverage::Partial
        );
    }
    #[test]
    fn ignored_nonempty_inputs_cannot_borrow_another_files_complete_coverage() {
        let temp = tempfile::tempdir().unwrap();
        let good = temp.path().join("good.jsonl");
        let ignored = temp.path().join("ignored.jsonl");
        std::fs::write(
            &good,
            include_str!("../../host/src/usage/fixtures/codex-0.160.0.jsonl"),
        )
        .unwrap();
        std::fs::write(
            &ignored,
            "{\"type\":\"assistant\",\"message\":\"not a Codex record\"}\n",
        )
        .unwrap();
        let report = scan(
            vec![
                root(&good, reader::Runtime::Codex),
                root(&ignored, reader::Runtime::Codex),
            ],
            query(),
        )
        .unwrap();
        assert!(report.scan_complete);
        assert_eq!(report.coverage, Coverage::Partial);
        assert_eq!(report.source_gaps, 1);
        assert_eq!(report.rows[0].counters.input_tokens.sum, Some(10));
        assert_eq!(
            report.rows[0].counters.input_tokens.coverage,
            Coverage::Partial
        );
        assert_eq!(report.supported_formats, ["codex-rollout-0.160.0-v1"]);
    }

    #[test]
    fn an_empty_or_unrecognized_selection_reports_unknown_counts_not_zero() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("empty.jsonl");
        for content in ["", "{\"unrecognized\":true}\n"] {
            std::fs::write(&path, content).unwrap();
            let report = scan(vec![root(&path, reader::Runtime::Codex)], query()).unwrap();
            assert!(report.scan_complete);
            assert_eq!(report.coverage, Coverage::Unknown);
            assert!(report.rows.is_empty() && report.supported_formats.is_empty());
            assert_eq!(report.source_gaps, u64::from(!content.is_empty()));
        }
    }

    #[test]
    fn missing_input_and_exhausted_bounds_refuse_a_report() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("input.jsonl");
        assert!(scan(vec![root(&path, reader::Runtime::Claude)], query()).is_err());
        std::fs::write(&path, claude(0)).unwrap();
        for (bytes, metadata, time) in [
            (0, MAX_METADATA_BYTES, DEADLINE),
            (MAX_INPUT_BYTES, 0, DEADLINE),
            (MAX_INPUT_BYTES, MAX_METADATA_BYTES, Duration::ZERO),
        ] {
            assert!(
                scan_bounded(
                    vec![root(&path, reader::Runtime::Claude)],
                    query(),
                    bytes,
                    metadata,
                    time
                )
                .is_err()
            );
        }
    }
}
