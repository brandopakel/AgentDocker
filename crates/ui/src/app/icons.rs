//! The rail's glyphs, drawn rather than shipped.
//!
//! Five strokes on a sixteen-point grid, scaled to whatever size they are
//! given and inked in whatever colour the row is using. Drawing them keeps
//! them crisp at any density and in both appearances without an icon font
//! or a bitmap per theme.
use super::Message;
use iced::widget::canvas::{self, Frame, Geometry, LineCap, LineJoin, Path, Stroke};
use iced::{Color, Element, Point, Rectangle, Renderer, Theme, mouse};
use std::cell::Cell;

/// The geometry of one drawn icon, kept between frames. Live geometry is
/// re-tessellated and repainted every frame; cached geometry is compared by
/// identity and left alone while nothing about it changed. The icon and the
/// colour it was inked in are remembered so a change of either redraws it
/// once: widget state survives a rebuild by position, so the same slot can
/// be asked to show a different glyph.
#[derive(Default)]
pub struct Cached {
    geometry: canvas::Cache,
    drawn: Cell<Option<(Icon, Color)>>,
}

impl Cached {
    /// Notes what is about to be drawn; clears the geometry and answers
    /// `true` when it differs from what the cache holds.
    fn refresh(&self, icon: Icon, color: Color) -> bool {
        if self.drawn.get() == Some((icon, color)) {
            return false;
        }
        self.geometry.clear();
        self.drawn.set(Some((icon, color)));
        true
    }
}

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
    /// Two stacked bars: sessions.
    Sessions,
    /// A pulse line: activity.
    Activity,
    /// A speech bubble: channels.
    Channels,
    /// Three dots: more.
    More,
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
    type State = Cached;

    fn draw(
        &self,
        state: &Cached,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        state.refresh(self.icon, self.color);
        vec![state.geometry.draw(renderer, bounds.size(), |frame| {
            self.paint(frame, bounds);
        })]
    }
}

impl Glyph {
    fn paint(&self, frame: &mut Frame, bounds: Rectangle) {
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
            Icon::Sessions => {
                for y in [3.0, 9.0] {
                    let bar =
                        Path::rounded_rectangle(p(2.5, y), iced::Size::new(11.0, 4.0), 1.5.into());
                    frame.stroke(&bar, stroke);
                }
            }
            Icon::Activity => {
                let pulse = Path::new(|b| {
                    b.move_to(p(2.0, 8.5));
                    b.line_to(p(5.0, 8.5));
                    b.line_to(p(6.8, 4.0));
                    b.line_to(p(9.2, 12.5));
                    b.line_to(p(11.0, 8.5));
                    b.line_to(p(14.0, 8.5));
                });
                frame.stroke(&pulse, stroke);
            }
            Icon::Channels => {
                let bubble = Path::new(|b| {
                    b.move_to(p(4.0, 3.0));
                    b.line_to(p(12.0, 3.0));
                    b.quadratic_curve_to(p(14.0, 3.0), p(14.0, 5.0));
                    b.line_to(p(14.0, 9.0));
                    b.quadratic_curve_to(p(14.0, 11.0), p(12.0, 11.0));
                    b.line_to(p(7.0, 11.0));
                    b.line_to(p(4.0, 13.5));
                    b.line_to(p(4.0, 11.0));
                    b.quadratic_curve_to(p(2.0, 11.0), p(2.0, 9.0));
                    b.line_to(p(2.0, 5.0));
                    b.quadratic_curve_to(p(2.0, 3.0), p(4.0, 3.0));
                    b.close();
                });
                frame.stroke(&bubble, stroke);
            }
            Icon::More => {
                for x in [3.5, 8.0, 12.5] {
                    frame.fill(&Path::circle(p(x, 8.0), 1.6), self.color);
                }
            }
        }
    }
}

/// An icon of `size` points, inked in `color`.
pub fn icon<'a>(icon: Icon, color: Color, size: f32) -> Element<'a, Message> {
    canvas::Canvas::new(Glyph { icon, color })
        .width(size)
        .height(size)
        .into()
}

#[cfg(test)]
mod tests {
    use super::{Cached, Icon};
    use iced::Color;

    #[test]
    fn the_cache_is_cleared_when_the_icon_or_its_colour_changes() {
        let cache = Cached::default();
        assert!(cache.refresh(Icon::Projects, Color::WHITE), "first draw");
        assert!(
            !cache.refresh(Icon::Projects, Color::WHITE),
            "same glyph, same ink"
        );
        assert!(
            cache.refresh(Icon::Inbox, Color::WHITE),
            "same ink, different glyph"
        );
        assert!(!cache.refresh(Icon::Inbox, Color::WHITE));
        assert!(
            cache.refresh(Icon::Inbox, Color::BLACK),
            "same glyph, different ink"
        );
        assert_eq!(cache.drawn.get(), Some((Icon::Inbox, Color::BLACK)));
    }
}
