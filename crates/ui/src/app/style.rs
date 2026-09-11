//! Shared visual roles; status is always accompanied by a text label.
//!
//! The palette comes from the mark: a deep navy ground, electric blue for
//! selection and primary actions, cyan as the secondary brand tone. Light
//! and dark keep the same roles so the hierarchy reads identically.
use iced::{Border, Color, Font, Theme, color, widget::container};

/// The interface face. Inter is bundled (Regular, Medium, SemiBold; SIL OFL),
/// so weights and glyph coverage are the same on every host. The system
/// sans on macOS has no Bold face for the default family and borrows
/// heavier glyphs from a monospace fallback, which is how this started.
pub const UI: Font = Font::with_name("Inter");

/// `UI` at a weight.
pub const fn weight(weight: iced::font::Weight) -> Font {
    Font { weight, ..UI }
}

#[derive(Clone, Copy)]
pub struct Colors {
    pub dark: bool,
    /// The window ground behind everything.
    pub ground: Color,
    /// The navigation rail.
    pub sidebar: Color,
    /// A raised surface: cards, list panels, inputs.
    pub card: Color,
    /// Hover and pressed states, and quiet secondary buttons.
    pub raised: Color,
    pub text: Color,
    pub muted: Color,
    /// Eyebrows and tertiary detail.
    pub faint: Color,
    pub line: Color,
    /// Primary actions and the selected mark.
    pub accent: Color,
    /// Tinted ground behind a selected control.
    pub accent_soft: Color,
    /// Ink on `accent_soft`.
    pub accent_ink: Color,
    pub cyan: Color,
    pub green: Color,
    pub amber: Color,
    pub red: Color,
}

impl Colors {
    pub fn new(dark: bool) -> Self {
        if dark {
            Self {
                dark,
                ground: color!(0x0f141c),
                sidebar: color!(0x0b1017),
                card: color!(0x161d28),
                raised: color!(0x1e2734),
                text: color!(0xe8edf5),
                muted: color!(0x98a4b8),
                faint: color!(0x8996aa),
                line: color!(0x263042),
                accent: color!(0x286be0),
                accent_soft: color!(0x1a2f52),
                accent_ink: color!(0xbdd3ff),
                cyan: color!(0x2bd4f0),
                green: color!(0x4fd18b),
                amber: color!(0xe8b95b),
                red: color!(0xf26d6d),
            }
        } else {
            Self {
                dark,
                ground: color!(0xf6f8fb),
                sidebar: color!(0xedf1f6),
                card: Color::WHITE,
                raised: color!(0xe6edf8),
                text: color!(0x111827),
                muted: color!(0x5a6678),
                faint: color!(0x5e6a7b),
                line: color!(0xdce3ec),
                accent: color!(0x1f6feb),
                accent_soft: color!(0xe4edff),
                accent_ink: color!(0x1749b3),
                cyan: color!(0x0e9bb8),
                green: color!(0x1f8a57),
                amber: color!(0xa3661c),
                red: color!(0xc43d3d),
            }
        }
    }

    /// The roles for whatever theme a style closure was handed.
    pub fn of(theme: &Theme) -> Self {
        Self::new(theme.palette().background.r < 0.5)
    }

    pub fn theme(self) -> Theme {
        Theme::custom(
            "agentdocker",
            iced::theme::Palette {
                background: self.ground,
                text: self.text,
                primary: self.accent,
                success: self.green,
                warning: self.amber,
                danger: self.red,
            },
        )
    }

    /// A flat surface with an optional hairline.
    pub fn surface(self, background: Color, bordered: bool) -> container::Style {
        container::Style {
            background: Some(background.into()),
            text_color: Some(self.text),
            border: Border {
                color: self.line,
                width: if bordered { 1.0 } else { 0.0 },
                radius: 12.0.into(),
            },
            ..Default::default()
        }
    }

    /// A card: raised surface and hairline. No blurred shadow: tiny-skia
    /// evaluates a shadow per pixel over the whole quad on every frame and
    /// allocates for it, which with a hundred session rows in light mode
    /// cost gigabytes and a third of a core (measured 2026-09-10). Depth
    /// on large surfaces comes from the tint and the hairline instead.
    pub fn card_style(self) -> container::Style {
        self.surface(self.card, true)
    }

    /// A card whose left-to-right hairline is tinted to ask for attention.
    pub fn attention_style(self, tint: Color) -> container::Style {
        container::Style {
            border: Border {
                color: alpha(tint, 0.55),
                width: 1.0,
                radius: 12.0.into(),
            },
            ..self.card_style()
        }
    }

    /// A small rounded label.
    pub fn pill(self, background: Color, ink: Color) -> container::Style {
        container::Style {
            background: Some(background.into()),
            text_color: Some(ink),
            border: Border {
                radius: 999.0.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// A one-pixel rule.
    pub fn rule(self) -> container::Style {
        container::Style {
            background: Some(self.line.into()),
            ..Default::default()
        }
    }

    /// A filled circle.
    pub fn dot(self, fill: Color) -> container::Style {
        container::Style {
            background: Some(fill.into()),
            border: Border {
                radius: 999.0.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }
}

/// `color` at `a` opacity.
/// A project's own colour: a tint to sit behind its monogram and an ink
/// to draw the letter in. The hue is a hash of the project's identity, so
/// the same repository looks the same on every machine and in both
/// themes, and nothing has to be chosen or stored. Saturation and
/// lightness are fixed per theme so every tint carries the ink at 4.5:1.
pub fn identity(seed: &str, dark: bool) -> (Color, Color) {
    // FNV-1a: cheap, stable, and spread well enough for a hue wheel.
    let mut hash: u32 = 0x811c_9dc5;
    for byte in seed.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    // Twelve stops rather than a continuous wheel: neighbouring projects
    // land on visibly different hues instead of two near-identical blues.
    let hue = f32::from((hash % 12) as u8) * 30.0 + 15.0;
    if dark {
        (hsl(hue, 0.42, 0.26), hsl(hue, 0.70, 0.84))
    } else {
        (hsl(hue, 0.60, 0.90), hsl(hue, 0.65, 0.28))
    }
}

/// HSL to sRGB, hue in degrees.
fn hsl(hue: f32, saturation: f32, lightness: f32) -> Color {
    let c = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let h = (hue.rem_euclid(360.0)) / 60.0;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u8 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = lightness - c / 2.0;
    Color::from_rgb(r + m, g + m, b + m)
}

pub fn alpha(color: Color, a: f32) -> Color {
    Color { a, ..color }
}

/// `t` of the way from `from` to `to`.
pub fn mix(from: Color, to: Color, t: f32) -> Color {
    Color {
        r: from.r + (to.r - from.r) * t,
        g: from.g + (to.g - from.g) * t,
        b: from.b + (to.b - from.b) * t,
        a: from.a + (to.a - from.a) * t,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linear(channel: f32) -> f32 {
        if channel <= 0.03928 {
            channel / 12.92
        } else {
            ((channel + 0.055) / 1.055).powf(2.4)
        }
    }
    /// WCAG relative luminance.
    fn luma(c: Color) -> f32 {
        0.2126 * linear(c.r) + 0.7152 * linear(c.g) + 0.0722 * linear(c.b)
    }
    fn contrast(a: Color, b: Color) -> f32 {
        let (l1, l2) = (luma(a) + 0.05, luma(b) + 0.05);
        l1.max(l2) / l1.min(l2)
    }

    #[test]
    fn text_roles_stay_legible_on_every_surface() {
        for dark in [false, true] {
            let c = Colors::new(dark);
            for ground in [c.ground, c.sidebar, c.card, c.raised] {
                assert!(
                    contrast(c.text, ground) > 7.0,
                    "text on surface, dark={dark}"
                );
                assert!(
                    contrast(c.muted, ground) >= 4.5,
                    "muted on surface, dark={dark}"
                );
                assert!(
                    contrast(c.faint, ground) >= 4.5,
                    "faint on surface, dark={dark}"
                );
            }
            assert!(
                contrast(c.accent_ink, c.accent_soft) >= 4.5,
                "selected ink, dark={dark}"
            );
            assert!(
                contrast(Color::WHITE, c.accent) >= 4.5,
                "primary label, dark={dark}"
            );
        }
    }

    #[test]
    fn the_theme_is_told_apart_from_its_palette() {
        for dark in [false, true] {
            assert_eq!(Colors::of(&Colors::new(dark).theme()).dark, dark);
        }
    }

    #[test]
    fn every_project_identity_carries_its_ink() {
        let seeds = [
            "AgentDocker",
            "aistor",
            "7d0e1c2b3a4f5e6d7c8b9a0f1e2d3c4b5a697887",
            "/Users/me/src/tools",
            "",
        ];
        for dark in [false, true] {
            for seed in seeds {
                let (tint, ink) = identity(seed, dark);
                assert!(
                    contrast(ink, tint) >= 4.5,
                    "ink on tint for {seed:?}, dark={dark}: {}",
                    contrast(ink, tint)
                );
                assert_eq!(identity(seed, dark), (tint, ink), "stable for {seed:?}");
            }
            // Exhaust the wheel: every stop must carry its ink, not just
            // the ones these seeds happen to land on.
            let mut hues = std::collections::BTreeSet::new();
            for n in 0..200u32 {
                let (tint, ink) = identity(&n.to_string(), dark);
                assert!(contrast(ink, tint) >= 4.5, "stop for seed {n}, dark={dark}");
                hues.insert(format!("{:.3},{:.3},{:.3}", tint.r, tint.g, tint.b));
            }
            assert_eq!(hues.len(), 12, "twelve distinct stops, dark={dark}");
        }
        assert_ne!(
            identity("AgentDocker", false).0,
            identity("aistor", false).0
        );
    }

    #[test]
    fn hsl_reaches_the_primaries() {
        let red = hsl(0.0, 1.0, 0.5);
        assert!((red.r - 1.0).abs() < 1e-6 && red.g.abs() < 1e-6 && red.b.abs() < 1e-6);
        let green = hsl(120.0, 1.0, 0.5);
        assert!(green.r.abs() < 1e-6 && (green.g - 1.0).abs() < 1e-6);
        let grey = hsl(200.0, 0.0, 0.5);
        assert!((grey.r - 0.5).abs() < 1e-6 && (grey.b - 0.5).abs() < 1e-6);
    }

    #[test]
    fn mixing_moves_towards_the_target() {
        let half = mix(Color::BLACK, Color::WHITE, 0.5);
        assert!((half.r - 0.5).abs() < 1e-6 && (half.g - 0.5).abs() < 1e-6);
        assert_eq!(alpha(Color::WHITE, 0.3).a, 0.3);
    }
}
