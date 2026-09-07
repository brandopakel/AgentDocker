//! The app's terminal: an agent's pty, drawn in the window.
//!
//! Same `attach` protocol the CLI speaks. A reader thread feeds the bytes
//! to a vt parser, which keeps a screen the UI thread draws; keystrokes go
//! back the other way. Detaching is closing the connection, so the agent
//! neither notices nor stops.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use agentdocker_core::{Request, Response, protocol};
use egui::text::LayoutJob;
use egui::{Color32, FontId, TextFormat};

use crate::client::Client;

/// Cells wide and tall a terminal starts at, until the view says otherwise.
const DEFAULT_SIZE: (u16, u16) = (80, 24);

/// Rows of history the parser keeps above the screen. The daemon replays
/// the last 64 KB on attach, which is worth more than the twenty-four
/// lines that fit.
const SCROLLBACK: usize = 2_000;

/// What the window sends the agent. Keystrokes are raw bytes and a resize
/// is a protocol frame; they travel together and must not be told apart
/// by looking at them — a typed `{` is not a JSON request.
enum Outbound {
    Keys(Vec<u8>),
    Frame(String),
}

/// What the window and the reader thread both hold: the screen one
/// writes and the other draws, whether the session is still up, and the
/// socket handle that lets a detach wake the reader.
#[derive(Clone)]
struct Shared {
    parser: Arc<Mutex<vt100::Parser>>,
    status: Arc<Mutex<Status>>,
    connection: Arc<Mutex<Option<agentdocker_host::ipc::BlockingStream>>>,
}

/// One attached agent.
pub struct Terminal {
    pub agent: String,
    shared: Shared,
    input: Sender<Outbound>,
    size: (u16, u16),
    /// How far above the live screen the view is scrolled.
    scrollback: usize,
}

impl Drop for Terminal {
    fn drop(&mut self) {
        // Shutting the socket down makes the reader's blocking `read_line`
        // return `Ok(0)`, which ends its loop; the writer thread ends when
        // this `Sender` drops.
        if let Some(stream) = lock(&self.shared.connection).take() {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
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
            connection: Arc::new(Mutex::new(None)),
        };
        let (input, outbound) = channel::<Outbound>();
        spawn_session(
            client,
            agent.clone(),
            (cols, rows),
            shared.clone(),
            outbound,
            ctx,
        );
        Self {
            agent,
            shared,
            input,
            size: (cols, rows),
            scrollback: 0,
        }
    }

    pub fn status(&self) -> Status {
        lock(&self.shared.status).clone()
    }

    /// Send bytes to the agent; a closed session simply drops them.
    pub fn send(&self, bytes: Vec<u8>) {
        let _ = self.input.send(Outbound::Keys(bytes));
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
        self.size = (cols, rows);
        lock(&self.shared.parser).screen_mut().set_size(rows, cols);
        if let Ok(frame) = serde_json::to_string(&Request::AttachResize { cols, rows }) {
            let _ = self.input.send(Outbound::Frame(frame));
        }
    }

    /// Draw the screen as it stands.
    pub fn ui(&self, ui: &mut egui::Ui) {
        let parser = lock(&self.shared.parser);
        let screen = parser.screen();
        let (rows, cols) = screen.size();
        let font = FontId::monospace(13.0);
        for row in 0..rows {
            let mut job = LayoutJob::default();
            let mut text = String::new();
            let mut style: Option<(Color32, bool)> = None;
            for col in 0..cols {
                let Some(cell) = screen.cell(row, col) else {
                    continue;
                };
                let colour = foreground(cell.fgcolor());
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

/// A terminal colour as something to draw with. The window has its own
/// theme, so the default is whatever "normal text" is rather than a
/// hard-coded white that would vanish on a light background.
fn foreground(colour: vt100::Color) -> Color32 {
    match colour {
        vt100::Color::Default => Color32::GRAY,
        vt100::Color::Idx(i) => indexed(i),
        vt100::Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
    }
}

/// An xterm palette index. 0–15 are the named colours; 16–231 are a
/// 6×6×6 cube; 232–255 are a greyscale ramp. Folding the last two ranges
/// into the first sixteen — which is what a modulo does — gives an agent
/// using the 256-colour palette a set of unrelated hues.
fn indexed(i: u8) -> Color32 {
    match i {
        0..=15 => ANSI[i as usize],
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

/// The sixteen the escape codes name.
const ANSI: [Color32; 16] = [
    Color32::from_rgb(0x3b, 0x3b, 0x3b),
    Color32::from_rgb(0xcc, 0x55, 0x55),
    Color32::from_rgb(0x55, 0xaa, 0x55),
    Color32::from_rgb(0xbb, 0x99, 0x33),
    Color32::from_rgb(0x55, 0x88, 0xcc),
    Color32::from_rgb(0xaa, 0x66, 0xcc),
    Color32::from_rgb(0x44, 0xaa, 0xaa),
    Color32::from_rgb(0xbb, 0xbb, 0xbb),
    Color32::from_rgb(0x66, 0x66, 0x66),
    Color32::from_rgb(0xff, 0x77, 0x77),
    Color32::from_rgb(0x77, 0xdd, 0x77),
    Color32::from_rgb(0xee, 0xcc, 0x55),
    Color32::from_rgb(0x77, 0xaa, 0xff),
    Color32::from_rgb(0xcc, 0x88, 0xff),
    Color32::from_rgb(0x66, 0xdd, 0xdd),
    Color32::from_rgb(0xff, 0xff, 0xff),
];

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
    outbound: Receiver<Outbound>,
    ctx: egui::Context,
) {
    std::thread::spawn(move || {
        let Shared {
            parser,
            status,
            connection,
        } = shared;
        let ended = |reason: String| {
            *lock(&status) = Status::Ended(reason);
            ctx.request_repaint();
        };
        let stream = match client.open(&Request::Attach {
            agent: agent.clone(),
            cols: Some(size.0),
            rows: Some(size.1),
        }) {
            Ok(stream) => stream,
            Err(err) => return ended(err.to_string()),
        };
        let mut writer = match stream.try_clone() {
            Ok(writer) => writer,
            Err(err) => return ended(err.to_string()),
        };
        // Kept where `Terminal::drop` can reach it, so detaching wakes the
        // blocking read below instead of leaving this thread behind.
        match stream.try_clone() {
            Ok(handle) => *lock(&connection) = Some(handle),
            Err(err) => return ended(err.to_string()),
        }
        // Typing runs on its own thread; the reader owns this one.
        std::thread::spawn(move || {
            use std::io::Write;
            while let Ok(message) = outbound.recv() {
                let frame = match message {
                    Outbound::Frame(frame) => frame,
                    Outbound::Keys(bytes) => {
                        match serde_json::to_string(&Request::AttachInput {
                            data: protocol::encode_bytes(&bytes),
                        }) {
                            Ok(frame) => frame,
                            Err(_) => continue,
                        }
                    }
                };
                if writer.write_all(format!("{frame}\n").as_bytes()).is_err()
                    || writer.flush().is_err()
                {
                    return;
                }
            }
        });

        use std::io::BufRead;
        let mut reader = std::io::BufReader::new(stream);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => return ended("the connection closed".to_owned()),
                Ok(_) => {}
                Err(err) => return ended(err.to_string()),
            }
            match serde_json::from_str::<Response>(&line) {
                Ok(Response::Output { data }) => {
                    if let Some(bytes) = protocol::decode_bytes(&data) {
                        lock(&parser).process(&bytes);
                        ctx.request_repaint();
                    }
                }
                Ok(Response::End) => return ended("the agent ended".to_owned()),
                Ok(Response::Error { message, .. }) => return ended(message),
                Ok(_) => {}
                Err(err) => return ended(err.to_string()),
            }
        }
    });
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
        assert_eq!(foreground(red.fgcolor()), ANSI[1], "index 1 is red");
        let plain = screen.cell(0, 4).unwrap();
        assert_eq!(
            foreground(plain.fgcolor()),
            Color32::GRAY,
            "default follows the window's own text colour"
        );
        assert_eq!(
            foreground(vt100::Color::Rgb(1, 2, 3)),
            Color32::from_rgb(1, 2, 3)
        );
    }

    #[test]
    fn the_256_colour_palette_is_not_folded_into_sixteen() {
        // The named sixteen are the table itself.
        assert_eq!(indexed(0), ANSI[0]);
        assert_eq!(indexed(15), ANSI[15]);
        // The cube: 16 is its black corner, 231 its white one, and the
        // levels step 0, 95, 135, 175, 215, 255.
        assert_eq!(indexed(16), Color32::from_rgb(0, 0, 0));
        assert_eq!(indexed(231), Color32::from_rgb(255, 255, 255));
        assert_eq!(indexed(196), Color32::from_rgb(255, 0, 0), "cube red");
        assert_eq!(indexed(46), Color32::from_rgb(0, 255, 0), "cube green");
        assert_eq!(indexed(21), Color32::from_rgb(0, 0, 255), "cube blue");
        // The greyscale ramp, which a modulo would have scattered.
        assert_eq!(indexed(232), Color32::from_rgb(8, 8, 8));
        assert_eq!(indexed(255), Color32::from_rgb(238, 238, 238));
        // And the palette is a palette, not sixteen colours repeated:
        // a modulo would have produced exactly sixteen distinct values.
        let distinct: std::collections::HashSet<_> = (0..=255u8).map(indexed).collect();
        assert!(distinct.len() > 240, "only {} distinct", distinct.len());
        assert_ne!(indexed(17), indexed(1), "17 is not 1 wrapped");
    }
}
