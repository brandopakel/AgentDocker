//! Shared visual roles; status is always accompanied by a text label.
//!
//! The palette comes from the mark: a deep navy ground, electric blue for
//! selection and primary actions, cyan as the secondary brand tone. Light
//! and dark keep the same roles so the hierarchy reads identically, and
//! the rail stays navy in both, so the cube always sits on navy
//! (`Colors::rail`).
//!
//! Depth comes from tint and hairline, never from blur: rail under ground
//! under card under raised, with `overlay` the only surface above a card.
//! Radii follow one scale (`RADIUS_*`): keycaps and checkboxes 4, controls
//! 6, menus and tracks 8, cards and panels 10.
use iced::{Border, Color, Font, Theme, color, widget::container};

/// Keycaps, checkboxes, menu items.
pub const RADIUS_XS: f32 = 4.0;
/// Buttons, inputs, tooltips, rows, small tiles.
pub const RADIUS_SM: f32 = 6.0;
/// Menus, segmented tracks, rail rows, icon tiles.
pub const RADIUS_MD: f32 = 8.0;
/// Cards and panels.
pub const RADIUS_LG: f32 = 10.0;

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
    /// Secondary-button fill, segmented tracks, keycaps and icon tiles.
    pub raised: Color,
    /// A row or a quiet control under the pointer.
    pub hover: Color,
    /// Menus, popovers and tooltips: the one surface above a card.
    pub overlay: Color,
    pub text: Color,
    pub muted: Color,
    /// Eyebrows and tertiary detail.
    pub faint: Color,
    /// Card borders and row rules.
    pub line: Color,
    /// Inputs, outline buttons and unchecked controls.
    pub line_strong: Color,
    /// Primary actions and the selected mark.
    pub accent: Color,
    /// A primary action under the pointer.
    pub accent_hover: Color,
    /// Tinted ground behind a selected control.
    pub accent_soft: Color,
    /// Ink on `accent_soft`.
    pub accent_ink: Color,
    pub cyan: Color,
    pub green: Color,
    pub amber: Color,
    pub red: Color,
    /// The fill of an armed destructive action; white reads on it.
    pub danger: Color,
}

impl Colors {
    pub fn new(dark: bool) -> Self {
        if dark {
            Self {
                dark,
                ground: color!(0x0b1018),
                sidebar: color!(0x070b12),
                card: color!(0x111826),
                raised: color!(0x18212f),
                hover: color!(0x141c29),
                overlay: color!(0x1a2333),
                text: color!(0xe8ecf2),
                muted: color!(0x97a2b4),
                faint: color!(0x808ca0),
                line: color!(0x1f2938),
                line_strong: color!(0x2e3a4d),
                accent: color!(0x2c6ae4),
                accent_hover: color!(0x326fe6),
                accent_soft: color!(0x132748),
                accent_ink: color!(0xb7cdfd),
                cyan: color!(0x2fc4dd),
                green: color!(0x46c08a),
                amber: color!(0xe0ad55),
                red: color!(0xee6b68),
                danger: color!(0xc43b3b),
            }
        } else {
            Self {
                dark,
                ground: color!(0xf8f9fc),
                sidebar: color!(0x0f1a2f),
                card: Color::WHITE,
                raised: color!(0xebeff6),
                hover: color!(0xf2f5f9),
                overlay: Color::WHITE,
                text: color!(0x0b1324),
                muted: color!(0x545f72),
                faint: color!(0x5f6a7c),
                line: color!(0xdfe4ec),
                line_strong: color!(0xc9d1dd),
                accent: color!(0x2563eb),
                accent_hover: color!(0x1d55d0),
                accent_soft: color!(0xe5edff),
                accent_ink: color!(0x1a45a8),
                cyan: color!(0x0a7c96),
                green: color!(0x1b7f50),
                amber: color!(0x9a5f12),
                red: color!(0xc43b3b),
                danger: color!(0xc43b3b),
            }
        }
    }

    /// The roles for whatever theme a style closure was handed.
    pub fn of(theme: &Theme) -> Self {
        Self::new(theme.palette().background.r < 0.5)
    }

    /// The roles inside the navigation rail. The rail is navy in both
    /// appearances, so in light mode it carries its own light-on-navy
    /// ink; every rail colour comes from here, never from the page roles.
    /// `ground` is the rail itself, `raised` the selected row and `hover`
    /// the row under the pointer.
    pub fn rail(self) -> Self {
        let base = if self.dark {
            self
        } else {
            Self {
                sidebar: color!(0x0f1a2f),
                text: color!(0xd4d9e2),
                muted: color!(0x8f9aae),
                faint: color!(0x8f9aae),
                ..Self::new(true)
            }
        };
        Self {
            ground: base.sidebar,
            card: base.sidebar,
            raised: mix(base.sidebar, base.text, 0.09),
            hover: mix(base.sidebar, base.text, 0.05),
            line: mix(base.sidebar, base.text, 0.10),
            line_strong: mix(base.sidebar, base.text, 0.18),
            ..base
        }
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
                radius: RADIUS_LG.into(),
            },
            ..Default::default()
        }
    }

    /// A menu, popover or tooltip: the overlay surface with the one small
    /// shadow the renderer can afford (a few hundred pixels, not a card).
    pub fn overlay_style(self) -> container::Style {
        container::Style {
            background: Some(self.overlay.into()),
            text_color: Some(self.text),
            border: Border {
                color: if self.dark {
                    self.line_strong
                } else {
                    self.line
                },
                width: 1.0,
                radius: RADIUS_MD.into(),
            },
            shadow: iced::Shadow {
                color: Color::from_rgba8(8, 12, 20, if self.dark { 0.35 } else { 0.12 }),
                offset: iced::Vector::new(0.0, 2.0),
                blur_radius: 8.0,
            },
            ..Default::default()
        }
    }

    /// A small square tile behind a glyph or a keycap: raised fill and a
    /// hairline.
    pub fn tile(self, radius: f32) -> container::Style {
        container::Style {
            background: Some(self.raised.into()),
            text_color: Some(self.muted),
            border: Border {
                color: self.line,
                width: 1.0,
                radius: radius.into(),
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

    /// An inline notice in a tone: a faint wash of the tone and a hairline
    /// of it, so it reads as a note rather than as another card.
    pub fn attention_style(self, tint: Color) -> container::Style {
        container::Style {
            background: Some(mix(self.card, tint, if self.dark { 0.08 } else { 0.06 }).into()),
            text_color: Some(self.text),
            border: Border {
                color: alpha(tint, if self.dark { 0.40 } else { 0.35 }),
                width: 1.0,
                radius: RADIUS_MD.into(),
            },
            ..Default::default()
        }
    }

    /// A small rounded label: counts and kinds, never status (status is a
    /// dot and a word).
    pub fn pill(self, background: Color, ink: Color) -> container::Style {
        container::Style {
            background: Some(background.into()),
            text_color: Some(ink),
            border: Border {
                radius: RADIUS_SM.into(),
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

/// `color` at `a` opacity.
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
            // The rail is checked with its own ink: it is navy in both
            // appearances, so the page's light-mode ink would vanish on it.
            let rail = c.rail();
            for (roles, ground, place) in [
                (c, c.ground, "ground"),
                (c, c.card, "card"),
                (c, c.raised, "raised"),
                (c, c.hover, "hover"),
                (c, c.overlay, "overlay"),
                (rail, rail.ground, "rail"),
                (rail, rail.raised, "selected rail row"),
                (rail, rail.hover, "rail row under the pointer"),
            ] {
                assert!(
                    contrast(roles.text, ground) > 7.0,
                    "text on {place}, dark={dark}"
                );
                assert!(
                    contrast(roles.muted, ground) >= 4.5,
                    "muted on {place}, dark={dark}: {}",
                    contrast(roles.muted, ground)
                );
                assert!(
                    contrast(roles.faint, ground) >= 4.5,
                    "faint on {place}, dark={dark}: {}",
                    contrast(roles.faint, ground)
                );
            }
            assert_eq!(rail.ground, c.sidebar, "the rail is the sidebar colour");
            assert!(
                contrast(c.accent_ink, c.accent_soft) >= 4.5,
                "selected ink, dark={dark}"
            );
            for fill in [c.accent, c.accent_hover, c.danger] {
                assert!(
                    contrast(Color::WHITE, fill) >= 4.5,
                    "white label on a filled action, dark={dark}: {}",
                    contrast(Color::WHITE, fill)
                );
            }
            for tone in [c.green, c.amber, c.red] {
                assert!(
                    contrast(tone, c.card) >= 4.5,
                    "status word on a card, dark={dark}: {}",
                    contrast(tone, c.card)
                );
            }
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
