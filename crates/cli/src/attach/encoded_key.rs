//! Keep the local detach key local when a nested console enables Win32 input.
//!
//! Conhost can return CSI Vk;Sc;Uc;Kd;Cs;Rc_ instead of a control byte after
//! nested output enables mode 9001. Only a complete U+001D key-down becomes
//! the existing detach byte. Other input remains byte-for-byte unchanged.
use std::collections::VecDeque;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, ReadBuf};
use tokio::time::Sleep;

const MAX_PREFIX: usize = 64;
const PREFIX_WAIT: Duration = Duration::from_millis(25);

#[derive(Default)]
struct Decoder {
    prefix: Vec<u8>,
    output: VecDeque<u8>,
}

impl Decoder {
    fn flush(&mut self) {
        self.output.extend(self.prefix.drain(..));
    }

    fn feed(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if self.prefix == b"\x1b" && byte == b'[' {
                self.prefix.push(byte);
                continue;
            }
            if self.prefix.starts_with(b"\x1b[") {
                if byte == b'_' {
                    if is_detach(&self.prefix[2..]) {
                        self.prefix.clear();
                        self.output.push_back(super::DETACH);
                    } else {
                        self.flush();
                        self.output.push_back(byte);
                    }
                    continue;
                }
                if (byte.is_ascii_digit() || byte == b';') && self.prefix.len() < MAX_PREFIX {
                    self.prefix.push(byte);
                    continue;
                }
            }
            // Unknown sequences, overflow and a second Escape flush exactly
            // the original prefix, then reconsider the current byte normally.
            self.flush();
            if byte == 0x1b {
                self.prefix.push(byte);
            } else {
                self.output.push_back(byte);
            }
        }
    }
}

fn is_detach(parameters: &[u8]) -> bool {
    // Win32 input permits omitted parameters: zero, except repeat count one.
    let mut values = [0u32, 0, 0, 0, 0, 1];
    for (index, field) in parameters.split(|byte| *byte == b';').enumerate() {
        let Some(value) = values.get_mut(index) else {
            return false;
        };
        if field.is_empty() {
            continue;
        }
        *value = 0;
        for byte in field {
            let Some(next) = value
                .checked_mul(10)
                .and_then(|v| v.checked_add(u32::from(*byte - b'0')))
            else {
                return false;
            };
            *value = next;
        }
    }
    values[0] <= u32::from(u16::MAX)
        && values[1] <= u32::from(u16::MAX)
        && values[2] == u32::from(super::DETACH)
        && values[3] == 1
        && (1..=u32::from(u16::MAX)).contains(&values[5])
}

/// Bound fragmented keyboard records without trapping a lone Escape key.
pub(super) struct Input<R> {
    inner: R,
    decoder: Decoder,
    timer: Option<Pin<Box<Sleep>>>,
    ended: bool,
    error: Option<io::Error>,
}

impl<R> Input<R> {
    pub(super) fn new(inner: R) -> Self {
        Self {
            inner,
            decoder: Decoder::default(),
            timer: None,
            ended: false,
            error: None,
        }
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for Input<R> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if buffer.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        loop {
            if !this.decoder.output.is_empty() {
                let count = buffer.remaining().min(this.decoder.output.len());
                for _ in 0..count {
                    buffer.put_slice(&[this.decoder.output.pop_front().expect("count bounded")]);
                }
                return Poll::Ready(Ok(()));
            }
            if this.ended {
                return Poll::Ready(this.error.take().map_or(Ok(()), Err));
            }
            let mut bytes = [0; 4096];
            let mut read = ReadBuf::new(&mut bytes);
            match Pin::new(&mut this.inner).poll_read(cx, &mut read) {
                Poll::Ready(Ok(())) if read.filled().is_empty() => {
                    this.ended = true;
                    this.decoder.flush();
                    this.timer = None;
                }
                Poll::Ready(Ok(())) => {
                    this.decoder.feed(read.filled());
                    this.timer = (!this.decoder.prefix.is_empty())
                        .then(|| Box::pin(tokio::time::sleep(PREFIX_WAIT)));
                }
                Poll::Ready(Err(error)) => {
                    this.ended = true;
                    this.error = Some(error);
                    this.decoder.flush();
                    this.timer = None;
                }
                Poll::Pending => {
                    if this
                        .timer
                        .as_mut()
                        .is_some_and(|timer| timer.as_mut().poll(cx).is_ready())
                    {
                        this.decoder.flush();
                        this.timer = None;
                    } else {
                        return Poll::Pending;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const DOWN: &[u8] = b"\x1b[221;27;29;1;8;1_";

    #[test]
    fn win32_detach_survives_every_read_boundary() {
        for split in 0..=DOWN.len() {
            let mut decoder = Decoder::default();
            decoder.feed(&DOWN[..split]);
            decoder.feed(&DOWN[split..]);
            decoder.flush();
            assert_eq!(Vec::from(decoder.output), b"\x1d", "split {split}");
        }
        let mut decoder = Decoder::default();
        for byte in DOWN {
            decoder.feed(&[*byte]);
        }
        assert_eq!(Vec::from(decoder.output), b"\x1d");
    }

    #[test]
    fn documented_parameter_defaults_preserve_the_detach_key() {
        let records: &[&[u8]] = &[
            b"\x1b[;;29;1_",
            b"\x1b[;27;29;1;8;1_",
            b"\x1b[221;;29;1;_",
            b"\x1b[;;29;1;;_",
        ];
        for record in records {
            for split in 0..=record.len() {
                let mut decoder = Decoder::default();
                decoder.feed(&record[..split]);
                decoder.feed(&record[split..]);
                decoder.flush();
                assert_eq!(
                    Vec::from(decoder.output),
                    b"\x1d",
                    "{record:?}, split {split}"
                );
            }
        }
    }

    #[test]
    fn other_keys_unicode_and_invalid_records_are_unchanged() {
        let mut bytes = "café_日本語_🙂\x1b[A\x1b".as_bytes().to_vec();
        bytes.extend_from_slice(b"\x1b[17;29;0;1;8;1_\x1b[221;27;29;0;8;1_\x1b[221;27;29;1;8;0_");
        bytes.extend_from_slice(
            b"\x1b[221;27;30;1;8;1_\x1b[221;27;29;2;8;1_\x1b[221;27;29;1;8;1;0_",
        );
        bytes.extend_from_slice(
            b"\x1b[;27;29;;8;1_\x1b[4294967296;27;29;1;8;1_\x1b[65536;27;29;1;8;1_",
        );
        bytes.extend_from_slice(b"\x1b[221;27;-29;1;8;1_\x1b[221;27;29;1;8;1~\x1b[");
        bytes.extend(std::iter::repeat_n(b'9', 200));
        bytes.extend_from_slice(b";27;29;1;8;1_\x1b[221;");
        for split in 0..=bytes.len() {
            let mut decoder = Decoder::default();
            decoder.feed(&bytes[..split]);
            assert!(decoder.prefix.len() <= MAX_PREFIX);
            decoder.feed(&bytes[split..]);
            assert!(decoder.prefix.len() <= MAX_PREFIX);
            decoder.flush();
            assert_eq!(Vec::from(decoder.output), bytes, "split {split}");
        }
    }

    #[test]
    fn only_the_detach_record_changes_among_surrounding_input() {
        let before = b"hello\x1b[17;29;0;1;8;1_";
        let after = b"\x1b[221;27;29;0;8;1_\x1b[17;29;0;0;0;1_goodbye";
        let mut decoder = Decoder::default();
        decoder.feed(before);
        decoder.feed(DOWN);
        decoder.feed(after);
        decoder.flush();
        assert_eq!(
            Vec::from(decoder.output),
            [before.as_slice(), b"\x1d", after].concat()
        );
    }

    #[tokio::test]
    async fn a_lone_escape_arrives_without_another_keystroke() {
        let (mut writer, reader) = tokio::io::duplex(64);
        writer.write_all(b"\x1b").await.unwrap();
        let mut input = Input::new(reader);
        let mut byte = [0];
        tokio::time::timeout(Duration::from_secs(5), input.read_exact(&mut byte))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(byte, [0x1b]);
        writer.write_all(b"[221;27;29;1;8;1_").await.unwrap();
        drop(writer);
        let mut remaining = vec![];
        input.read_to_end(&mut remaining).await.unwrap();
        assert_eq!(remaining, b"[221;27;29;1;8;1_");
    }

    #[tokio::test]
    async fn small_reads_and_eof_preserve_partial_sequences() {
        let mut bytes = DOWN.to_vec();
        bytes.extend_from_slice(b"text\x1b[221;");
        let mut input = Input::new(bytes.as_slice());
        let mut output = vec![];
        let mut byte = [0];
        while input.read(&mut byte).await.unwrap() != 0 {
            output.push(byte[0]);
        }
        assert_eq!(output, b"\x1dtext\x1b[221;");
    }

    #[tokio::test]
    async fn an_error_does_not_discard_its_preceding_escape_prefix() {
        struct Broken(bool);
        impl AsyncRead for Broken {
            fn poll_read(
                mut self: Pin<&mut Self>,
                _: &mut Context<'_>,
                buffer: &mut ReadBuf<'_>,
            ) -> Poll<io::Result<()>> {
                if !self.0 {
                    self.0 = true;
                    buffer.put_slice(b"\x1b[221;");
                    Poll::Ready(Ok(()))
                } else {
                    Poll::Ready(Err(io::Error::new(io::ErrorKind::BrokenPipe, "fixture")))
                }
            }
        }
        let mut input = Input::new(Broken(false));
        let mut output = vec![];
        let error = input.read_to_end(&mut output).await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        assert_eq!(output, b"\x1b[221;");
    }
}
