//! Bounded stdio workers independent of Tokio's blocking pool. An outstanding
//! pipe read/write cannot be cancelled; runtime shutdown must not wait for it.
//! These workers own only process stdio and end with this MCP process.
use anyhow::{Context, Result};
use serde_json::Value;
use std::{
    future::Future,
    io::{self, Read, Write},
    pin::Pin,
    task::{Context as TaskContext, Poll},
};
use tokio::{
    io::{AsyncRead, BufReader, ReadBuf},
    sync::{mpsc, oneshot},
};

pub(super) trait Output {
    fn send(&mut self, value: &Value) -> impl Future<Output = Result<()>>;
}

pub(super) struct Input {
    chunks: mpsc::Receiver<io::Result<Vec<u8>>>,
    current: Vec<u8>,
    offset: usize,
}

impl AsyncRead for Input {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if output.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        loop {
            if self.offset < self.current.len() {
                let count = output.remaining().min(self.current.len() - self.offset);
                output.put_slice(&self.current[self.offset..self.offset + count]);
                self.offset += count;
                return Poll::Ready(Ok(()));
            }
            match self.chunks.poll_recv(cx) {
                Poll::Ready(Some(Ok(bytes))) => {
                    self.current = bytes;
                    self.offset = 0;
                }
                Poll::Ready(Some(Err(error))) => return Poll::Ready(Err(error)),
                Poll::Ready(None) => return Poll::Ready(Ok(())),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

type Frame = (Vec<u8>, oneshot::Sender<io::Result<()>>);
pub(super) struct Writer(mpsc::Sender<Frame>);

impl Output for Writer {
    async fn send(&mut self, value: &Value) -> Result<()> {
        let mut bytes = serde_json::to_vec(value)?;
        bytes.push(b'\n');
        let (done, completed) = oneshot::channel();
        self.0
            .send((bytes, done))
            .await
            .context("MCP stdout worker stopped")?;
        completed
            .await
            .context("MCP stdout worker stopped before flushing")??;
        Ok(())
    }
}

pub(super) fn open() -> Result<(BufReader<Input>, Writer)> {
    let (chunks, received) = mpsc::channel(4);
    std::thread::Builder::new()
        .name("mcp-stdin".into())
        .spawn(move || {
            let mut input = io::stdin().lock();
            loop {
                let mut bytes = vec![0; 8192];
                match input.read(&mut bytes) {
                    Ok(0) => break,
                    Ok(count) => {
                        bytes.truncate(count);
                        if chunks.blocking_send(Ok(bytes)).is_err() {
                            break;
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => {
                        let _ = chunks.blocking_send(Err(error));
                        break;
                    }
                }
            }
        })?;
    let (frames, mut receive): (mpsc::Sender<Frame>, mpsc::Receiver<Frame>) = mpsc::channel(1);
    std::thread::Builder::new()
        .name("mcp-stdout".into())
        .spawn(move || {
            let mut output = io::stdout().lock();
            while let Some((bytes, done)) = receive.blocking_recv() {
                let result = output.write_all(&bytes).and_then(|()| output.flush());
                let failed = result.is_err();
                let _ = done.send(result);
                if failed {
                    break;
                }
            }
        })?;
    Ok((
        BufReader::new(Input {
            chunks: received,
            current: Vec::new(),
            offset: 0,
        }),
        Writer(frames),
    ))
}

#[cfg(test)]
pub(super) struct AsyncOutput<W>(pub W);

#[cfg(test)]
impl<W: tokio::io::AsyncWrite + Unpin> Output for AsyncOutput<W> {
    async fn send(&mut self, value: &Value) -> Result<()> {
        super::super::write_line(&mut self.0, value).await
    }
}
