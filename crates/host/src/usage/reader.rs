//! Bounded JSONL batches, with proposed cursors rather than hidden progress.
//!
//! The caller commits a returned cursor only with its samples and gaps. A
//! changed generation requires an explicit gap and a new scan; it never silently
//! carries parser state or coverage across a rewrite. Directory discovery and
//! durable ingestion belong to the collector, not this file-reading primitive.

use super::{Codex, Sample};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{self, BufRead, BufReader, Read, Seek, SeekFrom},
    path::Path,
    time::{Duration, Instant},
};

const MAX_BATCH: u64 = 16 * 1024 * 1024;
const MAX_RECORD: usize = 1024 * 1024;
const MAX_RECORDS: usize = 4096;
const PREFIX_DEADLINE: Duration = Duration::from_secs(1);

/// Local formats supported by the accounting parsers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Runtime {
    Codex,
    Claude,
}

/// Parser byte reads include buffered read-ahead. Prefix verification has a
/// separate 16 MiB / one-second bound per check, reported by the batch. Time
/// limits are cooperative deadlines, not cancellation of regular-file I/O.
#[derive(Clone, Copy, Debug)]
pub struct Budget {
    pub bytes: u64,
    pub record_bytes: usize,
    pub elapsed: Duration,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            bytes: 4 * 1024 * 1024,
            record_bytes: MAX_RECORD,
            elapsed: Duration::from_millis(100),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Generation {
    stamp: crate::files::Stamp,
    length: u64,
    #[cfg(unix)]
    identity: (u64, u64),
    #[cfg(windows)]
    identity: (u64, [u8; 16]),
}

impl Generation {
    fn capture(file: &File) -> io::Result<Self> {
        let meta = file.metadata()?;
        #[cfg(unix)]
        let identity = {
            use std::os::unix::fs::MetadataExt;
            (meta.dev(), meta.ino())
        };
        #[cfg(windows)]
        let identity = {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::Storage::FileSystem::{
                FILE_ID_INFO, FileIdInfo, GetFileInformationByHandleEx,
            };
            let mut id = FILE_ID_INFO::default();
            // SAFETY: file retains the handle; id has the information class's
            // required size and alignment. Failure never fabricates identity.
            if unsafe {
                GetFileInformationByHandleEx(
                    file.as_raw_handle(),
                    FileIdInfo,
                    (&mut id as *mut FILE_ID_INFO).cast(),
                    std::mem::size_of_val(&id) as u32,
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            (id.VolumeSerialNumber, id.FileId.Identifier)
        };
        Ok(Self {
            stamp: crate::files::stamp(file)?,
            length: meta.len(),
            identity,
        })
    }
}

/// Serializable proposal containing no raw line or transcript text. `offset`
/// is always after a complete newline, never after a partial tail. The digest
/// chains each committed complete record, independently of batch boundaries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    version: u32,
    runtime: Runtime,
    generation: Generation,
    offset: u64,
    prefix_digest: [u8; 32],
    codex: Codex,
    /// The parser byte budget that could not finish an oversized record at
    /// offset. Persist this with the complete prefix so an unchanged retry
    /// cannot loop; only a larger bounded budget may retry this generation.
    quarantined_at_budget: Option<u64>,
}

impl Cursor {
    /// Capture a file identity/high-water mark without parsing transcript bytes.
    /// Later scans must still validate that same generation and prefix.
    pub fn capture(path: &Path, runtime: Runtime) -> Result<Self, Error> {
        let file = crate::files::open_regular(path)?;
        let generation = Generation::capture(&file)?;
        if generation != Generation::capture(&crate::files::open_regular(path)?)? {
            return Err(Error::Changed);
        }
        Ok(Self {
            version: 2,
            runtime,
            generation,
            offset: 0,
            prefix_digest: [0; 32],
            codex: Codex::default(),
            quarantined_at_budget: None,
        })
    }

    /// This comparison only selects a candidate cursor. `scan` still verifies
    /// its complete prefix; matching metadata never establishes coverage.
    pub fn same_generation(&self, other: &Self) -> bool {
        self.runtime == other.runtime && self.generation == other.generation
    }

    /// Bytes covered by complete records in this file generation.
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Fixed length of the captured file generation, including a partial tail.
    pub fn captured_length(&self) -> u64 {
        self.generation.length
    }

    /// Check metadata and the complete-record prefix immediately before commit.
    /// Metadata alone cannot prove unchanged content on every filesystem. This
    /// check reads at most 16 MiB and has a cooperative one-second deadline;
    /// failure retains old progress and cannot establish coverage. It is an
    /// observation, not a lock against a writer changing the file afterward.
    pub fn validate(&self, path: &Path) -> Result<(), Error> {
        self.validate_counted(path).map(|_| ())
    }

    fn validate_counted(&self, path: &Path) -> Result<u64, Error> {
        let mut file = crate::files::open_regular(path)?;
        let bytes = self.validate_file(&mut file, PREFIX_DEADLINE)?;
        // Also check the current path: an old descriptor survives replacement.
        if self.generation != Generation::capture(&crate::files::open_regular(path)?)? {
            return Err(Error::Changed);
        }
        Ok(bytes)
    }

    fn validate_file(&self, file: &mut File, elapsed: Duration) -> Result<u64, Error> {
        if self.version != 2 || self.offset > self.generation.length {
            return Err(Error::Cursor);
        }
        if self.generation != Generation::capture(file)? {
            return Err(Error::Changed);
        }
        if self.offset > MAX_BATCH {
            return Err(Error::ValidationIncomplete);
        }
        let started = Instant::now();
        file.seek(SeekFrom::Start(0))?;
        let mut reader = BufReader::with_capacity(8192, file.take(self.offset));
        let mut prefix = [0; 32];
        let mut line = Vec::new();
        let mut covered = 0;
        while covered < self.offset {
            if started.elapsed() >= elapsed {
                return Err(Error::ValidationIncomplete);
            }
            let available = reader.fill_buf()?;
            if available.is_empty() {
                return Err(Error::Changed);
            }
            let count = available
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(available.len(), |index| index + 1);
            line.extend_from_slice(&available[..count]);
            reader.consume(count);
            covered += count as u64;
            if line.last() == Some(&b'\n') {
                prefix = record_digest(prefix, &line);
                line.clear();
            }
        }
        if !line.is_empty() || prefix != self.prefix_digest {
            return Err(Error::Changed);
        }
        if started.elapsed() >= elapsed {
            return Err(Error::ValidationIncomplete);
        }
        if self.generation != Generation::capture(reader.get_ref().get_ref())? {
            return Err(Error::Changed);
        }
        Ok(covered)
    }
}

/// Why reading stopped. Complete applies only to this captured file, never to
/// discovery, a runtime account or the collector's total coverage.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stop {
    Complete,
    Budget,
    PendingTail,
    /// The complete prefix is valid, but an oversized record could not finish
    /// within this batch's byte budget. Persist the quarantine with the cursor
    /// and samples; this does not establish coverage of the remaining file.
    Quarantined,
}

/// A complete record that could not establish supported accounting. Reasons
/// come from our parsers, never copied transcript text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gap {
    pub offset: u64,
    pub bytes: u64,
    pub reason: String,
}

#[derive(Clone, Debug)]
pub struct Batch {
    pub cursor: Cursor,
    pub samples: Vec<Sample>,
    pub gaps: Vec<Gap>,
    pub bytes_read: u64,
    /// Complete-prefix rechecks, separate from parser reads: at most 32 MiB
    /// total (previous prefix plus proposal). No transcript bytes are retained.
    pub validation_bytes_read: u64,
    pub stop: Stop,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("usage file I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("usage file generation changed; retain the previous cursor and record a source gap")]
    Changed,
    #[error("invalid usage scan budget")]
    Budget,
    #[error(
        "usage prefix validation exceeded its byte or time bound; retain the previous cursor and leave coverage incomplete"
    )]
    ValidationIncomplete,
    #[error("usage cursor is invalid or belongs to another runtime")]
    Cursor,
    #[error(
        "usage record at byte {offset} is quarantined; retry only with a larger bounded budget or a changed generation"
    )]
    Oversized { offset: u64 },
}

/// Read complete records within byte/memory/time bounds. An I/O, content or
/// validation failure returns no proposed progress. An oversized unfinished
/// record instead returns an explicit Quarantined batch containing only the
/// verified complete prefix; commit its samples and quarantined cursor together.
/// Retrying an ordinary cursor returns the same source IDs; ingestion dedupes.
pub fn scan(
    path: &Path,
    runtime: Runtime,
    previous: Option<&Cursor>,
    budget: Budget,
) -> Result<Batch, Error> {
    if budget.record_bytes == 0
        || budget.record_bytes > MAX_RECORD
        || budget.bytes <= budget.record_bytes as u64 + 1
        || budget.bytes > MAX_BATCH
        || budget.elapsed.is_zero()
    {
        return Err(Error::Budget);
    }
    let mut file = crate::files::open_regular(path)?;
    let generation = Generation::capture(&file)?;
    let mut validation_bytes_read = 0;
    let mut cursor = match previous {
        Some(prior) => {
            if prior.version != 2
                || prior.runtime != runtime
                || prior.offset > prior.generation.length
            {
                return Err(Error::Cursor);
            }
            if prior.generation != generation {
                return Err(Error::Changed);
            }
            validation_bytes_read += prior.validate_file(&mut file, PREFIX_DEADLINE)?;
            if prior
                .quarantined_at_budget
                .is_some_and(|bytes| budget.bytes <= bytes)
            {
                return Err(Error::Oversized {
                    offset: prior.offset,
                });
            }
            let mut next = prior.clone();
            next.quarantined_at_budget = None;
            next
        }
        None => Cursor {
            version: 2,
            runtime,
            generation: generation.clone(),
            offset: 0,
            prefix_digest: [0; 32],
            codex: Codex::default(),
            quarantined_at_budget: None,
        },
    };
    let started = Instant::now();
    // Validate the restored record boundary; charge the byte to the same pass.
    let boundary_bytes = u64::from(cursor.offset > 0);
    if cursor.offset > 0 {
        file.seek(SeekFrom::Start(cursor.offset - 1))?;
        let mut byte = [0];
        file.read_exact(&mut byte)?;
        if byte[0] != b'\n' {
            return Err(Error::Cursor);
        }
    }
    file.seek(SeekFrom::Start(cursor.offset))?;
    let allowance = (budget.bytes - boundary_bytes).min(generation.length - cursor.offset);
    let mut reader = BufReader::with_capacity(8192, file.take(allowance));
    let mut samples = Vec::new();
    let mut gaps = Vec::new();
    let mut line = Vec::new();
    let mut records = 0;
    let stop = loop {
        if cursor.offset == generation.length {
            break Stop::Complete;
        }
        if records == MAX_RECORDS || started.elapsed() >= budget.elapsed {
            break Stop::Budget;
        }
        line.clear();
        (&mut reader)
            .take(budget.record_bytes as u64 + 1)
            .read_until(b'\n', &mut line)?;
        let oversized = line.len() > budget.record_bytes;
        if oversized {
            // Drain through this same bounded reader, including prefetched
            // bytes. A complete oversized record is an explicit gap; an
            // unfinished record cannot authorize moving the durable cursor.
            if line.last() != Some(&b'\n') {
                reader.read_until(b'\n', &mut line)?;
            }
            if line.last() != Some(&b'\n') {
                cursor.quarantined_at_budget = Some(budget.bytes);
                break Stop::Quarantined;
            }
        }
        if line.last() != Some(&b'\n') {
            break if cursor.offset + line.len() as u64 == generation.length {
                Stop::PendingTail
            } else {
                Stop::Budget
            };
        }
        let parsed = if oversized {
            Err("record exceeds the configured size limit".to_owned())
        } else {
            serde_json::from_slice(&line)
                .map_err(|_| "invalid JSON record".to_owned())
                .and_then(|record| match runtime {
                    Runtime::Codex => cursor.codex.feed(&record, cursor.offset == 0),
                    Runtime::Claude => super::claude(&record),
                })
        };
        match parsed {
            Ok(Some(sample)) => samples.push(sample),
            Ok(None) => {}
            Err(reason) => {
                gaps.push(Gap {
                    offset: cursor.offset,
                    bytes: line.len() as u64,
                    reason,
                });
                // A malformed record could have carried new format/session
                // context. Subsequent Codex counts need supported metadata.
                cursor.codex = Codex::default();
            }
        }
        cursor.prefix_digest = record_digest(cursor.prefix_digest, &line);
        cursor.offset += line.len() as u64;
        records += 1;
    };
    let bytes_read = boundary_bytes + allowance - reader.get_ref().limit();
    // Check both the opened object and the path. A renamed/replaced file must
    // not validate the earlier path merely because its old descriptor survives.
    if generation != Generation::capture(reader.get_ref().get_ref())? {
        return Err(Error::Changed);
    }
    validation_bytes_read += cursor.validate_counted(path)?;
    Ok(Batch {
        cursor,
        samples,
        gaps,
        bytes_read,
        validation_bytes_read,
        stop,
    })
}

fn record_digest(prefix: [u8; 32], line: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(prefix);
    digest.update((line.len() as u64).to_le_bytes());
    digest.update(line);
    digest.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    fn row(id: usize) -> String {
        let mut value = json!({"type":"assistant","version":"2.1.270","sessionId":"session-a","timestamp":"2026-09-16T12:00:00Z","message":{"id":format!("message-{id}"),"model":"fixture","content":[{"text":"PRIVATE_TRANSCRIPT"}],"usage":{"input_tokens":2,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"output_tokens":3}}}).to_string();
        value.push('\n');
        value
    }

    fn budget() -> Budget {
        Budget {
            bytes: 750,
            record_bytes: 500,
            elapsed: Duration::from_secs(1),
        }
    }

    #[test]
    fn bounded_batches_resume_without_retaining_text_or_skipping_records() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log");
        let text: String = (0..12).map(row).collect();
        std::fs::write(&path, &text).unwrap();
        let mut prior = None;
        let mut ids = Vec::new();
        let final_cursor = loop {
            let batch = scan(&path, Runtime::Claude, prior.as_ref(), budget()).unwrap();
            assert!(batch.bytes_read <= budget().bytes);
            assert_eq!(
                batch.validation_bytes_read,
                batch.cursor.offset() + prior.as_ref().map_or(0, Cursor::offset)
            );
            assert!(batch.gaps.is_empty());
            assert!(batch.cursor.offset() > prior.as_ref().map_or(0, Cursor::offset));
            let replay = scan(&path, Runtime::Claude, prior.as_ref(), budget()).unwrap();
            assert_eq!(batch.cursor, replay.cursor);
            assert_eq!(batch.samples, replay.samples);
            ids.extend(batch.samples.iter().map(|s| s.source_id.clone()));
            let saved = serde_json::to_string(&batch.cursor).unwrap();
            assert!(!saved.contains("PRIVATE_TRANSCRIPT"));
            assert!(
                !serde_json::to_string(&batch.samples)
                    .unwrap()
                    .contains("PRIVATE_TRANSCRIPT")
            );
            if batch.stop == Stop::Complete {
                break batch.cursor;
            }
            assert_eq!(batch.stop, Stop::Budget);
            prior = Some(serde_json::from_str(&saved).unwrap());
        };
        assert_eq!(ids.len(), 12);
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 12);
        assert_eq!(final_cursor.offset(), text.len() as u64);
        let whole = scan(&path, Runtime::Claude, None, Budget::default()).unwrap();
        assert_eq!(whole.cursor.prefix_digest, final_cursor.prefix_digest);
        assert_eq!(
            scan(&path, Runtime::Codex, Some(&final_cursor), budget())
                .unwrap_err()
                .to_string(),
            Error::Cursor.to_string()
        );
    }

    #[test]
    fn partial_tail_is_not_covered_and_completion_requires_a_new_generation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log");
        let complete = row(0);
        let next = row(1);
        let incomplete = &next[..next.len() - 1];
        std::fs::write(&path, format!("{complete}{incomplete}")).unwrap();
        let batch = scan(&path, Runtime::Claude, None, Budget::default()).unwrap();
        assert_eq!(batch.stop, Stop::PendingTail);
        assert_eq!(batch.cursor.offset(), complete.len() as u64);
        assert_eq!(batch.samples.len(), 1);
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"\n")
            .unwrap();
        assert!(matches!(batch.cursor.validate(&path), Err(Error::Changed)));
        assert!(matches!(
            scan(&path, Runtime::Claude, Some(&batch.cursor), budget()),
            Err(Error::Changed)
        ));
        let again = scan(&path, Runtime::Claude, None, Budget::default()).unwrap();
        assert_eq!(again.stop, Stop::Complete);
        assert_eq!(again.samples.len(), 2);
        assert_eq!(batch.samples[0].source_id, again.samples[0].source_id);
    }

    #[test]
    fn replacement_truncation_and_same_length_rewrite_cannot_complete_old_coverage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log");
        std::fs::write(&path, row(0)).unwrap();
        let first = scan(&path, Runtime::Claude, None, budget()).unwrap();
        let replacement = dir.path().join("replacement");
        std::fs::write(&replacement, row(0)).unwrap();
        filetime::set_file_mtime(
            &replacement,
            filetime::FileTime::from_last_modification_time(&std::fs::metadata(&path).unwrap()),
        )
        .unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        assert!(matches!(first.cursor.validate(&path), Err(Error::Changed)));
        let second = scan(&path, Runtime::Claude, None, budget()).unwrap();
        assert_eq!(
            first.samples, second.samples,
            "copied logs retain source identity"
        );
        let modified =
            filetime::FileTime::from_last_modification_time(&std::fs::metadata(&path).unwrap());
        std::fs::write(&path, row(1)).unwrap();
        filetime::set_file_mtime(&path, modified).unwrap();
        assert!(matches!(second.cursor.validate(&path), Err(Error::Changed)));
        let third = scan(&path, Runtime::Claude, None, budget()).unwrap();
        std::fs::write(&path, b"").unwrap();
        assert!(matches!(third.cursor.validate(&path), Err(Error::Changed)));
    }

    #[test]
    fn matching_metadata_cannot_hide_a_rewritten_prefix_on_commit_or_resume() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log");
        std::fs::write(&path, format!("{}{}{}", row(0), row(1), row(2))).unwrap();
        let batch = scan(&path, Runtime::Claude, None, budget()).unwrap();
        assert_eq!(batch.stop, Stop::Budget);
        assert!(batch.cursor.offset() < batch.cursor.captured_length());
        let saved = serde_json::to_string(&batch.cursor).unwrap();
        let mut restored: Cursor = serde_json::from_str(&saved).unwrap();
        std::fs::write(&path, format!("{}{}{}", row(3), row(1), row(2))).unwrap();
        // Reproduce a filesystem reporting indistinguishable metadata on all
        // hosts, so this regression cannot pass just because Unix ctime moved.
        restored.generation = Generation::capture(&File::open(&path).unwrap()).unwrap();
        assert!(matches!(restored.validate(&path), Err(Error::Changed)));
        assert!(matches!(
            scan(&path, Runtime::Claude, Some(&restored), budget()),
            Err(Error::Changed)
        ));
        // A fresh scan remains valid and its supported source IDs are distinct.
        let fresh = scan(&path, Runtime::Claude, None, budget()).unwrap();
        fresh.cursor.validate(&path).unwrap();
        assert_ne!(fresh.samples[0].source_id, batch.samples[0].source_id);
    }

    #[test]
    fn prefix_limits_refuse_coverage_instead_of_trusting_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log");
        std::fs::write(&path, row(0)).unwrap();
        let batch = scan(&path, Runtime::Claude, None, budget()).unwrap();
        let mut file = File::open(&path).unwrap();
        assert!(matches!(
            batch.cursor.validate_file(&mut file, Duration::ZERO),
            Err(Error::ValidationIncomplete)
        ));
        batch.cursor.validate(&path).unwrap();

        // A restored cursor claiming a large prefix must fail before reading or
        // accepting it, even if the file identity/length metadata all match.
        let writer = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        writer.set_len(MAX_BATCH + 1).unwrap();
        drop(writer);
        let mut restored = batch.cursor;
        restored.generation = Generation::capture(&File::open(&path).unwrap()).unwrap();
        restored.offset = MAX_BATCH + 1;
        assert!(matches!(
            restored.validate(&path),
            Err(Error::ValidationIncomplete)
        ));
        assert!(matches!(
            scan(&path, Runtime::Claude, Some(&restored), budget()),
            Err(Error::ValidationIncomplete)
        ));
    }

    #[test]
    fn bad_records_are_explicit_gaps_and_oversized_records_cannot_advance_progress() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log");
        std::fs::write(&path, format!("PRIVATE_TRANSCRIPT\n{}", row(1))).unwrap();
        let batch = scan(&path, Runtime::Claude, None, budget()).unwrap();
        assert_eq!(batch.samples.len(), 1);
        assert_eq!(batch.gaps.len(), 1);
        assert_eq!(batch.gaps[0].offset, 0);
        assert!(
            !serde_json::to_string(&batch.gaps)
                .unwrap()
                .contains("PRIVATE_TRANSCRIPT")
        );
        std::fs::write(&path, vec![b'x'; 800]).unwrap();
        let quarantined = scan(&path, Runtime::Claude, None, budget()).unwrap();
        assert_eq!(quarantined.stop, Stop::Quarantined);
        assert_eq!(quarantined.cursor.offset(), 0);
        assert!(quarantined.samples.is_empty());
        assert!(quarantined.gaps.is_empty());
        assert!(matches!(
            scan(&path, Runtime::Claude, Some(&quarantined.cursor), budget()),
            Err(Error::Oversized { offset: 0 })
        ));
        assert!(matches!(
            scan(
                &path,
                Runtime::Claude,
                None,
                Budget {
                    bytes: 500,
                    ..budget()
                }
            ),
            Err(Error::Budget)
        ));
    }

    #[test]
    fn complete_oversized_records_are_bounded_gaps_with_resumable_following_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log");
        let mut content = vec![b'x'; MAX_RECORD + 3];
        content.push(b'\n');
        let gap_len = content.len();
        content.extend_from_slice(row(1).as_bytes());
        std::fs::write(&path, &content).unwrap();
        let all = scan(&path, Runtime::Claude, None, Budget::default()).unwrap();
        assert_eq!(all.stop, Stop::Complete);
        assert_eq!(all.samples.len(), 1);
        assert_eq!(all.gaps.len(), 1);
        assert_eq!(all.gaps[0].bytes, gap_len as u64);
        assert_eq!(all.cursor.offset, content.len() as u64);
        assert!(all.bytes_read <= Budget::default().bytes);
        assert!(
            !serde_json::to_string(&all.gaps)
                .unwrap()
                .contains(&"x".repeat(20))
        );
        let first = scan(
            &path,
            Runtime::Claude,
            None,
            Budget {
                bytes: gap_len as u64,
                ..Budget::default()
            },
        )
        .unwrap();
        assert_eq!(first.cursor.offset, gap_len as u64);
        assert!(first.samples.is_empty());
        let second = scan(
            &path,
            Runtime::Claude,
            Some(&first.cursor),
            Budget::default(),
        )
        .unwrap();
        assert_eq!(second.samples, all.samples);
        assert!(second.gaps.is_empty());
        assert_eq!(second.cursor.prefix_digest, all.cursor.prefix_digest);
        let incomplete = scan(
            &path,
            Runtime::Claude,
            None,
            Budget {
                bytes: (gap_len - 1) as u64,
                ..Budget::default()
            },
        )
        .unwrap();
        assert_eq!(incomplete.stop, Stop::Quarantined);
        assert_eq!(incomplete.cursor.offset(), 0);
        assert!(incomplete.samples.is_empty());
        let recovered = scan(
            &path,
            Runtime::Claude,
            Some(&incomplete.cursor),
            Budget::default(),
        )
        .unwrap();
        assert_eq!(recovered.stop, Stop::Complete);
        assert_eq!(recovered.cursor.prefix_digest, all.cursor.prefix_digest);
        assert_eq!(recovered.samples, all.samples);
    }

    #[test]
    fn resumed_minimum_budget_can_detect_and_finish_an_oversized_record() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log");
        let first = row(0);
        std::fs::write(&path, format!("{first}{}\n", "x".repeat(500))).unwrap();
        let prior = scan(
            &path,
            Runtime::Claude,
            None,
            Budget {
                bytes: first.len() as u64 + 2,
                record_bytes: first.len(),
                elapsed: Duration::from_secs(1),
            },
        )
        .unwrap();
        assert_eq!(prior.cursor.offset(), first.len() as u64);
        assert!(matches!(
            scan(
                &path,
                Runtime::Claude,
                Some(&prior.cursor),
                Budget {
                    bytes: 501,
                    record_bytes: 500,
                    elapsed: Duration::from_secs(1),
                }
            ),
            Err(Error::Budget)
        ));
        let next = scan(
            &path,
            Runtime::Claude,
            Some(&prior.cursor),
            Budget {
                bytes: 502,
                record_bytes: 500,
                elapsed: Duration::from_secs(1),
            },
        )
        .unwrap();
        assert_eq!(next.stop, Stop::Complete);
        assert_eq!(next.gaps.len(), 1);
        assert_eq!(next.gaps[0].offset, first.len() as u64);
        assert_eq!(next.gaps[0].bytes, 501);
    }

    #[test]
    fn oversized_tail_quarantines_with_complete_prefix_and_recovers_without_replaying_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log");
        let first = row(0);
        let limits = Budget {
            bytes: first.len() as u64 + 750,
            ..budget()
        };
        std::fs::write(&path, format!("{first}{}\n{}", "x".repeat(1000), row(1))).unwrap();
        let blocked = scan(&path, Runtime::Claude, None, limits).unwrap();
        assert_eq!(blocked.stop, Stop::Quarantined);
        assert_eq!(blocked.samples.len(), 1);
        assert_eq!(blocked.cursor.offset(), first.len() as u64);
        blocked.cursor.validate(&path).unwrap();
        let saved = serde_json::to_string(&blocked.cursor).unwrap();
        assert!(!saved.contains("PRIVATE_TRANSCRIPT"));
        let restored: Cursor = serde_json::from_str(&saved).unwrap();
        assert!(
            matches!(scan(&path, Runtime::Claude, Some(&restored), limits),
            Err(Error::Oversized { offset }) if offset == first.len() as u64)
        );
        let larger = Budget {
            bytes: 4000,
            ..budget()
        };
        let recovered = scan(&path, Runtime::Claude, Some(&restored), larger).unwrap();
        assert_eq!(recovered.stop, Stop::Complete);
        assert_eq!(recovered.samples.len(), 1);
        assert_eq!(recovered.gaps.len(), 1);
        assert_eq!(recovered.gaps[0].offset, first.len() as u64);
        assert_eq!(recovered.gaps[0].bytes, 1001);
        assert_ne!(blocked.samples[0].source_id, recovered.samples[0].source_id);
        let whole = scan(&path, Runtime::Claude, None, larger).unwrap();
        assert_eq!(whole.samples, [blocked.samples, recovered.samples].concat());
        assert_eq!(whole.cursor.prefix_digest, recovered.cursor.prefix_digest);
    }

    #[test]
    fn quarantine_retry_still_detects_a_rewritten_complete_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log");
        let first = row(0);
        let limits = Budget {
            bytes: first.len() as u64 + 750,
            ..budget()
        };
        std::fs::write(&path, format!("{first}{}\n", "x".repeat(1000))).unwrap();
        let blocked = scan(&path, Runtime::Claude, None, limits).unwrap();
        assert_eq!(blocked.stop, Stop::Quarantined);
        std::fs::write(&path, format!("{}{}\n", row(1), "x".repeat(1000))).unwrap();
        let mut restored = blocked.cursor;
        restored.generation = Generation::capture(&File::open(&path).unwrap()).unwrap();
        assert!(matches!(
            scan(&path, Runtime::Claude, Some(&restored), limits),
            Err(Error::Changed)
        ));
    }

    #[test]
    fn codex_context_survives_batches_but_not_a_skipped_malformed_record() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log");
        let metadata = json!({"type":"session_meta","payload":{"id":"thread-a","cli_version":"0.154.0","model_provider":"local"}}).to_string()+"\n";
        let context =
            json!({"type":"turn_context","payload":{"model":"fixture"}}).to_string() + "\n";
        let counts = json!({"input_tokens":5,"output_tokens":3});
        let usage = json!({"type":"event_msg","timestamp":"2026-09-16T12:00:00Z","payload":{"type":"token_count","info":{"total_token_usage":counts,"last_token_usage":counts}}}).to_string()+"\n";
        std::fs::write(&path, format!("{metadata}{context}{usage}")).unwrap();
        let limits = Budget {
            bytes: 300,
            record_bytes: 250,
            elapsed: Duration::from_secs(1),
        };
        let first = scan(&path, Runtime::Codex, None, limits).unwrap();
        assert_eq!(first.stop, Stop::Budget);
        assert!(first.samples.is_empty());
        let restored =
            serde_json::from_str(&serde_json::to_string(&first.cursor).unwrap()).unwrap();
        let second = scan(&path, Runtime::Codex, Some(&restored), limits).unwrap();
        assert_eq!(second.stop, Stop::Complete);
        assert_eq!(second.samples.len(), 1);
        assert!(second.samples[0].proves_zero_baseline);
        assert_eq!(second.samples[0].model.as_deref(), Some("fixture"));
        std::fs::write(
            &path,
            format!("{metadata}{context}PRIVATE_BAD_JSON\n{usage}"),
        )
        .unwrap();
        let invalid = scan(&path, Runtime::Codex, None, Budget::default()).unwrap();
        assert_eq!(invalid.stop, Stop::Complete);
        assert!(invalid.samples.is_empty());
        assert_eq!(
            invalid.gaps.len(),
            2,
            "counts cannot borrow context from before an unknown record"
        );
    }

    #[test]
    fn tiny_invalid_records_cannot_create_unbounded_gap_results() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log");
        std::fs::write(&path, vec![b'\n'; MAX_RECORDS + 1]).unwrap();
        let batch = scan(
            &path,
            Runtime::Claude,
            None,
            Budget {
                elapsed: Duration::from_secs(5),
                ..Budget::default()
            },
        )
        .unwrap();
        assert_eq!(batch.stop, Stop::Budget);
        assert_eq!(batch.gaps.len(), MAX_RECORDS);
        assert_eq!(batch.cursor.offset(), MAX_RECORDS as u64);
        let final_batch = scan(
            &path,
            Runtime::Claude,
            Some(&batch.cursor),
            Budget::default(),
        )
        .unwrap();
        assert_eq!(final_batch.stop, Stop::Complete);
        assert_eq!(final_batch.gaps.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_and_special_files_are_refused_without_reading_them() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        std::fs::write(&target, row(0)).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(matches!(
            scan(&link, Runtime::Claude, None, budget()),
            Err(Error::Io(_))
        ));
        let fifo = dir.path().join("fifo");
        let name = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: the fixture path is a valid NUL-terminated string.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(matches!(
            scan(&fifo, Runtime::Claude, None, budget()),
            Err(Error::Io(_))
        ));
    }
}
