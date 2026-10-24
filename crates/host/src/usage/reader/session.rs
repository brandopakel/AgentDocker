//! Bounded prefix verification across passes. No transcript bytes are
//! serialized. A whole proof rehashes every accepted byte; a session resuming
//! exactly the generation its cursor was proven under rehashes only the last
//! accepted record, and the collector schedules whole proofs for those files.
//!
//! A session reads within the snapshot it starts from. A writer may keep
//! appending meanwhile: the proof, the parse and the recheck before commit
//! still cover that snapshot, and the bytes past it wait for the next session.
//! Truncation, replacement or a same-length rewrite still fails the session.
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
    replay_parser: bool,
    /// The proof started at byte zero: every accepted byte is rehashed.
    whole: bool,
    /// The last complete record hashed by this proof, as an anchor.
    last_record: Option<Anchor>,
}

impl Session {
    /// A longer generation of the same file may reuse its parser state only
    /// after every accepted byte has been hashed again. Replacement/truncation
    /// requires a fresh scan with an explicit collector gap.
    pub fn open(path: &Path, runtime: Runtime, previous: Option<&Cursor>) -> Result<Self, Error> {
        Self::at_snapshot(Cursor::capture(path, runtime)?, previous)
    }

    /// Use the collector's already captured high-water mark for this generation.
    /// The proof rehashes every accepted byte of `previous`, and the session
    /// parses no further than `captured`, however far the file grows meanwhile.
    pub fn at_snapshot(captured: Cursor, previous: Option<&Cursor>) -> Result<Self, Error> {
        Self::start(captured, previous, false)
    }

    /// Like `at_snapshot`, except when `captured` reports exactly the
    /// generation `previous` was proven under: the same file identity, length
    /// and change metadata (on Unix the inode change time, which no writer can
    /// set). The proof then rehashes only the last accepted record, from its
    /// retained anchor, and must reproduce the whole-prefix digest. Any other
    /// case, including a cursor without an anchor, takes a whole proof.
    ///
    /// Metadata cannot rule out a same-length rewrite inside one timestamp
    /// tick, so this is not a substitute for a whole proof: the caller must
    /// still schedule one for every file within a bounded interval.
    pub fn at_unchanged_snapshot(
        captured: Cursor,
        previous: Option<&Cursor>,
    ) -> Result<Self, Error> {
        Self::start(captured, previous, true)
    }

    fn start(captured: Cursor, previous: Option<&Cursor>, unchanged: bool) -> Result<Self, Error> {
        let runtime = captured.runtime;
        // Versions 3 through 6 have the same prefix proof, but their parsers skipped
        // releases counted since: patch versions checked by version 6, and by
        // version 7 releases beyond the compatibility floor. Verify that proof
        // before replay; an upgrade must not hide a changed or truncated source.
        let replay_parser = previous.is_some_and(|previous| matches!(previous.version, 3..=6));
        let mut anchor = None;
        let cursor = if let Some(previous) = previous {
            if (previous.version != CURSOR_VERSION && !replay_parser)
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
            if unchanged && !replay_parser && previous.generation == captured.generation {
                anchor = previous
                    .last_record
                    .filter(|last| last.offset < previous.offset);
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
            checked: anchor.map_or(0, |last| last.offset),
            prefix: anchor.map_or([0; 32], |last| last.prefix_digest),
            line: Vec::new(),
            ready: false,
            replay_parser,
            whole: anchor.is_none(),
            last_record: None,
        })
    }

    /// Whether this session's proof rehashes every accepted byte, rather than
    /// only the last record of an unchanged generation.
    pub fn proves_whole_prefix(&self) -> bool {
        self.whole
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
        self.cursor
            .generation
            .check_within(Generation::capture(&file)?)?;
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
                self.last_record = Some(Anchor {
                    offset: self.checked - self.line.len() as u64,
                    prefix_digest: self.prefix,
                });
                self.prefix = record_digest(self.prefix, &self.line);
                self.line.clear();
            }
        }
        let bytes_read = allowance - reader.get_ref().limit();
        self.cursor
            .generation
            .check_within(Generation::capture(reader.get_ref().get_ref())?)?;
        self.cursor
            .generation
            .check_within(Generation::capture(&crate::files::open_regular(path)?)?)?;
        self.ready = self.checked == self.cursor.offset;
        if self.ready && (!self.line.is_empty() || self.prefix != self.cursor.prefix_digest) {
            self.ready = false;
            return Err(Error::Changed);
        }
        if self.ready {
            // The proof hashed at least the last record whenever the prefix is
            // not empty, so this anchor matches the one the scan retained, and
            // a cursor written before anchors existed gains one here.
            self.cursor.last_record = self.last_record;
        }
        if self.ready && self.replay_parser {
            self.cursor.version = CURSOR_VERSION;
            self.cursor.offset = 0;
            self.cursor.prefix_digest = [0; 32];
            self.cursor.codex = Codex::default();
            self.cursor.quarantined_at_budget = None;
            self.cursor.last_record = None;
            self.checked = 0;
            self.prefix = [0; 32];
            self.replay_parser = false;
        }
        Ok(Preparation {
            ready: self.ready,
            bytes_read,
        })
    }

    /// Next source offset after preparation, including a verified parser replay.
    pub fn offset(&self) -> u64 {
        self.cursor.offset()
    }

    /// Parse/recheck only the new bounded suffix after a fully verified prefix,
    /// up to the session's snapshot: a record the snapshot cuts mid-line is a
    /// pending tail, even if the writer has finished it since. File generation
    /// checks cover both that proof and the new proposal.
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
        cursor
            .generation
            .check_within(Generation::capture(&crate::files::open_regular(path)?)?)?;
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
    fn a_session_reads_within_its_snapshot_while_the_file_grows() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("growing.jsonl");
        std::fs::write(&path, row(0)).unwrap();
        let accepted = scan(&path, Runtime::Claude, None, Budget::default()).unwrap();
        let append = |text: &str| {
            std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap()
                .write_all(text.as_bytes())
                .unwrap();
        };
        let through = |ids: std::ops::Range<usize>| ids.map(|id| row(id).len() as u64).sum::<u64>();

        // The writer appends during every stage: the proof, the parse and the
        // recheck before commit. Each keeps to the snapshot the session took.
        append(&row(1));
        let mut session = Session::open(&path, Runtime::Claude, Some(&accepted.cursor)).unwrap();
        append(&row(2));
        prepare(&mut session, &path).unwrap();
        append(&row(3));
        let proposal = session.scan(&path, Budget::default()).unwrap();
        assert_eq!(proposal.stop, Stop::Complete);
        assert_eq!(proposal.samples.len(), 1);
        assert_eq!(proposal.cursor.offset(), through(0..2));
        assert_eq!(proposal.bytes_read, 1 + row(1).len() as u64);
        append(&row(4));
        session.validate(&path, &proposal.cursor).unwrap();

        // The next session takes what was appended since, once, after proving
        // the whole prefix again: growth moved the metadata. A record its
        // snapshot cuts mid-line stays pending and unparsed, though the writer
        // finishes it straight away; the session after that takes it whole.
        let cut = row(6).len() / 2;
        append(&row(5));
        append(&row(6)[..cut]);
        let mut next = Session::open(&path, Runtime::Claude, Some(&proposal.cursor)).unwrap();
        assert!(next.proves_whole_prefix());
        append(&row(6)[cut..]);
        assert_eq!(prove(&mut next, &path).unwrap(), through(0..2));
        let batch = next.scan(&path, Budget::default()).unwrap();
        assert_eq!(batch.stop, Stop::PendingTail);
        assert_eq!(batch.samples.len(), 4);
        assert_eq!(batch.cursor.offset(), through(0..6));
        next.validate(&path, &batch.cursor).unwrap();
        let (last, samples) = collect(&path, Some(&batch.cursor));
        assert_eq!(samples, 1);
        assert_eq!(last.offset(), through(0..7));

        // Growth never certifies an append-only writer. A changed accepted
        // record, even alongside an append, fails the fresh prefix proof.
        let rewrite = |rows: &[usize]| {
            std::fs::write(&path, rows.iter().map(|id| row(*id)).collect::<String>()).unwrap();
        };
        rewrite(&[9, 1, 2, 3, 4]);
        let mut changed = Session::open(&path, Runtime::Claude, Some(&accepted.cursor)).unwrap();
        assert!(matches!(prepare(&mut changed, &path), Err(Error::Changed)));

        // One that lands after a session read the prefix is past that session:
        // its digests matched what it read, and growth hides the rewrite from
        // metadata. The cursor it proposes keeps its snapshot's generation,
        // which a grown file never reports again, so the next session takes a
        // whole proof, and that proof finds the rewrite.
        rewrite(&[0, 1]);
        let (cursor, _) = collect(&path, None);
        append(&row(2));
        let mut session = Session::open(&path, Runtime::Claude, Some(&cursor)).unwrap();
        prepare(&mut session, &path).unwrap();
        let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.write_all(row(8).as_bytes()).unwrap();
        drop(file);
        append(&row(3));
        let proposal = session.scan(&path, Budget::default()).unwrap();
        assert_eq!(proposal.samples.len(), 1);
        session.validate(&path, &proposal.cursor).unwrap();
        let mut next = Session::open(&path, Runtime::Claude, Some(&proposal.cursor)).unwrap();
        assert!(next.proves_whole_prefix());
        assert!(matches!(prepare(&mut next, &path), Err(Error::Changed)));

        // Truncation, replacement and a same-length rewrite during a session
        // still fail it: none of them is growth.
        let session_at = |rows: &[usize]| {
            rewrite(rows);
            let (cursor, _) = collect(&path, None);
            append(&row(2));
            let mut session = Session::open(&path, Runtime::Claude, Some(&cursor)).unwrap();
            prepare(&mut session, &path).unwrap();
            session
        };
        let mut session = session_at(&[0, 1]);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(row(0).len() as u64)
            .unwrap();
        assert!(matches!(
            session.scan(&path, Budget::default()),
            Err(Error::Changed)
        ));
        let mut session = session_at(&[0, 1]);
        let replacement = temp.path().join("replacement.jsonl");
        std::fs::write(&replacement, [0, 1, 2, 3].map(row).concat()).unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        assert!(matches!(
            session.scan(&path, Budget::default()),
            Err(Error::Changed)
        ));
        let mut session = session_at(&[0, 1]);
        let proposal = session.scan(&path, Budget::default()).unwrap();
        let length = std::fs::metadata(&path).unwrap().len();
        let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.write_all(row(8).as_bytes()).unwrap();
        drop(file);
        filetime::set_file_mtime(&path, filetime::FileTime::from_unix_time(1, 0)).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), length);
        assert!(matches!(
            session.validate(&path, &proposal.cursor),
            Err(Error::Changed)
        ));
    }

    #[test]
    fn parser_upgrade_replays_only_after_the_old_prefix_is_verified() {
        for version in [3, 4, 5, 6] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("upgrade.jsonl");
            std::fs::write(&path, format!("{}{}", row(0), row(1))).unwrap();
            let original = scan(&path, Runtime::Claude, None, Budget::default()).unwrap();
            let mut old = original.cursor.clone();
            old.version = version;
            let mut session = Session::open(&path, Runtime::Claude, Some(&old)).unwrap();
            assert_eq!(session.offset(), old.offset());
            prepare(&mut session, &path).unwrap();
            assert_eq!(session.offset(), 0);
            let replay = session.scan(&path, Budget::default()).unwrap();
            assert_eq!(replay.samples, original.samples);
            assert_eq!(replay.cursor, original.cursor);
            let mut future = old.clone();
            future.version = CURSOR_VERSION + 1;
            assert!(matches!(
                Session::open(&path, Runtime::Claude, Some(&future)),
                Err(Error::Cursor)
            ));

            // Matching length and file identity do not establish the old prefix.
            std::fs::write(&path, format!("{}{}", row(2), row(1))).unwrap();
            let mut changed = Session::open(&path, Runtime::Claude, Some(&old)).unwrap();
            let verification = prepare(&mut changed, &path);
            assert!(
                matches!(verification, Err(Error::Changed)),
                "rewritten prefix result: {verification:?}"
            );
            assert!(matches!(
                changed.scan(&path, Budget::default()),
                Err(Error::ValidationIncomplete)
            ));
            std::fs::write(&path, row(0)).unwrap();
            assert!(matches!(
                Session::open(&path, Runtime::Claude, Some(&old)),
                Err(Error::Changed)
            ));
        }
    }

    #[test]
    fn a_v5_codex_cursor_replays_previously_unsupported_0160_records() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("codex.jsonl");
        std::fs::write(&path, include_str!("../fixtures/codex-0.160.0.jsonl")).unwrap();
        let current = scan(&path, Runtime::Codex, None, Budget::default()).unwrap();
        assert_eq!(current.samples.len(), 2);
        let mut old = current.cursor.clone();
        old.version = 5;
        old.codex = Codex::default(); // v5 rejected the session metadata.
        let mut session = Session::open(&path, Runtime::Codex, Some(&old)).unwrap();
        prepare(&mut session, &path).unwrap();
        assert_eq!(session.offset(), 0);
        let replay = session.scan(&path, Budget::default()).unwrap();
        assert_eq!(replay.samples, current.samples);
        assert_eq!(replay.cursor, current.cursor);
        session.validate(&path, &replay.cursor).unwrap();
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
        // The session whose snapshot is the current length refuses the
        // rewrite on its recheck. The first session's snapshot has since been
        // outgrown, which hides the rewrite from its metadata; a later proof,
        // like the one above, is what finds it.
        filetime::set_file_mtime(&path, filetime::FileTime::from_unix_time(1, 0)).unwrap();
        assert!(matches!(
            resumed.validate(&path, &batch.cursor),
            Err(Error::Changed)
        ));
        session.validate(&path, &last).unwrap();
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

    /// Prove a session's prefix, returning the bytes it rehashed.
    fn prove(session: &mut Session, path: &Path) -> Result<u64, Error> {
        let mut bytes = 0;
        for _ in 0..64 {
            let progress = session.prepare_next(path, 1024 * 1024, Duration::from_millis(100))?;
            bytes += progress.bytes_read;
            if progress.ready {
                return Ok(bytes);
            }
        }
        panic!("a bounded fixture proof did not finish")
    }

    /// Parse a file to its end the way the collector does, returning the
    /// retained cursor and the number of samples.
    fn collect(path: &Path, previous: Option<&Cursor>) -> (Cursor, usize) {
        let mut session = Session::open(path, Runtime::Claude, previous).unwrap();
        prove(&mut session, path).unwrap();
        let mut samples = 0;
        for _ in 0..256 {
            let batch = session.scan(path, Budget::default()).unwrap();
            session.validate(path, &batch.cursor).unwrap();
            samples += batch.samples.len();
            if batch.stop == Stop::Complete {
                return (batch.cursor, samples);
            }
        }
        panic!("a bounded fixture scan did not finish")
    }

    #[test]
    fn an_unchanged_generation_rehashes_only_its_last_record() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("unchanged.jsonl");
        let text: String = (0..400).map(row).collect();
        std::fs::write(&path, &text).unwrap();
        let (cursor, samples) = collect(&path, None);
        assert_eq!(samples, 400);
        // The anchor is retained in the durable cursor, never transcript text.
        let saved = serde_json::to_string(&cursor).unwrap();
        assert!(!saved.contains("private"));
        let restored: Cursor = serde_json::from_str(&saved).unwrap();
        assert_eq!(restored, cursor);

        let captured = Cursor::capture(&path, Runtime::Claude).unwrap();
        let mut session =
            Session::at_unchanged_snapshot(captured.clone(), Some(&restored)).unwrap();
        assert!(!session.proves_whole_prefix());
        assert_eq!(prove(&mut session, &path).unwrap(), row(399).len() as u64);
        let batch = session.scan(&path, Budget::default()).unwrap();
        assert_eq!(batch.stop, Stop::Complete);
        assert!(batch.samples.is_empty() && batch.gaps.is_empty());
        assert_eq!(batch.bytes_read, 1, "only the restored boundary byte");
        assert_eq!(batch.validation_bytes_read, 0);
        assert_eq!(batch.cursor, cursor);
        session.validate(&path, &batch.cursor).unwrap();

        // The ordinary constructor still rehashes every accepted byte.
        let mut whole = Session::at_snapshot(captured, Some(&restored)).unwrap();
        assert!(whole.proves_whole_prefix());
        assert_eq!(prove(&mut whole, &path).unwrap(), text.len() as u64);
    }

    #[test]
    fn an_append_takes_a_whole_proof_and_parses_only_the_new_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("growing.jsonl");
        let text: String = (0..50).map(row).collect();
        std::fs::write(&path, &text).unwrap();
        let (cursor, _) = collect(&path, None);
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(row(50).as_bytes())
            .unwrap();
        let captured = Cursor::capture(&path, Runtime::Claude).unwrap();
        assert!(!cursor.same_generation(&captured));
        let mut session = Session::at_unchanged_snapshot(captured, Some(&cursor)).unwrap();
        assert!(
            session.proves_whole_prefix(),
            "growth changes the metadata, so it is no evidence for the old prefix"
        );
        assert_eq!(prove(&mut session, &path).unwrap(), text.len() as u64);
        let batch = session.scan(&path, Budget::default()).unwrap();
        assert_eq!(batch.stop, Stop::Complete);
        assert_eq!(batch.samples.len(), 1);
        assert_eq!(batch.bytes_read, 1 + row(50).len() as u64);
        assert_eq!(batch.validation_bytes_read, row(50).len() as u64);
        // The new cursor anchors on the appended record, so the next unchanged
        // generation rehashes that record alone.
        let captured = Cursor::capture(&path, Runtime::Claude).unwrap();
        let mut next = Session::at_unchanged_snapshot(captured, Some(&batch.cursor)).unwrap();
        assert!(!next.proves_whole_prefix());
        assert_eq!(prove(&mut next, &path).unwrap(), row(50).len() as u64);
    }

    #[test]
    fn unchanged_metadata_cannot_hide_truncation_replacement_or_a_changed_last_record() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("source.jsonl");
        let write = |rows: &[usize]| {
            std::fs::write(&path, rows.iter().map(|id| row(*id)).collect::<String>()).unwrap();
        };
        write(&[0, 1, 2]);
        let (cursor, _) = collect(&path, None);
        let unchanged = |cursor: &Cursor| {
            Session::at_unchanged_snapshot(
                Cursor::capture(&path, Runtime::Claude).unwrap(),
                Some(cursor),
            )
        };

        // A rewrite that moves the change metadata takes a whole proof, which
        // finds it. (Set the time: a coarse clock could repeat the old one.)
        write(&[9, 1, 2]);
        filetime::set_file_mtime(&path, filetime::FileTime::from_unix_time(1, 0)).unwrap();
        let mut rewritten = unchanged(&cursor).unwrap();
        assert!(rewritten.proves_whole_prefix());
        assert!(matches!(prove(&mut rewritten, &path), Err(Error::Changed)));

        // A filesystem reporting indistinguishable metadata for a rewrite of
        // the last record: the last-record proof still fails.
        write(&[0, 1, 9]);
        let mut forged = cursor.clone();
        forged.generation = Generation::capture(&File::open(&path).unwrap()).unwrap();
        let mut tail = unchanged(&forged).unwrap();
        assert!(!tail.proves_whole_prefix());
        assert!(matches!(prove(&mut tail, &path), Err(Error::Changed)));

        // The same for an earlier record is the case metadata cannot reveal:
        // the last-record proof passes, and only a whole proof finds it. The
        // collector schedules one for every unchanged file within a bound.
        write(&[9, 1, 2]);
        let mut forged = cursor.clone();
        forged.generation = Generation::capture(&File::open(&path).unwrap()).unwrap();
        prove(&mut unchanged(&forged).unwrap(), &path).unwrap();
        let mut whole = Session::at_snapshot(
            Cursor::capture(&path, Runtime::Claude).unwrap(),
            Some(&forged),
        )
        .unwrap();
        assert!(matches!(prove(&mut whole, &path), Err(Error::Changed)));

        // Truncation and replacement fail before any byte is read.
        write(&[0, 1, 2]);
        let (cursor, _) = collect(&path, None);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(row(0).len() as u64)
            .unwrap();
        assert!(matches!(unchanged(&cursor), Err(Error::Changed)));
        write(&[0, 1, 2]);
        let (cursor, _) = collect(&path, None);
        let replacement = temp.path().join("replacement.jsonl");
        std::fs::write(&replacement, [0, 1, 2].map(row).concat()).unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        assert!(matches!(unchanged(&cursor), Err(Error::Changed)));
    }

    #[test]
    fn a_cursor_without_an_anchor_takes_one_whole_proof_and_gains_its_anchor() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("older.jsonl");
        let text: String = (0..20).map(row).collect();
        std::fs::write(&path, &text).unwrap();
        let (cursor, _) = collect(&path, None);
        let mut value = serde_json::to_value(&cursor).unwrap();
        assert!(
            value
                .as_object_mut()
                .unwrap()
                .remove("last_record")
                .is_some()
        );
        let older: Cursor = serde_json::from_value(value).unwrap();
        assert_ne!(older, cursor);
        let captured = Cursor::capture(&path, Runtime::Claude).unwrap();
        let mut session = Session::at_unchanged_snapshot(captured, Some(&older)).unwrap();
        assert!(session.proves_whole_prefix());
        assert_eq!(prove(&mut session, &path).unwrap(), text.len() as u64);
        let batch = session.scan(&path, Budget::default()).unwrap();
        assert_eq!(batch.cursor, cursor, "the proof restores the anchor");
        // Empty prefixes carry no anchor and need no bytes.
        let empty = temp.path().join("empty.jsonl");
        std::fs::write(&empty, "").unwrap();
        let (cursor, _) = collect(&empty, None);
        assert!(cursor.last_record.is_none());
        let mut session = Session::at_unchanged_snapshot(
            Cursor::capture(&empty, Runtime::Claude).unwrap(),
            Some(&cursor),
        )
        .unwrap();
        assert_eq!(prove(&mut session, &empty).unwrap(), 0);
    }
}
