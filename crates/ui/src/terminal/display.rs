//! A real VT grid with focused input, bounded transport and native IME support.
use super::*;
use crate::{app::Message, controls::Focus};
use iced::advanced::{
    Clipboard, Layout, Shell, Widget, input_method, layout, renderer, text,
    widget::{self, Operation, Tree, tree},
};
use iced::advanced::{Renderer as _, text::Renderer as _};
use iced::{Event, Font, Length, Point, Rectangle, Size, keyboard, mouse};

#[path = "selection.rs"]
mod selection;
use selection::Selection;

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
    selection: Option<Selection>,
    source: std::sync::Weak<Mutex<vt100::Parser>>,
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
        let state = tree.state.downcast_ref::<State>();
        let contents = state
            .selection
            .as_ref()
            .filter(|_| state.source.as_ptr() == Arc::as_ptr(&self.terminal.shared.parser))
            .map(Selection::contents)
            .unwrap_or_else(|| lock(&self.terminal.shared.parser).screen().contents());
        let mut semantic = crate::accessibility::Semantic::terminal(contents);
        operation.custom(Some(&id), layout.bounds(), &mut semantic);
        operation.focusable(
            Some(&id),
            layout.bounds(),
            &mut tree.state.downcast_mut::<State>().focus,
        );
    }
    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _: &Rectangle,
        _: &iced::Renderer,
    ) -> mouse::Interaction {
        if cursor.is_over(layout.bounds())
            || tree
                .state
                .downcast_ref::<State>()
                .selection
                .as_ref()
                .is_some_and(|selection| selection.dragging)
        {
            mouse::Interaction::Text
        } else {
            mouse::Interaction::default()
        }
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
        if state.source.as_ptr() != Arc::as_ptr(&self.terminal.shared.parser) {
            state.source = Arc::downgrade(&self.terminal.shared.parser);
            state.selection = None;
            state.preedit = Default::default();
        }
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
                if cursor.is_over(bounds) =>
            {
                if let Some(point) = cursor.position() {
                    state.selection = Some(Selection::begin(
                        lock(&self.terminal.shared.parser).screen(),
                        point,
                        bounds,
                        self.size,
                    ));
                }
                shell.publish(Message::Focus("terminal".into()));
                shell.request_redraw();
                shell.capture_event();
            }
            Event::Mouse(mouse::Event::CursorMoved { position })
                if state
                    .selection
                    .as_ref()
                    .is_some_and(|selection| selection.dragging) =>
            {
                if let Some(selection) = &mut state.selection {
                    selection.extend(*position, bounds, self.size);
                }
                shell.request_redraw();
                shell.capture_event();
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
                if state
                    .selection
                    .as_ref()
                    .is_some_and(|selection| selection.dragging) =>
            {
                if let Some(selection) = &mut state.selection {
                    if let Some(point) = cursor.position() {
                        selection.extend(point, bounds, self.size);
                    }
                    selection.dragging = false;
                    if selection.is_empty() {
                        state.selection = None;
                    }
                }
                shell.request_redraw();
                shell.capture_event();
            }
            Event::Mouse(mouse::Event::WheelScrolled { delta }) if cursor.is_over(bounds) => {
                state.selection = None;
                let rows = match delta {
                    mouse::ScrollDelta::Lines { y, .. } => *y * 3.0,
                    mouse::ScrollDelta::Pixels { y, .. } => *y / (self.size * 1.4),
                };
                shell.publish(Message::TerminalScroll(rows.round() as i32));
                shell.request_redraw();
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
                if matches!(key, keyboard::Key::Named(keyboard::key::Named::Escape))
                    && state.selection.take().is_some()
                {
                    shell.request_redraw();
                    shell.capture_event();
                    return;
                }
                if (modifiers.logo() || (modifiers.control() && modifiers.shift()))
                    && matches!(key,keyboard::Key::Character(c) if c.eq_ignore_ascii_case("v"))
                {
                    state.selection = None;
                    if let Some(paste) = clipboard.read(iced::advanced::clipboard::Kind::Standard) {
                        let bracketed = lock(&self.terminal.shared.parser)
                            .screen()
                            .bracketed_paste();
                        let bytes = paste_bytes(paste, bracketed);
                        shell.publish(Message::TerminalInput(bytes));
                    }
                } else if (modifiers.logo() || (modifiers.control() && modifiers.shift()))
                    && matches!(key,keyboard::Key::Character(c) if c.eq_ignore_ascii_case("c"))
                {
                    let copied = state
                        .selection
                        .take()
                        .filter(|selection| !selection.is_empty())
                        .map(|selection| selection.text())
                        .unwrap_or_else(|| lock(&self.terminal.shared.parser).screen().contents());
                    clipboard.write(iced::advanced::clipboard::Kind::Standard, copied);
                } else if state.preedit.content.is_empty() {
                    state.selection = None;
                    let bytes = keys::encode(
                        event,
                        lock(&self.terminal.shared.parser)
                            .screen()
                            .application_cursor(),
                    );
                    shell.publish(Message::TerminalInput(bytes));
                }
                shell.request_redraw();
                shell.capture_event();
            }
            Event::InputMethod(input_method::Event::Preedit(content, selection))
                if state.focus.focused =>
            {
                state.selection = None;
                state.preedit = input_method::Preedit {
                    content: content.clone(),
                    selection: selection.clone(),
                    text_size: Some(self.size.into()),
                };
                shell.request_redraw();
                shell.capture_event();
            }
            Event::InputMethod(input_method::Event::Commit(content)) if state.focus.focused => {
                state.selection = None;
                state.preedit = Default::default();
                shell.publish(Message::TerminalInput(content.as_bytes().to_vec()));
                shell.capture_event();
            }
            Event::InputMethod(input_method::Event::Closed) => state.preedit = Default::default(),
            Event::Window(iced::window::Event::Unfocused) => {
                state.selection = None;
                state.preedit = Default::default();
                shell.request_redraw();
            }
            Event::Window(iced::window::Event::RedrawRequested(_)) => {
                let cols = (bounds.width / (self.size * 0.61))
                    .floor()
                    .clamp(20.0, 500.0) as u16;
                let rows = (bounds.height / (self.size * 1.4))
                    .floor()
                    .clamp(4.0, 200.0) as u16;
                if self.terminal.size != (cols, rows) {
                    state.selection = None;
                    shell.publish(Message::TerminalResize(cols, rows));
                }
                if state
                    .selection
                    .as_ref()
                    .is_some_and(|selection| selection.size() != (rows, cols))
                {
                    state.selection = None;
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
        let selection = state
            .selection
            .as_ref()
            .filter(|_| state.source.as_ptr() == Arc::as_ptr(&self.terminal.shared.parser));
        renderer.fill_quad(
            renderer::Quad {
                bounds,
                ..Default::default()
            },
            iced::Color::from(self.palette.ground),
        );
        let (cw, ch) = (self.size * 0.61, self.size * 1.4);
        // Copy only painted cells while holding the parser lock. Shaping and
        // software drawing must not keep the output reader from draining frames.
        let (cells, cursor_position) = if let Some(selection) = selection {
            (
                selection
                    .cells()
                    .filter(|(_, _, cell)| needs_paint(cell))
                    .map(|(row, col, cell)| (row, col, cell.clone()))
                    .collect(),
                None,
            )
        } else {
            let parser = lock(&self.terminal.shared.parser);
            let screen = parser.screen();
            let cells = painted_cells(
                screen,
                (bounds.height / ch).ceil() as u16,
                (bounds.width / cw).ceil() as u16,
            );
            let cursor_position = (!screen.hide_cursor()).then(|| screen.cursor_position());
            (cells, cursor_position)
        };
        renderer.with_layer(clip, |renderer| {
            if let Some(selection) = selection {
                for (row, col, cell) in selection.cells() {
                    if selection.contains(row, col, cell.is_wide()) {
                        renderer.fill_quad(
                            renderer::Quad {
                                bounds: Rectangle {
                                    x: bounds.x + f32::from(col) * cw,
                                    y: bounds.y + f32::from(row) * ch,
                                    width: cw * if cell.is_wide() { 2.0 } else { 1.0 },
                                    height: ch,
                                },
                                ..Default::default()
                            },
                            iced::Color::from(self.palette.accent),
                        );
                    }
                }
            }
            for (row, col, cell) in cells {
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
                if selection.is_some_and(|selection| selection.contains(row, col, cell.is_wide())) {
                    fg = self.palette.ground;
                    bg = self.palette.accent;
                }
                if bg != self.palette.ground {
                    renderer.fill_quad(
                        renderer::Quad {
                            bounds: Rectangle::new(pos, Size::new(width, ch)),
                            ..Default::default()
                        },
                        iced::Color::from(bg),
                    );
                }
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
                let contents = cell.contents();
                if !contents.is_empty() && contents != " " {
                    renderer.fill_text(
                        text::Text {
                            content: contents.to_owned(),
                            bounds: Size::new(width, ch),
                            size: self.size.into(),
                            line_height: text::LineHeight::Absolute(ch.into()),
                            font,
                            align_x: text::Alignment::Left,
                            align_y: iced::alignment::Vertical::Top,
                            shaping: if contents.is_ascii() {
                                text::Shaping::Basic
                            } else {
                                text::Shaping::Advanced
                            },
                            wrapping: text::Wrapping::None,
                        },
                        pos,
                        fg.into(),
                        clip,
                    );
                }
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
            if state.focus.focused
                && !self.terminal.scrolled_back()
                && let Some((row, col)) = cursor_position
            {
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

fn painted_cells(screen: &vt100::Screen, rows: u16, cols: u16) -> Vec<(u16, u16, vt100::Cell)> {
    let (height, width) = screen.size();
    let mut cells = Vec::new();
    for row in 0..rows.min(height) {
        for col in 0..cols.min(width) {
            let Some(cell) = screen.cell(row, col) else {
                continue;
            };
            if cell.is_wide_continuation() {
                continue;
            }
            if !needs_paint(cell) {
                continue;
            }
            cells.push((row, col, cell.clone()));
        }
    }
    cells
}

fn needs_paint(cell: &vt100::Cell) -> bool {
    let content = cell.contents();
    !((content.is_empty() || content == " ")
        && cell.bgcolor() == vt100::Color::Default
        && !cell.inverse()
        && !cell.underline())
}

fn paste_bytes(mut paste: String, bracketed: bool) -> Vec<u8> {
    if !bracketed {
        return paste.into_bytes();
    }
    // Clipboard text cannot terminate its own paste frame. Removing escape
    // introducers also prevents nested terminators from appearing after filtering.
    paste.retain(|c| c != '\u{1b}' && c != '\u{9b}');
    let mut bytes = b"\x1b[200~".to_vec();
    bytes.extend_from_slice(paste.as_bytes());
    bytes.extend_from_slice(b"\x1b[201~");
    // The input queue admits the whole framed payload or reports its size limit.
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct MemoryClipboard(Option<String>);
    impl Clipboard for MemoryClipboard {
        fn read(&self, _: iced::advanced::clipboard::Kind) -> Option<String> {
            self.0.clone()
        }
        fn write(&mut self, _: iced::advanced::clipboard::Kind, text: String) {
            self.0 = Some(text);
        }
    }

    fn fixture() -> Terminal {
        Terminal {
            agent: "selection-fixture".into(),
            shared: Shared {
                parser: Arc::new(Mutex::new(vt100::Parser::new(24, 80, 10))),
                status: Arc::new(Mutex::new(Status::Attached)),
                connection: Arc::new(Mutex::new(Connection::default())),
                input: Arc::new(Input::default()),
            },
            input_notice: None,
            size: DEFAULT_SIZE,
            scrollback: 0,
        }
    }

    fn key(key: keyboard::Key, modifiers: keyboard::Modifiers) -> Event {
        Event::Keyboard(keyboard::Event::KeyPressed {
            modified_key: key.clone(),
            key,
            physical_key: keyboard::key::Physical::Unidentified(
                keyboard::key::NativeCode::Unidentified,
            ),
            location: keyboard::Location::Standard,
            modifiers,
            text: None,
            repeat: false,
        })
    }

    fn update(
        element: &mut iced::Element<'_, Message>,
        tree: &mut Tree,
        event: Event,
        cursor: mouse::Cursor,
        clipboard: &mut MemoryClipboard,
    ) -> Vec<Message> {
        let renderer = iced::Renderer::new(Font::MONOSPACE, 10.0.into());
        let bounds = Rectangle::with_size(Size::new(488.0, 336.0));
        let node = element.as_widget_mut().layout(
            tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, bounds.size()),
        );
        let mut messages = Vec::new();
        element.as_widget_mut().update(
            tree,
            &event,
            Layout::new(&node),
            cursor,
            &renderer,
            clipboard,
            &mut Shell::new(&mut messages),
            &bounds,
        );
        messages
    }

    fn drag(
        element: &mut iced::Element<'_, Message>,
        tree: &mut Tree,
        clipboard: &mut MemoryClipboard,
    ) {
        update(
            element,
            tree,
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            mouse::Cursor::Available(Point::new(0.0, 7.0)),
            clipboard,
        );
        let point = Point::new(4.0 * 6.1, 7.0);
        update(
            element,
            tree,
            Event::Mouse(mouse::Event::CursorMoved { position: point }),
            mouse::Cursor::Available(point),
            clipboard,
        );
        update(
            element,
            tree,
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            mouse::Cursor::Unavailable,
            clipboard,
        );
    }

    #[test]
    fn mouse_selection_copies_the_highlighted_snapshot_without_sending_input() {
        let terminal = fixture();
        lock(&terminal.shared.parser).process(b"original");
        let mut element: iced::Element<'_, Message> = Display {
            terminal: &terminal,
            palette: &crate::theme::PALETTES[0],
            size: 10.0,
            height: 336.0,
        }
        .into();
        let mut tree = Tree::new(&element);
        tree.state.downcast_mut::<State>().focus.focused = true;
        let mut clipboard = MemoryClipboard::default();
        drag(&mut element, &mut tree, &mut clipboard);
        assert!(
            !tree
                .state
                .downcast_ref::<State>()
                .selection
                .as_ref()
                .unwrap()
                .dragging
        );
        lock(&terminal.shared.parser).process(b"\rchanged ");
        let messages = update(
            &mut element,
            &mut tree,
            key(
                keyboard::Key::Character("c".into()),
                keyboard::Modifiers::CTRL | keyboard::Modifiers::SHIFT,
            ),
            mouse::Cursor::Unavailable,
            &mut clipboard,
        );
        assert_eq!(clipboard.0.as_deref(), Some("orig"));
        assert!(messages.is_empty());
        assert!(tree.state.downcast_ref::<State>().selection.is_none());
        let messages = update(
            &mut element,
            &mut tree,
            key(
                keyboard::Key::Character("c".into()),
                keyboard::Modifiers::CTRL,
            ),
            mouse::Cursor::Unavailable,
            &mut clipboard,
        );
        assert!(matches!(messages.as_slice(), [Message::TerminalInput(bytes)] if bytes == &[3]));
    }

    #[test]
    fn escape_clears_selection_then_reaches_the_terminal_and_scroll_clears_snapshot() {
        let terminal = fixture();
        lock(&terminal.shared.parser).process(b"original");
        let mut element: iced::Element<'_, Message> = Display {
            terminal: &terminal,
            palette: &crate::theme::PALETTES[0],
            size: 10.0,
            height: 336.0,
        }
        .into();
        let mut tree = Tree::new(&element);
        tree.state.downcast_mut::<State>().focus.focused = true;
        let mut clipboard = MemoryClipboard::default();
        drag(&mut element, &mut tree, &mut clipboard);
        let escape = key(
            keyboard::Key::Named(keyboard::key::Named::Escape),
            keyboard::Modifiers::empty(),
        );
        assert!(
            update(
                &mut element,
                &mut tree,
                escape.clone(),
                mouse::Cursor::Unavailable,
                &mut clipboard
            )
            .is_empty()
        );
        let messages = update(
            &mut element,
            &mut tree,
            escape,
            mouse::Cursor::Unavailable,
            &mut clipboard,
        );
        assert!(matches!(messages.as_slice(), [Message::TerminalInput(bytes)] if bytes == &[27]));
        drag(&mut element, &mut tree, &mut clipboard);
        let messages = update(
            &mut element,
            &mut tree,
            Event::Mouse(mouse::Event::WheelScrolled {
                delta: mouse::ScrollDelta::Lines { x: 0.0, y: 1.0 },
            }),
            mouse::Cursor::Available(Point::new(10.0, 10.0)),
            &mut clipboard,
        );
        assert!(tree.state.downcast_ref::<State>().selection.is_none());
        assert!(matches!(messages.as_slice(), [Message::TerminalScroll(3)]));
    }

    #[test]
    fn bracketed_paste_cannot_end_early_or_fall_back_to_raw_input() {
        let bytes = paste_bytes("one\x1b[201~\nsecond\u{9b}201~".into(), true);
        assert_eq!(bytes, b"\x1b[200~one[201~\nsecond201~\x1b[201~");
        for size in [input::MAX_BYTES - 11, input::MAX_BYTES] {
            let queue = input::Input::default();
            let bytes = paste_bytes("x".repeat(size), true);
            assert_eq!(
                queue.push(input::Outbound::Keys(bytes.into())),
                Err(input::Rejected::TooLarge)
            );
            assert_eq!(queue.retained(), (0, 0));
        }
        let queue = input::Input::default();
        let bytes = paste_bytes("x".repeat(input::MAX_BYTES - 12), true);
        queue.push(input::Outbound::Keys(bytes.into())).unwrap();
        assert_eq!(queue.retained(), (1, input::MAX_BYTES));
    }

    #[test]
    fn render_snapshot_skips_empty_cells_and_retains_styles_and_wide_text() {
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process("A日\x1b[41m \x1b[0m\x1b[4m \x1b[0m\x1b[7m ".as_bytes());
        let cells = painted_cells(parser.screen(), 24, 80);
        assert_eq!(cells.len(), 5);
        assert_eq!(cells[1].2.contents(), "日");
        assert!(cells[1].2.is_wide());
        assert_eq!(cells[2].1, 3);
        assert_ne!(cells[2].2.bgcolor(), vt100::Color::Default);
        assert!(cells[3].2.underline());
        assert!(cells[4].2.inverse());
        parser.process(b"\x1b[2J");
        assert_eq!(
            cells[0].2.contents(),
            "A",
            "snapshot no longer borrows the parser"
        );
    }
}
