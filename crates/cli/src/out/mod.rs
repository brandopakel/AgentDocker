//! What a command prints for a person at a terminal.
//!
//! A command has two readers. A script, or an agent, reads what it has
//! always read: the same table with every column, the same JSON, the same
//! id alone on a line, byte for byte, whatever this module learns to do.
//! A person at a terminal reads a table that fits the terminal, a state
//! in its tone, and text that cannot move the cursor. Which reader it is
//! is decided once for a stream, here ([`resolve`]), from what the stream
//! is and what was asked:
//!
//! - **How**: for a person when stdout is a terminal or `--format human`
//!   is given; plainly, as before, when it is not or `--format plain` is.
//! - **Colour**, in what a person reads only: `--color always` or `never`
//!   decides; otherwise a non-empty `NO_COLOR` turns it off, then a
//!   `CLICOLOR_FORCE` that is set and not `0` turns it on, then
//!   `TERM=dumb` turns it off, and otherwise it is on at a terminal.
//!   Plain output is never coloured, whatever is asked.
//! - **Width**: the terminal's own; where there is none to ask (output a
//!   person forced to be human, in a pipe to a pager), `COLUMNS` when it
//!   says, and otherwise nothing is fitted to anything.
//!
//! Each stream is decided for itself: stdout in a pipe with stderr at a
//! terminal is not stdout at a terminal with stderr in a file.
//!
//! What is not text for a reader at all (a file's bytes, a terminal's own
//! bytes, a protocol's frames) does not come through here.

pub mod table;
pub mod text;
pub mod views;

use agentdocker_core::Tone;
use anstyle::{AnsiColor, Effects, Style};

/// How a result is printed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Format {
    /// For a person: fitted to the terminal, a state in its tone.
    Human,
    /// As a script has always read it: every column, nothing shortened,
    /// no colour.
    Plain,
}

/// When what a person reads is coloured.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum When {
    #[default]
    Auto,
    Always,
    Never,
}

/// The two flags of a command that prints through this module.
#[derive(clap::Args, Clone, Copy, Debug, Default)]
pub struct Look {
    /// How it is printed: `human` for a person (what a terminal gets),
    /// `plain` as a script has always read it (what a pipe gets).
    #[arg(long, value_enum, value_name = "HOW")]
    pub format: Option<Format>,
    /// Colour in what a person reads: `auto` (at a terminal, unless
    /// NO_COLOR is set or TERM is dumb), `always` or `never`. Plain
    /// output is never coloured.
    #[arg(long, value_enum, value_name = "WHEN")]
    pub color: Option<When>,
}

/// What the environment says of colour and width, read once.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Env {
    /// `NO_COLOR` is set and not empty.
    pub no_color: bool,
    /// `CLICOLOR_FORCE` is set, not empty and not `0`.
    pub force_color: bool,
    /// `TERM` is `dumb`.
    pub dumb: bool,
    /// `COLUMNS`, when it is a number.
    pub columns: Option<usize>,
}

impl Env {
    pub fn read() -> Self {
        let said = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
        Self {
            no_color: said("NO_COLOR").is_some(),
            force_color: said("CLICOLOR_FORCE").is_some_and(|value| value != "0"),
            dumb: said("TERM").is_some_and(|value| value == "dumb"),
            columns: std::env::var("COLUMNS")
                .ok()
                .and_then(|value| value.trim().parse().ok()),
        }
    }
}

/// The narrowest a terminal is taken to be: one that says it is narrower
/// (or says zero) is laid out for as this.
pub const NARROWEST: usize = 20;

/// What one stream is printed for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caps {
    /// For a person, through this module; otherwise plainly, as before.
    pub human: bool,
    pub color: bool,
    /// The columns there are, when that is known.
    pub width: Option<usize>,
}

impl Caps {
    /// A stream printed plainly: what a script reads.
    pub const PLAIN: Caps = Caps {
        human: false,
        color: false,
        width: None,
    };
}

/// What a stream is printed for: `terminal` is whether that stream is
/// one, and `window` its width when it is and says.
pub fn resolve(look: Look, terminal: bool, window: Option<usize>, env: &Env) -> Caps {
    let human = match look.format {
        Some(Format::Human) => true,
        Some(Format::Plain) => false,
        None => terminal,
    };
    if !human {
        return Caps::PLAIN;
    }
    let color = match look.color.unwrap_or_default() {
        When::Always => true,
        When::Never => false,
        When::Auto if env.no_color => false,
        When::Auto if env.force_color => true,
        When::Auto if env.dumb => false,
        When::Auto => terminal,
    };
    let width = window
        .filter(|_| terminal)
        .or(env.columns)
        .map(|columns| columns.max(NARROWEST));
    Caps {
        human,
        color,
        width,
    }
}

#[cfg(unix)]
fn window(fd: std::os::fd::RawFd) -> Option<usize> {
    agentdocker_host::pty::window_size(fd).map(|(columns, _)| usize::from(columns))
}

impl Caps {
    /// What stdout is printed for.
    pub fn stdout(look: Look) -> Caps {
        use std::io::IsTerminal;
        let out = std::io::stdout();
        #[cfg(unix)]
        let width = window(std::os::fd::AsRawFd::as_raw_fd(&out));
        #[cfg(not(unix))]
        let width = None;
        resolve(look, out.is_terminal(), width, &Env::read())
    }

    /// What stderr is printed for. Stderr is always for a person; what
    /// is decided is its colour and its width, from stderr itself.
    pub fn stderr(look: Look) -> Caps {
        use std::io::IsTerminal;
        let err = std::io::stderr();
        #[cfg(unix)]
        let width = window(std::os::fd::AsRawFd::as_raw_fd(&err));
        #[cfg(not(unix))]
        let width = None;
        let look = Look {
            format: Some(Format::Human),
            ..look
        };
        resolve(look, err.is_terminal(), width, &Env::read())
    }
}

/// A table's heading, and a view's.
pub const HEADING: Style = Style::new().effects(Effects::BOLD);
/// What is set back: a note, a row listed beside the rest.
pub const FAINT: Style = Style::new().effects(Effects::DIMMED);

/// A tone's colour at a terminal: the sixteen every terminal has, so a
/// person's own palette decides what "green" is. The word beside it
/// always says the state; the colour only adds weight.
pub fn style_of(tone: Tone) -> Style {
    match tone {
        Tone::Neutral => Style::new(),
        Tone::Positive => AnsiColor::Green.on_default(),
        Tone::Attention => AnsiColor::Yellow.on_default(),
        Tone::Alert => AnsiColor::Red.on_default().effects(Effects::BOLD),
        Tone::Info => AnsiColor::Cyan.on_default(),
    }
}

/// `text` in `style` where `caps` has colour, and as it is where it has
/// none or the style is none. The text is the caller's to have made safe.
pub fn paint(text: &str, style: Style, caps: Caps) -> String {
    if !caps.color || style == Style::new() || text.is_empty() {
        return text.to_owned();
    }
    format!("{style}{text}{style:#}")
}

/// Write what a person reads to stdout. A reader that went away (a pager
/// closed) ends the writing quietly.
pub fn print(text: &str) {
    use std::io::Write;
    let mut out = anstream::AutoStream::always(std::io::stdout().lock());
    let _ = out.write_all(text.as_bytes());
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn look(format: Option<Format>, color: Option<When>) -> Look {
        Look { format, color }
    }

    /// How a result is printed: for a person at a terminal and plainly in
    /// a pipe, unless one or the other is asked for.
    #[test]
    fn a_terminal_reads_for_a_person_and_a_pipe_plainly() {
        let env = Env::default();
        assert!(resolve(Look::default(), true, Some(120), &env).human);
        assert_eq!(resolve(Look::default(), false, None, &env), Caps::PLAIN);
        assert!(resolve(look(Some(Format::Human), None), false, None, &env).human);
        assert_eq!(
            resolve(look(Some(Format::Plain), None), true, Some(120), &env),
            Caps::PLAIN
        );
    }

    /// Plain output is never coloured, fitted or shortened, whatever is
    /// asked and whatever the environment says.
    #[test]
    fn plain_output_is_never_decorated() {
        let loud = Env {
            force_color: true,
            columns: Some(40),
            ..Env::default()
        };
        for terminal in [true, false] {
            assert_eq!(
                resolve(
                    look(Some(Format::Plain), Some(When::Always)),
                    terminal,
                    Some(40),
                    &loud
                ),
                Caps::PLAIN
            );
        }
        assert_eq!(
            resolve(look(None, Some(When::Always)), false, None, &loud),
            Caps::PLAIN,
            "a pipe nobody asked to be human"
        );
    }

    /// Colour's one order: the flag, then NO_COLOR, then CLICOLOR_FORCE,
    /// then a dumb terminal, then whether the stream is a terminal.
    #[test]
    fn colour_has_one_precedence() {
        let human = |color, terminal, env: Env| {
            resolve(look(Some(Format::Human), color), terminal, Some(100), &env).color
        };
        let none = Env::default();
        let no_color = Env {
            no_color: true,
            ..none
        };
        let forced = Env {
            force_color: true,
            ..none
        };
        let dumb = Env { dumb: true, ..none };
        let everything = Env {
            no_color: true,
            force_color: true,
            dumb: true,
            columns: None,
        };

        // Nothing said: a terminal has colour, a pipe has not.
        assert!(human(None, true, none));
        assert!(!human(None, false, none));
        // The flag is over everything.
        assert!(human(Some(When::Always), false, everything));
        assert!(!human(Some(When::Never), true, forced));
        // NO_COLOR is over CLICOLOR_FORCE.
        assert!(!human(None, true, no_color));
        assert!(!human(None, true, everything));
        assert!(!human(Some(When::Auto), true, everything));
        // CLICOLOR_FORCE colours a pipe, and is over a dumb terminal.
        assert!(human(None, false, forced));
        assert!(human(
            None,
            true,
            Env {
                force_color: true,
                dumb: true,
                ..none
            }
        ));
        // A dumb terminal has none.
        assert!(!human(None, true, dumb));
    }

    /// Each stream is decided for itself, from what that stream is.
    #[test]
    fn each_stream_is_decided_for_itself() {
        let env = Env::default();
        // stdout in a pipe, stderr at a terminal.
        let stdout = resolve(Look::default(), false, None, &env);
        let stderr = resolve(look(Some(Format::Human), None), true, Some(90), &env);
        assert_eq!(stdout, Caps::PLAIN);
        assert_eq!(
            stderr,
            Caps {
                human: true,
                color: true,
                width: Some(90)
            }
        );
        // stdout at a terminal, stderr in a file.
        let stdout = resolve(Look::default(), true, Some(90), &env);
        let stderr = resolve(look(Some(Format::Human), None), false, None, &env);
        assert!(stdout.color && stdout.width == Some(90));
        assert_eq!(
            stderr,
            Caps {
                human: true,
                color: false,
                width: None
            }
        );
    }

    /// The width is the terminal's own; COLUMNS only where there is no
    /// terminal to ask; a width too small to lay anything out in, zero
    /// included, is taken as the narrowest.
    #[test]
    fn width_is_the_terminals_and_never_tiny() {
        let said = Env {
            columns: Some(70),
            ..Env::default()
        };
        let human = look(Some(Format::Human), None);
        assert_eq!(resolve(human, true, Some(132), &said).width, Some(132));
        assert_eq!(resolve(human, false, None, &said).width, Some(70));
        // A width that came with no terminal is nobody's.
        assert_eq!(resolve(human, false, Some(132), &said).width, Some(70));
        assert_eq!(resolve(human, false, None, &Env::default()).width, None);
        assert_eq!(resolve(human, true, None, &Env::default()).width, None);
        for tiny in [0, 1, 19] {
            assert_eq!(
                resolve(human, true, Some(tiny), &said).width,
                Some(NARROWEST)
            );
        }
        let zero = Env {
            columns: Some(0),
            ..Env::default()
        };
        assert_eq!(resolve(human, false, None, &zero).width, Some(NARROWEST));
    }

    /// A tone's style: nothing for what is quiet, and no escape at all
    /// where there is no colour, no style or no text.
    #[test]
    fn paint_writes_escapes_only_where_there_is_colour() {
        let on = Caps {
            human: true,
            color: true,
            width: None,
        };
        let off = Caps { color: false, ..on };
        assert_eq!(
            paint("running", style_of(Tone::Positive), on),
            "\u{1b}[32mrunning\u{1b}[0m"
        );
        assert_eq!(paint("running", style_of(Tone::Positive), off), "running");
        assert_eq!(paint("idle", style_of(Tone::Neutral), on), "idle");
        assert_eq!(paint("", style_of(Tone::Alert), on), "");
        // Every tone that says something has a style of its own.
        let tones = [Tone::Positive, Tone::Attention, Tone::Alert, Tone::Info];
        for (at, tone) in tones.iter().enumerate() {
            assert_ne!(style_of(*tone), Style::new(), "{tone:?}");
            for other in &tones[at + 1..] {
                assert_ne!(style_of(*tone), style_of(*other), "{tone:?} and {other:?}");
            }
        }
        // What is not known is quiet: it is never given the colour of
        // good news.
        assert_eq!(style_of(Tone::default()), Style::new());
    }
}
