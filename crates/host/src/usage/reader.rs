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

/// Local formats supported by the accounting parsers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Runtime {
    Codex,
    Claude,
}

/// Byte reads include buffered read-ahead. Time is checked between bounded
/// regular-file reads; it is a cooperative deadline, not cancellation of I/O.
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
}

impl Cursor {
    /// Bytes covered by complete records in this file generation.
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Fixed length of the captured file generation, including a partial tail.
    pub fn captured_length(&self) -> u64 {
        self.generation.length
    }

    /// Check the proposed generation again immediately before committing it.
    /// A caller must retain old progress on failure and record a source gap.
    pub fn validate(&self, path: &Path) -> Result<(), Error> {
        let file = crate::files::open_regular(path)?;
        if self.generation != Generation::capture(&file)? {
            return Err(Error::Changed);
        }
        Ok(())
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
    #[error("usage cursor is invalid or belongs to another runtime")]
    Cursor,
    #[error("usage record at byte {offset} exceeds the configured size limit")]
    Oversized { offset: u64 },
}

/// Read complete records within byte/memory/time bounds. A failed read returns
/// no proposed progress. Retrying the same cursor returns the same source IDs;
/// the ingestion transaction, not this reader, deduplicates those IDs.
pub fn scan(
    path: &Path,
    runtime: Runtime,
    previous: Option<&Cursor>,
    budget: Budget,
) -> Result<Batch, Error> {
    if budget.record_bytes == 0
        || budget.record_bytes > MAX_RECORD
        || budget.bytes <= budget.record_bytes as u64
        || budget.bytes > MAX_BATCH
        || budget.elapsed.is_zero()
    {
        return Err(Error::Budget);
    }
    let started = Instant::now();
    let mut file = crate::files::open_regular(path)?;
    let generation = Generation::capture(&file)?;
    let mut cursor = match previous {
        Some(prior) => {
            if prior.version != 1
                || prior.runtime != runtime
                || prior.offset > prior.generation.length
            {
                return Err(Error::Cursor);
            }
            if prior.generation != generation {
                return Err(Error::Changed);
            }
            prior.clone()
        }
        None => Cursor {
            version: 1,
            runtime,
            generation: generation.clone(),
            offset: 0,
            prefix_digest: [0; 32],
            codex: Codex::default(),
        },
    };
    // Validate the restored record boundary; charge the byte to the same pass.
    let boundary_bytes = u64::from(cursor.offset > 0);
    if cursor.offset > 0 {
        file.seek(SeekFrom::Start(cursor.offset - 1))?;
        let mut byte = [0];
        file.read_exact(&mut byte)?;
        if byte != [b'\n'] {
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
        if line.len() > budget.record_bytes {
            return Err(Error::Oversized {
                offset: cursor.offset,
            });
        }
        if line.last() != Some(&b'\n') {
            break if cursor.offset + line.len() as u64 == generation.length {
                Stop::PendingTail
            } else {
                Stop::Budget
            };
        }
        let parsed = serde_json::from_slice(&line)
            .map_err(|_| "invalid JSON record".to_owned())
            .and_then(|record| match runtime {
                Runtime::Codex => cursor.codex.feed(&record, cursor.offset == 0),
                Runtime::Claude => super::claude(&record),
            });
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
        let mut digest = Sha256::new();
        digest.update(cursor.prefix_digest);
        digest.update((line.len() as u64).to_le_bytes());
        digest.update(&line);
        cursor.prefix_digest = digest.finalize().into();
        cursor.offset += line.len() as u64;
        records += 1;
    };
    let bytes_read = boundary_bytes + allowance - reader.get_ref().limit();
    // Check both the opened object and the path. A renamed/replaced file must
    // not validate the earlier path merely because its old descriptor survives.
    if generation != Generation::capture(reader.get_ref().get_ref())? {
        return Err(Error::Changed);
    }
    cursor.validate(path)?;
    Ok(Batch {
        cursor,
        samples,
        gaps,
        bytes_read,
        stop,
    })
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
        assert!(matches!(
            scan(&path, Runtime::Claude, None, budget()),
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
