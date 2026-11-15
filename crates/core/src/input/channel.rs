//! Who delivers a Claude session's messages: the channel adapter that owns
//! the session, or the session's hooks. Pure: the caller says what it
//! found under the delivery gates (the owner locks free or held, and the
//! token a holder wrote) and what the daemon has of this provider
//! generation, and this says who may read the queue.
//!
//! The hooks deliver only on proof. Every owner lock free while they hold
//! every gate they can name is proof that no adapter serves the session,
//! and none can start serving before they let the gates go. A
//! receipt-missing pause written by the very instance that holds the lock
//! is proof that the owner stopped on that one message, which the hooks
//! may then take over. Nothing else lets them deliver: an owner whose
//! state is not proven, and ownership that could not be read, keep the
//! queue for the channel.

use chrono::{DateTime, Utc};

use super::{AdapterMark, InputDelivery, PauseKind, pause_kind};
use crate::MessageId;

/// How many hex characters an adapter instance's token has.
pub const TOKEN_LEN: usize = 32;

/// Whether `text` is an adapter instance's token: [`TOKEN_LEN`] lowercase
/// hex characters.
pub fn is_token(text: &str) -> bool {
    text.len() == TOKEN_LEN && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// What a held owner lock file says of the instance holding it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TokenEvidence {
    /// Nothing: an adapter from before instances were named.
    Empty,
    /// Exactly one token and a newline.
    Token(String),
    /// Anything else: no evidence.
    Malformed,
}

impl TokenEvidence {
    /// The evidence in an owner lock file's first bytes.
    pub fn read(bytes: &[u8]) -> Self {
        if bytes.is_empty() {
            return Self::Empty;
        }
        match std::str::from_utf8(bytes)
            .ok()
            .and_then(|text| text.strip_suffix('\n'))
        {
            Some(token) if is_token(token) => Self::Token(token.to_owned()),
            _ => Self::Malformed,
        }
    }
}

/// What a probe under the delivery gates found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ownership {
    /// An adapter holds an owner lock, and its file says this of it.
    Held(TokenEvidence),
    /// Every owner lock is free while the gates are held.
    Absent,
    /// A gate or an owner lock could not be read or taken in time.
    Unknown,
}

/// Who serves the session's input, from the hooks' side.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChannelState {
    /// No adapter serves it: the hooks deliver.
    Absent,
    /// An adapter holds it and has not said it is ready: the hooks hold.
    Starting,
    /// An adapter holds it and said it is ready or received input.
    Ready,
    /// The adapter holding it offered `message` and has no receipt: the
    /// hooks may deliver that message, and nothing behind it.
    Stalled { message: MessageId },
    /// Ownership could not be read: nobody can say who may deliver, so the
    /// hooks hold and say so.
    Unknown,
}

/// What the hooks may read from the queue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HookShare {
    All,
    /// Only this message, and only while it is the queue's head.
    Only(MessageId),
    Nothing,
}

impl ChannelState {
    pub fn hooks(&self) -> HookShare {
        match self {
            Self::Absent => HookShare::All,
            Self::Stalled { message } => HookShare::Only(message.clone()),
            Self::Starting | Self::Ready | Self::Unknown => HookShare::Nothing,
        }
    }
}

/// Who serves the input of the provider process born at `generation`,
/// given what the gates' probe found and the daemon's record of its
/// delivery. Only the record of exactly that generation counts.
pub fn channel_state(
    ownership: &Ownership,
    delivery: Option<&InputDelivery>,
    generation: Option<DateTime<Utc>>,
) -> ChannelState {
    let evidence = match ownership {
        Ownership::Absent => return ChannelState::Absent,
        Ownership::Unknown => return ChannelState::Unknown,
        Ownership::Held(evidence) => evidence,
    };
    let Some(delivery) =
        delivery.filter(|delivery| generation == Some(delivery.process_started_at))
    else {
        return ChannelState::Starting;
    };
    if !delivery.paused {
        return ChannelState::Ready;
    }
    let Some(PauseKind::ReceiptMissing { message, adapter }) =
        delivery.pause_reason.as_deref().map(pause_kind)
    else {
        // An exit pause, a failed start, another kind: words for the
        // person, not proof that the instance now holding the lock stopped.
        return ChannelState::Starting;
    };
    match (evidence, adapter) {
        (TokenEvidence::Token(held), AdapterMark::Token(wrote)) if *held == wrote => {
            ChannelState::Stalled { message }
        }
        // An adapter from before instances were named, holding a lock file
        // it never wrote, and its own unnamed pause.
        (TokenEvidence::Empty, AdapterMark::Unnamed) => ChannelState::Stalled { message },
        _ => ChannelState::Starting,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{
        PAUSE_CONTROLLER_ENDED, channel_not_started_reason, receipt_missing_reason,
    };

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";
    const OTHER: &str = "fedcba9876543210fedcba9876543210";

    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::<Utc>::UNIX_EPOCH + chrono::Duration::seconds(seconds)
    }

    fn report(generation: i64, pause: Option<String>) -> InputDelivery {
        InputDelivery {
            process_started_at: at(generation),
            paused: pause.is_some(),
            pause_reason: pause,
            reported_at: at(generation + 1),
            received: None,
            received_at: None,
            typing_until: None,
        }
    }

    fn message() -> MessageId {
        MessageId::from("ee72ad08c7df5911".to_owned())
    }

    #[test]
    fn a_token_is_exactly_one_line_of_lowercase_hex() {
        assert!(is_token(TOKEN));
        assert!(!is_token(&TOKEN.to_uppercase()));
        assert!(!is_token(&TOKEN[1..]));
        assert_eq!(TokenEvidence::read(b""), TokenEvidence::Empty);
        assert_eq!(
            TokenEvidence::read(format!("{TOKEN}\n").as_bytes()),
            TokenEvidence::Token(TOKEN.into())
        );
        for malformed in [
            TOKEN.as_bytes().to_vec(),
            format!("{TOKEN}\n\n").into_bytes(),
            format!("{TOKEN}\n{OTHER}\n").into_bytes(),
            b"\n".to_vec(),
            vec![0xff, b'\n'],
        ] {
            assert_eq!(TokenEvidence::read(&malformed), TokenEvidence::Malformed);
        }
    }

    /// Every row of the decision, with the generation matched exactly.
    #[test]
    fn the_hooks_deliver_only_on_proof() {
        let held = Ownership::Held(TokenEvidence::Token(TOKEN.into()));
        let old_owner = Ownership::Held(TokenEvidence::Empty);
        let stalled_by = |token: &str| Some(receipt_missing_reason(&message(), token));
        let unnamed = Some(format!(
            "{} {}; following messages remain queued.",
            crate::input::PAUSE_RECEIPT_MISSING,
            message()
        ));
        let stalled = ChannelState::Stalled { message: message() };
        let cases: Vec<(&str, &Ownership, Option<InputDelivery>, ChannelState)> = vec![
            ("no owner", &Ownership::Absent, None, ChannelState::Absent),
            (
                "no owner, a pause",
                &Ownership::Absent,
                Some(report(10, stalled_by(TOKEN))),
                ChannelState::Absent,
            ),
            (
                "unreadable",
                &Ownership::Unknown,
                None,
                ChannelState::Unknown,
            ),
            (
                "unreadable, its owner ready",
                &Ownership::Unknown,
                Some(report(10, None)),
                ChannelState::Unknown,
            ),
            (
                "unreadable, a failed start",
                &Ownership::Unknown,
                Some(report(10, Some(channel_not_started_reason("disk")))),
                ChannelState::Unknown,
            ),
            ("held, no word yet", &held, None, ChannelState::Starting),
            (
                "held, ready",
                &held,
                Some(report(10, None)),
                ChannelState::Ready,
            ),
            (
                "held, its own stall",
                &held,
                Some(report(10, stalled_by(TOKEN))),
                stalled.clone(),
            ),
            (
                "held, an earlier instance's stall",
                &held,
                Some(report(10, stalled_by(OTHER))),
                ChannelState::Starting,
            ),
            (
                "held, an unnamed stall",
                &held,
                Some(report(10, unnamed.clone())),
                ChannelState::Starting,
            ),
            (
                "held, a failed start",
                &held,
                Some(report(10, Some(channel_not_started_reason("disk")))),
                ChannelState::Starting,
            ),
            (
                "held, an exit pause",
                &held,
                Some(report(10, Some("stdin closed".into()))),
                ChannelState::Starting,
            ),
            (
                "held, another kind",
                &held,
                Some(report(10, Some(PAUSE_CONTROLLER_ENDED.into()))),
                ChannelState::Starting,
            ),
            (
                "held, an older generation's stall",
                &held,
                Some(report(9, stalled_by(TOKEN))),
                ChannelState::Starting,
            ),
            (
                "held, an older generation's ready",
                &held,
                Some(report(9, None)),
                ChannelState::Starting,
            ),
            (
                "old owner, its unnamed stall",
                &old_owner,
                Some(report(10, unnamed.clone())),
                stalled.clone(),
            ),
            (
                "old owner, a named stall",
                &old_owner,
                Some(report(10, stalled_by(TOKEN))),
                ChannelState::Starting,
            ),
            (
                "malformed file, a stall",
                &Ownership::Held(TokenEvidence::Malformed),
                Some(report(10, stalled_by(TOKEN))),
                ChannelState::Starting,
            ),
            (
                "malformed file, an unnamed stall",
                &Ownership::Held(TokenEvidence::Malformed),
                Some(report(10, unnamed)),
                ChannelState::Starting,
            ),
        ];
        for (name, ownership, delivery, expected) in cases {
            assert_eq!(
                channel_state(ownership, delivery.as_ref(), Some(at(10))),
                expected,
                "{name}"
            );
        }
        // No generation matches no record: ownership alone decides.
        let ready = report(10, None);
        assert_eq!(
            channel_state(&held, Some(&ready), None),
            ChannelState::Starting
        );
        assert_eq!(
            channel_state(&Ownership::Absent, Some(&ready), None),
            ChannelState::Absent
        );
        assert_eq!(
            channel_state(&Ownership::Unknown, None, None),
            ChannelState::Unknown
        );
    }

    #[test]
    fn only_absence_and_a_proven_stall_give_the_hooks_anything() {
        assert_eq!(ChannelState::Absent.hooks(), HookShare::All);
        assert_eq!(
            ChannelState::Stalled { message: message() }.hooks(),
            HookShare::Only(message())
        );
        for state in [
            ChannelState::Starting,
            ChannelState::Ready,
            ChannelState::Unknown,
        ] {
            assert_eq!(state.hooks(), HookShare::Nothing, "{state:?}");
        }
    }
}
