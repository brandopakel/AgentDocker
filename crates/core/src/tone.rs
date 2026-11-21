//! How a state reads at a glance: one of five tones, the same on every
//! surface.
//!
//! A tone is not a colour. The app gives each a colour of its palette and
//! the terminal one of its own, and neither is known here: nothing in a
//! tone is stored, sent, or asked of a terminal. What is said once, here,
//! is which tone a state has, so that the app and the terminal cannot
//! colour one state two ways.
//!
//! Five, so that the colour adds weight to the word without having to be
//! learnt: the word always says it. What is not known is [`Tone::Neutral`]
//! and never [`Tone::Positive`]: an answer that did not come must not look
//! like a good one.
//!
//! Attention that depends on who is looking (a message new to this
//! reader, a card that is theirs) is each surface's own and is not here.

use crate::AgentStatus;
use crate::task::Column;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Tone {
    /// Quiet: idle, ended, a draft, nothing known.
    #[default]
    Neutral,
    /// Healthy: receiving, delivered, passed, done, live.
    Positive,
    /// Waits on the person, or something stands in the way: needs you,
    /// paused, time running.
    Attention,
    /// Failed, overdue, nearly out of time.
    Alert,
    /// News, or on its way: finished since you looked, sent, ready, in
    /// progress.
    Info,
}

/// A lease with less than this left is said to be running out.
pub const LEASE_RUNNING_OUT_SECS: i64 = 60;

impl Tone {
    /// A lane's, by its category: quiet before and after the work, on its
    /// way while it is ready or under way, attention in review, positive
    /// done, alert blocked.
    pub fn of_lane(kind: Column) -> Tone {
        match kind {
            Column::Backlog | Column::Cancelled => Tone::Neutral,
            Column::Ready | Column::InProgress => Tone::Info,
            Column::Review => Tone::Attention,
            Column::Done => Tone::Positive,
            Column::Blocked => Tone::Alert,
        }
    }

    /// An agent's, by its record's own status: on its way while created,
    /// live while running, attention while a stop is not yet seen to have
    /// ended it or when it ended with a status that was not zero, quiet
    /// once ended cleanly or with no status known, alert failed.
    pub fn of_status(status: &AgentStatus) -> Tone {
        match status {
            AgentStatus::Created => Tone::Info,
            AgentStatus::Running => Tone::Positive,
            AgentStatus::Stopping => Tone::Attention,
            AgentStatus::Exited {
                code: None | Some(0),
            } => Tone::Neutral,
            AgentStatus::Exited { code: Some(_) } => Tone::Attention,
            AgentStatus::Failed { .. } => Tone::Alert,
        }
    }

    /// A lease's, by the seconds it has left: alert once expired,
    /// attention while it is running out, quiet otherwise. A lease that
    /// is held is the ordinary case and is not good news.
    pub fn of_lease_left(seconds: i64) -> Tone {
        if seconds <= 0 {
            Tone::Alert
        } else if seconds < LEASE_RUNNING_OUT_SECS {
            Tone::Attention
        } else {
            Tone::Neutral
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pilot's states, each with its tone, in one table: what the
    /// terminal and the app both read.
    #[test]
    fn each_state_has_one_tone() {
        let lanes = [
            (Column::Backlog, Tone::Neutral),
            (Column::Ready, Tone::Info),
            (Column::InProgress, Tone::Info),
            (Column::Review, Tone::Attention),
            (Column::Done, Tone::Positive),
            (Column::Blocked, Tone::Alert),
            (Column::Cancelled, Tone::Neutral),
        ];
        for (lane, tone) in lanes {
            assert_eq!(Tone::of_lane(lane), tone, "{lane:?}");
        }
        let statuses = [
            (AgentStatus::Created, Tone::Info),
            (AgentStatus::Running, Tone::Positive),
            (AgentStatus::Stopping, Tone::Attention),
            (AgentStatus::Exited { code: Some(0) }, Tone::Neutral),
            (AgentStatus::Exited { code: None }, Tone::Neutral),
            (AgentStatus::Exited { code: Some(130) }, Tone::Attention),
            (
                AgentStatus::Failed {
                    reason: "it could not be started".into(),
                },
                Tone::Alert,
            ),
        ];
        for (status, tone) in statuses {
            assert_eq!(Tone::of_status(&status), tone, "{status:?}");
        }
        for (left, tone) in [
            (-5, Tone::Alert),
            (0, Tone::Alert),
            (1, Tone::Attention),
            (59, Tone::Attention),
            (60, Tone::Neutral),
            (7200, Tone::Neutral),
        ] {
            assert_eq!(Tone::of_lease_left(left), tone, "{left}s");
        }
    }

    /// What is not known never looks like success: the tone of nothing
    /// said is quiet, and an ending whose status nobody knows is quiet
    /// too, not good.
    #[test]
    fn nothing_known_is_quiet_and_never_good() {
        assert_eq!(Tone::default(), Tone::Neutral);
        assert_ne!(
            Tone::of_status(&AgentStatus::Exited { code: None }),
            Tone::Positive
        );
    }
}
