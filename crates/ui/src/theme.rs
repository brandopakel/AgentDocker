//! What the window looks like, and what the person at it can change.
//!
//! The console and the agent terminal are the two surfaces that have to
//! look like a terminal, and they use the same palette so they read as
//! one product rather than two panes that happen to be adjacent.
//!
//! There is no attempt to read the palette out of whatever terminal
//! emulator is installed. Terminal.app keeps its profiles in a binary
//! plist of `NSKeyedArchiver` colour blobs, iTerm2, Ghostty, WezTerm and
//! Alacritty each keep theirs somewhere else in some other format, and
//! the app is usually started from the Dock, so there is no terminal to
//! inherit from in the first place. Guessing wrong here is worse than
//! not guessing: a palette that is nearly right looks broken. So the
//! well-known palettes are reproduced by name and the choice is the
//! reader's, which is what terminal clients that do this well already
//! do.

use eframe::egui::Color32;
use serde::{Deserialize, Serialize};

/// A terminal palette: the ground, the ink, and the sixteen the escape
/// codes name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub name: &'static str,
    pub ground: Color32,
    pub text: Color32,
    /// The prompt, and anything else the surface says in its own voice.
    pub accent: Color32,
    pub ansi: [Color32; 16],
}

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

/// The palettes on offer, in the order the picker lists them.
///
/// The first is ours. The rest are the profiles people recognise from
/// the terminals they already use, reproduced from their published
/// colours rather than read off the machine.
pub const PALETTES: &[Palette] = &[
    Palette {
        name: "AgentDocker",
        ground: rgb(0x14171D),
        text: rgb(0xD6DBE4),
        accent: rgb(0x77DD77),
        ansi: [
            rgb(0x3B3B3B),
            rgb(0xCC5555),
            rgb(0x55AA55),
            rgb(0xBB9933),
            rgb(0x5588CC),
            rgb(0xAA66CC),
            rgb(0x44AAAA),
            rgb(0xBBBBBB),
            rgb(0x666666),
            rgb(0xFF7777),
            rgb(0x77DD77),
            rgb(0xEECC55),
            rgb(0x77AAFF),
            rgb(0xCC88FF),
            rgb(0x66DDDD),
            rgb(0xFFFFFF),
        ],
    },
    Palette {
        name: "Basic",
        ground: rgb(0xFFFFFF),
        text: rgb(0x000000),
        accent: rgb(0x0057B7),
        ansi: [
            rgb(0x000000),
            rgb(0x990000),
            rgb(0x00A600),
            rgb(0x999900),
            rgb(0x0000B2),
            rgb(0xB200B2),
            rgb(0x00A6B2),
            rgb(0xBFBFBF),
            rgb(0x666666),
            rgb(0xE50000),
            rgb(0x00D900),
            rgb(0xE5E500),
            rgb(0x0000FF),
            rgb(0xE500E5),
            rgb(0x00E5E5),
            rgb(0xE5E5E5),
        ],
    },
    Palette {
        name: "Pro",
        ground: rgb(0x000000),
        text: rgb(0xF2F2F2),
        accent: rgb(0x00D900),
        ansi: [
            rgb(0x000000),
            rgb(0xC63127),
            rgb(0x31C531),
            rgb(0xC5C531),
            rgb(0x3131C6),
            rgb(0xC631C6),
            rgb(0x31C5C5),
            rgb(0xC5C5C5),
            rgb(0x686868),
            rgb(0xFF6E67),
            rgb(0x5FFA68),
            rgb(0xFFFC67),
            rgb(0x6871FF),
            rgb(0xFF77FF),
            rgb(0x60FCFF),
            rgb(0xFFFFFF),
        ],
    },
    Palette {
        name: "Homebrew",
        ground: rgb(0x000000),
        text: rgb(0x00FF00),
        accent: rgb(0x00FF00),
        ansi: [
            rgb(0x000000),
            rgb(0x990000),
            rgb(0x00A600),
            rgb(0x999900),
            rgb(0x0000B2),
            rgb(0xB200B2),
            rgb(0x00A6B2),
            rgb(0xBFBFBF),
            rgb(0x666666),
            rgb(0xE50000),
            rgb(0x00D900),
            rgb(0xE5E500),
            rgb(0x0000FF),
            rgb(0xE500E5),
            rgb(0x00E5E5),
            rgb(0xE5E5E5),
        ],
    },
    Palette {
        name: "Solarized Dark",
        ground: rgb(0x002B36),
        text: rgb(0x93A1A1),
        accent: rgb(0x2AA198),
        ansi: [
            rgb(0x073642),
            rgb(0xDC322F),
            rgb(0x859900),
            rgb(0xB58900),
            rgb(0x268BD2),
            rgb(0xD33682),
            rgb(0x2AA198),
            rgb(0xEEE8D5),
            rgb(0x002B36),
            rgb(0xCB4B16),
            rgb(0x586E75),
            rgb(0x657B83),
            rgb(0x839496),
            rgb(0x6C71C4),
            rgb(0x93A1A1),
            rgb(0xFDF6E3),
        ],
    },
    Palette {
        name: "Solarized Light",
        ground: rgb(0xFDF6E3),
        text: rgb(0x586E75),
        accent: rgb(0x2AA198),
        ansi: [
            rgb(0x073642),
            rgb(0xDC322F),
            rgb(0x859900),
            rgb(0xB58900),
            rgb(0x268BD2),
            rgb(0xD33682),
            rgb(0x2AA198),
            rgb(0xEEE8D5),
            rgb(0x002B36),
            rgb(0xCB4B16),
            rgb(0x586E75),
            rgb(0x657B83),
            rgb(0x839496),
            rgb(0x6C71C4),
            rgb(0x93A1A1),
            rgb(0xFDF6E3),
        ],
    },
    Palette {
        name: "Novel",
        ground: rgb(0xDFDBC3),
        text: rgb(0x3B2322),
        accent: rgb(0x7D4F00),
        ansi: [
            rgb(0x000000),
            rgb(0xCC0000),
            rgb(0x009600),
            rgb(0xD06B00),
            rgb(0x0000CC),
            rgb(0xCC00CC),
            rgb(0x0087CC),
            rgb(0xCCCCCC),
            rgb(0x808080),
            rgb(0xCC0000),
            rgb(0x009600),
            rgb(0xD06B00),
            rgb(0x0000CC),
            rgb(0xCC00CC),
            rgb(0x0087CC),
            rgb(0xFFFFFF),
        ],
    },
];

impl Palette {
    /// The palette of this name, or ours.
    pub fn named(name: &str) -> &'static Palette {
        PALETTES
            .iter()
            .find(|p| p.name == name)
            .unwrap_or(&PALETTES[0])
    }

    /// Whether this palette is a light one, which decides what the rest
    /// of the window does around it.
    pub fn is_light(&self) -> bool {
        let g = self.ground;
        // Rec. 601 luma, which is close enough for a yes-or-no.
        (0.299 * g.r() as f32 + 0.587 * g.g() as f32 + 0.114 * g.b() as f32) > 140.0
    }

    /// Ink for something that should be present but quiet.
    pub fn dim(&self) -> Color32 {
        let (t, g) = (self.text, self.ground);
        Color32::from_rgb(
            ((t.r() as u16 + g.r() as u16 * 2) / 3) as u8,
            ((t.g() as u16 + g.g() as u16 * 2) / 3) as u8,
            ((t.b() as u16 + g.b() as u16 * 2) / 3) as u8,
        )
    }
}

/// What the reader has chosen, and what is stored between runs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The name of a palette in `PALETTES`.
    pub palette: String,
    /// Point size for the console and the terminal.
    pub terminal_size: f32,
    /// Point size for everything else.
    pub text_size: f32,
    /// Roomier rows, for a window that is being watched rather than read.
    pub roomy: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            palette: PALETTES[0].name.to_owned(),
            terminal_size: 13.0,
            text_size: 14.0,
            roomy: false,
        }
    }
}

impl Settings {
    pub fn palette(&self) -> &'static Palette {
        Palette::named(&self.palette)
    }

    /// Sizes are clamped on the way in and on the way out: a stored file
    /// can say anything, and a window with 400-point text cannot be used
    /// to fix itself.
    pub fn clamped(mut self) -> Self {
        self.terminal_size = self.terminal_size.clamp(9.0, 24.0);
        self.text_size = self.text_size.clamp(10.0, 24.0);
        if Palette::named(&self.palette).name != self.palette {
            self.palette = PALETTES[0].name.to_owned();
        }
        self
    }

    fn path(home: &std::path::Path) -> std::path::PathBuf {
        home.join("ui.json")
    }

    /// Read what was chosen last time. Anything unreadable is the
    /// default: settings are a convenience, and failing to start over
    /// one would be absurd.
    pub fn load(home: &std::path::Path) -> Self {
        std::fs::read(Self::path(home))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Self>(&bytes).ok())
            .unwrap_or_default()
            .clamped()
    }

    pub fn save(&self, home: &std::path::Path) {
        if let Ok(bytes) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(Self::path(home), bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_palette_falls_back_rather_than_failing() {
        assert_eq!(Palette::named("no such thing").name, PALETTES[0].name);
        assert_eq!(Palette::named("Novel").name, "Novel");
    }

    #[test]
    fn light_and_dark_grounds_are_told_apart() {
        assert!(Palette::named("Basic").is_light());
        assert!(Palette::named("Novel").is_light());
        assert!(!Palette::named("AgentDocker").is_light());
        assert!(!Palette::named("Solarized Dark").is_light());
    }

    #[test]
    fn dim_ink_sits_between_the_ground_and_the_text() {
        for palette in PALETTES {
            let dim = palette.dim();
            for channel in [
                (dim.r(), palette.text.r(), palette.ground.r()),
                (dim.g(), palette.text.g(), palette.ground.g()),
                (dim.b(), palette.text.b(), palette.ground.b()),
            ] {
                let (d, t, g) = channel;
                let (low, high) = (t.min(g), t.max(g));
                assert!(
                    d >= low && d <= high,
                    "{} outside {low}..{high}",
                    palette.name
                );
            }
        }
    }

    #[test]
    fn stored_settings_survive_a_round_trip_and_nonsense_is_clamped() {
        let dir = tempfile::TempDir::new().unwrap();
        let chosen = Settings {
            palette: "Solarized Dark".into(),
            terminal_size: 16.0,
            text_size: 15.0,
            roomy: true,
        };
        chosen.save(dir.path());
        assert_eq!(Settings::load(dir.path()), chosen);

        // A file that says something impossible must not produce a
        // window nobody can read well enough to fix it.
        std::fs::write(
            dir.path().join("ui.json"),
            br#"{"palette":"nope","terminal_size":400,"text_size":0.1,"roomy":true}"#,
        )
        .unwrap();
        let recovered = Settings::load(dir.path());
        assert_eq!(recovered.palette, PALETTES[0].name);
        assert_eq!(recovered.terminal_size, 24.0);
        assert_eq!(recovered.text_size, 10.0);
        assert!(recovered.roomy);
    }

    #[test]
    fn a_missing_file_is_the_default_not_a_failure() {
        let dir = tempfile::TempDir::new().unwrap();
        assert_eq!(Settings::load(dir.path()), Settings::default());
    }
}
