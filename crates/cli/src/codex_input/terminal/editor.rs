//! Small append/erase line editor for the managed bridge, not the Codex TUI.
//! Unfinished input stays in bounded reader state across cancelled reads.
use super::MAX_TEXT;
use anyhow::{Result, ensure};
use chrono::{DateTime, Utc};
use std::io::Write;
use tokio::io::{AsyncBufRead, AsyncBufReadExt};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Default)]
pub(super) struct Editor {
    bytes: Vec<u8>,
    oversized: bool,
    escape: u8,
    /// The last keystroke of a line not yet ended, on the monotonic clock:
    /// what the typing guard reads ([`agentdocker_core::typing`]). Enter, or
    /// the end of input, clears it: a submitted line is the person's own
    /// and holds nothing.
    typed: Option<std::time::Instant>,
}

enum Event {
    Pending,
    Line(Option<String>),
    Closed,
}

fn columns(text: &str) -> usize {
    let mut parts = text.split('\t');
    let mut width = parts.next().unwrap_or_default().width();
    for part in parts {
        width = (width / 8 + 1) * 8 + part.width();
    }
    width
}

impl Editor {
    fn erase_to(&mut self, size: usize, echo: &mut Vec<u8>) {
        let before = std::str::from_utf8(&self.bytes).map(columns).unwrap_or(0);
        self.bytes.truncate(size);
        let after = std::str::from_utf8(&self.bytes).map(columns).unwrap_or(0);
        for _ in after..before {
            echo.extend_from_slice(b"\x08 \x08");
        }
    }

    fn feed(&mut self, byte: u8, echo: &mut Vec<u8>) -> Event {
        if matches!(byte, b'\r' | b'\n') {
            echo.push(b'\n');
            self.escape = 0;
            let bytes = std::mem::take(&mut self.bytes);
            if std::mem::take(&mut self.oversized) {
                echo.extend_from_slice(b"Terminal input ignored: it exceeds 16000 bytes.\n");
                return Event::Line(None);
            }
            return match String::from_utf8(bytes) {
                Ok(text) => Event::Line((!text.is_empty()).then_some(text)),
                Err(_) => {
                    echo.extend_from_slice(b"Terminal input ignored: it is not UTF-8.\n");
                    Event::Line(None)
                }
            };
        }
        // Once a line exceeds the bound, editing cannot make its unseen suffix
        // into a fresh prompt. Discard through the next complete line boundary.
        if self.oversized {
            return Event::Pending;
        }
        if self.escape != 0 {
            self.escape = match (self.escape, byte) {
                (1, b'[' | b'O') => 2,
                (2, 0x20..=0x3f) => 2,
                _ => 0,
            };
            return Event::Pending;
        }
        match byte {
            0x1b => self.escape = 1,
            0x08 | 0x7f => {
                let size = std::str::from_utf8(&self.bytes)
                    .ok()
                    .and_then(|text| text.grapheme_indices(true).next_back().map(|(i, _)| i))
                    .unwrap_or_else(|| self.bytes.len().saturating_sub(1));
                self.erase_to(size, echo);
            }
            0x15 => self.erase_to(0, echo), // Ctrl-U: discard this draft.
            0x17 => {
                // Ctrl-W: erase trailing whitespace and the last word.
                let size = std::str::from_utf8(&self.bytes)
                    .ok()
                    .map(|text| {
                        let trimmed = text.trim_end_matches(char::is_whitespace);
                        trimmed
                            .rfind(char::is_whitespace)
                            .map_or(0, |i| i + trimmed[i..].chars().next().unwrap().len_utf8())
                    })
                    .unwrap_or(0);
                self.erase_to(size, echo);
            }
            0x04 if self.bytes.is_empty() => return Event::Closed,
            0x00..=0x1f if byte != b'\t' => echo.push(0x07),
            _ => {
                if self.bytes.len() == MAX_TEXT {
                    self.oversized = true;
                    self.bytes.clear();
                } else {
                    self.bytes.push(byte);
                    echo.push(byte);
                }
            }
        }
        Event::Pending
    }

    /// When that keystroke was, on the wall clock now: its age is measured
    /// on the monotonic one, so a wall clock that steps back cannot stamp
    /// it in the future and hold input for longer than a window.
    pub fn typed_at(&self) -> Option<DateTime<Utc>> {
        let age = chrono::Duration::from_std(self.typed?.elapsed()).ok()?;
        Some(Utc::now() - age)
    }

    pub async fn read<R: AsyncBufRead + Unpin>(
        &mut self,
        reader: &mut R,
    ) -> Result<Option<String>> {
        self.read_with_echo(reader, &mut std::io::stdout()).await
    }

    async fn read_with_echo<R: AsyncBufRead + Unpin, W: Write>(
        &mut self,
        reader: &mut R,
        output: &mut W,
    ) -> Result<Option<String>> {
        loop {
            let available = reader.fill_buf().await?;
            ensure!(
                !available.is_empty(),
                "terminal closed before the next complete line"
            );
            let mut echo = Vec::new();
            let mut event = Event::Pending;
            let mut consumed = 0;
            for byte in available.iter().take(4096) {
                consumed += 1;
                event = self.feed(*byte, &mut echo);
                if !matches!(event, Event::Pending) {
                    break;
                }
            }
            reader.consume(consumed);
            // No await occurs between consuming bytes and retaining their
            // effect. A provider event may cancel the next read safely.
            self.typed = matches!(event, Event::Pending).then(std::time::Instant::now);
            output.write_all(&echo)?;
            output.flush()?;
            match event {
                Event::Pending => (),
                Event::Line(text) => return Ok(text),
                Event::Closed => anyhow::bail!("terminal input closed"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::{
        io::{AsyncWriteExt, BufReader},
        time::timeout,
    };

    #[tokio::test]
    async fn interrupted_unicode_and_oversized_reads_preserve_their_boundaries() {
        let (mut writer, reader) = tokio::io::duplex(MAX_TEXT * 2);
        let mut reader = BufReader::new(reader);
        let mut editor = Editor::default();
        let mut output = Vec::new();
        writer.write_all(&"日".as_bytes()[..2]).await.unwrap();
        assert!(
            timeout(
                Duration::from_millis(10),
                editor.read_with_echo(&mut reader, &mut output)
            )
            .await
            .is_err()
        );
        writer.write_all(&"日".as_bytes()[2..]).await.unwrap();
        writer.write_all(b"\x7fnext\n").await.unwrap();
        assert_eq!(
            editor
                .read_with_echo(&mut reader, &mut output)
                .await
                .unwrap(),
            Some("next".into())
        );
        writer.write_all(&vec![b'x'; MAX_TEXT + 1]).await.unwrap();
        assert!(
            timeout(
                Duration::from_millis(10),
                editor.read_with_echo(&mut reader, &mut output)
            )
            .await
            .is_err()
        );
        assert!(editor.oversized && editor.bytes.is_empty());
        writer
            .write_all(b"discarded suffix\nvalid\n")
            .await
            .unwrap();
        assert_eq!(
            editor
                .read_with_echo(&mut reader, &mut output)
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            editor
                .read_with_echo(&mut reader, &mut output)
                .await
                .unwrap(),
            Some("valid".into())
        );
        assert!(
            String::from_utf8(output)
                .unwrap()
                .contains("exceeds 16000 bytes")
        );
    }

    /// The typing guard's clock: every keystroke of an unfinished line,
    /// arrows and erasures included, and nothing once the line is ended.
    #[tokio::test]
    async fn the_last_keystroke_of_an_unfinished_line_is_kept_until_it_ends() {
        let (mut writer, reader) = tokio::io::duplex(64);
        let mut reader = BufReader::new(reader);
        let mut editor = Editor::default();
        let mut output = Vec::new();
        assert_eq!(editor.typed_at(), None);
        let before = Utc::now() - chrono::Duration::seconds(1);
        writer.write_all(b"half a thou").await.unwrap();
        assert!(
            timeout(
                Duration::from_millis(10),
                editor.read_with_echo(&mut reader, &mut output)
            )
            .await
            .is_err()
        );
        let first = editor.typed_at().expect("typing");
        assert!(first >= before && first <= Utc::now());
        writer.write_all(b"\x7f\x1b[A").await.unwrap();
        assert!(
            timeout(
                Duration::from_millis(10),
                editor.read_with_echo(&mut reader, &mut output)
            )
            .await
            .is_err()
        );
        assert!(editor.typed_at().is_some_and(|at| at <= Utc::now()));
        writer.write_all(b"ght\n").await.unwrap();
        assert_eq!(
            editor
                .read_with_echo(&mut reader, &mut output)
                .await
                .unwrap(),
            Some("half a thoght".into())
        );
        assert_eq!(editor.typed_at(), None, "a submitted line holds nothing");
    }

    fn supply(editor: &mut Editor, bytes: &[u8]) -> (Vec<Option<String>>, Vec<u8>) {
        let mut lines = Vec::new();
        let mut echo = Vec::new();
        for byte in bytes {
            if let Event::Line(line) = editor.feed(*byte, &mut echo) {
                lines.push(line);
            }
        }
        (lines, echo)
    }

    #[test]
    fn oversized_paste_is_discarded_without_submitting_a_tail_or_losing_next_line() {
        let mut editor = Editor::default();
        let prefix = "é".repeat(MAX_TEXT / 2);
        assert_eq!(
            supply(&mut editor, format!("{prefix}\n").as_bytes()).0,
            vec![Some(prefix)]
        );
        supply(&mut editor, &vec![b'x'; MAX_TEXT + 1]);
        assert!(editor.oversized);
        assert!(editor.bytes.is_empty());
        let (lines, echo) = supply(&mut editor, b"\x15\x7fdo not submit\nnext message\n");
        assert_eq!(lines, vec![None, Some("next message".into())]);
        assert!(
            String::from_utf8(echo)
                .unwrap()
                .contains("exceeds 16000 bytes")
        );
        assert!(editor.bytes.capacity() <= MAX_TEXT.next_power_of_two());
    }

    #[test]
    fn editing_unicode_and_discarding_terminal_sequences_keeps_exact_text() {
        let mut editor = Editor::default();
        supply(&mut editor, "draft café 日本語 e\u{301}".as_bytes());
        supply(&mut editor, b"\x7f\x7f\x17");
        assert_eq!(std::str::from_utf8(&editor.bytes).unwrap(), "draft café ");
        supply(&mut editor, b"\x15");
        let (lines, _) = supply(&mut editor, "\x1b[Ax\x1b[1;5Dy 日本語\n".as_bytes());
        assert_eq!(lines, vec![Some("xy 日本語".into())]);
        let (lines, echo) = supply(&mut editor, b"\xff\nvalid\n");
        assert_eq!(lines, vec![None, Some("valid".into())]);
        assert!(String::from_utf8_lossy(&echo).contains("not UTF-8"));
    }
}
