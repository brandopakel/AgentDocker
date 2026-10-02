//! The app's terminal: an agent's pty, drawn in the window.
//!
//! Same `attach` protocol the CLI speaks. A reader thread feeds the bytes
//! to a vt parser, which keeps a screen the UI thread draws; keystrokes go
//! back the other way. Detaching is closing the connection, so the agent
//! neither notices nor stops.

use std::sync::{Arc, Mutex};

use crate::color::Rgb;
use crate::wake::Wake;
use agentdocker_core::{Request, Response, protocol};

use crate::client::Client;
use crate::theme::Palette;

mod display;
pub mod keys;
pub use display::Display;
mod control;
mod input;
use input::{Input, Outbound};

/// Cells wide and tall a terminal starts at, until the view says otherwise.
const DEFAULT_SIZE: (u16, u16) = (80, 24);

/// Rows of history the parser keeps above the screen. The daemon replays
/// the last 64 KB on attach, which is worth more than the twenty-four
/// lines that fit.
const SCROLLBACK: usize = 2_000;

// Includes the encoded 64 KiB replay, protocol fields and terminating newline.
const MAX_FRAME_BYTES: usize = 256 * 1024;

/// What the window and the reader thread both hold: the screen one
/// writes and the other draws, whether the session is still up, and the
/// socket handle that lets a detach wake the reader.
#[derive(Clone)]
struct Shared {
    parser: Arc<Mutex<vt100::Parser>>,
    status: Arc<Mutex<Status>>,
    connection: Arc<Mutex<Connection>>,
    input: Arc<Input>,
}

#[derive(Default)]
struct Connection {
    closed: bool,
    stream: Option<agentdocker_host::ipc::BlockingStream>,
}

impl Connection {
    fn install(&mut self, stream: agentdocker_host::ipc::BlockingStream) -> bool {
        if self.closed {
            let _ = stream.shutdown(std::net::Shutdown::Both);
            return false;
        }
        self.stream = Some(stream);
        true
    }

    fn close(&mut self) {
        self.closed = true;
        if let Some(stream) = self.stream.take() {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    }
}

impl Shared {
    fn close(&self) {
        // Closing is remembered even before the connection has been installed.
        lock(&self.connection).close();
        self.input.close();
    }

    fn ended(&self, reason: String, ctx: &Wake) {
        {
            let mut status = lock(&self.status);
            // Preserve the writer's original error when shutdown wakes a reader.
            if *status == Status::Attached {
                *status = Status::Ended(reason);
            }
        }
        self.close();
        ctx.request_repaint();
    }
}

/// One attached agent.
pub struct Terminal {
    pub agent: String,
    shared: Shared,
    pub input_notice: Option<&'static str>,
    size: (u16, u16),
    /// How far above the live screen the view is scrolled.
    scrollback: usize,
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.shared.close();
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Attached,
    Ended(String),
}

impl Terminal {
    /// Attach to an agent and start reading its terminal.
    pub fn attach(client: Arc<Client>, agent: String, ctx: Wake) -> Self {
        let (cols, rows) = DEFAULT_SIZE;
        let shared = Shared {
            parser: Arc::new(Mutex::new(vt100::Parser::new(rows, cols, SCROLLBACK))),
            status: Arc::new(Mutex::new(Status::Attached)),
            connection: Arc::new(Mutex::new(Connection::default())),
            input: Arc::new(Input::default()),
        };
        spawn_session(client, agent.clone(), (cols, rows), shared.clone(), ctx);
        Self {
            agent,
            shared,
            input_notice: None,
            size: (cols, rows),
            scrollback: 0,
        }
    }

    pub fn status(&self) -> Status {
        lock(&self.shared.status).clone()
    }

    /// Admit all bytes together or report rejection; never replay partial input.
    pub fn send(&mut self, bytes: Vec<u8>) {
        if bytes.is_empty() {
            return;
        }
        let admitted = if bytes.len() > input::MAX_BYTES {
            Err(input::Rejected::TooLarge)
        } else {
            self.shared
                .input
                .push(Outbound::Keys(bytes.into_boxed_slice()))
        };
        if let Err(error) = admitted {
            // Keep the notice until dismissed, even if later input is accepted.
            self.input_notice = Some(error.message());
        }
    }

    /// Move the view through the history the parser kept. Scrolling up
    /// past the top simply stops there.
    pub fn scroll(&mut self, rows: i32) {
        let want = (self.scrollback as i32 + rows).max(0) as usize;
        if want == self.scrollback {
            return;
        }
        self.scrollback = want;
        lock(&self.shared.parser).screen_mut().set_scrollback(want);
        // The parser clamps to what it actually has; take its answer back
        // so a long scroll up does not build a number nothing matches.
        self.scrollback = lock(&self.shared.parser).screen().scrollback();
    }

    /// Whether the view is showing history rather than the live screen.
    pub fn scrolled_back(&self) -> bool {
        self.scrollback > 0
    }

    /// Tell the agent its window changed. Cheap to call every frame: it
    /// only acts when the size actually differs.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let (cols, rows) = (cols.max(20), rows.max(4));
        if (cols, rows) == self.size {
            return;
        }
        if self
            .shared
            .input
            .push(Outbound::Resize { cols, rows })
            .is_ok()
        {
            self.size = (cols, rows);
            lock(&self.shared.parser).screen_mut().set_size(rows, cols);
        }
        // A rejected resize leaves size unchanged so the next UI pass retries
        // that desired size. Keystrokes are never retried automatically.
    }
}

/// A terminal colour as something to draw with, in the chosen palette.
///
/// `Default` is the palette's own text colour rather than a hard-coded
/// grey: on a light palette a hard-coded one either vanishes or shouts.
fn foreground(palette: &Palette, colour: vt100::Color) -> Rgb {
    match colour {
        vt100::Color::Default => palette.text,
        vt100::Color::Idx(i) => indexed(palette, i),
        vt100::Color::Rgb(r, g, b) => Rgb::from_rgb(r, g, b),
    }
}

/// An xterm palette index. 0–15 are the named colours; 16–231 are a
/// 6×6×6 cube; 232–255 are a greyscale ramp. Folding the last two ranges
/// into the first sixteen — which is what a modulo does — gives an agent
/// using the 256-colour palette a set of unrelated hues.
fn indexed(palette: &Palette, i: u8) -> Rgb {
    match i {
        0..=15 => palette.ansi[i as usize],
        16..=231 => {
            // The cube's six levels are not evenly spaced: the first step
            // is to 95, and the rest are 40 apart.
            let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            let c = i - 16;
            Rgb::from_rgb(level(c / 36), level((c / 6) % 6), level(c % 6))
        }
        232..=255 => {
            let grey = 8 + (i - 232) * 10;
            Rgb::from_rgb(grey, grey, grey)
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Read the agent's terminal into the parser, and type what the window
/// sends. Both ends live on their own threads so the window never waits
/// on the socket.
fn spawn_session(client: Arc<Client>, agent: String, size: (u16, u16), shared: Shared, ctx: Wake) {
    let close = shared.clone();
    let on_error = ctx.clone();
    if let Err(error) = spawn_connected(
        move || {
            client.open(&Request::Attach {
                agent,
                cols: Some(size.0),
                rows: Some(size.1),
            })
        },
        shared,
        ctx,
    ) {
        close.ended(
            format!("cannot start terminal connection: {error}"),
            &on_error,
        );
    }
}

fn spawn_connected(
    connect: impl FnOnce() -> anyhow::Result<agentdocker_host::ipc::BlockingStream> + Send + 'static,
    shared: Shared,
    ctx: Wake,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("agentdocker-terminal".into())
        .spawn(move || {
            let stream = match connect() {
                Ok(stream) => stream,
                Err(err) => return shared.ended(err.to_string(), &ctx),
            };
            let writer = match stream.try_clone() {
                Ok(writer) => writer,
                Err(err) => return shared.ended(err.to_string(), &ctx),
            };
            match stream.try_clone() {
                Ok(handle) => {
                    if !lock(&shared.connection).install(handle) {
                        return;
                    }
                }
                Err(err) => return shared.ended(err.to_string(), &ctx),
            }
            let writer_state = shared.clone();
            let writer_ctx = ctx.clone();
            let writer = match std::thread::Builder::new()
                .name("agentdocker-terminal-input".into())
                .spawn(move || {
                    run_writer(writer, &writer_state, &writer_ctx);
                }) {
                Ok(writer) => writer,
                Err(error) => {
                    return shared.ended(format!("cannot start terminal input: {error}"), &ctx);
                }
            };
            let reason = read_output(stream, &shared, &ctx);
            shared.ended(reason, &ctx);
            // Close wakes an idle writer as well as one blocked on the socket.
            // Only this background thread waits for its owned writer to finish.
            let _ = writer.join();
        })
}

fn run_writer(writer: impl std::io::Write, shared: &Shared, ctx: &Wake) {
    if let Err(error) = write_input(writer, &shared.input) {
        shared.ended(format!("terminal input failed: {error}"), ctx);
    }
}

fn write_input(mut writer: impl std::io::Write, input: &Input) -> std::io::Result<()> {
    while let Some(message) = input.recv() {
        let request = match message {
            Outbound::Keys(bytes) => Request::AttachInput {
                data: protocol::encode_bytes(&bytes),
            },
            Outbound::Resize { cols, rows } => Request::AttachResize { cols, rows },
        };
        let mut frame = serde_json::to_string(&request).map_err(std::io::Error::other)?;
        frame.push('\n');
        writer.write_all(frame.as_bytes())?;
        writer.flush()?;
    }
    Ok(())
}

/// Periodically observe closure even if shutting down another handle did not
/// wake a blocked read. Partial frames survive each read deadline.
const READ_POLL: std::time::Duration = std::time::Duration::from_secs(1);

/// Reads that wait in `poll`, never in the receive itself.
///
/// Darwin can strand a receive that begins while another descriptor of the
/// same socket is shut down: neither that shutdown nor `SO_RCVTIMEO` wakes
/// it. `poll` carries its own timeout, and `MSG_DONTWAIT` keeps the receive
/// from blocking without changing the descriptor the writer shares.
#[cfg(unix)]
struct PolledStream(agentdocker_host::ipc::BlockingStream);

#[cfg(unix)]
impl std::io::Read for PolledStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        use std::io::{Error, ErrorKind};
        use std::os::fd::AsRawFd;
        if buf.is_empty() {
            return Ok(0);
        }
        let fd = self.0.as_raw_fd();
        let mut ready = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one initialized pollfd outlives the call.
        match unsafe { libc::poll(&mut ready, 1, READ_POLL.as_millis() as libc::c_int) } {
            0 => return Err(ErrorKind::TimedOut.into()),
            n if n < 0 => {
                let error = Error::last_os_error();
                return Err(if error.kind() == ErrorKind::Interrupted {
                    ErrorKind::TimedOut.into()
                } else {
                    error
                });
            }
            _ => {}
        }
        // SAFETY: buf is valid for writes of buf.len() bytes.
        let read =
            unsafe { libc::recv(fd, buf.as_mut_ptr().cast(), buf.len(), libc::MSG_DONTWAIT) };
        if read < 0 {
            let error = Error::last_os_error();
            return Err(if error.kind() == ErrorKind::Interrupted {
                ErrorKind::WouldBlock.into()
            } else {
                error
            });
        }
        Ok(read as usize)
    }
}

fn read_frame(
    reader: &mut impl std::io::BufRead,
    line: &mut Vec<u8>,
    closed: impl Fn() -> bool,
) -> std::io::Result<usize> {
    // Retain bytes, not a String: read_line can discard an incomplete UTF-8
    // character when the underlying read returns a timeout error. Observe
    // closure between chunks too, so a continuous partial frame cannot hide it.
    loop {
        if closed() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionAborted,
                "the connection closed",
            ));
        }
        if line.len() > MAX_FRAME_BYTES {
            return Err(std::io::Error::other(
                "terminal output frame exceeds 256 KiB",
            ));
        }
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return if line.is_empty() {
                Ok(0)
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "incomplete terminal output frame",
                ))
            };
        }
        let available = &available[..available.len().min(MAX_FRAME_BYTES + 1 - line.len())];
        let count = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        line.extend_from_slice(&available[..count]);
        reader.consume(count);
        if line.len() > MAX_FRAME_BYTES {
            return Err(std::io::Error::other(
                "terminal output frame exceeds 256 KiB",
            ));
        }
        if line.ends_with(b"\n") {
            return Ok(line.len());
        }
    }
}

fn read_output(
    stream: agentdocker_host::ipc::BlockingStream,
    shared: &Shared,
    ctx: &Wake,
) -> String {
    #[cfg(unix)]
    let stream = PolledStream(stream);
    #[cfg(not(unix))]
    if let Err(error) = stream.set_read_timeout(Some(READ_POLL)) {
        return format!("cannot bound terminal reads: {error}");
    }
    let mut reader = std::io::BufReader::new(stream);
    let mut line = Vec::new();
    let mut control = control::ControlBudget::default();
    loop {
        line.clear();
        loop {
            match read_frame(&mut reader, &mut line, || lock(&shared.connection).closed) {
                Ok(0) => return "the connection closed".to_owned(),
                Ok(_) => break,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    if lock(&shared.connection).closed {
                        return "the connection closed".to_owned();
                    }
                }
                Err(error) => return error.to_string(),
            }
        }
        match serde_json::from_slice::<Response>(&line) {
            Ok(Response::Output { data }) => {
                if let Some(bytes) = protocol::decode_bytes(&data) {
                    if let Err(reason) = control.check(&bytes) {
                        return reason.to_owned();
                    }
                    lock(&shared.parser).process(&bytes);
                    ctx.request_repaint();
                } else {
                    return "invalid terminal output encoding".to_owned();
                }
            }
            Ok(Response::End) => return "the agent ended".to_owned(),
            Ok(Response::Error { message, .. }) => return message,
            Ok(_) => {}
            Err(error) => return error.to_string(),
        }
    }
}

/// The byte a key produces while Ctrl is held, from the key itself.
///
/// Not from `Key::name()`: that returns the variant's name, so `ArrowUp`
/// would start with `A` and answer 1, `Enter` would answer 5, and
/// `CloseBracket` — which is the detach key everywhere else — would
/// answer 3 instead of 0x1d.
fn control(key: keys::Key) -> Option<u8> {
    use keys::Key::*;
    let letter = |c: u8| Some(c - b'A' + 1);
    Some(match key {
        A => return letter(b'A'),
        B => return letter(b'B'),
        C => return letter(b'C'),
        D => return letter(b'D'),
        E => return letter(b'E'),
        F => return letter(b'F'),
        G => return letter(b'G'),
        H => return letter(b'H'),
        I => return letter(b'I'),
        J => return letter(b'J'),
        K => return letter(b'K'),
        L => return letter(b'L'),
        M => return letter(b'M'),
        N => return letter(b'N'),
        O => return letter(b'O'),
        P => return letter(b'P'),
        Q => return letter(b'Q'),
        R => return letter(b'R'),
        S => return letter(b'S'),
        T => return letter(b'T'),
        U => return letter(b'U'),
        V => return letter(b'V'),
        W => return letter(b'W'),
        X => return letter(b'X'),
        Y => return letter(b'Y'),
        Z => return letter(b'Z'),
        // The punctuation the escape codes use, in its usual order.
        OpenBracket => 0x1b,
        Backslash => 0x1c,
        CloseBracket => 0x1d,
        Space => 0x00,
        Minus => 0x1f,
        // Everything else keeps its ordinary meaning.
        _ => return None,
    })
}

/// What a window's key and text events mean to a terminal. Pure, so the
/// mapping every interactive agent depends on can be tested without a
/// window.
pub fn keystrokes(events: &[keys::Event]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for event in events {
        match event {
            keys::Event::Text(text) => bytes.extend_from_slice(text.as_bytes()),
            keys::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } => {
                if (modifiers.ctrl || modifiers.mac_cmd)
                    && let Some(code) = control(*key)
                {
                    bytes.push(code);
                    continue;
                }
                match key {
                    keys::Key::Enter => bytes.push(b'\r'),
                    keys::Key::Backspace => bytes.push(0x7f),
                    keys::Key::Tab => bytes.push(b'\t'),
                    keys::Key::Escape => bytes.push(0x1b),
                    keys::Key::Delete => bytes.extend_from_slice(b"\x1b[3~"),
                    keys::Key::Home => bytes.extend_from_slice(b"\x1b[H"),
                    keys::Key::End => bytes.extend_from_slice(b"\x1b[F"),
                    keys::Key::PageUp => bytes.extend_from_slice(b"\x1b[5~"),
                    keys::Key::PageDown => bytes.extend_from_slice(b"\x1b[6~"),
                    keys::Key::ArrowUp => bytes.extend_from_slice(b"\x1b[A"),
                    keys::Key::ArrowDown => bytes.extend_from_slice(b"\x1b[B"),
                    keys::Key::ArrowRight => bytes.extend_from_slice(b"\x1b[C"),
                    keys::Key::ArrowLeft => bytes.extend_from_slice(b"\x1b[D"),
                    _ => {}
                }
            }
            _ => {}
        }
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn close_peer(peer: &agentdocker_host::ipc::BlockingStream) {
        // Darwin reports ENOTCONN when the successful test already closed its
        // peer; cleanup is complete in that case, rather than a test failure.
        if let Err(error) = peer.shutdown(std::net::Shutdown::Both) {
            assert_eq!(error.kind(), std::io::ErrorKind::NotConnected);
        }
    }

    /// Abort with thread diagnostics if a socket fixture outlives `limit`.
    /// Nextest's own timeout kills the process without saying where each
    /// thread waited; the historical macOS hang left nothing else to go on.
    #[cfg(unix)]
    struct HangWatch {
        disarm: Option<std::sync::mpsc::Sender<()>>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    #[cfg(unix)]
    impl HangWatch {
        fn arm(fixture: &'static str, limit: std::time::Duration) -> Self {
            let (disarm, armed) = std::sync::mpsc::channel::<()>();
            let thread = std::thread::spawn(move || {
                if armed.recv_timeout(limit) != Err(std::sync::mpsc::RecvTimeoutError::Timeout) {
                    return;
                }
                eprintln!("{fixture}: still running after {limit:?}; thread diagnostics follow");
                eprintln!("{}", thread_report());
                std::process::abort();
            });
            Self {
                disarm: Some(disarm),
                thread: Some(thread),
            }
        }
    }

    #[cfg(unix)]
    impl Drop for HangWatch {
        fn drop(&mut self) {
            drop(self.disarm.take());
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    /// Every thread's stack from `sample`, run in a private directory and
    /// bounded so a stalled sampler cannot keep the watchdog from aborting.
    #[cfg(target_os = "macos")]
    fn thread_report() -> String {
        use std::time::{Duration, Instant};
        const SAMPLE_LIMIT: Duration = Duration::from_secs(30);
        let dir = match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(error) => return format!("cannot create a sample directory: {error}"),
        };
        let report = dir.path().join("report.sample");
        let output = dir.path().join("sample.out");
        let mut sampler = match std::fs::File::create(&output)
            .and_then(|out| Ok((out.try_clone()?, out)))
            .and_then(|(out, err)| {
                std::process::Command::new("/usr/bin/sample")
                    .arg(std::process::id().to_string())
                    .args(["2", "-mayDie", "-file"])
                    .arg(&report)
                    .stdin(std::process::Stdio::null())
                    .stdout(out)
                    .stderr(err)
                    .spawn()
            }) {
            Ok(sampler) => sampler,
            Err(error) => return format!("cannot run sample: {error}"),
        };
        let started = Instant::now();
        let finished = loop {
            match sampler.try_wait() {
                Ok(Some(status)) => break format!("sample exited: {status}"),
                Ok(None) if started.elapsed() < SAMPLE_LIMIT => {
                    std::thread::sleep(Duration::from_millis(100));
                }
                Ok(None) => {
                    // Only the sampler this watchdog started is ended.
                    let _ = sampler.kill();
                    let _ = sampler.wait();
                    break format!("sample did not finish within {SAMPLE_LIMIT:?}; ended it");
                }
                Err(error) => {
                    let _ = sampler.kill();
                    let _ = sampler.wait();
                    break format!("cannot wait for sample ({error}); ended it");
                }
            }
        };
        let read = |path: &std::path::Path| {
            std::fs::read_to_string(path).unwrap_or_else(|error| format!("<unreadable: {error}>"))
        };
        format!("{finished}\n{}\n{}", read(&output).trim(), read(&report))
    }

    /// Linux exposes each task's wait channel and state here, not its stack.
    #[cfg(all(unix, not(target_os = "macos")))]
    fn thread_report() -> String {
        let mut report = String::from("task wait states (not stacks):\n");
        let tasks = std::fs::read_dir("/proc/self/task").into_iter().flatten();
        for task in tasks.flatten() {
            let read =
                |name: &str| std::fs::read_to_string(task.path().join(name)).unwrap_or_default();
            report.push_str(&format!(
                "{:?} {} wchan={} {}\n",
                task.file_name(),
                read("comm").trim(),
                read("wchan").trim(),
                read("stat").trim()
            ));
        }
        report
    }

    fn unserved_terminal() -> Terminal {
        Terminal {
            agent: "owned-fixture".into(),
            shared: Shared {
                parser: Arc::new(Mutex::new(vt100::Parser::new(24, 80, SCROLLBACK))),
                status: Arc::new(Mutex::new(Status::Attached)),
                connection: Arc::new(Mutex::new(Connection::default())),
                input: Arc::new(Input::default()),
            },
            input_notice: None,
            size: DEFAULT_SIZE,
            scrollback: 0,
        }
    }

    #[test]
    fn terminal_input_burst_has_bounded_admission() {
        let mut terminal = unserved_terminal();
        for _ in 0..10_000 {
            terminal.send(vec![b'x']);
        }
        assert!(terminal.shared.input.retained().0 <= 32);
        assert_eq!(terminal.input_notice, Some(input::Rejected::Full.message()));
    }

    #[test]
    fn terminal_input_has_a_total_byte_budget() {
        let mut terminal = unserved_terminal();
        for _ in 0..5 {
            terminal.send(vec![b'x'; 16 * 1024]);
        }
        let retained = terminal.shared.input.retained().1;
        assert!(retained <= 64 * 1024, "retained {retained} input bytes");
    }

    #[cfg(unix)]
    #[test]
    fn detaching_while_connecting_closes_the_late_connection() {
        use std::io::Read;
        use std::time::Duration;
        let terminal = unserved_terminal();
        let shared = terminal.shared.clone();
        let (stream, mut peer) = agentdocker_host::ipc::BlockingStream::pair().unwrap();
        // Configure the socket before releasing the thread that shuts its
        // other end down. macOS may reject setsockopt after that shutdown.
        peer.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let (ready, started) = std::sync::mpsc::sync_channel(0);
        let (resume, blocked) = std::sync::mpsc::sync_channel(0);
        let session = spawn_connected(
            move || {
                ready.send(()).unwrap();
                blocked.recv_timeout(Duration::from_secs(3)).unwrap();
                Ok(stream)
            },
            shared,
            Wake::default(),
        )
        .unwrap();
        started.recv_timeout(Duration::from_secs(3)).unwrap();
        drop(terminal);
        resume.send(()).unwrap();
        let observed = peer.read(&mut [0]);
        // Clean up the original failing behavior before asserting its result.
        close_peer(&peer);
        session.join().unwrap();
        assert!(matches!(observed, Ok(0)), "late connection: {observed:?}");
    }

    #[test]
    fn rejected_input_remains_visible_and_resize_waits_for_capacity() {
        let mut terminal = unserved_terminal();
        terminal.send(vec![b'x'; input::MAX_BYTES + 1]);
        assert_eq!(
            terminal.input_notice,
            Some(input::Rejected::TooLarge.message())
        );
        assert_eq!(terminal.shared.input.retained(), (0, 0));
        terminal.send(vec![b'x'; input::MAX_BYTES]);
        // A later successful key does not erase evidence of previously lost input.
        assert_eq!(
            terminal.input_notice,
            Some(input::Rejected::TooLarge.message())
        );
        terminal.resize(120, 30);
        assert_eq!(terminal.size, DEFAULT_SIZE);
        assert_eq!(lock(&terminal.shared.parser).screen().size(), (24, 80));
        assert!(matches!(
            terminal.shared.input.recv(),
            Some(Outbound::Keys(_))
        ));
        terminal.resize(120, 30);
        assert_eq!(terminal.size, (120, 30));
        assert_eq!(lock(&terminal.shared.parser).screen().size(), (30, 120));
        terminal.shared.close();
        terminal.send(b"closed".to_vec());
        assert_eq!(
            terminal.input_notice,
            Some(input::Rejected::Closed.message())
        );
    }

    #[test]
    fn terminal_frames_are_bounded_and_keep_the_next_frame() {
        use std::io::{BufReader, Cursor};
        // The daemon's largest replay still fits after protocol encoding.
        let response = Response::Output {
            data: protocol::encode_bytes(&vec![b'x'; 64 * 1024]),
        };
        let replay = serde_json::to_string(&response).unwrap() + "\n";
        let mut reader = BufReader::new(Cursor::new(replay.clone() + "{\"type\":\"end\"}\n"));
        let mut line = Vec::new();
        assert_eq!(
            read_frame(&mut reader, &mut line, || false).unwrap(),
            replay.len()
        );
        assert_eq!(line, replay.as_bytes());
        line.clear();
        read_frame(&mut reader, &mut line, || false).unwrap();
        assert_eq!(
            serde_json::from_slice::<Response>(&line).unwrap(),
            Response::End
        );
        let mut huge = BufReader::new(Cursor::new(vec![b'x'; MAX_FRAME_BYTES * 2]));
        line.clear();
        assert!(
            read_frame(&mut huge, &mut line, || false)
                .unwrap_err()
                .to_string()
                .contains("256 KiB")
        );
        assert_eq!(line.len(), MAX_FRAME_BYTES + 1);
        let mut incomplete = Cursor::new(b"{\"type\":\"end\"}");
        line.clear();
        assert_eq!(
            read_frame(&mut incomplete, &mut line, || false)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn terminal_close_interrupts_a_continuously_available_partial_frame() {
        use std::cell::Cell;
        use std::io::{BufReader, Cursor, ErrorKind};
        let mut reader = BufReader::with_capacity(8, Cursor::new(vec![b'x'; MAX_FRAME_BYTES]));
        let observations = Cell::new(0);
        let mut line = Vec::new();
        let error = read_frame(&mut reader, &mut line, || {
            observations.set(observations.get() + 1);
            observations.get() > 1
        })
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ConnectionAborted);
        assert_eq!(line, b"xxxxxxxx");
        assert_eq!(reader.get_ref().position(), 8);
    }

    #[cfg(unix)]
    #[test]
    fn terminal_read_deadlines_preserve_every_utf8_split_and_the_following_frame() {
        use std::io::{BufReader, ErrorKind, Write};
        use std::time::Duration;
        let response = Response::Error {
            code: agentdocker_core::ErrorCode::Internal,
            message: "終わり 😀".into(),
            details: None,
        };
        let mut frame = serde_json::to_vec(&response).unwrap();
        frame.push(b'\n');
        // Include ASCII boundaries and every byte of each multi-byte character.
        for split in 0..frame.len() {
            let (stream, mut peer) = agentdocker_host::ipc::BlockingStream::pair().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_millis(5)))
                .unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = Vec::new();
            peer.write_all(&frame[..split]).unwrap();
            let error = read_frame(&mut reader, &mut line, || false).unwrap_err();
            assert!(matches!(
                error.kind(),
                ErrorKind::WouldBlock | ErrorKind::TimedOut
            ));
            assert_eq!(line, frame[..split]);
            peer.write_all(&frame[split..]).unwrap();
            peer.write_all(b"{\"type\":\"end\"}\n").unwrap();
            assert_eq!(
                read_frame(&mut reader, &mut line, || false).unwrap(),
                frame.len()
            );
            assert_eq!(serde_json::from_slice::<Response>(&line).unwrap(), response);
            line.clear();
            read_frame(&mut reader, &mut line, || false).unwrap();
            assert_eq!(
                serde_json::from_slice::<Response>(&line).unwrap(),
                Response::End
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn terminal_closure_without_socket_wakeup_is_bounded_and_keeps_the_first_error() {
        let _watch = HangWatch::arm(
            "terminal_closure_without_socket_wakeup_is_bounded_and_keeps_the_first_error",
            std::time::Duration::from_secs(20),
        );
        use std::sync::mpsc;
        use std::time::Duration;
        let terminal = unserved_terminal();
        let shared = terminal.shared.clone();
        let (stream, peer) = agentdocker_host::ipc::BlockingStream::pair().unwrap();
        // Deliberately omit a shutdown handle: exercise the fallback when
        // closure cannot wake the reader through the socket itself.
        let (finished, done) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let ctx = Wake::default();
            let reason = read_output(stream, &shared, &ctx);
            shared.ended(reason, &ctx);
            finished.send(()).unwrap();
        });
        terminal
            .shared
            .ended("original writer failure".into(), &Wake::default());
        let observed = done.recv_timeout(READ_POLL + Duration::from_secs(2));
        // Keep the peer open until after the deadline observation. Always
        // close it before joining so an old implementation cleans up too.
        close_peer(&peer);
        reader.join().unwrap();
        observed.unwrap();
        assert_eq!(
            terminal.status(),
            Status::Ended("original writer failure".into())
        );
    }

    #[cfg(unix)]
    #[test]
    fn terminal_end_joins_an_idle_writer_without_dropping_the_window_handle() {
        let _watch = HangWatch::arm(
            "terminal_end_joins_an_idle_writer_without_dropping_the_window_handle",
            std::time::Duration::from_secs(20),
        );
        use std::io::Write;
        use std::sync::mpsc;
        use std::time::Duration;
        let terminal = unserved_terminal();
        let shared = terminal.shared.clone();
        let (stream, mut peer) = agentdocker_host::ipc::BlockingStream::pair().unwrap();
        let session = spawn_connected(move || Ok(stream), shared, Wake::default()).unwrap();
        peer.write_all(b"{\"type\":\"end\"}\n").unwrap();
        let (finished, done) = mpsc::channel();
        let waiter = std::thread::spawn(move || finished.send(session.join()).unwrap());
        let observed = done.recv_timeout(Duration::from_secs(3));
        // A failing implementation must still release its fixture threads.
        terminal.shared.close();
        close_peer(&peer);
        waiter.join().unwrap();
        assert!(observed.unwrap().is_ok());
        assert_eq!(terminal.status(), Status::Ended("the agent ended".into()));
    }

    #[cfg(unix)]
    #[test]
    fn writer_failure_closes_the_reader_and_preserves_its_reason() {
        let _watch = HangWatch::arm(
            "writer_failure_closes_the_reader_and_preserves_its_reason",
            std::time::Duration::from_secs(20),
        );
        use std::io::{self, Write};
        use std::sync::mpsc;
        use std::time::Duration;
        struct FailedWriter;
        impl Write for FailedWriter {
            fn write(&mut self, _bytes: &[u8]) -> io::Result<usize> {
                eprintln!("terminal failure fixture: injecting write failure");
                Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "injected write failure",
                ))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        // Retain phase evidence if the historical macOS hang recurs. These
        // traces are test-only and successful nextest output is not archived.
        // A later passing run does not explain the original timeout.
        eprintln!("terminal failure fixture: creating socket pair");
        let mut terminal = unserved_terminal();
        let shared = terminal.shared.clone();
        let (stream, peer) = agentdocker_host::ipc::BlockingStream::pair().unwrap();
        assert!(lock(&shared.connection).install(stream.try_clone().unwrap()));
        eprintln!("terminal failure fixture: connection installed");
        let (finished, done) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            eprintln!("terminal failure fixture: reader starting");
            let ctx = Wake::default();
            let reason = read_output(stream, &shared, &ctx);
            eprintln!("terminal failure fixture: reader returned: {reason}");
            shared.ended(reason, &ctx);
            eprintln!("terminal failure fixture: reader ended cleanup returned");
            finished.send(()).unwrap();
        });
        // Inject an error while a real socket reader waits with an open peer.
        // SHUT_RD alone does not force an immediate EPIPE on Darwin.
        terminal.send(b"owned fixture".to_vec());
        eprintln!("terminal failure fixture: input queued, writer starting");
        run_writer(FailedWriter, &terminal.shared, &Wake::default());
        eprintln!("terminal failure fixture: writer returned");
        let observed = done.recv_timeout(Duration::from_secs(3));
        eprintln!("terminal failure fixture: reader completion: {observed:?}");
        terminal.shared.close();
        eprintln!("terminal failure fixture: terminal close returned");
        close_peer(&peer);
        eprintln!("terminal failure fixture: peer close returned, joining reader");
        reader.join().unwrap();
        eprintln!("terminal failure fixture: reader joined");
        observed.unwrap();
        assert!(
            matches!(terminal.status(), Status::Ended(reason) if reason.starts_with("terminal input failed:"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_empty_read_returns_at_once_without_waiting_for_data() {
        use std::io::Read;
        let (stream, _peer) = agentdocker_host::ipc::BlockingStream::pair().unwrap();
        let started = std::time::Instant::now();
        assert_eq!(PolledStream(stream).read(&mut []).unwrap(), 0);
        assert!(started.elapsed() < READ_POLL / 2);
    }

    /// Darwin can strand a receive that starts while another descriptor of
    /// the same socket is being shut down: neither the shutdown nor
    /// SO_RCVTIMEO wakes it. Race closure against the reader's first receive
    /// and require every reader to observe it.
    #[cfg(unix)]
    #[test]
    fn closing_as_a_read_starts_cannot_strand_the_reader() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::mpsc;
        use std::time::Duration;
        for round in 0..500 {
            let terminal = unserved_terminal();
            let shared = terminal.shared.clone();
            let (stream, peer) = agentdocker_host::ipc::BlockingStream::pair().unwrap();
            assert!(lock(&shared.connection).install(stream.try_clone().unwrap()));
            let started = Arc::new(AtomicBool::new(false));
            let reader_started = started.clone();
            let (finished, done) = mpsc::channel();
            let reader = std::thread::spawn(move || {
                reader_started.store(true, Ordering::Release);
                // The receiver is gone once a failed round has given up.
                let _ = finished.send(read_output(stream, &shared, &Wake::default()));
            });
            while !started.load(Ordering::Acquire) {
                std::hint::spin_loop();
            }
            // Vary where closure lands relative to the reader's first receive.
            for _ in 0..round * 37 % 4000 {
                std::hint::spin_loop();
            }
            terminal.shared.close();
            let observed = done.recv_timeout(READ_POLL + Duration::from_secs(2));
            close_peer(&peer);
            // A stranded reader cannot be joined; fail without waiting on it.
            assert!(observed.is_ok(), "round {round}: the reader missed closure");
            reader.join().unwrap();
        }
    }

    #[test]
    fn terminal_writer_preserves_input_and_resize_order() {
        use std::io::Write;
        struct Capture<'a> {
            frames: &'a mut Vec<u8>,
            input: &'a Input,
            flushes: usize,
        }
        impl Write for Capture<'_> {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.frames.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                self.flushes += 1;
                if self.flushes == 3 {
                    self.input.close();
                }
                Ok(())
            }
        }
        let input = Input::default();
        input.push(Outbound::Keys(Box::from(*b"{\0\xff"))).unwrap();
        input
            .push(Outbound::Resize {
                cols: 120,
                rows: 30,
            })
            .unwrap();
        input.push(Outbound::Keys(Box::from(*b"\r"))).unwrap();
        let mut frames = Vec::new();
        write_input(
            Capture {
                frames: &mut frames,
                input: &input,
                flushes: 0,
            },
            &input,
        )
        .unwrap();
        let requests: Vec<Request> = String::from_utf8(frames)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(
            requests,
            [
                Request::AttachInput {
                    data: protocol::encode_bytes(b"{\0\xff")
                },
                Request::AttachResize {
                    cols: 120,
                    rows: 30
                },
                Request::AttachInput {
                    data: protocol::encode_bytes(b"\r")
                }
            ]
        );
    }

    fn key(key: keys::Key, ctrl: bool) -> keys::Event {
        keys::Event::Key {
            key,
            pressed: true,
            modifiers: keys::Modifiers {
                ctrl,
                ..Default::default()
            },
        }
    }

    #[test]
    fn keys_become_what_a_terminal_expects() {
        assert_eq!(keystrokes(&[keys::Event::Text("hi".into())]), b"hi");
        assert_eq!(keystrokes(&[key(keys::Key::Enter, false)]), b"\r");
        assert_eq!(keystrokes(&[key(keys::Key::Backspace, false)]), &[0x7f]);
        assert_eq!(keystrokes(&[key(keys::Key::Escape, false)]), &[0x1b]);
        assert_eq!(keystrokes(&[key(keys::Key::ArrowUp, false)]), b"\x1b[A");
        assert_eq!(keystrokes(&[key(keys::Key::ArrowLeft, false)]), b"\x1b[D");
        assert_eq!(keystrokes(&[key(keys::Key::PageUp, false)]), b"\x1b[5~");
        // The control codes an agent's own key bindings depend on.
        assert_eq!(keystrokes(&[key(keys::Key::C, true)]), &[3], "Ctrl-C");
        assert_eq!(keystrokes(&[key(keys::Key::D, true)]), &[4], "Ctrl-D");
        assert_eq!(keystrokes(&[key(keys::Key::A, true)]), &[1]);
        assert_eq!(keystrokes(&[key(keys::Key::Z, true)]), &[26]);
        // Ctrl-] is what detaches everywhere else, so it must be 0x1d
        // rather than whatever the variant's *name* begins with.
        assert_eq!(
            keystrokes(&[key(keys::Key::CloseBracket, true)]),
            &[0x1d],
            "Ctrl-]"
        );
        assert_eq!(keystrokes(&[key(keys::Key::OpenBracket, true)]), &[0x1b]);
        assert_eq!(keystrokes(&[key(keys::Key::Space, true)]), &[0x00]);
        // A held Ctrl must not turn a named key into a letter code:
        // `ArrowUp` is not Ctrl-A and `Enter` is not Ctrl-E.
        assert_eq!(keystrokes(&[key(keys::Key::ArrowUp, true)]), b"\x1b[A");
        assert_eq!(keystrokes(&[key(keys::Key::Enter, true)]), b"\r");
        assert_eq!(keystrokes(&[key(keys::Key::Tab, true)]), b"\t");
        // A release is not a keystroke, and unknown keys are ignored.
        assert!(
            keystrokes(&[keys::Event::Key {
                key: keys::Key::A,
                pressed: false,
                modifiers: keys::Modifiers::default(),
            }])
            .is_empty()
        );
        // Several events in one frame arrive in order.
        assert_eq!(
            keystrokes(&[keys::Event::Text("ls".into()), key(keys::Key::Enter, false)]),
            b"ls\r"
        );
    }

    #[test]
    fn colours_survive_the_round_trip_from_escape_codes() {
        let mut parser = vt100::Parser::new(4, 20, 0);
        parser.process(b"\x1b[31mred\x1b[0m plain");
        let screen = parser.screen();
        let red = screen.cell(0, 0).unwrap();
        assert_eq!(red.contents(), "r");
        let palette = Palette::named("AgentDocker");
        assert_eq!(
            foreground(palette, red.fgcolor()),
            palette.ansi[1],
            "index 1 is red"
        );
        let plain = screen.cell(0, 4).unwrap();
        assert_eq!(
            foreground(palette, plain.fgcolor()),
            palette.text,
            "default is the palette's own text colour"
        );
        // And it moves with the palette, rather than being one grey
        // that happens to sit on both grounds badly.
        let light = Palette::named("Basic");
        assert_eq!(foreground(light, plain.fgcolor()), light.text);
        assert_eq!(
            foreground(palette, vt100::Color::Rgb(1, 2, 3)),
            Rgb::from_rgb(1, 2, 3)
        );
    }

    #[test]
    fn the_256_colour_palette_is_not_folded_into_sixteen() {
        // The named sixteen are the table itself.
        let palette = Palette::named("AgentDocker");
        assert_eq!(indexed(palette, 0), palette.ansi[0]);
        assert_eq!(indexed(palette, 15), palette.ansi[15]);
        // The cube: 16 is its black corner, 231 its white one, and the
        // levels step 0, 95, 135, 175, 215, 255.
        assert_eq!(indexed(palette, 16), Rgb::from_rgb(0, 0, 0));
        assert_eq!(indexed(palette, 231), Rgb::from_rgb(255, 255, 255));
        assert_eq!(indexed(palette, 196), Rgb::from_rgb(255, 0, 0), "cube red");
        assert_eq!(indexed(palette, 46), Rgb::from_rgb(0, 255, 0), "cube green");
        assert_eq!(indexed(palette, 21), Rgb::from_rgb(0, 0, 255), "cube blue");
        // The greyscale ramp, which a modulo would have scattered.
        assert_eq!(indexed(palette, 232), Rgb::from_rgb(8, 8, 8));
        assert_eq!(indexed(palette, 255), Rgb::from_rgb(238, 238, 238));
        // And the palette is a palette, not sixteen colours repeated:
        // a modulo would have produced exactly sixteen distinct values.
        let distinct: std::collections::HashSet<_> =
            (0..=255u8).map(|i| indexed(palette, i)).collect();
        assert!(distinct.len() > 240, "only {} distinct", distinct.len());
        assert_ne!(
            indexed(palette, 17),
            indexed(palette, 1),
            "17 is not 1 wrapped"
        );
    }
}
