//! Ephemeral prefix verification across bounded passes. No transcript bytes are
//! serialized. A restart verifies the retained prefix again before parsing.
use super::*;

pub struct Preparation {
    pub ready: bool,
    pub bytes_read: u64,
}

pub struct Session {
    cursor: Cursor,
    checked: u64,
    prefix: [u8; 32],
    line: Vec<u8>,
    ready: bool,
}

impl Session {
    /// A longer generation of the same file may reuse its parser state only
    /// after every accepted byte has been hashed again. Replacement/truncation
    /// requires a fresh scan with an explicit collector gap.
    pub fn open(path: &Path, runtime: Runtime, previous: Option<&Cursor>) -> Result<Self, Error> {
        Self::at_snapshot(Cursor::capture(path, runtime)?, previous)
    }

    /// Use the collector's already captured high-water mark for this generation.
    pub fn at_snapshot(captured: Cursor, previous: Option<&Cursor>) -> Result<Self, Error> {
        let runtime = captured.runtime;
        let cursor = if let Some(previous) = previous {
            if previous.version != CURSOR_VERSION
                || previous.runtime != runtime
                || previous.offset > previous.generation.length
            {
                return Err(Error::Cursor);
            }
            if previous.generation.identity != captured.generation.identity
                || captured.generation.length < previous.generation.length
            {
                return Err(Error::Changed);
            }
            let mut cursor = previous.clone();
            if cursor.generation != captured.generation {
                cursor.quarantined_at_budget = None;
                cursor.generation = captured.generation;
            }
            cursor
        } else {
            captured
        };
        Ok(Self {
            cursor,
            checked: 0,
            prefix: [0; 32],
            line: Vec::new(),
            ready: false,
        })
    }

    /// At most the chosen byte allowance (up to 16 MiB) and a cooperative
    /// deadline. A partial pass establishes no accounting/coverage progress.
    /// The unfinished record stays only in this bounded, ephemeral buffer.
    pub fn prepare_next(
        &mut self,
        path: &Path,
        bytes: u64,
        elapsed: Duration,
    ) -> Result<Preparation, Error> {
        if bytes == 0 || bytes > MAX_BATCH || elapsed.is_zero() {
            return Err(Error::Budget);
        }
        let mut file = crate::files::open_regular(path)?;
        if Generation::capture(&file)? != self.cursor.generation {
            return Err(Error::Changed);
        }
        let start = Instant::now();
        file.seek(SeekFrom::Start(self.checked))?;
        let allowance = bytes.min(self.cursor.offset - self.checked);
        let mut reader = BufReader::with_capacity(8192, file.take(allowance));
        while self.checked < self.cursor.offset && start.elapsed() < elapsed {
            let available = reader.fill_buf()?;
            if available.is_empty() {
                break;
            }
            let count = available
                .iter()
                .position(|b| *b == b'\n')
                .map_or(available.len(), |i| i + 1);
            if self.line.len().saturating_add(count) > MAX_BATCH as usize {
                return Err(Error::ValidationIncomplete);
            }
            self.line.extend_from_slice(&available[..count]);
            reader.consume(count);
            self.checked += count as u64;
            if self.line.last() == Some(&b'\n') {
                self.prefix = record_digest(self.prefix, &self.line);
                self.line.clear();
            }
        }
        let bytes_read = allowance - reader.get_ref().limit();
        if Generation::capture(reader.get_ref().get_ref())? != self.cursor.generation
            || Generation::capture(&crate::files::open_regular(path)?)? != self.cursor.generation
        {
            return Err(Error::Changed);
        }
        self.ready = self.checked == self.cursor.offset;
        if self.ready && (!self.line.is_empty() || self.prefix != self.cursor.prefix_digest) {
            self.ready = false;
            return Err(Error::Changed);
        }
        Ok(Preparation {
            ready: self.ready,
            bytes_read,
        })
    }

    /// Parse/recheck only the new bounded suffix after a fully verified prefix.
    /// File generation checks cover both that proof and the new proposal.
    pub fn scan(&mut self, path: &Path, budget: Budget) -> Result<Batch, Error> {
        if !self.ready {
            return Err(Error::ValidationIncomplete);
        }
        let batch = scan_checked(path, self.cursor.runtime, Some(&self.cursor), budget, true)?;
        self.cursor = batch.cursor.clone();
        self.checked = self.cursor.offset;
        self.prefix = self.cursor.prefix_digest;
        Ok(batch)
    }

    /// Recheck the exact proposal immediately before its atomic commit. This
    /// proof belongs to this session; a reconstructed cursor alone is not proof.
    pub fn validate(&self, path: &Path, cursor: &Cursor) -> Result<(), Error> {
        if !self.ready || cursor != &self.cursor {
            return Err(Error::Cursor);
        }
        if Generation::capture(&crate::files::open_regular(path)?)? != cursor.generation {
            return Err(Error::Changed);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn row(id: usize) -> String {
        serde_json::json!({"type":"assistant","version":"2.1.270","sessionId":"large-session",
            "timestamp":"2026-09-18T01:00:00Z","message":{"id":format!("response-{id}"),
                "model":"fixture","content":[{"text":"private".repeat(800)}],
                "usage":{"input_tokens":2,"output_tokens":3}}})
        .to_string()
            + "\n"
    }

    fn prepare(session: &mut Session, path: &Path) -> Result<usize, Error> {
        let mut passes = 0;
        loop {
            let progress = session.prepare_next(path, 1024 * 1024, Duration::from_millis(100))?;
            assert!(progress.bytes_read <= 1024 * 1024);
            passes += 1;
            if progress.ready {
                return Ok(passes);
            }
        }
    }

    #[test]
    fn large_prefixes_resume_and_append_without_replaying_accepted_records() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("large.jsonl");
        let mut file = File::create(&path).unwrap();
        for id in 0..4000 {
            file.write_all(row(id).as_bytes()).unwrap();
        }
        drop(file);
        assert!(std::fs::metadata(&path).unwrap().len() > MAX_BATCH);
        let mut session = Session::open(&path, Runtime::Claude, None).unwrap();
        prepare(&mut session, &path).unwrap();
        let mut samples = Vec::new();
        let last = loop {
            let batch = session.scan(&path, Budget::default()).unwrap();
            assert!(batch.bytes_read <= Budget::default().bytes);
            assert!(batch.validation_bytes_read <= MAX_BATCH);
            assert!(batch.gaps.is_empty());
            session.validate(&path, &batch.cursor).unwrap();
            samples.extend(batch.samples.iter().map(|s| s.source_id.clone()));
            if batch.stop == Stop::Complete {
                break batch.cursor;
            }
        };
        assert_eq!(samples.len(), 4000);
        assert_eq!(
            samples
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            4000
        );
        assert!(
            matches!(last.validate(&path), Err(Error::ValidationIncomplete)),
            "standalone validation remains explicitly bounded"
        );
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(row(4000).as_bytes()).unwrap();
        drop(file);
        // A restart/append rebuilds the prefix proof in bounded passes.
        let persisted: Cursor =
            serde_json::from_slice(&serde_json::to_vec(&last).unwrap()).unwrap();
        let mut resumed = Session::open(&path, Runtime::Claude, Some(&persisted)).unwrap();
        assert!(matches!(
            resumed.scan(&path, Budget::default()),
            Err(Error::ValidationIncomplete)
        ));
        assert!(prepare(&mut resumed, &path).unwrap() > 16);
        let batch = resumed.scan(&path, Budget::default()).unwrap();
        assert_eq!(batch.stop, Stop::Complete);
        assert_eq!(batch.samples.len(), 1);
        assert!(!samples.contains(&batch.samples[0].source_id));
        assert!(
            !serde_json::to_string(&batch.cursor)
                .unwrap()
                .contains("private")
        );

        // A rewrite with unchanged length is found by the prefix hash. A
        // truncated file cannot reuse even a previously verified prefix.
        let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.write_all(b" ").unwrap();
        drop(file);
        let mut changed = Session::open(&path, Runtime::Claude, Some(&batch.cursor)).unwrap();
        assert!(matches!(prepare(&mut changed, &path), Err(Error::Changed)));
        assert!(matches!(
            session.validate(&path, &last),
            Err(Error::Changed)
        ));
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(5)
            .unwrap();
        assert!(matches!(
            Session::open(&path, Runtime::Claude, Some(&last)),
            Err(Error::Changed)
        ));
    }
}
