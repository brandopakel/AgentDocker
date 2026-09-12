//! Bounded JSONL transport to an owned Codex app-server child.
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{collections::VecDeque, path::Path, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    time::{Instant, timeout, timeout_at},
};

const MAX_FRAME: usize = 4 * 1024 * 1024;
const MAX_BUFFERED_EVENTS: usize = 512;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

pub(super) struct Provider {
    child: Child,
    input: Option<ChildStdin>,
    output: Option<BufReader<ChildStdout>>,
    frame: Vec<u8>,
    pending: VecDeque<(Value, usize)>,
    pending_bytes: usize,
    sequence: u64,
}

impl Provider {
    pub fn start(program: &Path, arguments: &[String], cwd: &Path) -> Result<Self> {
        // Inherit the supervised bridge's process group, provider configuration,
        // authentication, and permission policy. No detached provider daemon.
        let mut child = Command::new(program)
            .args(["app-server", "--stdio"])
            .args(arguments)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .context("cannot start the owned Codex app server")?;
        let input = child.stdin.take().context("Codex stdin is unavailable")?;
        let output = child.stdout.take().context("Codex stdout is unavailable")?;
        Ok(Self {
            child,
            input: Some(input),
            output: Some(BufReader::new(output)),
            frame: Vec::new(),
            pending: VecDeque::new(),
            pending_bytes: 0,
            sequence: 0,
        })
    }

    pub async fn send(&mut self, value: &Value) -> Result<()> {
        let mut data = serde_json::to_vec(value)?;
        ensure!(
            data.len() <= MAX_FRAME,
            "Codex request exceeds the frame limit"
        );
        data.push(b'\n');
        let input = self.input.as_mut().context("Codex transport is closed")?;
        timeout(Duration::from_secs(5), async {
            input.write_all(&data).await?;
            input.flush().await
        })
        .await
        .context("Codex transport write timed out")??;
        Ok(())
    }

    async fn receive(&mut self) -> Result<Value> {
        let output = self.output.as_mut().context("Codex transport is closed")?;
        let data = read_frame(output, &mut self.frame).await?;
        serde_json::from_slice(&data).context("Codex sent an invalid protocol frame")
    }

    pub async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        self.sequence = self
            .sequence
            .checked_add(1)
            .context("Codex request ID limit reached")?;
        let id = self.sequence;
        let deadline = Instant::now() + REQUEST_TIMEOUT;
        self.send(&json!({"id":id,"method":method,"params":params}))
            .await?;
        loop {
            let value = timeout_at(deadline, self.receive())
                .await
                .with_context(|| format!("Codex {method} reply timed out"))??;
            if value.get("id").and_then(Value::as_u64) == Some(id) && value.get("method").is_none()
            {
                if let Some(error) = value.get("error") {
                    bail!(
                        "Codex {method} refused: {}",
                        error
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("provider error")
                    );
                }
                return value
                    .get("result")
                    .cloned()
                    .context("Codex response has no result");
            }
            // A provider may emit the user item before the turn/start response.
            // Retain it in order; never silently drop a potential receipt.
            let bytes = serde_json::to_vec(&value)?.len();
            ensure!(
                self.pending.len() < MAX_BUFFERED_EVENTS
                    && self.pending_bytes.saturating_add(bytes) <= MAX_FRAME,
                "Codex events exceeded the pending-request bound; delivery needs recovery"
            );
            self.pending_bytes += bytes;
            self.pending.push_back((value, bytes));
        }
    }

    pub async fn next(&mut self) -> Result<Value> {
        if let Some((value, bytes)) = self.pending.pop_front() {
            self.pending_bytes -= bytes;
            return Ok(value);
        }
        self.receive().await
    }

    pub async fn initialize(&mut self) -> Result<()> {
        self.request("initialize", json!({"clientInfo": {
            "name":"agentdocker_codex_input", "title":"AgentDocker", "version":env!("CARGO_PKG_VERSION")
        },"capabilities":{"experimentalApi":true}})).await?;
        self.send(&json!({"method":"initialized","params":{}}))
            .await
    }

    pub async fn shutdown(mut self) -> Result<()> {
        // Drop both owned pipes before waiting: EOF permits normal shutdown and
        // a full output pipe cannot prevent the child from exiting.
        self.input.take();
        self.output.take();
        match timeout(Duration::from_secs(5), self.child.wait()).await {
            Ok(result) => {
                result?;
                Ok(())
            }
            Err(_) => {
                self.child.start_kill()?;
                timeout(Duration::from_secs(5), self.child.wait())
                    .await
                    .context("owned Codex process did not exit")??;
                bail!("owned Codex process required forced shutdown; session recovery is paused")
            }
        }
    }
}

pub(super) async fn read_frame<R: AsyncBufRead + Unpin>(
    input: &mut R,
    frame: &mut Vec<u8>,
) -> Result<Vec<u8>> {
    loop {
        let available = input.fill_buf().await?;
        ensure!(
            !available.is_empty(),
            "Codex transport closed before the next complete frame"
        );
        let newline = available.iter().position(|byte| *byte == b'\n');
        let count = newline.map_or(available.len(), |at| at + 1);
        ensure!(
            frame.len().saturating_add(count) <= MAX_FRAME,
            "Codex protocol frame exceeds the size limit"
        );
        frame.extend_from_slice(&available[..count]);
        input.consume(count);
        if newline.is_some() {
            return Ok(std::mem::take(frame));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn cancelled_partial_frame_is_retained_and_next_frame_stays_separate() {
        let (mut writer, reader) = tokio::io::duplex(128);
        let mut reader = BufReader::new(reader);
        let mut frame = Vec::new();
        writer.write_all(b"{\"part\":").await.unwrap();
        assert!(
            timeout(
                Duration::from_millis(10),
                read_frame(&mut reader, &mut frame)
            )
            .await
            .is_err()
        );
        writer.write_all(b"1}\n{\"next\":2}\n").await.unwrap();
        assert_eq!(
            read_frame(&mut reader, &mut frame).await.unwrap(),
            b"{\"part\":1}\n"
        );
        assert_eq!(
            read_frame(&mut reader, &mut frame).await.unwrap(),
            b"{\"next\":2}\n"
        );
        drop(writer);
        assert!(read_frame(&mut reader, &mut frame).await.is_err());
    }
    #[tokio::test]
    async fn oversized_and_unterminated_frames_are_refused() {
        let mut oversized = std::io::Cursor::new(vec![b'x'; MAX_FRAME + 1]);
        assert!(
            read_frame(&mut oversized, &mut Vec::new())
                .await
                .unwrap_err()
                .to_string()
                .contains("size limit")
        );
        let mut partial = std::io::Cursor::new(b"{\"unterminated\":true}");
        assert!(read_frame(&mut partial, &mut Vec::new()).await.is_err());
    }
}
