//! Minimal client for the agentd socket protocol.

use std::path::PathBuf;

use agentdocker_core::{Request, Response, paths};
use anyhow::{Context, Result, bail};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

pub struct Client {
    socket: PathBuf,
}

impl Client {
    pub fn new(socket: Option<PathBuf>) -> Self {
        let socket = socket.unwrap_or_else(|| paths::socket_path(&paths::default_home()));
        Self { socket }
    }

    async fn connect(&self, request: &Request) -> Result<BufReader<UnixStream>> {
        let mut stream = UnixStream::connect(&self.socket).await.with_context(|| {
            format!(
                "cannot reach agentd at {} (start it with `agentd`)",
                self.socket.display()
            )
        })?;
        let mut line = serde_json::to_string(request)?;
        line.push('\n');
        stream.write_all(line.as_bytes()).await?;
        Ok(BufReader::new(stream))
    }

    /// Send one request and read exactly one response. Error responses
    /// become `Err`.
    pub async fn call(&self, request: &Request) -> Result<Response> {
        let mut reader = self.connect(request).await?;
        let mut line = String::new();
        if reader.read_line(&mut line).await? == 0 {
            bail!("agentd closed the connection without answering");
        }
        into_result(serde_json::from_str(&line)?)
    }

    /// Send one request and feed every response to `on_response` until the
    /// daemon ends the stream or `on_response` returns `false`.
    pub async fn stream(
        &self,
        request: &Request,
        mut on_response: impl FnMut(Response) -> Result<bool>,
    ) -> Result<()> {
        let mut reader = self.connect(request).await?;
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line).await? == 0 {
                return Ok(());
            }
            match into_result(serde_json::from_str(&line)?)? {
                Response::End => return Ok(()),
                response => {
                    if !on_response(response)? {
                        return Ok(());
                    }
                }
            }
        }
    }
}

fn into_result(response: Response) -> Result<Response> {
    match response {
        Response::Error {
            code,
            message,
            details,
        } => {
            let mut text = format!("{message} ({code:?})");
            if let Some(details) = details {
                text.push('\n');
                text.push_str(&serde_json::to_string_pretty(&details)?);
            }
            bail!(text)
        }
        other => Ok(other),
    }
}
