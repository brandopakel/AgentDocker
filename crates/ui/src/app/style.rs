//! Shared visual roles; status is always accompanied by a text label.
use iced::{Border, Color, Theme, color, widget::container};

#[derive(Clone, Copy)]
pub struct Colors {
    pub ground: Color,
    pub sidebar: Color,
    pub subtle: Color,
    pub text: Color,
    pub muted: Color,
    pub line: Color,
    pub accent: Color,
    pub green: Color,
    pub amber: Color,
}

impl Colors {
    pub fn new(dark: bool) -> Self {
        if dark {
            Self {
                ground: color!(0x181b21),
                sidebar: color!(0x14171c),
                subtle: color!(0x20252e),
                text: color!(0xe9edf4),
                muted: color!(0xa0aabb),
                line: color!(0x303743),
                accent: color!(0x8cb4ff),
                green: color!(0x83c9a4),
                amber: color!(0xe2b86d),
            }
        } else {
            Self {
                ground: Color::WHITE,
                sidebar: color!(0xf5f6f8),
                subtle: color!(0xf8f9fb),
                text: color!(0x202734),
                muted: color!(0x626d7e),
                line: color!(0xe4e8ee),
                accent: color!(0x285fc4),
                green: color!(0x28724f),
                amber: color!(0x946321),
            }
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
                danger: color!(0xbb4141),
            },
        )
    }

    pub fn surface(self, background: Color, bordered: bool) -> container::Style {
        container::Style {
            background: Some(background.into()),
            text_color: Some(self.text),
            border: Border {
                color: self.line,
                width: if bordered { 1.0 } else { 0.0 },
                radius: 10.0.into(),
            },
            ..Default::default()
        }
    }
}
