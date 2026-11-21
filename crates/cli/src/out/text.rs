//! Text as a terminal shows it: made safe to print, measured in columns,
//! and cut where a reader's eye would cut it.
//!
//! A name, a title or a note is somebody's data. Printed as it is, a
//! control character in it moves the cursor, clears the line or recolours
//! what follows, and a direction mark reorders it; and a string's length
//! in bytes or in characters is not the room it takes, which is what a
//! column needs to know.

use std::borrow::Cow;

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// What marks a cut.
pub const ELLIPSIS: &str = "…";

/// Marks that change the direction text is laid out in, which a name can
/// use to read as another name. They take no room and are not controls.
fn reorders(c: char) -> bool {
    matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// `text` with nothing in it a terminal would act on: each control
/// character, and each mark that reorders text, is written out as the
/// escape that names it. Text with none is handed back as it is.
pub fn shown(text: &str) -> Cow<'_, str> {
    if !text.chars().any(|c| c.is_control() || reorders(c)) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{1b}' => out.push_str("\\e"),
            c if c.is_control() || reorders(c) => {
                out.push_str(&format!("\\u{{{:x}}}", u32::from(c)));
            }
            c => out.push(c),
        }
    }
    Cow::Owned(out)
}

/// The columns `text` takes: by what a reader sees as one character (a
/// letter with its accents, an emoji joined of several), each as wide as
/// a terminal draws it.
pub fn width(text: &str) -> usize {
    text.graphemes(true).map(UnicodeWidthStr::width).sum()
}

/// `text` in at most `max` columns: whole when it fits, and otherwise its
/// beginning and an ellipsis, cut between what a reader sees as two
/// characters and never inside one. With no room at all, nothing.
pub fn cut(text: &str, max: usize) -> Cow<'_, str> {
    if width(text) <= max {
        return Cow::Borrowed(text);
    }
    if max == 0 {
        return Cow::Borrowed("");
    }
    let room = max - width(ELLIPSIS).min(max);
    let mut out = String::new();
    let mut used = 0;
    for piece in text.graphemes(true) {
        let wide = piece.width();
        if used + wide > room {
            break;
        }
        out.push_str(piece);
        used += wide;
    }
    out.push_str(ELLIPSIS);
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Nothing a terminal would act on is sent: a control character and a
    /// mark that reorders text are written out, and ordinary text is not
    /// touched (nor copied).
    #[test]
    fn what_a_terminal_would_act_on_is_written_out() {
        assert!(matches!(shown("writer"), Cow::Borrowed("writer")));
        assert!(matches!(shown("naïve 作者 👩‍💻"), Cow::Borrowed(_)));
        assert_eq!(shown("a\x1b[2Jb"), "a\\e[2Jb");
        assert_eq!(shown("one\ntwo\tthree\r"), "one\\ntwo\\tthree\\r");
        assert_eq!(shown("bell\x07 del\x7f"), "bell\\u{7} del\\u{7f}");
        assert_eq!(shown("c1\u{9b}31m"), "c1\\u{9b}31m");
        // A right-to-left override makes "exe.txt" read as "txt.exe".
        assert_eq!(shown("a\u{202e}txt.exe"), "a\\u{202e}txt.exe");
        assert_eq!(shown("\u{2066}x\u{2069}"), "\\u{2066}x\\u{2069}");
        // What joins an emoji is not a control and stays.
        assert!(!shown("👩‍💻").contains('\\'));
    }

    /// Room is counted as a terminal draws it: a wide glyph is two
    /// columns, an accent on its letter none, a joined emoji one glyph.
    #[test]
    fn room_is_what_a_terminal_draws() {
        assert_eq!(width(""), 0);
        assert_eq!(width("writer"), 6);
        assert_eq!(width("作者"), 4);
        assert_eq!(width("e\u{301}"), 1, "a letter and its combining accent");
        assert_eq!(width("👩‍💻"), 2, "one emoji joined of two");
        assert_eq!(width(ELLIPSIS), 1);
    }

    /// A cut keeps to the room it is given, says it cut, and never splits
    /// what a reader sees as one character.
    #[test]
    fn a_cut_keeps_to_its_room_and_splits_no_character() {
        assert!(matches!(cut("writer", 6), Cow::Borrowed("writer")));
        assert_eq!(cut("reviewer", 6), "revie…");
        assert_eq!(cut("reviewer", 1), "…");
        assert_eq!(cut("reviewer", 0), "");
        // A wide glyph that would straddle the edge is left out whole.
        assert_eq!(cut("作者作者", 4), "作…");
        assert_eq!(cut("作者作者", 5), "作者…");
        // The accent stays with its letter, on either side of the cut.
        assert_eq!(cut("e\u{301}e\u{301}e\u{301}", 2), "e\u{301}…");
        // A joined emoji is kept whole or left out, never halved.
        assert_eq!(cut("ab👩‍💻cd", 4), "ab…");
        assert_eq!(cut("ab👩‍💻cd", 5), "ab👩‍💻…");
        for max in 0..12 {
            for text in ["reviewer", "作者作者", "ab👩‍💻cd", "e\u{301}e\u{301}"] {
                assert!(width(&cut(text, max)) <= max, "{text:?} in {max}");
            }
        }
    }
}
