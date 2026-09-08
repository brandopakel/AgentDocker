//! Bound OSC control strings before feeding the VT parser, across wire frames.
//!
//! vte 0.15's default `std` feature stores OSC data in an unbounded Vec. Its
//! 1024-byte generic limit applies only without `std`. Track its ESC/OSC entry
//! and termination rules here; other escape families keep fixed parser state.
//! This guard forwards accepted bytes unchanged and closes on excess data.

pub(super) const MAX_OSC_BYTES: usize = 64 * 1024;

#[derive(Default)]
enum State {
    #[default]
    Other,
    Escape,
    Intermediate,
    Osc(usize),
}

#[derive(Default)]
pub(super) struct ControlBudget {
    state: State,
}

impl ControlBudget {
    pub(super) fn check(&mut self, bytes: &[u8]) -> Result<(), &'static str> {
        for &byte in bytes {
            self.state = match (byte, &self.state) {
                (0x1b, _) => State::Escape,
                (0x18 | 0x1a, _) => State::Other,
                (b']', State::Escape) => State::Osc(0),
                (0x20..=0x2f, State::Escape) => State::Intermediate,
                (0x30..=0x7e, State::Escape | State::Intermediate) => State::Other,
                (_, State::Escape) => State::Escape,
                (_, State::Intermediate) => State::Intermediate,
                (0x07, State::Osc(_)) => State::Other,
                (_, State::Osc(size)) if *size == MAX_OSC_BYTES => {
                    return Err("terminal control string exceeds 64 KiB");
                }
                (_, State::Osc(size)) => State::Osc(size + 1),
                (_, State::Other) => State::Other,
            };
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unfinished_control_strings_cannot_accumulate_across_frames() {
        let mut budget = ControlBudget::default();
        budget.check(b"\x1b").unwrap();
        budget.check(b"]").unwrap();
        for _ in 0..64 {
            budget.check(&[b'x'; 1024]).unwrap();
        }
        assert!(budget.check(b"x").is_err());
    }

    #[test]
    fn termination_and_cancellation_reset_the_budget() {
        for end in [b"\x07".as_slice(), b"\x1b\\", b"\x18", b"\x1a"] {
            let mut budget = ControlBudget::default();
            for _ in 0..3 {
                budget.check(b"\x1b]0;").unwrap();
                budget.check(&vec![b'x'; MAX_OSC_BYTES - 2]).unwrap();
                budget.check(end).unwrap();
            }
        }
    }

    #[test]
    fn escape_controls_and_intermediates_follow_parser_entry_rules() {
        let mut budget = ControlBudget::default();
        // BEL/DEL do not leave the escape state. ESC-intermediate followed by
        // ']' dispatches an ordinary escape instead of starting an OSC string.
        budget.check(b"\x1b \x07]").unwrap();
        budget.check(&vec![b'x'; MAX_OSC_BYTES + 1]).unwrap();
        budget.check(b"\x1b\x07\x7f]").unwrap();
        assert!(budget.check(&vec![b'x'; MAX_OSC_BYTES + 1]).is_err());
    }

    #[test]
    fn ordinary_text_and_short_controls_pass_unchanged_across_chunks() {
        let text = b"hello \x1b[31mred\x1b[0m\x1b]0;owned fixture\x1b\\ end";
        let mut budget = ControlBudget::default();
        let mut guarded = vt100::Parser::new(24, 80, 0);
        let mut reference = vt100::Parser::new(24, 80, 0);
        reference.process(text);
        for bytes in text.chunks(3) {
            budget.check(bytes).unwrap();
            guarded.process(bytes);
        }
        assert_eq!(
            guarded.screen().contents_formatted(),
            reference.screen().contents_formatted()
        );
        for _ in 0..10 {
            budget.check(&vec![b'x'; MAX_OSC_BYTES]).unwrap();
        }
    }
}
