//! The app's glyphs, drawn rather than shipped.
//!
//! Strokes on a sixteen-point grid, scaled to whatever size they are
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
    /// A speech bubble: channels.
    Channels,
    /// Three dots: more.
    More,
    /// Three columns: the board.
    Board,
    /// A prompt in a window: a terminal.
    Terminal,
    /// Chevrons, pointing where they open.
    ChevronDown,
    ChevronRight,
    ChevronLeft,
    /// Two bars: pause.
    Pause,
    /// A lens: search.
    Search,
    /// A tick: done.
    Check,
    /// A cross: close or remove.
    Close,
    /// An arrow up: send.
    ArrowUp,
    /// A pulse line: activity.
    Pulse,
    /// A page with a folded corner: a file.
    File,
    /// A clock face: time.
    Clock,
    /// A question mark in a circle: a question.
    Question,
    /// Two overlapping pages: copy.
    Copy,
    /// An arrow into a tray: an update.
    Download,
    /// A hash: a channel.
    Hash,
    /// A circle half filled: light or dark appearance.
    Contrast,
    // board, history, files in use, usage
    /// A node on a line: a commit.
    Commit,
    /// A pencil: a note.
    Note,
    /// An arrow through a door, inwards: someone joined.
    Join,
    /// An arrow through a door, outwards: someone left.
    Leave,
    /// Two arrows passing: work handed over.
    Handoff,
    /// A shield: access asked for.
    Shield,
    /// A chevron pointing up: a disclosure that is open.
    ChevronUp,
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
            Icon::Board => {
                for x in [2.5, 6.5, 10.5] {
                    let column =
                        Path::rounded_rectangle(p(x, 3.0), iced::Size::new(3.0, 10.0), 1.0.into());
                    frame.stroke(&column, stroke);
                }
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
            Icon::Terminal => {
                let window =
                    Path::rounded_rectangle(p(1.75, 2.75), iced::Size::new(12.5, 10.5), 2.0.into());
                let prompt = Path::new(|b| {
                    b.move_to(p(4.5, 6.0));
                    b.line_to(p(6.5, 8.0));
                    b.line_to(p(4.5, 10.0));
                    b.move_to(p(8.0, 10.0));
                    b.line_to(p(11.0, 10.0));
                });
                frame.stroke(&window, stroke);
                frame.stroke(&prompt, stroke);
            }
            Icon::ChevronDown => {
                frame.stroke(
                    &polyline(&[p(4.0, 6.0), p(8.0, 10.0), p(12.0, 6.0)]),
                    stroke,
                );
            }
            Icon::ChevronRight => {
                frame.stroke(
                    &polyline(&[p(6.0, 4.0), p(10.0, 8.0), p(6.0, 12.0)]),
                    stroke,
                );
            }
            Icon::ChevronLeft => {
                frame.stroke(
                    &polyline(&[p(10.0, 4.0), p(6.0, 8.0), p(10.0, 12.0)]),
                    stroke,
                );
            }
            Icon::Pause => {
                for x in [5.5, 10.5] {
                    frame.stroke(&Path::line(p(x, 3.5), p(x, 12.5)), self.stroke(2.0));
                }
            }
            Icon::Search => {
                frame.stroke(&Path::circle(p(7.0, 7.0), 4.25), stroke);
                frame.stroke(&Path::line(p(10.2, 10.2), p(13.5, 13.5)), stroke);
            }
            Icon::Check => {
                frame.stroke(
                    &polyline(&[p(3.0, 8.5), p(6.5, 12.0), p(13.0, 4.5)]),
                    self.stroke(1.75),
                );
            }
            Icon::Close => {
                let cross = Path::new(|b| {
                    b.move_to(p(4.0, 4.0));
                    b.line_to(p(12.0, 12.0));
                    b.move_to(p(12.0, 4.0));
                    b.line_to(p(4.0, 12.0));
                });
                frame.stroke(&cross, stroke);
            }
            Icon::ArrowUp => {
                let arrow = Path::new(|b| {
                    b.move_to(p(8.0, 13.0));
                    b.line_to(p(8.0, 3.5));
                    b.move_to(p(4.0, 7.5));
                    b.line_to(p(8.0, 3.5));
                    b.line_to(p(12.0, 7.5));
                });
                frame.stroke(&arrow, self.stroke(1.75));
            }
            Icon::Pulse => {
                frame.stroke(
                    &polyline(&[
                        p(1.5, 8.5),
                        p(4.5, 8.5),
                        p(6.0, 4.0),
                        p(9.0, 12.5),
                        p(10.5, 8.5),
                        p(14.5, 8.5),
                    ]),
                    stroke,
                );
            }
            Icon::File => {
                let page = Path::new(|b| {
                    b.move_to(p(4.0, 2.0));
                    b.line_to(p(9.5, 2.0));
                    b.line_to(p(12.5, 5.0));
                    b.line_to(p(12.5, 14.0));
                    b.line_to(p(4.0, 14.0));
                    b.close();
                    b.move_to(p(9.5, 2.0));
                    b.line_to(p(9.5, 5.0));
                    b.line_to(p(12.5, 5.0));
                });
                frame.stroke(&page, stroke);
            }
            Icon::Clock => {
                frame.stroke(&Path::circle(p(8.0, 8.0), 5.75), stroke);
                frame.stroke(
                    &polyline(&[p(8.0, 4.75), p(8.0, 8.0), p(10.25, 9.5)]),
                    stroke,
                );
            }
            Icon::Question => {
                frame.stroke(&Path::circle(p(8.0, 8.0), 6.0), stroke);
                let hook = Path::new(|b| {
                    b.move_to(p(6.2, 6.4));
                    b.quadratic_curve_to(p(6.4, 4.6), p(8.1, 4.6));
                    b.quadratic_curve_to(p(9.9, 4.7), p(9.9, 6.3));
                    b.quadratic_curve_to(p(9.8, 7.5), p(8.0, 8.4));
                    b.line_to(p(8.0, 9.3));
                });
                frame.stroke(&hook, stroke);
                frame.fill(&Path::circle(p(8.0, 11.6), 0.95), self.color);
            }
            Icon::Copy => {
                frame.stroke(
                    &Path::rounded_rectangle(p(5.5, 5.5), iced::Size::new(8.0, 8.0), 1.5.into()),
                    stroke,
                );
                frame.stroke(
                    &polyline(&[
                        p(3.0, 10.5),
                        p(2.5, 10.5),
                        p(2.5, 2.5),
                        p(10.5, 2.5),
                        p(10.5, 3.0),
                    ]),
                    stroke,
                );
            }
            Icon::Download => {
                let arrow = Path::new(|b| {
                    b.move_to(p(8.0, 2.5));
                    b.line_to(p(8.0, 10.0));
                    b.move_to(p(4.75, 7.0));
                    b.line_to(p(8.0, 10.25));
                    b.line_to(p(11.25, 7.0));
                    b.move_to(p(2.5, 11.0));
                    b.line_to(p(2.5, 13.5));
                    b.line_to(p(13.5, 13.5));
                    b.line_to(p(13.5, 11.0));
                });
                frame.stroke(&arrow, stroke);
            }
            Icon::Hash => {
                let hash = Path::new(|b| {
                    b.move_to(p(6.5, 2.5));
                    b.line_to(p(5.0, 13.5));
                    b.move_to(p(11.0, 2.5));
                    b.line_to(p(9.5, 13.5));
                    b.move_to(p(3.0, 6.0));
                    b.line_to(p(13.5, 6.0));
                    b.move_to(p(2.5, 10.0));
                    b.line_to(p(13.0, 10.0));
                });
                frame.stroke(&hash, stroke);
            }
            Icon::Contrast => {
                frame.stroke(&Path::circle(p(8.0, 8.0), 5.5), stroke);
                let half = Path::new(|b| {
                    b.move_to(p(8.0, 2.5));
                    b.arc(canvas::path::Arc {
                        center: p(8.0, 8.0),
                        radius: 5.5,
                        start_angle: iced::Radians(-std::f32::consts::FRAC_PI_2),
                        end_angle: iced::Radians(std::f32::consts::FRAC_PI_2),
                    });
                    b.close();
                });
                frame.fill(&half, self.color);
            }
            // board, history, files in use, usage
            Icon::Commit => {
                frame.stroke(&Path::circle(p(8.0, 8.0), 2.75), stroke);
                frame.stroke(&Path::line(p(1.5, 8.0), p(5.25, 8.0)), stroke);
                frame.stroke(&Path::line(p(10.75, 8.0), p(14.5, 8.0)), stroke);
            }
            Icon::Note => {
                let pencil = Path::new(|b| {
                    b.move_to(p(10.5, 2.75));
                    b.line_to(p(13.25, 5.5));
                    b.line_to(p(5.75, 13.0));
                    b.line_to(p(2.5, 13.5));
                    b.line_to(p(3.0, 10.25));
                    b.close();
                    b.move_to(p(9.0, 4.25));
                    b.line_to(p(11.75, 7.0));
                });
                frame.stroke(&pencil, stroke);
            }
            Icon::Join | Icon::Leave => {
                let door = polyline(&[p(9.5, 2.5), p(13.5, 2.5), p(13.5, 13.5), p(9.5, 13.5)]);
                let arrow = if self.icon == Icon::Join {
                    Path::new(|b| {
                        b.move_to(p(2.0, 8.0));
                        b.line_to(p(10.0, 8.0));
                        b.move_to(p(7.0, 5.0));
                        b.line_to(p(10.0, 8.0));
                        b.line_to(p(7.0, 11.0));
                    })
                } else {
                    Path::new(|b| {
                        b.move_to(p(11.0, 8.0));
                        b.line_to(p(2.5, 8.0));
                        b.move_to(p(5.5, 5.0));
                        b.line_to(p(2.5, 8.0));
                        b.line_to(p(5.5, 11.0));
                    })
                };
                frame.stroke(&door, stroke);
                frame.stroke(&arrow, stroke);
            }
            Icon::Handoff => {
                let arrows = Path::new(|b| {
                    b.move_to(p(2.5, 5.5));
                    b.line_to(p(13.0, 5.5));
                    b.move_to(p(10.5, 3.0));
                    b.line_to(p(13.0, 5.5));
                    b.line_to(p(10.5, 8.0));
                    b.move_to(p(13.5, 10.5));
                    b.line_to(p(3.0, 10.5));
                    b.move_to(p(5.5, 8.0));
                    b.line_to(p(3.0, 10.5));
                    b.line_to(p(5.5, 13.0));
                });
                frame.stroke(&arrows, stroke);
            }
            Icon::Shield => {
                let shield = Path::new(|b| {
                    b.move_to(p(8.0, 1.75));
                    b.line_to(p(13.0, 3.75));
                    b.line_to(p(13.0, 7.75));
                    b.quadratic_curve_to(p(12.6, 12.1), p(8.0, 14.25));
                    b.quadratic_curve_to(p(3.4, 12.1), p(3.0, 7.75));
                    b.line_to(p(3.0, 3.75));
                    b.close();
                });
                frame.stroke(&shield, stroke);
                frame.stroke(&polyline(&[p(5.9, 8.0), p(7.4, 9.5), p(10.2, 6.5)]), stroke);
            }
            Icon::ChevronUp => {
                frame.stroke(
                    &polyline(&[p(4.0, 10.0), p(8.0, 6.0), p(12.0, 10.0)]),
                    stroke,
                );
            }
        }
    }
}

/// An open path through `points`.
fn polyline(points: &[Point]) -> Path {
    Path::new(|b| {
        if let Some((first, rest)) = points.split_first() {
            b.move_to(*first);
            for point in rest {
                b.line_to(*point);
            }
        }
    })
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
