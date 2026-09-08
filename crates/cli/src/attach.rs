//! `agentdocker attach`: your terminal, connected to an agent's.
//!
//! The agent's terminal belongs to the daemon, so attaching and detaching
//! are just a client coming and going. Detaching leaves the agent running
//! and typing at it, exactly as it was.

use std::io::IsTerminal;
use std::os::fd::{AsFd, AsRawFd};

use agentdocker_core::{Request, Response, protocol};
use anyhow::{Context, Result, bail};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

use crate::client::Client;

mod input;

/// Ctrl-] detaches, the way `telnet` has always done it.
const DETACH: u8 = 0x1d;

pub async fn run(client: &Client, agent: &str) -> Result<()> {
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        bail!("attach needs a terminal; run it from a shell rather than a pipe");
    }
    let size = agentdocker_host::pty::window_size(stdin.as_raw_fd());
    let (cols, rows) = size.unzip();

    let stream = client
        .open(&Request::Attach {
            agent: agent.to_owned(),
            cols,
            rows,
        })
        .await?;

    // Split before the handshake, and keep the same reader afterwards.
    // The daemon can write `events_ready` and the first screenful in one
    // go; a reader built for the handshake and then dropped would take
    // whatever it had buffered with it.
    let (read_half, write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    // The daemon acknowledges before the first byte, so a refusal arrives
    // as an error rather than as silence.
    let mut line = String::new();
    if tokio::io::AsyncBufReadExt::read_line(&mut reader, &mut line).await? == 0 {
        bail!("agentd closed the connection without attaching");
    }
    match serde_json::from_str::<Response>(&line)? {
        Response::EventsReady => {}
        Response::Error { message, .. } => bail!("{message}"),
        other => bail!("unexpected reply to attach: {other:?}"),
    }

    eprintln!("attached to {agent}; press Ctrl-] to detach without stopping it");
    // Raw mode from here, restored by the guard however this ends.
    let _raw = agentdocker_host::pty::RawMode::enter(stdin.as_raw_fd())
        .context("cannot put this terminal in raw mode")?;
    let keys = input::Input::open(stdin.as_fd()).context("cannot open terminal input")?;
    let outcome = pump(reader, write_half, agent, keys, tokio::io::stdout()).await;
    // The guard restores the terminal as it drops; say goodbye on a fresh
    // line either way.
    eprint!("\r\n");
    outcome
}

/// Keystrokes out, terminal bytes in, until the agent ends or the human
/// detaches.
async fn pump<R, W, K, S>(
    mut reader: BufReader<R>,
    mut write_half: W,
    agent: &str,
    mut keys: K,
    mut screen: S,
) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
    K: AsyncRead + Unpin,
    S: AsyncWrite + Unpin,
{
    let mut buffer = [0_u8; 4096];
    let mut line = String::new();
    let mut winch = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())
        .context("cannot watch for window changes")?;

    loop {
        line.clear();
        tokio::select! {
            // What the agent printed.
            read = tokio::io::AsyncBufReadExt::read_line(&mut reader, &mut line) => {
                if read? == 0 {
                    break;
                }
                match serde_json::from_str::<Response>(&line)? {
                    Response::Output { data } => {
                        if let Some(bytes) = protocol::decode_bytes(&data) {
                            screen.write_all(&bytes).await?;
                            screen.flush().await?;
                        }
                    }
                    Response::Lagged { skipped } => {
                        eprint!("\r\n[{skipped} bytes of output were dropped]\r\n");
                    }
                    Response::End => break,
                    Response::Error { message, .. } => bail!("{message}"),
                    _ => {}
                }
            }
            // What the human typed.
            typed = keys.read(&mut buffer) => {
                let read = typed?;
                if read == 0 {
                    break;
                }
                if let Some(at) = buffer[..read].iter().position(|byte| *byte == DETACH) {
                    // Send whatever preceded the detach, then leave.
                    if at > 0 {
                        send_input(&mut write_half, &buffer[..at]).await?;
                    }
                    eprint!("\r\ndetached from {agent}; it is still running\r\n");
                    return Ok(());
                }
                send_input(&mut write_half, &buffer[..read]).await?;
            }
            // The window changed.
            _ = winch.recv() => {
                if let Some((cols, rows)) =
                    agentdocker_host::pty::window_size(std::io::stdin().as_raw_fd())
                {
                    let frame = serde_json::to_string(&Request::AttachResize { cols, rows })?;
                    write_half.write_all(format!("{frame}\n").as_bytes()).await?;
                }
            }
        }
    }
    eprint!("\r\n{agent} ended\r\n");
    Ok(())
}

async fn send_input(write_half: &mut (impl AsyncWrite + Unpin), bytes: &[u8]) -> Result<()> {
    let frame = serde_json::to_string(&Request::AttachInput {
        data: protocol::encode_bytes(bytes),
    })?;
    write_half
        .write_all(format!("{frame}\n").as_bytes())
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whatever the daemon wrote before the handshake was read is still
    /// the agent's output. A `BufReader` can pull `events_ready` and the
    /// first screenful out of the socket in one read, so the reader the
    /// handshake used has to be the one that carries on.
    #[tokio::test]
    async fn output_batched_with_the_handshake_is_not_lost() {
        let ready = serde_json::to_string(&Response::EventsReady).unwrap();
        let output = serde_json::to_string(&Response::Output {
            data: protocol::encode_bytes(b"hello from the agent"),
        })
        .unwrap();
        let end = serde_json::to_string(&Response::End).unwrap();

        // One write: both frames land in the same buffer fill.
        let (mut daemon, client) = tokio::io::duplex(4096);
        daemon
            .write_all(format!("{ready}\n{output}\n{end}\n").as_bytes())
            .await
            .unwrap();

        let (read_half, write_half) = tokio::io::split(client);
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        tokio::io::AsyncBufReadExt::read_line(&mut reader, &mut line)
            .await
            .unwrap();
        assert!(matches!(
            serde_json::from_str::<Response>(&line).unwrap(),
            Response::EventsReady
        ));

        // Nothing is ever typed, so the keyboard side must not end the
        // loop by reaching EOF; the far end of this pair stays open.
        let (_typing, keys) = tokio::io::duplex(64);
        let mut screen = Vec::new();
        pump(reader, write_half, "agent", keys, &mut screen)
            .await
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&screen),
            "hello from the agent",
            "the frame buffered behind the handshake still reached the screen"
        );
    }
}
