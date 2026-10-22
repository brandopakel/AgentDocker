//! Bounded terminal lines. Reject one bad paste without stopping the provider
//! or interpreting the tail of that paste as another submitted message.
use anyhow::{Result, ensure};
use tokio::io::{AsyncBufRead, AsyncBufReadExt};

mod editor;
mod mode;
pub(super) use mode::Mode;

/// Read and discard exactly one line without retaining or echoing any bytes.
/// Cancellation is safe: consumed bytes need no resumable draft state. A caller
/// keeps suspension until a fresh Enter is observed after the review closes.
pub(super) async fn discard<R: AsyncBufRead + Unpin>(reader: &mut R) -> Result<()> {
    loop {
        let bytes = reader.fill_buf().await?;
        ensure!(
            !bytes.is_empty(),
            "terminal closed while input was suspended"
        );
        let end = bytes.iter().position(|byte| matches!(byte, b'\r' | b'\n'));
        let count = end.map_or(bytes.len(), |n| n + 1);
        reader.consume(count);
        if end.is_some() {
            return Ok(());
        }
    }
}

const MAX_TEXT: usize = 16_000;
const MAX_LINE: usize = MAX_TEXT + 2; // Optional CRLF.

#[derive(Default)]
pub(super) struct Input {
    bytes: Vec<u8>,
    oversized: bool,
    editor: Option<editor::Editor>,
}

impl Input {
    pub fn new(editing: bool) -> Self {
        Self {
            editor: editing.then(editor::Editor::default),
            ..Self::default()
        }
    }

    /// The last keystroke of the line the person is typing, when the
    /// bridge's editor reads them one by one. Without it the terminal's own
    /// line discipline keeps the line until Enter, and nothing is seen.
    pub fn typed_at(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        self.editor.as_ref().and_then(editor::Editor::typed_at)
    }

    // State lives outside the future: a provider event can cancel this read
    // after any chunk, including while an oversized line is being discarded.
    pub async fn read<R: AsyncBufRead + Unpin>(
        &mut self,
        reader: &mut R,
    ) -> Result<Option<String>> {
        if let Some(editor) = &mut self.editor {
            return editor.read(reader).await;
        }
        loop {
            let available = reader.fill_buf().await?;
            ensure!(
                !available.is_empty(),
                "terminal closed before the next complete line"
            );
            let newline = available.iter().position(|byte| *byte == b'\n');
            let count = newline.map_or(available.len(), |at| at + 1);
            if !self.oversized {
                if self.bytes.len().saturating_add(count) > MAX_LINE {
                    self.oversized = true;
                    self.bytes.clear();
                } else {
                    self.bytes.extend_from_slice(&available[..count]);
                }
            }
            reader.consume(count);
            if newline.is_none() {
                continue;
            }
            let oversized = std::mem::take(&mut self.oversized);
            let bytes = std::mem::take(&mut self.bytes);
            if oversized {
                eprintln!("Terminal input ignored: it exceeds 16000 bytes.");
                return Ok(None);
            }
            let Ok(text) = String::from_utf8(bytes) else {
                eprintln!("Terminal input ignored: it is not UTF-8.");
                return Ok(None);
            };
            let text = text.trim_end_matches(['\r', '\n']);
            if text.len() > MAX_TEXT {
                eprintln!("Terminal input ignored: it exceeds 16000 bytes.");
                return Ok(None);
            }
            return Ok((!text.is_empty()).then(|| text.to_owned()));
        }
    }
}

/// The person's terminal while the bridge runs: its line reader, the lines
/// it has completed and not yet queued, and whether it is open or suspended
/// by a secret review. The bridge keeps reading it while it waits on other
/// calls ([`Self::during`]) so that the keystroke clock the typing guard
/// reads is never stale, and reads what has arrived before every decision
/// ([`Self::drain`]).
pub(super) struct Keyboard<R> {
    reader: R,
    input: Input,
    pub open: bool,
    pub suspended: bool,
    lines: std::collections::VecDeque<String>,
}

impl<R: AsyncBufRead + Unpin> Keyboard<R> {
    pub fn new(reader: R, editing: bool, open: bool) -> Self {
        Self {
            reader,
            input: Input::new(editing),
            open,
            suspended: false,
            lines: Default::default(),
        }
    }

    /// The last keystroke of the line the person is typing, among the bytes
    /// read so far.
    pub fn typed_at(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        self.input.typed_at()
    }

    /// The next line the person ended, oldest first, to be queued.
    pub fn next_line(&mut self) -> Option<String> {
        self.lines.pop_front()
    }

    /// One read for the bridge's own select: a line, nothing, or the end of
    /// the terminal. While suspended, the line is discarded unseen. Safe to
    /// cancel, like the reads it makes.
    pub async fn read(&mut self) -> Result<Option<String>> {
        if self.suspended {
            discard(&mut self.reader).await.map(|()| None)
        } else {
            self.input.read(&mut self.reader).await
        }
    }

    /// What a read outside a suspension gave: a line is kept to be queued,
    /// an error closes the terminal.
    pub fn take(&mut self, line: Result<Option<String>>) {
        match line {
            Ok(Some(text)) => self.lines.push_back(text),
            Ok(None) => (),
            Err(error) => {
                self.open = false;
                eprintln!("Terminal input closed: {error}");
            }
        }
    }

    /// Await `call` while the person's typing keeps being read: every byte
    /// already available is read before `call` is looked at, so when it
    /// returns, nothing that arrived meanwhile is unread. A suspended or
    /// closed terminal is left alone.
    pub async fn during<F: std::future::Future>(&mut self, call: F) -> F::Output {
        tokio::pin!(call);
        loop {
            tokio::select! {
                biased;
                line = self.input.read(&mut self.reader), if self.open && !self.suspended => {
                    self.take(line);
                }
                output = &mut call => return output,
            }
        }
    }

    /// Read every byte that has arrived, without waiting for more.
    pub async fn drain(&mut self) {
        self.during(std::future::ready(())).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Cursor, time::Duration};
    use tokio::{
        io::{AsyncWriteExt, BufReader},
        time::timeout,
    };

    #[tokio::test]
    async fn invalid_and_oversized_lines_leave_later_input_intact() {
        let mut data = b"\xff\n".to_vec();
        data.extend(vec![b'x'; 4 * 1024 * 1024 + 1]);
        data.extend_from_slice(b"\n\r\nnext message\r\n");
        let mut reader = BufReader::with_capacity(128, Cursor::new(data));
        let mut input = Input::default();
        assert_eq!(input.read(&mut reader).await.unwrap(), None);
        assert_eq!(input.read(&mut reader).await.unwrap(), None);
        assert!(input.bytes.capacity() <= MAX_LINE.next_power_of_two());
        assert_eq!(input.read(&mut reader).await.unwrap(), None);
        assert_eq!(
            input.read(&mut reader).await.unwrap().as_deref(),
            Some("next message")
        );
    }

    #[tokio::test]
    async fn cancelling_a_partial_or_discarded_line_never_submits_its_tail() {
        let (mut writer, reader) = tokio::io::duplex(MAX_LINE * 2);
        let mut reader = BufReader::new(reader);
        let mut input = Input::default();
        writer.write_all(b"prefix").await.unwrap();
        assert!(
            timeout(Duration::from_millis(10), input.read(&mut reader))
                .await
                .is_err()
        );
        writer.write_all(b" suffix\n").await.unwrap();
        assert_eq!(
            input.read(&mut reader).await.unwrap().as_deref(),
            Some("prefix suffix")
        );
        writer.write_all(&vec![b'x'; MAX_LINE + 1]).await.unwrap();
        assert!(
            timeout(Duration::from_millis(10), input.read(&mut reader))
                .await
                .is_err()
        );
        assert!(input.oversized);
        assert!(input.bytes.is_empty());
        writer
            .write_all(b"do not submit this tail\nvalid\n")
            .await
            .unwrap();
        assert_eq!(input.read(&mut reader).await.unwrap(), None);
        assert_eq!(
            input.read(&mut reader).await.unwrap().as_deref(),
            Some("valid")
        );
    }

    #[tokio::test]
    async fn suspended_input_is_discarded_across_cancellation_without_touching_a_draft() {
        let (mut writer, reader) = tokio::io::duplex(256);
        let mut reader = BufReader::with_capacity(32, reader);
        let mut input = Input::default();
        writer.write_all(b"saved draft").await.unwrap();
        assert!(
            timeout(Duration::from_millis(10), input.read(&mut reader))
                .await
                .is_err()
        );
        writer.write_all(b"invented-secret-prefix").await.unwrap();
        assert!(
            timeout(Duration::from_millis(10), discard(&mut reader))
                .await
                .is_err()
        );
        writer.write_all(b"private-tail\n").await.unwrap();
        discard(&mut reader).await.unwrap();
        // The caller stays suspended through route closure. This Enter cannot
        // submit text buffered while the request was open.
        writer
            .write_all(b"late-secret-before-resume\n")
            .await
            .unwrap();
        discard(&mut reader).await.unwrap();
        writer.write_all(b" suffix\n").await.unwrap();
        assert_eq!(
            input.read(&mut reader).await.unwrap().as_deref(),
            Some("saved draft suffix")
        );
        assert!(input.bytes.is_empty());
    }

    #[tokio::test]
    async fn exact_byte_limit_and_unicode_are_preserved() {
        let text = "é".repeat(MAX_TEXT / 2);
        let mut reader = Cursor::new(format!("{text}\r\n{text}x\nlast\n"));
        let mut input = Input::default();
        assert_eq!(input.read(&mut reader).await.unwrap(), Some(text));
        assert_eq!(input.read(&mut reader).await.unwrap(), None);
        assert_eq!(
            input.read(&mut reader).await.unwrap().as_deref(),
            Some("last")
        );
    }
}
