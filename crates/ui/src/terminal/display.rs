//! A real VT grid with focused input, bounded transport and native IME support.
use super::*;
use crate::{app::Message, controls::Focus};
use iced::advanced::{
    Clipboard, Layout, Shell, Widget, input_method, layout, renderer, text,
    widget::{self, Operation, Tree, tree},
};
use iced::advanced::{Renderer as _, text::Renderer as _};
use iced::{Event, Font, Length, Point, Rectangle, Size, keyboard, mouse};

pub struct Display<'a> {
    pub terminal: &'a Terminal,
    pub palette: &'a Palette,
    pub size: f32,
    pub height: f32,
}
#[derive(Default)]
struct State {
    focus: Focus,
    preedit: input_method::Preedit,
}

impl<'a> From<Display<'a>> for iced::Element<'a, Message> {
    fn from(value: Display<'a>) -> Self {
        Self::new(value)
    }
}
impl Widget<Message, iced::Theme, iced::Renderer> for Display<'_> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }
    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fixed(self.height))
    }
    fn layout(
        &mut self,
        _: &mut Tree,
        _: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout::Node::new(limits.resolve(Length::Fill, self.height, Size::ZERO))
    }
    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        _: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        let id = widget::Id::new("terminal");
        let mut semantic = crate::accessibility::Semantic::terminal(
            lock(&self.terminal.shared.parser).screen().contents(),
        );
        operation.custom(Some(&id), layout.bounds(), &mut semantic);
        operation.focusable(
            Some(&id),
            layout.bounds(),
            &mut tree.state.downcast_mut::<State>().focus,
        );
    }
    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _: &Rectangle,
    ) {
        let state = tree.state.downcast_mut::<State>();
        let bounds = layout.bounds();
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
                if cursor.is_over(bounds) =>
            {
                shell.publish(Message::Focus("terminal".into()));
                shell.capture_event();
            }
            Event::Mouse(mouse::Event::WheelScrolled { delta }) if cursor.is_over(bounds) => {
                let rows = match delta {
                    mouse::ScrollDelta::Lines { y, .. } => *y * 3.0,
                    mouse::ScrollDelta::Pixels { y, .. } => *y / (self.size * 1.35),
                };
                shell.publish(Message::TerminalScroll(rows.round() as i32));
                shell.capture_event();
            }
            Event::Keyboard(event @ keyboard::Event::KeyPressed { key, modifiers, .. })
                if state.focus.focused =>
            {
                // App shortcuts and F6 leave the terminal available to keyboard users.
                if matches!(key, keyboard::Key::Named(keyboard::key::Named::F6)) {
                    return;
                }
                if modifiers.command()
                    && matches!(key,keyboard::Key::Character(c) if ["1","2","3","4"].contains(&c.as_str()))
                {
                    return;
                }
                if (modifiers.logo() || (modifiers.control() && modifiers.shift()))
                    && matches!(key,keyboard::Key::Character(c) if c.eq_ignore_ascii_case("v"))
                {
                    if let Some(paste) = clipboard.read(iced::advanced::clipboard::Kind::Standard) {
                        let mut bytes = paste.into_bytes();
                        if bytes.len() <= input::MAX_BYTES - 12
                            && lock(&self.terminal.shared.parser)
                                .screen()
                                .bracketed_paste()
                        {
                            let mut framed = b"\x1b[200~".to_vec();
                            framed.append(&mut bytes);
                            framed.extend_from_slice(b"\x1b[201~");
                            bytes = framed;
                        }
                        shell.publish(Message::TerminalInput(bytes));
                    }
                } else if (modifiers.logo() || (modifiers.control() && modifiers.shift()))
                    && matches!(key,keyboard::Key::Character(c) if c.eq_ignore_ascii_case("c"))
                {
                    clipboard.write(
                        iced::advanced::clipboard::Kind::Standard,
                        lock(&self.terminal.shared.parser).screen().contents(),
                    );
                } else if state.preedit.content.is_empty() {
                    let bytes = keys::encode(
                        event,
                        lock(&self.terminal.shared.parser)
                            .screen()
                            .application_cursor(),
                    );
                    shell.publish(Message::TerminalInput(bytes));
                }
                shell.capture_event();
            }
            Event::InputMethod(input_method::Event::Preedit(content, selection))
                if state.focus.focused =>
            {
                state.preedit = input_method::Preedit {
                    content: content.clone(),
                    selection: selection.clone(),
                    text_size: Some(self.size.into()),
                };
                shell.request_redraw();
                shell.capture_event();
            }
            Event::InputMethod(input_method::Event::Commit(content)) if state.focus.focused => {
                state.preedit = Default::default();
                shell.publish(Message::TerminalInput(content.as_bytes().to_vec()));
                shell.capture_event();
            }
            Event::InputMethod(input_method::Event::Closed) => state.preedit = Default::default(),
            Event::Window(iced::window::Event::RedrawRequested(_)) => {
                let cols = (bounds.width / (self.size * 0.61))
                    .floor()
                    .clamp(20.0, 500.0) as u16;
                let rows = (bounds.height / (self.size * 1.4))
                    .floor()
                    .clamp(4.0, 200.0) as u16;
                if self.terminal.size != (cols, rows) {
                    shell.publish(Message::TerminalResize(cols, rows));
                }
                if state.focus.focused {
                    let (row, col) = lock(&self.terminal.shared.parser)
                        .screen()
                        .cursor_position();
                    shell.request_input_method(&input_method::InputMethod::Enabled {
                        cursor: Rectangle {
                            x: bounds.x + f32::from(col) * self.size * 0.61,
                            y: bounds.y + f32::from(row) * self.size * 1.4,
                            width: self.size * 0.61,
                            height: self.size * 1.4,
                        },
                        purpose: input_method::Purpose::Terminal,
                        preedit: Some(state.preedit.clone()),
                    });
                }
            }
            _ => {}
        }
    }
    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        _: &iced::Theme,
        _: &renderer::Style,
        layout: Layout<'_>,
        _: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let Some(clip) = bounds.intersection(viewport) else {
            return;
        };
        let state = tree.state.downcast_ref::<State>();
        renderer.fill_quad(
            renderer::Quad {
                bounds,
                ..Default::default()
            },
            iced::Color::from(self.palette.ground),
        );
        let parser = lock(&self.terminal.shared.parser);
        let screen = parser.screen();
        let (rows, cols) = screen.size();
        let (cw, ch) = (self.size * 0.61, self.size * 1.4);
        renderer.with_layer(clip, |renderer| {
            for row in 0..rows {
                for col in 0..cols {
                    let Some(cell) = screen.cell(row, col) else {
                        continue;
                    };
                    if cell.is_wide_continuation() {
                        continue;
                    }
                    let pos = Point::new(
                        bounds.x + f32::from(col) * cw,
                        bounds.y + f32::from(row) * ch,
                    );
                    let width = if cell.is_wide() { cw * 2.0 } else { cw };
                    let (mut fg, mut bg) = (
                        foreground(self.palette, cell.fgcolor()),
                        if cell.bgcolor() == vt100::Color::Default {
                            self.palette.ground
                        } else {
                            foreground(self.palette, cell.bgcolor())
                        },
                    );
                    if cell.inverse() {
                        std::mem::swap(&mut fg, &mut bg);
                    }
                    renderer.fill_quad(
                        renderer::Quad {
                            bounds: Rectangle::new(pos, Size::new(width, ch)),
                            ..Default::default()
                        },
                        iced::Color::from(bg),
                    );
                    let font = Font {
                        // Use the system CJK face for wide cells on macOS; the generic
                        // monospace fallback may have no visible glyph for these scalars.
                        family: if cfg!(target_os = "macos") && cell.is_wide() {
                            iced::font::Family::Name("Hiragino Sans")
                        } else {
                            Font::MONOSPACE.family
                        },
                        weight: if cell.bold() {
                            iced::font::Weight::Bold
                        } else {
                            iced::font::Weight::Normal
                        },
                        style: if cell.italic() {
                            iced::font::Style::Italic
                        } else {
                            iced::font::Style::Normal
                        },
                        ..Font::MONOSPACE
                    };
                    renderer.fill_text(
                        text::Text {
                            content: cell.contents().to_owned(),
                            bounds: Size::new(width, ch),
                            size: self.size.into(),
                            line_height: text::LineHeight::Absolute(ch.into()),
                            font,
                            align_x: text::Alignment::Left,
                            align_y: iced::alignment::Vertical::Top,
                            shaping: text::Shaping::Advanced,
                            wrapping: text::Wrapping::None,
                        },
                        pos,
                        fg.into(),
                        clip,
                    );
                    if cell.underline() {
                        renderer.fill_quad(
                            renderer::Quad {
                                bounds: Rectangle {
                                    x: pos.x,
                                    y: pos.y + ch - 2.0,
                                    width,
                                    height: 1.0,
                                },
                                ..Default::default()
                            },
                            iced::Color::from(fg),
                        );
                    }
                }
            }
            if state.focus.focused && !screen.hide_cursor() && !self.terminal.scrolled_back() {
                let (row, col) = screen.cursor_position();
                let mut color: iced::Color = self.palette.text.into();
                color.a = 0.45;
                renderer.fill_quad(
                    renderer::Quad {
                        bounds: Rectangle {
                            x: bounds.x + f32::from(col) * cw,
                            y: bounds.y + f32::from(row) * ch,
                            width: cw,
                            height: ch,
                        },
                        ..Default::default()
                    },
                    color,
                );
            }
        });
    }
}
