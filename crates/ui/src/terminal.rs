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

/// One attached agent.
pub struct Terminal {
    pub agent: String,
    parser: Arc<Mutex<vt100::Parser>>,
    input: Sender<Vec<u8>>,
    status: Arc<Mutex<Status>>,
    size: (u16, u16),
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
        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 0)));
        let status = Arc::new(Mutex::new(Status::Attached));
        let (input, keystrokes) = channel::<Vec<u8>>();
        spawn_session(
            client,
            agent.clone(),
            (cols, rows),
            parser.clone(),
            status.clone(),
            keystrokes,
            ctx,
        );
        Self {
            agent,
            parser,
            input,
            status,
            size: (cols, rows),
        }
    }

    pub fn status(&self) -> Status {
        lock(&self.status).clone()
    }

    /// Send bytes to the agent; a closed session simply drops them.
    pub fn send(&self, bytes: Vec<u8>) {
        let _ = self.input.send(bytes);
    }

    /// Tell the agent its window changed. Cheap to call every frame: it
    /// only acts when the size actually differs.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let (cols, rows) = (cols.max(20), rows.max(4));
        if (cols, rows) == self.size {
            return;
        }
        self.size = (cols, rows);
        lock(&self.parser).screen_mut().set_size(rows, cols);
        if let Ok(frame) = serde_json::to_string(&Request::AttachResize { cols, rows }) {
            self.send(format!("{frame}\n").into_bytes());
        }
    }

    /// Draw the screen as it stands.
    pub fn ui(&self, ui: &mut egui::Ui) {
        let parser = lock(&self.parser);
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
        vt100::Color::Idx(i) => ANSI[(i as usize) % ANSI.len()],
        vt100::Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
    }
}

/// The sixteen the escape codes name; anything higher wraps into them,
/// which is close enough to read by.
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
    parser: Arc<Mutex<vt100::Parser>>,
    status: Arc<Mutex<Status>>,
    keystrokes: Receiver<Vec<u8>>,
    ctx: egui::Context,
) {
    std::thread::spawn(move || {
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
        // Typing runs on its own thread; the reader owns this one.
        std::thread::spawn(move || {
            use std::io::Write;
            while let Ok(bytes) = keystrokes.recv() {
                // A resize arrives already framed; anything else is input.
                let frame = if bytes.starts_with(b"{") {
                    bytes
                } else {
                    match serde_json::to_string(&Request::AttachInput {
                        data: protocol::encode_bytes(&bytes),
                    }) {
                        Ok(frame) => format!("{frame}\n").into_bytes(),
                        Err(_) => continue,
                    }
                };
                if writer.write_all(&frame).is_err() || writer.flush().is_err() {
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
                if modifiers.ctrl || modifiers.mac_cmd {
                    // Ctrl-A is 1, Ctrl-Z is 26, and the run continues
                    // through the punctuation the escape codes use.
                    if let Some(letter) =
                        key.name().chars().next().filter(char::is_ascii_alphabetic)
                    {
                        bytes.push(letter.to_ascii_uppercase() as u8 - b'A' + 1);
                        continue;
                    }
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
}
