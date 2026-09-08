//! The app's terminal: an agent's pty, drawn in the window.
//!
//! Same `attach` protocol the CLI speaks. A reader thread feeds the bytes
//! to a vt parser, which keeps a screen the UI thread draws; keystrokes go
//! back the other way. Detaching is closing the connection, so the agent
//! neither notices nor stops.

use std::sync::{Arc, Mutex};

use agentdocker_core::{Request, Response, protocol};
use egui::text::LayoutJob;
use egui::{Color32, FontId, TextFormat};

use crate::client::Client;
use crate::theme::Palette;

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

    fn ended(&self, reason: String, ctx: &egui::Context) {
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
    input_notice: Option<&'static str>,
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
    pub fn attach(client: Arc<Client>, agent: String, ctx: egui::Context) -> Self {
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

    /// Draw the screen as it stands.
    pub fn ui(&mut self, ui: &mut egui::Ui, palette: &Palette, size: f32) {
        if let Some(reason) = self.input_notice {
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(Color32::from_rgb(200, 80, 60), reason);
                if ui.button("Dismiss").clicked() {
                    self.input_notice = None;
                }
            });
        }
        let parser = lock(&self.shared.parser);
        let screen = parser.screen();
        let (rows, cols) = screen.size();
        let font = FontId::monospace(size);
        for row in 0..rows {
            let mut job = LayoutJob::default();
            let mut text = String::new();
            let mut style: Option<(Color32, bool)> = None;
            for col in 0..cols {
                let Some(cell) = screen.cell(row, col) else {
                    continue;
                };
                let colour = foreground(palette, cell.fgcolor());
                let bold = cell.bold();
                if style != Some((colour, bold)) && !text.is_empty() {
                    push(&mut job, &text, style, &font);
                    text.clear();
                }
                style = Some((colour, bold));
                // An unwritten cell is a space, not nothing, or the row
                // would shift left as the screen fills.
                let contents = cell.contents();
                if contents.is_empty() {
                    text.push(' ');
                } else {
                    text.push_str(contents);
                }
            }
            // A blank row is drawn as a space rather than skipped, so the
            // screen keeps its shape as content comes and goes.
            let text = text.trim_end();
            push(
                &mut job,
                if text.is_empty() { " " } else { text },
                style,
                &font,
            );
            ui.label(job);
        }
    }
}

fn push(job: &mut LayoutJob, text: &str, style: Option<(Color32, bool)>, font: &FontId) {
    if text.is_empty() {
        return;
    }
    let (colour, _bold) = style.unwrap_or((Color32::GRAY, false));
    job.append(
        text,
        0.0,
        TextFormat {
            font_id: font.clone(),
            color: colour,
            ..Default::default()
        },
    );
}

/// A terminal colour as something to draw with, in the chosen palette.
///
/// `Default` is the palette's own text colour rather than a hard-coded
/// grey: on a light palette a hard-coded one either vanishes or shouts.
fn foreground(palette: &Palette, colour: vt100::Color) -> Color32 {
    match colour {
        vt100::Color::Default => palette.text,
        vt100::Color::Idx(i) => indexed(palette, i),
        vt100::Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
    }
}

/// An xterm palette index. 0–15 are the named colours; 16–231 are a
/// 6×6×6 cube; 232–255 are a greyscale ramp. Folding the last two ranges
/// into the first sixteen — which is what a modulo does — gives an agent
/// using the 256-colour palette a set of unrelated hues.
fn indexed(palette: &Palette, i: u8) -> Color32 {
    match i {
        0..=15 => palette.ansi[i as usize],
        16..=231 => {
            // The cube's six levels are not evenly spaced: the first step
            // is to 95, and the rest are 40 apart.
            let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            let c = i - 16;
            Color32::from_rgb(level(c / 36), level((c / 6) % 6), level(c % 6))
        }
        232..=255 => {
            let grey = 8 + (i - 232) * 10;
            Color32::from_rgb(grey, grey, grey)
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
fn spawn_session(
    client: Arc<Client>,
    agent: String,
    size: (u16, u16),
    shared: Shared,
    ctx: egui::Context,
) {
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
    ctx: egui::Context,
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
                    if let Err(error) = write_input(writer, &writer_state.input) {
                        writer_state.ended(format!("terminal input failed: {error}"), &writer_ctx);
                    }
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

fn read_frame(reader: &mut impl std::io::BufRead, line: &mut String) -> std::io::Result<usize> {
    use std::io::{BufRead, Read};
    line.clear();
    let read = reader.take((MAX_FRAME_BYTES + 1) as u64).read_line(line)?;
    if read > MAX_FRAME_BYTES {
        return Err(std::io::Error::other(
            "terminal output frame exceeds 256 KiB",
        ));
    }
    if read > 0 && !line.ends_with('\n') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "incomplete terminal output frame",
        ));
    }
    Ok(read)
}

fn read_output(
    stream: agentdocker_host::ipc::BlockingStream,
    shared: &Shared,
    ctx: &egui::Context,
) -> String {
    let mut reader = std::io::BufReader::new(stream);
    let mut line = String::new();
    let mut control = control::ControlBudget::default();
    loop {
        match read_frame(&mut reader, &mut line) {
            Ok(0) => return "the connection closed".to_owned(),
            Ok(_) => {}
            Err(error) => return error.to_string(),
        }
        match serde_json::from_str::<Response>(&line) {
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
fn control(key: egui::Key) -> Option<u8> {
    use egui::Key::*;
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
pub fn keystrokes(events: &[egui::Event]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for event in events {
        match event {
            egui::Event::Text(text) => bytes.extend_from_slice(text.as_bytes()),
            egui::Event::Key {
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
                    egui::Key::Enter => bytes.push(b'\r'),
                    egui::Key::Backspace => bytes.push(0x7f),
                    egui::Key::Tab => bytes.push(b'\t'),
                    egui::Key::Escape => bytes.push(0x1b),
                    egui::Key::Delete => bytes.extend_from_slice(b"\x1b[3~"),
                    egui::Key::Home => bytes.extend_from_slice(b"\x1b[H"),
                    egui::Key::End => bytes.extend_from_slice(b"\x1b[F"),
                    egui::Key::PageUp => bytes.extend_from_slice(b"\x1b[5~"),
                    egui::Key::PageDown => bytes.extend_from_slice(b"\x1b[6~"),
                    egui::Key::ArrowUp => bytes.extend_from_slice(b"\x1b[A"),
                    egui::Key::ArrowDown => bytes.extend_from_slice(b"\x1b[B"),
                    egui::Key::ArrowRight => bytes.extend_from_slice(b"\x1b[C"),
                    egui::Key::ArrowLeft => bytes.extend_from_slice(b"\x1b[D"),
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
        let (ready, started) = std::sync::mpsc::sync_channel(0);
        let (resume, blocked) = std::sync::mpsc::sync_channel(0);
        let session = spawn_connected(
            move || {
                ready.send(()).unwrap();
                blocked.recv_timeout(Duration::from_secs(3)).unwrap();
                Ok(stream)
            },
            shared,
            egui::Context::default(),
        )
        .unwrap();
        started.recv_timeout(Duration::from_secs(3)).unwrap();
        drop(terminal);
        resume.send(()).unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let observed = peer.read(&mut [0]);
        // Clean up the original failing behavior before asserting its result.
        peer.shutdown(std::net::Shutdown::Both).unwrap();
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
        let mut line = String::new();
        assert_eq!(read_frame(&mut reader, &mut line).unwrap(), replay.len());
        assert_eq!(line, replay);
        read_frame(&mut reader, &mut line).unwrap();
        assert_eq!(
            serde_json::from_str::<Response>(&line).unwrap(),
            Response::End
        );
        let mut huge = BufReader::new(Cursor::new(vec![b'x'; MAX_FRAME_BYTES * 2]));
        assert!(
            read_frame(&mut huge, &mut line)
                .unwrap_err()
                .to_string()
                .contains("256 KiB")
        );
        assert_eq!(line.len(), MAX_FRAME_BYTES + 1);
        let mut incomplete = Cursor::new(b"{\"type\":\"end\"}");
        assert_eq!(
            read_frame(&mut incomplete, &mut line).unwrap_err().kind(),
            std::io::ErrorKind::UnexpectedEof
        );
    }

    #[cfg(unix)]
    #[test]
    fn terminal_end_joins_an_idle_writer_without_dropping_the_window_handle() {
        use std::io::Write;
        use std::sync::mpsc;
        use std::time::Duration;
        let terminal = unserved_terminal();
        let shared = terminal.shared.clone();
        let (stream, mut peer) = agentdocker_host::ipc::BlockingStream::pair().unwrap();
        let session =
            spawn_connected(move || Ok(stream), shared, egui::Context::default()).unwrap();
        peer.write_all(b"{\"type\":\"end\"}\n").unwrap();
        let (finished, done) = mpsc::channel();
        let waiter = std::thread::spawn(move || finished.send(session.join()).unwrap());
        let observed = done.recv_timeout(Duration::from_secs(3));
        // A failing implementation must still release its fixture threads.
        terminal.shared.close();
        peer.shutdown(std::net::Shutdown::Both).unwrap();
        waiter.join().unwrap();
        assert!(observed.unwrap().is_ok());
        assert_eq!(terminal.status(), Status::Ended("the agent ended".into()));
    }

    #[cfg(unix)]
    #[test]
    fn writer_failure_closes_the_reader_and_preserves_its_reason() {
        use std::sync::mpsc;
        use std::time::Duration;
        let mut terminal = unserved_terminal();
        let shared = terminal.shared.clone();
        let (stream, peer) = agentdocker_host::ipc::BlockingStream::pair().unwrap();
        // Keep the peer's write side open: a reader cannot end by itself.
        peer.shutdown(std::net::Shutdown::Read).unwrap();
        let session =
            spawn_connected(move || Ok(stream), shared, egui::Context::default()).unwrap();
        terminal.send(b"owned fixture".to_vec());
        let (finished, done) = mpsc::channel();
        let waiter = std::thread::spawn(move || finished.send(session.join()).unwrap());
        let observed = done.recv_timeout(Duration::from_secs(3));
        terminal.shared.close();
        peer.shutdown(std::net::Shutdown::Both).unwrap();
        waiter.join().unwrap();
        assert!(observed.unwrap().is_ok());
        assert!(
            matches!(terminal.status(), Status::Ended(reason) if reason.starts_with("terminal input failed:"))
        );
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

    fn key(key: egui::Key, ctrl: bool) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers {
                ctrl,
                ..Default::default()
            },
        }
    }

    #[test]
    fn keys_become_what_a_terminal_expects() {
        assert_eq!(keystrokes(&[egui::Event::Text("hi".into())]), b"hi");
        assert_eq!(keystrokes(&[key(egui::Key::Enter, false)]), b"\r");
        assert_eq!(keystrokes(&[key(egui::Key::Backspace, false)]), &[0x7f]);
        assert_eq!(keystrokes(&[key(egui::Key::Escape, false)]), &[0x1b]);
        assert_eq!(keystrokes(&[key(egui::Key::ArrowUp, false)]), b"\x1b[A");
        assert_eq!(keystrokes(&[key(egui::Key::ArrowLeft, false)]), b"\x1b[D");
        assert_eq!(keystrokes(&[key(egui::Key::PageUp, false)]), b"\x1b[5~");
        // The control codes an agent's own key bindings depend on.
        assert_eq!(keystrokes(&[key(egui::Key::C, true)]), &[3], "Ctrl-C");
        assert_eq!(keystrokes(&[key(egui::Key::D, true)]), &[4], "Ctrl-D");
        assert_eq!(keystrokes(&[key(egui::Key::A, true)]), &[1]);
        assert_eq!(keystrokes(&[key(egui::Key::Z, true)]), &[26]);
        // Ctrl-] is what detaches everywhere else, so it must be 0x1d
        // rather than whatever the variant's *name* begins with.
        assert_eq!(
            keystrokes(&[key(egui::Key::CloseBracket, true)]),
            &[0x1d],
            "Ctrl-]"
        );
        assert_eq!(keystrokes(&[key(egui::Key::OpenBracket, true)]), &[0x1b]);
        assert_eq!(keystrokes(&[key(egui::Key::Space, true)]), &[0x00]);
        // A held Ctrl must not turn a named key into a letter code:
        // `ArrowUp` is not Ctrl-A and `Enter` is not Ctrl-E.
        assert_eq!(keystrokes(&[key(egui::Key::ArrowUp, true)]), b"\x1b[A");
        assert_eq!(keystrokes(&[key(egui::Key::Enter, true)]), b"\r");
        assert_eq!(keystrokes(&[key(egui::Key::Tab, true)]), b"\t");
        // A release is not a keystroke, and unknown keys are ignored.
        assert!(
            keystrokes(&[egui::Event::Key {
                key: egui::Key::A,
                physical_key: None,
                pressed: false,
                repeat: false,
                modifiers: egui::Modifiers::default(),
            }])
            .is_empty()
        );
        // Several events in one frame arrive in order.
        assert_eq!(
            keystrokes(&[egui::Event::Text("ls".into()), key(egui::Key::Enter, false)]),
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
            Color32::from_rgb(1, 2, 3)
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
        assert_eq!(indexed(palette, 16), Color32::from_rgb(0, 0, 0));
        assert_eq!(indexed(palette, 231), Color32::from_rgb(255, 255, 255));
        assert_eq!(
            indexed(palette, 196),
            Color32::from_rgb(255, 0, 0),
            "cube red"
        );
        assert_eq!(
            indexed(palette, 46),
            Color32::from_rgb(0, 255, 0),
            "cube green"
        );
        assert_eq!(
            indexed(palette, 21),
            Color32::from_rgb(0, 0, 255),
            "cube blue"
        );
        // The greyscale ramp, which a modulo would have scattered.
        assert_eq!(indexed(palette, 232), Color32::from_rgb(8, 8, 8));
        assert_eq!(indexed(palette, 255), Color32::from_rgb(238, 238, 238));
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
