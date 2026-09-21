//! `agentdocker attach`: your terminal, connected to an agent's.
//!
//! The agent's terminal belongs to the daemon, so attaching and detaching
//! are just a client coming and going. Detaching leaves the agent running
//! and typing at it, exactly as it was.

use std::io::IsTerminal;
#[cfg(unix)]
use std::os::fd::{AsFd, AsRawFd};

use agentdocker_core::{Request, Response, protocol};
use anyhow::{Context, Result, bail};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

use crate::client::Client;

#[cfg_attr(windows, path = "attach/input_windows.rs")]
mod input;

/// Ctrl-] detaches, the way `telnet` has always done it.
const DETACH: u8 = 0x1d;

pub async fn run(client: &Client, agent: &str) -> Result<()> {
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        bail!("attach needs a terminal; run it from a shell rather than a pipe");
    }
    let (mut reader, mut write_half) = open(client, agent).await?;

    eprintln!("attached to {agent}; press Ctrl-] to detach without stopping it");
    // Raw mode from here, restored by the guard however this ends.
    #[cfg(unix)]
    let _raw = agentdocker_host::pty::RawMode::enter(stdin.as_raw_fd())
        .context("cannot put this terminal in raw mode")?;
    #[cfg(windows)]
    let _raw = agentdocker_host::pty::RawMode::enter(std_handle::input(), std_handle::output())
        .context("cannot put this console in raw mode")?;
    #[cfg(unix)]
    let mut keys = input::Input::open(stdin.as_fd()).context("cannot open terminal input")?;
    #[cfg(windows)]
    let mut keys = input::Input::open(std_handle::input()).context("cannot read the console")?;
    let mut retries = crate::client::StreamRetries::default();
    let outcome = loop {
        match pump(reader, write_half, &mut keys, tokio::io::stdout()).await {
            Ok(Left::Lost { progressed }) if client.still_served().await => {
                // The daemon was replaced underneath this attachment. The
                // terminal itself never moved: its session owner outlives
                // any daemon, so the successor attaches to the same one.
                if let Err(error) = retries.wait(progressed).await {
                    break Err(error).with_context(|| {
                        format!("cannot reconnect attachment to {agent}; its exit is unconfirmed")
                    });
                }
                match open(client, agent).await {
                    Ok(reopened) => {
                        eprint!("\r\n[the daemon was replaced; attached again to {agent}]\r\n");
                        (reader, write_half) = reopened;
                    }
                    Err(error) => {
                        break Err(error).with_context(|| {
                            format!(
                                "cannot reconnect attachment to {agent}; its exit is unconfirmed"
                            )
                        });
                    }
                }
            }
            Ok(Left::Lost { .. }) => {
                break Err(anyhow::anyhow!(
                    "lost attachment to {agent}; its exit is unconfirmed"
                ));
            }
            Ok(Left::Ended) => {
                eprint!("\r\n{agent} ended\r\n");
                break Ok(());
            }
            Ok(Left::Detached) => {
                eprint!("\r\ndetached from {agent}; it is still running\r\n");
                break Ok(());
            }
            Err(error) => break Err(error),
        }
    };
    // The guard restores the terminal as it drops; say goodbye on a fresh
    // line either way.
    eprint!("\r\n");
    outcome
}

type Attached = (
    BufReader<agentdocker_host::ipc::OwnedReadHalf>,
    agentdocker_host::ipc::OwnedWriteHalf,
);

/// The size of the window this command runs in, as the terminal reports
/// it: the standard input's on Unix, the console's screen buffer window
/// on Windows.
fn current_window_size() -> Option<(u16, u16)> {
    #[cfg(unix)]
    return agentdocker_host::pty::window_size(std::io::stdin().as_raw_fd());
    #[cfg(windows)]
    agentdocker_host::pty::window_size(std_handle::output())
}

/// The process's standard console handles, for the modes and the size:
/// borrowed for the process's lifetime, which is how long they are open.
#[cfg(windows)]
mod std_handle {
    use std::os::windows::io::{AsHandle, BorrowedHandle};
    pub(super) fn input() -> BorrowedHandle<'static> {
        static STDIN: std::sync::LazyLock<std::io::Stdin> =
            std::sync::LazyLock::new(std::io::stdin);
        STDIN.as_handle()
    }
    pub(super) fn output() -> BorrowedHandle<'static> {
        static STDOUT: std::sync::LazyLock<std::io::Stdout> =
            std::sync::LazyLock::new(std::io::stdout);
        STDOUT.as_handle()
    }
}

/// When the window changes: `SIGWINCH` on Unix; on Windows nothing tells
/// a process, so the size is looked at a few times a second and a
/// difference is the change.
#[cfg(unix)]
struct WindowWatch(tokio::signal::unix::Signal);
#[cfg(unix)]
impl WindowWatch {
    fn new() -> Result<Self> {
        Ok(Self(
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())
                .context("cannot watch for window changes")?,
        ))
    }
    async fn changed(&mut self) -> Option<(u16, u16)> {
        self.0.recv().await;
        current_window_size()
    }
}
#[cfg(windows)]
struct WindowWatch {
    ticks: tokio::time::Interval,
    last: Option<(u16, u16)>,
}
#[cfg(windows)]
impl WindowWatch {
    fn new() -> Result<Self> {
        Ok(Self {
            ticks: tokio::time::interval(std::time::Duration::from_millis(250)),
            last: current_window_size(),
        })
    }
    async fn changed(&mut self) -> Option<(u16, u16)> {
        loop {
            self.ticks.tick().await;
            let now = current_window_size();
            if now != self.last {
                self.last = now;
                return now;
            }
        }
    }
}

/// Attach to the agent's terminal at this window size and read the
/// daemon's acknowledgement, so a refusal arrives as an error rather than
/// as silence.
async fn open(client: &Client, agent: &str) -> Result<Attached> {
    let (cols, rows) = current_window_size().unzip();
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
    let mut line = String::new();
    if tokio::io::AsyncBufReadExt::read_line(&mut reader, &mut line).await? == 0 {
        bail!("agentd closed the connection without attaching");
    }
    match crate::client::into_result(serde_json::from_str::<Response>(&line)?)? {
        Response::EventsReady => {}
        other => bail!("unexpected reply to attach: {other:?}"),
    }
    Ok((reader, write_half))
}

/// How an attachment came to an end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Left {
    /// The human pressed Ctrl-]; the agent runs on.
    Detached,
    /// The daemon said `end`: the agent's terminal is closed.
    Ended,
    /// The connection closed with nothing said: the daemon went away,
    /// which is not the same as the agent ending.
    Lost { progressed: bool },
}

/// Keystrokes out, terminal bytes in, until the agent ends, the human
/// detaches, or the daemon goes away.
async fn pump<R, W, K, S>(
    mut reader: BufReader<R>,
    mut write_half: W,
    mut keys: K,
    mut screen: S,
) -> Result<Left>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
    K: AsyncRead + Unpin,
    S: AsyncWrite + Unpin,
{
    let mut buffer = [0_u8; 4096];
    let mut line = String::new();
    let mut progressed = false;
    let mut window = WindowWatch::new()?;

    loop {
        line.clear();
        tokio::select! {
            // What the agent printed.
            read = tokio::io::AsyncBufReadExt::read_line(&mut reader, &mut line) => {
                if read? == 0 {
                    return Ok(Left::Lost { progressed });
                }
                match serde_json::from_str::<Response>(&line)? {
                    Response::Output { data } => {
                        if let Some(bytes) = protocol::decode_bytes(&data) {
                            progressed |= !bytes.is_empty();
                            screen.write_all(&bytes).await?;
                            screen.flush().await?;
                        }
                    }
                    Response::Lagged { skipped } => {
                        eprint!("\r\n[{skipped} bytes of output were dropped]\r\n");
                    }
                    Response::End => return Ok(Left::Ended),
                    error @ Response::Error { .. } => {
                        crate::client::into_result(error)?;
                    }
                    _ => {}
                }
            }
            // What the human typed.
            typed = keys.read(&mut buffer) => {
                let read = typed?;
                if read == 0 {
                    return Ok(Left::Detached);
                }
                if let Some(at) = buffer[..read].iter().position(|byte| *byte == DETACH) {
                    // Send whatever preceded the detach, then leave.
                    if at > 0 {
                        send_input(&mut write_half, &buffer[..at]).await?;
                    }
                    return Ok(Left::Detached);
                }
                send_input(&mut write_half, &buffer[..read]).await?;
            }
            // The window changed.
            size = window.changed() => {
                if let Some((cols, rows)) = size {
                    let frame = serde_json::to_string(&Request::AttachResize { cols, rows })?;
                    write_half.write_all(format!("{frame}\n").as_bytes()).await?;
                }
            }
        }
    }
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
        let left = pump(reader, write_half, keys, &mut screen).await.unwrap();
        assert_eq!(left, Left::Ended);
        assert_eq!(
            String::from_utf8_lossy(&screen),
            "hello from the agent",
            "the frame buffered behind the handshake still reached the screen"
        );
    }

    /// A connection that closes with nothing said is the daemon going
    /// away, which `run` tells apart from the agent ending: a replaced
    /// daemon's successor still has the same terminal to attach to.
    #[tokio::test]
    async fn a_silent_close_is_reported_as_lost_not_ended() {
        let output = serde_json::to_string(&Response::Output {
            data: protocol::encode_bytes(b"still here"),
        })
        .unwrap();
        let (mut daemon, client) = tokio::io::duplex(4096);
        daemon
            .write_all(format!("{output}\n").as_bytes())
            .await
            .unwrap();
        drop(daemon);
        let (read_half, write_half) = tokio::io::split(client);
        let (_typing, keys) = tokio::io::duplex(64);
        let mut screen = Vec::new();
        let left = pump(BufReader::new(read_half), write_half, keys, &mut screen)
            .await
            .unwrap();
        assert_eq!(left, Left::Lost { progressed: true });
        assert_eq!(String::from_utf8_lossy(&screen), "still here");
    }
}
