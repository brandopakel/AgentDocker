//! A table that fits the terminal it is printed on.
//!
//! In a pipe a table is what it has always been: every column, every
//! cell whole, as wide as it needs. At a terminal that is a table whose
//! rows wrap, each onto two or three lines, and nobody can read down a
//! column. So here each column says what it is to its row: its identity,
//! which is never dropped and never cut; something always shown, perhaps
//! shortened; or something a narrow terminal can do without. A table that
//! is too wide first leaves out what it can do without, least needed
//! first, and says which; then shortens what may be shortened, the widest
//! first, with an ellipsis. A row is one line that still names what it is
//! a row of; and on a terminal too narrow for even that, the table gives
//! way to one block a row, every field on a labelled line of its own,
//! rather than run over or lose what tells two rows apart.

use agentdocker_core::Tone;

use super::{Caps, text};

/// Between two columns.
const GAP: usize = 3;

/// What a column is to its row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Keep {
    /// What the row is a row of: never left out, never shortened.
    Identity,
    /// Always shown.
    Always,
    /// Left out when the terminal is too narrow: the highest number
    /// first.
    Optional(u8),
}

#[derive(Clone, Debug)]
pub struct Column {
    pub header: String,
    pub keep: Keep,
    /// The fewest columns its cells may be shortened to; none for a
    /// column that is never shortened.
    pub cut_to: Option<usize>,
}

impl Column {
    pub fn identity(header: &str) -> Self {
        Self {
            header: header.to_owned(),
            keep: Keep::Identity,
            cut_to: None,
        }
    }

    pub fn always(header: &str) -> Self {
        Self {
            keep: Keep::Always,
            ..Self::identity(header)
        }
    }

    pub fn optional(header: &str, rank: u8) -> Self {
        Self {
            keep: Keep::Optional(rank),
            ..Self::identity(header)
        }
    }

    /// Its cells may be shortened, to no fewer than `least` columns. An
    /// identity is never shortened, whatever is asked here.
    pub fn cut_to(mut self, least: usize) -> Self {
        if self.keep != Keep::Identity {
            self.cut_to = Some(least.max(1));
        }
        self
    }
}

#[derive(Clone, Debug, Default)]
pub struct Cell {
    pub text: String,
    pub tone: Tone,
}

impl Cell {
    pub fn toned(text: impl Into<String>, tone: Tone) -> Self {
        Self {
            text: text.into(),
            tone,
        }
    }
}

impl From<String> for Cell {
    fn from(text: String) -> Self {
        Self {
            text,
            tone: Tone::Neutral,
        }
    }
}

impl From<&str> for Cell {
    fn from(text: &str) -> Self {
        Self::from(text.to_owned())
    }
}

#[derive(Clone, Debug, Default)]
pub struct Row {
    pub cells: Vec<Cell>,
    /// The whole row is set back: something listed beside the rest that
    /// is not one of them.
    pub faint: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Table {
    pub columns: Vec<Column>,
    pub rows: Vec<Row>,
}

impl Table {
    pub fn new(columns: Vec<Column>) -> Self {
        Self {
            columns,
            rows: Vec::new(),
        }
    }

    pub fn row(&mut self, cells: Vec<Cell>) {
        self.rows.push(Row {
            cells,
            faint: false,
        });
    }

    /// The table as text for `caps`: its header, a line a row, and, when
    /// columns were left out for want of room, a last line naming them.
    /// Every cell is made safe to print before it is measured.
    pub fn render(&self, caps: Caps) -> String {
        let cells: Vec<Vec<String>> = self
            .rows
            .iter()
            .map(|row| {
                (0..self.columns.len())
                    .map(|i| {
                        row.cells
                            .get(i)
                            .map(|cell| text::shown(&cell.text).into_owned())
                            .unwrap_or_default()
                    })
                    .collect()
            })
            .collect();
        let mut widths: Vec<usize> = self
            .columns
            .iter()
            .enumerate()
            .map(|(i, column)| {
                cells
                    .iter()
                    .map(|row| text::width(&row[i]))
                    .fold(text::width(&column.header), usize::max)
            })
            .collect();
        let mut shown: Vec<usize> = (0..self.columns.len()).collect();
        let mut left_out: Vec<&str> = Vec::new();
        if let Some(room) = caps.width {
            let total = |shown: &[usize], widths: &[usize]| {
                shown.iter().map(|&i| widths[i]).sum::<usize>()
                    + GAP * shown.len().saturating_sub(1)
            };
            // What a narrow terminal can do without goes first, the least
            // needed before the rest.
            while total(&shown, &widths) > room {
                let least_needed = shown
                    .iter()
                    .copied()
                    .filter_map(|i| match self.columns[i].keep {
                        Keep::Optional(rank) => Some((rank, i)),
                        _ => None,
                    })
                    .max();
                let Some((_, gone)) = least_needed else { break };
                shown.retain(|&i| i != gone);
                left_out.push(&self.columns[gone].header);
            }
            // Then what may be shortened is, the widest first, a column
            // at a time, down to the least each allows.
            while total(&shown, &widths) > room {
                let widest = shown
                    .iter()
                    .copied()
                    .filter(|&i| {
                        self.columns[i]
                            .cut_to
                            .is_some_and(|least| widths[i] > least)
                    })
                    .max_by_key(|&i| widths[i]);
                let Some(widest) = widest else { break };
                widths[widest] -= 1;
            }
            // Still too wide with everything optional gone and everything
            // else at its least: a table cannot be this narrow.
            if total(&shown, &widths) > room {
                return self.stacked(&cells, caps);
            }
        }
        // Left out in the order they stand in the table, not the order
        // they went.
        left_out.sort_by_key(|header| {
            self.columns
                .iter()
                .position(|column| column.header == *header)
        });

        let line = |texts: &[(String, Tone)], bold: bool, faint: bool| {
            // Nothing follows the last cell that has something in it: no
            // row ends in spaces.
            let last = texts.iter().rposition(|(text, _)| !text.is_empty());
            let mut out = String::new();
            for (at, &column) in shown.iter().enumerate() {
                let Some(last) = last else { break };
                if at > last {
                    break;
                }
                if at > 0 {
                    out.push_str(&" ".repeat(GAP));
                }
                let (text, tone) = &texts[at];
                let cell = text::cut(text, widths[column]);
                let style = match (bold, faint) {
                    (true, _) => super::HEADING,
                    (_, true) => super::FAINT,
                    _ => super::style_of(*tone),
                };
                out.push_str(&super::paint(&cell, style, caps));
                if at < last {
                    out.extend(std::iter::repeat_n(
                        ' ',
                        widths[column].saturating_sub(text::width(&cell)),
                    ));
                }
            }
            out.push('\n');
            out
        };

        let headers: Vec<(String, Tone)> = shown
            .iter()
            .map(|&i| (self.columns[i].header.clone(), Tone::Neutral))
            .collect();
        let mut out = line(&headers, true, false);
        for (row, texts) in self.rows.iter().zip(&cells) {
            let texts: Vec<(String, Tone)> = shown
                .iter()
                .map(|&i| {
                    (
                        texts[i].clone(),
                        row.cells.get(i).map(|cell| cell.tone).unwrap_or_default(),
                    )
                })
                .collect();
            out.push_str(&line(&texts, false, row.faint));
        }
        if !left_out.is_empty() {
            let note = format!(
                "not shown at this width: {} (`--format plain` has every column)",
                left_out.join(", ")
            );
            out.push_str(&super::paint(&note, super::FAINT, caps));
            out.push('\n');
        }
        out
    }
}

impl Table {
    /// A row a block, for a terminal too narrow for the table: the row's
    /// identity on a line of its own, then each thing it has to say on a
    /// labelled line, every column there whatever a table would have left
    /// out, and nothing shortened. A cell that says nothing (empty, or
    /// `-`) is not listed.
    fn stacked(&self, cells: &[Vec<String>], caps: Caps) -> String {
        let label = self
            .columns
            .iter()
            .filter(|column| column.keep != Keep::Identity)
            .map(|column| text::width(&column.header))
            .max()
            .unwrap_or(0);
        let mut out = String::new();
        for (row, texts) in self.rows.iter().zip(cells) {
            let named: Vec<String> = self
                .columns
                .iter()
                .zip(texts)
                .filter(|(column, _)| column.keep == Keep::Identity)
                .map(|(_, text)| {
                    let style = if row.faint {
                        super::FAINT
                    } else {
                        super::HEADING
                    };
                    super::paint(text, style, caps)
                })
                .collect();
            out.push_str(named.join("  ").trim_end());
            out.push('\n');
            for (at, (column, text)) in self.columns.iter().zip(texts).enumerate() {
                if column.keep == Keep::Identity || text.is_empty() || text == "-" {
                    continue;
                }
                let tone = row.cells.get(at).map(|cell| cell.tone).unwrap_or_default();
                let style = if row.faint {
                    super::FAINT
                } else {
                    super::style_of(tone)
                };
                out.push_str("  ");
                out.push_str(&super::paint(&column.header, super::FAINT, caps));
                out.extend(std::iter::repeat_n(
                    ' ',
                    label.saturating_sub(text::width(&column.header)) + 2,
                ));
                out.push_str(&super::paint(text, style, caps));
                out.push('\n');
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(width: Option<usize>, color: bool) -> Caps {
        Caps {
            human: true,
            color,
            width,
        }
    }

    /// Agents as `ps` would list them: an identity, a name that may be
    /// shortened, a state with a tone, and three things a narrow terminal
    /// can do without.
    fn agents() -> Table {
        let mut table = Table::new(vec![
            Column::identity("AGENT ID"),
            Column::always("NAME").cut_to(6),
            Column::always("STATUS"),
            Column::always("DOING").cut_to(8),
            Column::optional("MODEL", 2),
            Column::optional("PID", 3),
            Column::optional("CREATED", 1),
        ]);
        table.row(vec![
            "aaaaaaaaaaaa".into(),
            "writer".into(),
            Cell::toned("running", Tone::Positive),
            "editing the router's error type".into(),
            "fable".into(),
            "4242".into(),
            "2m ago".into(),
        ]);
        table.row(vec![
            "bbbbbbbbbbbb".into(),
            "reviewer-of-everything".into(),
            Cell::toned("failed", Tone::Alert),
            "-".into(),
            "-".into(),
            "-".into(),
            "3h ago".into(),
        ]);
        table
    }

    /// With room, and with no width to fit, the table is every column
    /// and every cell whole, and no row ends in spaces.
    #[test]
    fn with_room_nothing_is_left_out_or_shortened() {
        let wide = "\
AGENT ID       NAME                     STATUS    DOING                             MODEL   PID    CREATED
aaaaaaaaaaaa   writer                   running   editing the router's error type   fable   4242   2m ago
bbbbbbbbbbbb   reviewer-of-everything   failed    -                                 -       -      3h ago
";
        assert_eq!(agents().render(caps(Some(160), false)), wide);
        assert_eq!(
            agents().render(caps(Some(106), false)),
            wide,
            "exactly fits"
        );
        assert_eq!(agents().render(caps(None, false)), wide, "no width known");
        assert!(wide.lines().all(|line| line == line.trim_end()));
    }

    /// A narrower terminal first does without what it can, least needed
    /// first, and says what it left out, in the table's own order.
    #[test]
    fn a_narrow_terminal_does_without_the_least_needed_first() {
        assert_eq!(
            agents().render(caps(Some(100), false)),
            "\
AGENT ID       NAME                     STATUS    DOING                             MODEL   CREATED
aaaaaaaaaaaa   writer                   running   editing the router's error type   fable   2m ago
bbbbbbbbbbbb   reviewer-of-everything   failed    -                                 -       3h ago
not shown at this width: PID (`--format plain` has every column)
"
        );
        assert_eq!(
            agents().render(caps(Some(95), false)),
            "\
AGENT ID       NAME                     STATUS    DOING                             CREATED
aaaaaaaaaaaa   writer                   running   editing the router's error type   2m ago
bbbbbbbbbbbb   reviewer-of-everything   failed    -                                 3h ago
not shown at this width: MODEL, PID (`--format plain` has every column)
"
        );
    }

    /// Narrower still, what may be shortened is, the widest first, and
    /// the identity is whole at any width: a row always says what it is
    /// a row of.
    #[test]
    fn then_the_widest_is_shortened_and_the_identity_never() {
        let narrow = agents().render(caps(Some(60), false));
        assert_eq!(
            narrow,
            "\
AGENT ID       NAME               STATUS    DOING
aaaaaaaaaaaa   writer             running   editing the rou…
bbbbbbbbbbbb   reviewer-of-eve…   failed    -
not shown at this width: MODEL, PID, CREATED (`--format plain` has every column)
"
        );
        for line in narrow.lines().take(3) {
            assert!(text::width(line) <= 60, "{line:?}");
        }
    }

    /// On a terminal too narrow for even the least each column allows,
    /// the table gives way to a block a row: the identity on its own
    /// line, every field labelled, nothing shortened and nothing left
    /// out, and what says nothing not listed.
    #[test]
    fn too_narrow_for_a_table_is_a_block_a_row() {
        let stacked = "\
aaaaaaaaaaaa
  NAME     writer
  STATUS   running
  DOING    editing the router's error type
  MODEL    fable
  PID      4242
  CREATED  2m ago
bbbbbbbbbbbb
  NAME     reviewer-of-everything
  STATUS   failed
  CREATED  3h ago
";
        assert_eq!(agents().render(caps(Some(20), false)), stacked);
        assert_eq!(agents().render(caps(Some(41), false)), stacked);
        // One column more and the table fits again, cut to its least.
        let fits = agents().render(caps(Some(42), false));
        assert!(
            fits.starts_with("AGENT ID       NAME     STATUS    DOING\n"),
            "{fits}"
        );
        // The state keeps its tone, and the labels are set back.
        let coloured = agents().render(caps(Some(20), true));
        assert!(
            coloured.contains("\u{1b}[2mSTATUS\u{1b}[0m   \u{1b}[32mrunning\u{1b}[0m"),
            "{coloured:?}"
        );
        assert!(
            coloured.starts_with("\u{1b}[1maaaaaaaaaaaa\u{1b}[0m\n"),
            "{coloured:?}"
        );
    }

    /// Colour is the state's tone and the heading's weight, and only
    /// when asked for: without it not one escape is written, and with it
    /// the cells line up exactly as without.
    #[test]
    fn colour_adds_escapes_and_moves_nothing() {
        let plain = agents().render(caps(Some(160), false));
        assert!(!plain.contains('\u{1b}'));
        let coloured = agents().render(caps(Some(160), true));
        assert!(
            coloured.contains("\u{1b}[32mrunning\u{1b}[0m"),
            "{coloured:?}"
        );
        assert!(
            coloured.contains("\u{1b}[1m\u{1b}[31mfailed\u{1b}[0m"),
            "{coloured:?}"
        );
        assert!(
            coloured.starts_with("\u{1b}[1mAGENT ID\u{1b}[0m   "),
            "{coloured:?}"
        );
        assert_eq!(strip(&coloured), plain);
    }

    /// A row set back is faint as a whole, and its state's tone is not
    /// laid over that.
    #[test]
    fn a_row_set_back_is_faint_whole() {
        let mut table = agents();
        table.rows[1].faint = true;
        let coloured = table.render(caps(Some(160), true));
        assert!(
            coloured.contains("\u{1b}[2mfailed\u{1b}[0m"),
            "{coloured:?}"
        );
        assert!(
            coloured.contains("\u{1b}[2mbbbbbbbbbbbb\u{1b}[0m"),
            "{coloured:?}"
        );
        assert_eq!(strip(&coloured), agents().render(caps(Some(160), false)));
    }

    /// A cell is somebody's data: what a terminal would act on in it is
    /// written out before it is measured, so it can neither run nor push
    /// the columns after it out of line; and a wide glyph takes the two
    /// columns it is drawn in.
    #[test]
    fn a_cell_cannot_run_or_misalign() {
        let mut table = Table::new(vec![
            Column::identity("CARD"),
            Column::always("TITLE").cut_to(8),
            Column::always("HOLDER"),
        ]);
        table.row(vec![
            "AD-1".into(),
            "clear\u{1b}[2J the screen".into(),
            "writer".into(),
        ]);
        table.row(vec!["AD-2".into(), "作者 writes".into(), "-".into()]);
        table.row(vec!["AD-3".into(), "".into(), "".into()]);
        let said = table.render(caps(Some(80), true));
        assert_eq!(said.matches('\u{1b}').count(), 2 * 3, "the heading's only");
        assert_eq!(
            strip(&said),
            "\
CARD   TITLE                   HOLDER
AD-1   clear\\e[2J the screen   writer
AD-2   作者 writes             -
AD-3
"
        );
    }

    /// A table with no rows is its heading; one with no columns is an
    /// empty line, and neither panics at any width.
    #[test]
    fn empty_tables_are_what_they_are() {
        let empty = Table::new(vec![Column::identity("CARD"), Column::always("TITLE")]);
        assert_eq!(empty.render(caps(Some(80), false)), "CARD   TITLE\n");
        assert_eq!(Table::default().render(caps(Some(0), false)), "\n");
        for width in 0..40 {
            let _ = agents().render(caps(Some(width), true));
        }
    }

    /// What is printed, without its escapes.
    fn strip(text: &str) -> String {
        let mut out = String::new();
        let mut rest = text;
        while let Some(start) = rest.find('\u{1b}') {
            out.push_str(&rest[..start]);
            let end = rest[start..]
                .find('m')
                .map_or(rest.len(), |m| start + m + 1);
            rest = &rest[end..];
        }
        out.push_str(rest);
        out
    }
}
