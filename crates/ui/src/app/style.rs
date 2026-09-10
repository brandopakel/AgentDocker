//! Shared visual roles; status is always accompanied by a text label.
//!
//! The palette comes from the mark: a deep navy ground, electric blue for
//! selection and primary actions, cyan as the secondary brand tone. Light
//! and dark keep the same roles so the hierarchy reads identically.
use iced::{Border, Color, Shadow, Theme, Vector, color, widget::container};

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
                faint: color!(0x6b778a),
                line: color!(0x263042),
                accent: color!(0x3f82ff),
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
                faint: color!(0x7d8a9c),
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

    /// A card: raised surface, hairline, and in light mode a whisper of depth.
    pub fn card_style(self) -> container::Style {
        container::Style {
            shadow: if self.dark {
                Shadow::default()
            } else {
                Shadow {
                    color: Color::from_rgba8(16, 24, 40, 0.06),
                    offset: Vector::new(0.0, 1.0),
                    blur_radius: 3.0,
                }
            },
            ..self.surface(self.card, true)
        }
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
                    contrast(c.muted, ground) > 3.0,
                    "muted on surface, dark={dark}"
                );
            }
            assert!(
                contrast(c.accent_ink, c.accent_soft) > 4.0,
                "selected ink, dark={dark}"
            );
            assert!(
                contrast(Color::WHITE, c.accent) > 3.0,
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
    fn mixing_moves_towards_the_target() {
        let half = mix(Color::BLACK, Color::WHITE, 0.5);
        assert!((half.r - 0.5).abs() < 1e-6 && (half.g - 0.5).abs() < 1e-6);
        assert_eq!(alpha(Color::WHITE, 0.3).a, 0.3);
    }
}
