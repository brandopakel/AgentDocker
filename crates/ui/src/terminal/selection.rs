//! A bounded snapshot of visible cells keeps copy tied to the highlighted text.
//! Live output continues into the parser while the user selects this snapshot.
use iced::{Point, Rectangle};

pub(super) struct Selection {
    rows: u16,
    cols: u16,
    cells: Vec<vt100::Cell>,
    wrapped: Vec<bool>,
    anchor: usize,
    head: usize,
    pub dragging: bool,
}

impl Selection {
    pub fn begin(screen: &vt100::Screen, point: Point, bounds: Rectangle, size: f32) -> Self {
        let (rows, cols) = screen.size();
        let mut selection = Self {
            rows,
            cols,
            cells: (0..rows)
                .flat_map(|row| {
                    (0..cols).map(move |col| {
                        screen
                            .cell(row, col)
                            .expect("cell within visible screen bounds")
                            .clone()
                    })
                })
                .collect(),
            wrapped: (0..rows).map(|row| screen.row_wrapped(row)).collect(),
            anchor: 0,
            head: 0,
            dragging: true,
        };
        selection.head = selection.offset(point, bounds, size);
        selection.anchor = selection.head;
        selection
    }

    fn offset(&self, point: Point, bounds: Rectangle, size: f32) -> usize {
        let row = ((point.y - bounds.y) / (size * 1.4))
            .floor()
            .clamp(0.0, f32::from(self.rows.saturating_sub(1))) as usize;
        let col = ((point.x - bounds.x) / (size * 0.61))
            .round()
            .clamp(0.0, f32::from(self.cols)) as usize;
        row * usize::from(self.cols) + col
    }

    pub fn extend(&mut self, point: Point, bounds: Rectangle, size: f32) {
        self.head = self.offset(point, bounds, size);
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    pub fn size(&self) -> (u16, u16) {
        (self.rows, self.cols)
    }

    pub fn contains(&self, row: u16, col: u16, wide: bool) -> bool {
        let cell = usize::from(row) * usize::from(self.cols) + usize::from(col);
        cell < self.anchor.max(self.head)
            && cell + if wide { 2 } else { 1 } > self.anchor.min(self.head)
    }

    pub fn cells(&self) -> impl Iterator<Item = (u16, u16, &vt100::Cell)> {
        self.cells
            .iter()
            .enumerate()
            .filter(|(_, cell)| !cell.is_wide_continuation())
            .map(|(i, cell)| {
                (
                    (i / usize::from(self.cols)) as u16,
                    (i % usize::from(self.cols)) as u16,
                    cell,
                )
            })
    }

    pub fn text(&self) -> String {
        self.text_between(self.anchor.min(self.head), self.anchor.max(self.head))
    }

    pub fn contents(&self) -> String {
        self.text_between(0, self.cells.len())
            .trim_end_matches('\n')
            .to_owned()
    }

    fn text_between(&self, start: usize, end: usize) -> String {
        if start == end || self.cols == 0 {
            return String::new();
        }
        let cols = usize::from(self.cols);
        let first = start / cols;
        let last = (end - 1) / cols;
        let mut text = String::new();
        for row in first..=last {
            let mut line = String::new();
            for col in 0..self.cols {
                let cell = &self.cells[row * cols + usize::from(col)];
                let offset = row * cols + usize::from(col);
                if !cell.is_wide_continuation()
                    && offset < end
                    && offset + if cell.is_wide() { 2 } else { 1 } > start
                {
                    let contents = cell.contents();
                    if contents.is_empty() {
                        line.push(' ');
                    } else {
                        line.push_str(contents);
                    }
                }
            }
            // Padding at hard line ends is not user text. Preserve spaces at
            // soft wraps so a wrapped command can be pasted without alteration.
            if self.wrapped[row] {
                text.push_str(&line);
            } else {
                text.push_str(line.trim_end_matches(' '));
            }
            if row < last && !self.wrapped[row] {
                text.push('\n');
            }
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn select(parser: &vt100::Parser, anchor: usize, head: usize) -> Selection {
        let mut selection =
            Selection::begin(parser.screen(), Point::ORIGIN, Rectangle::default(), 12.0);
        selection.anchor = anchor;
        selection.head = head;
        selection
    }

    #[test]
    fn reverse_multiline_ranges_preserve_newlines_and_skip_padding() {
        let mut parser = vt100::Parser::new(4, 10, 10);
        parser.process(b"first\r\n\r\nlast");
        assert_eq!(select(&parser, 2, 24).text(), "rst\n\nlast");
        assert_eq!(select(&parser, 24, 2).text(), "rst\n\nlast");
        assert_eq!(select(&parser, 1, 2).text(), "i");
        assert_eq!(select(&parser, 2, 2).text(), "");
    }

    #[test]
    fn wide_combining_and_wrapped_cells_copy_complete_characters_once() {
        let mut parser = vt100::Parser::new(4, 6, 0);
        parser.process("A日e\u{301} xxYZ".as_bytes());
        assert_eq!(select(&parser, 2, 3).text(), "日");
        assert_eq!(select(&parser, 1, 4).text(), "日e\u{301}");
        assert_eq!(select(&parser, 0, 9).text(), "A日e\u{301} xxYZ");
    }

    #[test]
    fn snapshot_does_not_retarget_copy_when_output_or_scrollback_changes() {
        let mut parser = vt100::Parser::new(4, 10, 100);
        parser.process(b"original\r\n");
        let selected = select(&parser, 0, 8);
        parser.process(b"\x1b[2J\x1b[Hchanged\r\nmore\r\nlines\r\nnow");
        parser.screen_mut().set_scrollback(1);
        assert_eq!(selected.text(), "original");
        assert_eq!(
            selected.cells.len(),
            40,
            "only the visible grid is retained"
        );
        assert!(selected.contains(0, 7, false));
        assert!(!selected.contains(0, 8, false));
    }

    #[test]
    fn pointer_positions_clamp_to_visible_cell_boundaries() {
        let parser = vt100::Parser::new(4, 10, 0);
        let bounds = Rectangle {
            x: 100.0,
            y: 200.0,
            width: 61.0,
            height: 56.0,
        };
        let mut selected =
            Selection::begin(parser.screen(), Point::new(-100.0, -100.0), bounds, 10.0);
        selected.extend(Point::new(10000.0, 10000.0), bounds, 10.0);
        assert_eq!((selected.anchor, selected.head), (0, 40));
    }
}
