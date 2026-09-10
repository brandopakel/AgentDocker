//! The rail's glyphs, drawn rather than shipped.
//!
//! Five strokes on a sixteen-point grid, scaled to whatever size they are
//! given and inked in whatever colour the row is using. Drawing them keeps
//! them crisp at any density and in both appearances without an icon font
//! or a bitmap per theme.
use super::Message;
use iced::widget::canvas::{self, Frame, Geometry, LineCap, LineJoin, Path, Stroke};
use iced::{Color, Element, Point, Rectangle, Renderer, Theme, mouse};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    /// A folder: the projects.
    Projects,
    /// An envelope: the inbox.
    Inbox,
    /// Three joined nodes: connections.
    Connections,
    /// Sliders: settings.
    Settings,
    /// A plus.
    Add,
}

/// An icon inked in one colour.
pub struct Glyph {
    pub icon: Icon,
    pub color: Color,
}

impl Glyph {
    fn stroke(&self, width: f32) -> Stroke<'_> {
        Stroke::default()
            .with_color(self.color)
            .with_width(width)
            .with_line_cap(LineCap::Round)
            .with_line_join(LineJoin::Round)
    }
}

impl canvas::Program<Message> for Glyph {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        // Everything below is drawn on a 16 × 16 grid.
        frame.scale(bounds.width.min(bounds.height) / 16.0);
        let stroke = self.stroke(1.5);
        let p = Point::new;
        match self.icon {
            Icon::Projects => {
                let folder = Path::new(|b| {
                    b.move_to(p(2.0, 4.5));
                    b.line_to(p(6.0, 4.5));
                    b.line_to(p(7.5, 6.0));
                    b.line_to(p(14.0, 6.0));
                    b.line_to(p(14.0, 12.5));
                    b.line_to(p(2.0, 12.5));
                    b.close();
                });
                frame.stroke(&folder, stroke);
            }
            Icon::Inbox => {
                let body =
                    Path::rounded_rectangle(p(2.0, 4.0), iced::Size::new(12.0, 8.5), 1.5.into());
                let flap = Path::new(|b| {
                    b.move_to(p(2.5, 5.0));
                    b.line_to(p(8.0, 9.3));
                    b.line_to(p(13.5, 5.0));
                });
                frame.stroke(&body, stroke);
                frame.stroke(&flap, stroke);
            }
            Icon::Connections => {
                let links = Path::new(|b| {
                    b.move_to(p(8.0, 5.6));
                    b.line_to(p(3.8, 10.6));
                    b.move_to(p(8.0, 5.6));
                    b.line_to(p(12.2, 10.6));
                });
                frame.stroke(&links, stroke);
                for centre in [p(8.0, 4.0), p(3.5, 12.0), p(12.5, 12.0)] {
                    frame.fill(&Path::circle(centre, 1.7), self.color);
                }
            }
            Icon::Settings => {
                for (y, knob) in [(4.5, 10.5), (8.0, 5.5), (11.5, 11.0)] {
                    frame.stroke(&Path::line(p(2.0, y), p(14.0, y)), stroke);
                    frame.fill(&Path::circle(p(knob, y), 1.9), self.color);
                }
            }
            Icon::Add => {
                let plus = Path::new(|b| {
                    b.move_to(p(8.0, 3.0));
                    b.line_to(p(8.0, 13.0));
                    b.move_to(p(3.0, 8.0));
                    b.line_to(p(13.0, 8.0));
                });
                frame.stroke(&plus, stroke);
            }
        }
        vec![frame.into_geometry()]
    }
}

/// An icon of `size` points, inked in `color`.
pub fn icon<'a>(icon: Icon, color: Color, size: f32) -> Element<'a, Message> {
    canvas::Canvas::new(Glyph { icon, color })
        .width(size)
        .height(size)
        .into()
}
