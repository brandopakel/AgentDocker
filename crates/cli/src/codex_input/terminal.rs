//! Bounded terminal lines. Reject one bad paste without stopping the provider
//! or interpreting the tail of that paste as another submitted message.
use anyhow::{Result, ensure};
use tokio::io::{AsyncBufRead, AsyncBufReadExt};

const MAX_TEXT: usize = 16_000;
const MAX_LINE: usize = MAX_TEXT + 2; // Optional CRLF.

#[derive(Default)]
pub(super) struct Input {
    bytes: Vec<u8>,
    oversized: bool,
}

impl Input {
    // State lives outside the future: a provider event can cancel this read
    // after any chunk, including while an oversized line is being discarded.
    pub async fn read<R: AsyncBufRead + Unpin>(
        &mut self,
        reader: &mut R,
    ) -> Result<Option<String>> {
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
