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

#[cfg(any(windows, test))]
mod encoded_key;

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
    let mut keys = encoded_key::Input::new(
        input::Input::open(std_handle::input()).context("cannot read the console")?,
    );
    let mut retries = crate::client::StreamRetries::default();
    let mut notices = Notices::default();
    let outcome = loop {
        match pump(
            reader,
            write_half,
            &mut keys,
            tokio::io::stdout(),
            &mut notices,
        )
        .await
        {
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
                        notices.reattached += 1;
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
    // The agent's program no longer has this screen, so what happened while
    // it did can be said now.
    for line in notices.lines(agent) {
        eprint!("{line}\r\n");
    }
    // The guard restores the terminal as it drops; say goodbye on a fresh
    // line either way.
    eprint!("\r\n");
    outcome
}

/// What happened to an attachment while the agent's program had the
/// screen. It is said once the attachment ends, never over that screen: a
/// line written there lands on whatever row the program is drawing, its
/// input box as likely as not, and its next redraw eats part of it.
#[derive(Debug, Default, PartialEq, Eq)]
struct Notices {
    /// Bytes of the agent's output the daemon could not keep for this
    /// terminal.
    dropped: u64,
    /// Times the daemon was replaced and the attachment made again.
    reattached: u32,
}

impl Notices {
    fn lines(&self, agent: &str) -> Vec<String> {
        let mut lines = Vec::new();
        match self.reattached {
            0 => {}
            1 => lines.push(format!(
                "[while attached, the daemon was replaced; attached again to {agent}]"
            )),
            times => lines.push(format!(
                "[while attached, the daemon was replaced {times} times; attached again to {agent} each time]"
            )),
        }
        if self.dropped > 0 {
            lines.push(format!(
                "[while attached, {} bytes of {agent}'s output were dropped; its screen may have been incomplete]",
                self.dropped
            ));
        }
        lines
    }
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
/// detaches, or the daemon goes away. Only the agent's own bytes reach
/// `screen`; anything to say about the attachment goes into `notices`.
async fn pump<R, W, K, S>(
    mut reader: BufReader<R>,
    mut write_half: W,
    mut keys: K,
    mut screen: S,
    notices: &mut Notices,
) -> Result<Left>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
    K: AsyncRead + Unpin,
    S: AsyncWrite + Unpin,
{
    let mut buffer = [0_u8; 4096];
    let mut line = Vec::new();
    let mut progressed = false;
    let mut window = WindowWatch::new()?;

    loop {
        tokio::select! {
            // Keep partial frames when input or resize wins the select.
            // read_until preserves appended bytes across cancellation; clear
            // them only after the complete response has been decoded.
            read = tokio::io::AsyncBufReadExt::read_until(&mut reader, b'\n', &mut line) => {
                if read? == 0 {
                    return Ok(Left::Lost { progressed });
                }
                let response = serde_json::from_slice::<Response>(&line)?;
                line.clear();
                match response {
                    Response::Output { data } => {
                        if let Some(bytes) = protocol::decode_bytes(&data) {
                            progressed |= !bytes.is_empty();
                            screen.write_all(&bytes).await?;
                            screen.flush().await?;
                        }
                    }
                    Response::Lagged { skipped } => {
                        notices.dropped = notices.dropped.saturating_add(skipped);
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
        let left = pump(
            reader,
            write_half,
            keys,
            &mut screen,
            &mut Notices::default(),
        )
        .await
        .unwrap();
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
        let left = pump(
            BufReader::new(read_half),
            write_half,
            keys,
            &mut screen,
            &mut Notices::default(),
        )
        .await
        .unwrap();
        assert_eq!(left, Left::Lost { progressed: true });
        assert_eq!(String::from_utf8_lossy(&screen), "still here");
    }

    /// The agent's program has the screen while attached: a lag is noted
    /// for after the attachment, and only the program's own bytes are
    /// written while it lasts.
    #[tokio::test]
    async fn dropped_output_is_noted_for_later_and_never_written_over_the_screen() {
        let frame = |response: Response| serde_json::to_string(&response).unwrap() + "\n";
        let output = |text: &str| Response::Output {
            data: protocol::encode_bytes(text.as_bytes()),
        };
        let (mut daemon, client) = tokio::io::duplex(4096);
        let mut frames = frame(output("\x1b[Hbefore"));
        frames += &frame(Response::Lagged { skipped: 700 });
        frames += &frame(Response::Lagged { skipped: 34 });
        frames += &frame(output(" after"));
        frames += &frame(Response::End);
        daemon.write_all(frames.as_bytes()).await.unwrap();
        let (read_half, write_half) = tokio::io::split(client);
        let (_typing, keys) = tokio::io::duplex(64);
        let mut screen = Vec::new();
        let mut notices = Notices::default();
        let left = pump(
            BufReader::new(read_half),
            write_half,
            keys,
            &mut screen,
            &mut notices,
        )
        .await
        .unwrap();
        assert_eq!(left, Left::Ended);
        assert_eq!(String::from_utf8_lossy(&screen), "\x1b[Hbefore after");
        assert_eq!(
            notices,
            Notices {
                dropped: 734,
                reattached: 0
            }
        );
        assert_eq!(
            notices.lines("fixture"),
            [
                "[while attached, 734 bytes of fixture's output were dropped; its screen may have been incomplete]"
            ]
        );
    }

    #[test]
    fn a_quiet_attachment_has_nothing_to_say_afterwards() {
        assert!(Notices::default().lines("fixture").is_empty());
        let replaced = Notices {
            dropped: 0,
            reattached: 1,
        };
        assert_eq!(
            replaced.lines("fixture"),
            ["[while attached, the daemon was replaced; attached again to fixture]"]
        );
        let twice = Notices {
            dropped: 5,
            reattached: 2,
        };
        assert_eq!(twice.lines("fixture").len(), 2);
        assert!(twice.lines("fixture")[0].contains("replaced 2 times"));
    }
    /// Typing may win select after only part of a daemon frame was read.
    /// Wait for the actual partial read and echoed input request, not a timer,
    /// before releasing the rest of the output frame.
    #[tokio::test]
    async fn typing_during_a_partial_output_frame_preserves_the_frame() {
        use std::{
            pin::Pin,
            task::{Context, Poll},
        };
        use tokio::io::{AsyncBufReadExt, ReadBuf};
        struct ObservedRead<R> {
            inner: R,
            consumed: Option<tokio::sync::oneshot::Sender<()>>,
        }
        impl<R: AsyncRead + Unpin> AsyncRead for ObservedRead<R> {
            fn poll_read(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
                buf: &mut ReadBuf<'_>,
            ) -> Poll<std::io::Result<()>> {
                let before = buf.filled().len();
                let result = Pin::new(&mut self.inner).poll_read(cx, buf);
                if buf.filled().len() > before
                    && let Some(consumed) = self.consumed.take()
                {
                    let _ = consumed.send(());
                }
                result
            }
        }
        let output = serde_json::to_vec(&Response::Output {
            data: protocol::encode_bytes("first é雪 then last".as_bytes()),
        })
        .unwrap();
        let split = output.len() / 2;
        let (mut daemon, client) = tokio::io::duplex(4096);
        daemon.write_all(&output[..split]).await.unwrap();
        let (read_half, write_half) = tokio::io::split(client);
        let (consumed, partial_read) = tokio::sync::oneshot::channel();
        let reader = ObservedRead {
            inner: read_half,
            consumed: Some(consumed),
        };
        let (mut typing, keys) = tokio::io::duplex(64);
        let mut screen = Vec::new();
        let driver = async {
            partial_read.await.unwrap();
            typing.write_all(b"typed").await.unwrap();
            let mut daemon = BufReader::new(daemon);
            let mut request = String::new();
            daemon.read_line(&mut request).await.unwrap();
            let Request::AttachInput { data } = serde_json::from_str(&request).unwrap() else {
                panic!("expected typed input while the output frame is incomplete");
            };
            assert_eq!(protocol::decode_bytes(&data).unwrap(), b"typed");
            daemon.get_mut().write_all(&output[split..]).await.unwrap();
            daemon.get_mut().write_all(b"\n").await.unwrap();
            let mut end = serde_json::to_vec(&Response::End).unwrap();
            end.push(b'\n');
            daemon.get_mut().write_all(&end).await.unwrap();
            // Keep the input peer alive until pump consumes End.
            std::future::pending::<()>().await;
        };
        let mut notices = Notices::default();
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::select! {
                result = pump(
                    BufReader::new(reader),
                    write_half,
                    keys,
                    &mut screen,
                    &mut notices,
                ) => result,
                () = driver => unreachable!(),
            }
        })
        .await
        .expect("partial output/typed input exchange must complete");
        assert_eq!(result.unwrap(), Left::Ended);
        assert_eq!(screen, "first é雪 then last".as_bytes());
    }
}
