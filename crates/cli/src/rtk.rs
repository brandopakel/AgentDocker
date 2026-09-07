//! A compressed *view* of a retained log, where [rtk] is installed.
//!
//! Everything an agent reads from us costs it input tokens, and the
//! things we hand back that are somebody else's output — a validation
//! log, an agent's captured stdout — are the largest of them by far.
//! rtk exists to shrink exactly that kind of text before a model sees
//! it, so where the host has it, we offer it.
//!
//! What is never done is compressing the log itself. A validation log
//! is evidence: `integrate` will not take source that has not passed,
//! and the log is how somebody checks the claim afterwards. Evidence is
//! kept whole. This reads a retained log, pipes a copy through rtk, and
//! prints what came back — the file on disk is not touched, and a
//! failure to compress falls back to printing the log as it is, with a
//! word about why, rather than printing nothing.
//!
//! [rtk]: https://github.com/rtk-ai/rtk

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Long enough for a large log, short enough that a compressor which
/// hangs does not take the CLI with it.
const TIMEOUT: Duration = Duration::from_secs(30);

/// Where rtk is, if it is anywhere.
pub fn installed() -> Option<PathBuf> {
    which("rtk")
}

fn which(program: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(program))
            .find(|candidate| candidate.is_file())
    })
}

/// What came of trying to compress some text.
pub enum View {
    /// rtk ran and this is what it said, with what it saved.
    Compressed { text: String, saved: usize },
    /// rtk is not installed, or would not run. The text is unchanged and
    /// the reason is worth saying once.
    Plain { text: String, why: String },
}

impl View {
    pub fn text(&self) -> &str {
        match self {
            View::Compressed { text, .. } | View::Plain { text, .. } => text,
        }
    }

    /// A line for the reader about what happened, or nothing when the
    /// text is simply the text.
    pub fn note(&self) -> Option<String> {
        match self {
            View::Compressed { saved, .. } if *saved > 0 => Some(format!(
                "(rtk view, {saved} bytes lighter; the log is unchanged)"
            )),
            View::Compressed { .. } => Some("(rtk view; nothing to save)".to_owned()),
            View::Plain { why, .. } if why.is_empty() => None,
            View::Plain { why, .. } => Some(format!("({why}; showing the log as it is)")),
        }
    }
}

/// Pipe `text` through rtk and return what it said.
///
/// Never fails: anything that goes wrong comes back as the original text
/// and a reason. A view of a log is a convenience, and refusing to show
/// somebody their log because a compressor is missing would be absurd.
pub fn view(text: &str) -> View {
    view_with(installed(), text)
}

/// The same, told where rtk is rather than looking. Split out so tests
/// can say "not installed" or "here is one" without touching `PATH`,
/// which is process-wide and shared with every other test running.
fn view_with(rtk: Option<PathBuf>, text: &str) -> View {
    let Some(rtk) = rtk else {
        return View::Plain {
            text: text.to_owned(),
            why: "rtk is not installed".to_owned(),
        };
    };
    match compress(&rtk, text) {
        Ok(compressed) => {
            let saved = text.len().saturating_sub(compressed.len());
            View::Compressed {
                text: compressed,
                saved,
            }
        }
        Err(why) => View::Plain {
            text: text.to_owned(),
            why,
        },
    }
}

/// Run rtk over the text on its stdin. Separated so the decision about
/// what to do with a failure is made in one place, above.
fn compress(rtk: &std::path::Path, text: &str) -> Result<String, String> {
    let mut child = Command::new(rtk)
        .arg("compress")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("rtk would not start: {e}"))?;
    // All three pipes get their own thread, and this is not
    // over-engineering: a pipe holds about 64 KiB, so any two of
    // "write the whole log", "read the whole answer" and "read the
    // whole complaint" done in sequence can wedge against a child
    // doing one of the others. Which one deadlocks depends on whether
    // the compressor reads before it writes, and that is the
    // compressor's business, not ours.
    let mut stdin = child.stdin.take().ok_or("rtk has no stdin")?;
    let mut stdout = child.stdout.take().ok_or("rtk has no stdout")?;
    let mut stderr = child.stderr.take().ok_or("rtk has no stderr")?;
    let owned = text.to_owned();
    let writer = std::thread::spawn(move || stdin.write_all(owned.as_bytes()));
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        std::io::Read::read_to_end(&mut stdout, &mut out).map(|_| out)
    });
    let complaints = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = std::io::Read::read_to_end(&mut stderr, &mut out);
        out
    });

    let started = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => return Err(format!("rtk could not be waited on: {e}")),
        }
        if started.elapsed() >= TIMEOUT {
            // Killing it is what unblocks the three threads, so they are
            // joined after, not before.
            let _ = child.kill();
            let _ = child.wait();
            let _ = writer.join();
            let _ = reader.join();
            let _ = complaints.join();
            return Err(format!("rtk did not finish within {}s", TIMEOUT.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    // The whole log has to have gone in. A compressor that stopped
    // reading part-way through produced a view of part of a log, and
    // showing that as the log's compressed view would be a quiet lie
    // about evidence.
    match writer.join() {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(format!("rtk stopped reading the log: {e}")),
        Err(_) => return Err("the thread feeding rtk panicked".to_owned()),
    }
    let said = complaints.join().unwrap_or_default();
    if !status.success() {
        let said = String::from_utf8_lossy(&said);
        let said = said.trim();
        return Err(if said.is_empty() {
            format!("rtk exited {}", status)
        } else {
            format!("rtk: {said}")
        });
    }
    let out = match reader.join() {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => return Err(format!("rtk output could not be read: {e}")),
        Err(_) => return Err("the thread reading rtk panicked".to_owned()),
    };
    let compressed = String::from_utf8_lossy(&out).into_owned();
    if compressed.trim().is_empty() {
        return Err("rtk returned nothing".to_owned());
    }
    Ok(compressed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A shell script standing in for rtk, since the real one is not on
    /// most machines and never on CI.
    fn fake(dir: &tempfile::TempDir, name: &str, body: &str) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    /// The one behaviour that matters when rtk is not installed, which
    /// is most machines: the log is still shown, and the reader is told
    /// why it was not compressed.
    #[test]
    fn without_rtk_the_log_is_shown_unchanged_with_a_reason() {
        let original = "line one\nline two\n";
        let view = view_with(None, original);
        assert_eq!(view.text(), original, "the log is not altered");
        let note = view.note().expect("a reason");
        assert!(note.contains("not installed"), "{note}");
        assert!(matches!(view, View::Plain { .. }));
    }

    /// The whole pipe, end to end: the log goes in on stdin and what
    /// comes back on stdout is what the reader sees.
    #[test]
    fn a_compressor_that_works_is_piped_the_log_and_its_answer_is_shown() {
        let dir = tempfile::TempDir::new().unwrap();
        // Reads all of stdin and answers with its line count, which is
        // both shorter than the input and derived from it.
        let rtk = fake(&dir, "rtk", "wc -l");
        let original = "a\nb\nc\nd\ne\nf\ng\nh\n";
        let view = view_with(Some(rtk), original);
        assert!(
            view.text().contains('8'),
            "it saw every line: {:?}",
            view.text()
        );
        assert!(
            matches!(view, View::Compressed { saved, .. } if saved > 0),
            "and it is shorter than what went in"
        );
    }

    /// A big log must not deadlock: writing it all before reading
    /// anything back would fill the pipe and stall against a child
    /// doing the same in reverse.
    #[test]
    fn a_log_larger_than_a_pipe_buffer_still_gets_through() {
        let dir = tempfile::TempDir::new().unwrap();
        let rtk = fake(&dir, "rtk", "wc -c");
        // Comfortably past the 64 KiB a pipe usually buffers.
        let original = "x".repeat(1_000_000);
        let view = view_with(Some(rtk), &original);
        assert!(
            view.text().contains("1000000"),
            "all of it arrived: {:?}",
            view.text()
        );
    }

    /// And the other direction, which the test above does not reach:
    /// `wc` reads all of its input before writing a word, so it can
    /// never wedge us. A compressor that writes a large answer *first*
    /// can — its output pipe fills, it blocks, and it never gets round
    /// to reading the log we are blocked trying to give it. Draining
    /// stdout on its own thread is what makes this finish.
    #[test]
    fn a_compressor_that_answers_before_it_reads_does_not_deadlock() {
        let dir = tempfile::TempDir::new().unwrap();
        let rtk = fake(
            &dir,
            "rtk",
            // Well past a pipe's capacity, written before stdin is
            // touched at all.
            "yes 'compressed line' | head -40000; cat >/dev/null",
        );
        let original = "y".repeat(1_000_000);
        let started = std::time::Instant::now();
        let view = view_with(Some(rtk), &original);
        assert!(
            started.elapsed() < TIMEOUT,
            "it finished rather than timing out: {:?}",
            started.elapsed()
        );
        assert!(
            matches!(view, View::Compressed { .. }),
            "and it is the compressor's answer, not the log back again"
        );
        assert!(view.text().lines().count() > 30_000, "all of the answer");
    }

    /// A compressor that stops reading part-way through has produced a
    /// view of part of a log. Showing that as the log's compressed view
    /// would be a quiet lie about evidence, so it falls back instead.
    #[test]
    fn a_compressor_that_stops_reading_early_is_not_trusted() {
        let dir = tempfile::TempDir::new().unwrap();
        let rtk = fake(&dir, "rtk", "head -c 100 >/dev/null; echo 'a summary'");
        let original = "z".repeat(1_000_000);
        let view = view_with(Some(rtk), &original);
        assert_eq!(view.text(), original, "the whole log, not the summary");
        let note = view.note().unwrap();
        assert!(note.contains("stopped reading"), "{note}");
    }

    /// A compressor that fails is not a reason to withhold somebody's
    /// log. It is shown whole, with what went wrong.
    #[test]
    fn a_compressor_that_fails_falls_back_to_the_log_itself() {
        let dir = tempfile::TempDir::new().unwrap();
        let rtk = fake(&dir, "rtk", "cat >/dev/null; echo 'no' >&2; exit 2");
        let original = "the evidence\n";
        let view = view_with(Some(rtk), original);
        assert_eq!(view.text(), original, "evidence is kept whole");
        let note = view.note().unwrap();
        assert!(note.contains("no"), "it says what went wrong: {note}");
        assert!(note.contains("showing the log as it is"), "{note}");
    }

    /// And one that succeeds but says nothing is a failure too: an
    /// empty view of a log is worse than no view.
    #[test]
    fn a_compressor_that_returns_nothing_falls_back() {
        let dir = tempfile::TempDir::new().unwrap();
        let rtk = fake(&dir, "rtk", "cat >/dev/null");
        let original = "the evidence\n";
        let view = view_with(Some(rtk), original);
        assert_eq!(view.text(), original);
        assert!(view.note().unwrap().contains("returned nothing"));
    }

    #[test]
    fn a_compressed_view_reports_what_it_saved() {
        let view = View::Compressed {
            text: "short".into(),
            saved: 400,
        };
        assert_eq!(view.text(), "short");
        let note = view.note().unwrap();
        assert!(note.contains("400 bytes lighter"), "{note}");
        assert!(note.contains("log is unchanged"), "{note}");
    }

    #[test]
    fn a_compressor_that_saves_nothing_still_says_so() {
        let view = View::Compressed {
            text: "same".into(),
            saved: 0,
        };
        assert!(view.note().unwrap().contains("nothing to save"));
    }

    /// A compressor that will not finish is given up on rather than
    /// taking the CLI with it. Not run by default: proving the real
    /// thirty-second timeout costs thirty seconds.
    #[test]
    #[ignore = "waits out the real timeout"]
    fn a_compressor_that_will_not_finish_is_given_up_on() {
        let dir = tempfile::TempDir::new().unwrap();
        let rtk = fake(&dir, "rtk", "sleep 600");
        let original = "the evidence\n";
        let started = std::time::Instant::now();
        let view = view_with(Some(rtk), original);
        assert_eq!(view.text(), original);
        assert!(view.note().unwrap().contains("did not finish"));
        assert!(started.elapsed() < TIMEOUT + Duration::from_secs(5));
    }
}
