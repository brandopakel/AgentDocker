//! How a question ends, every way it can. A [`Closing`] gathers what ending
//! some questions takes: the events, a held message's first and the
//! question's own after it; the documents its commit deletes; and the
//! memory it clears once that commit lands, every side collection a
//! question has (its record, a refused default answer, a held message).
//! A waiting `ask` keeps its own collections (`question_waiters`,
//! `held_answers`): the Waiter that owns them drains them, so a waiting
//! `ask` is still told how its question ended. The publish path closes a
//! question its message answers, or the card a released message rode on,
//! in the message's own transaction, and clears the same memory with
//! [`Closing::forget`].
use super::*;
use agentdocker_core::{CloseReason, HeldMessage};

/// `held_message_dropped.by` for a held message found at startup whose
/// question was already gone: nothing says how that question ended, so
/// the drop is attributed to the daemon's recovery, not to a cause.
pub(super) const RECOVERY: &str = "recovery";
/// `held_message_dropped.by` when the question it rode on was withdrawn
/// by its stale rule.
pub(super) const WITHDRAWAL: &str = "withdrawal";

/// How one question ends, for [`Closing::end`].
pub(super) enum Ending {
    /// Closed without an answer, for this reason.
    Closed(CloseReason),
    /// Its asker cancelled it (`question_cancelled`).
    Cancelled(AgentId),
}

/// What ending some questions takes, built before the commit and applied
/// to memory only once it lands.
#[derive(Default)]
pub(super) struct Closing {
    /// The questions it ends.
    questions: Vec<MessageId>,
    /// The questions whose held message it drops.
    held: Vec<MessageId>,
    /// In the order they are said.
    events: Vec<EventKind>,
}

impl Closing {
    /// End `question`. A held message it carries is dropped first, by
    /// `by`, then the question closes or is cancelled.
    pub(super) fn end(
        &mut self,
        question: &MessageId,
        held: Option<&HeldMessage>,
        ending: Ending,
        by: &str,
    ) {
        if let Some(held) = held {
            self.drop_held(held, by);
        }
        self.events.push(match ending {
            Ending::Closed(reason) => EventKind::QuestionClosed {
                question: question.clone(),
                answer: None,
                reason: Some(reason),
            },
            Ending::Cancelled(agent) => EventKind::QuestionCancelled {
                question: question.clone(),
                agent,
            },
        });
        self.questions.push(question.clone());
    }

    /// Drop a held message whose question is already gone. No question
    /// closes: nothing is known of how it ended.
    pub(super) fn orphan(&mut self, held: &HeldMessage) {
        self.drop_held(held, RECOVERY);
    }

    fn drop_held(&mut self, held: &HeldMessage, by: &str) {
        self.events.push(EventKind::HeldMessageDropped {
            message: held.envelope.id.clone(),
            question: held.question.clone(),
            by: by.to_owned(),
        });
        self.held.push(held.question.clone());
    }

    pub(super) fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Its events, after `before` (a `question_stale` that explains the
    /// ending goes before it), all at `at`.
    pub(super) fn events(&self, before: Vec<EventKind>, at: DateTime<Utc>) -> Vec<Event> {
        before
            .into_iter()
            .chain(self.events.iter().cloned())
            .map(|kind| Event::new(kind, at))
            .collect()
    }

    /// The documents its commit deletes: each question's, and each
    /// dropped held message's.
    pub(super) fn deletes(&self) -> Vec<(&'static str, String)> {
        self.questions
            .iter()
            .map(|question| ("question", question.as_str().to_owned()))
            .chain(
                self.held
                    .iter()
                    .map(|question| (inbound::DOCUMENT, question.as_str().to_owned())),
            )
            .collect()
    }

    /// Clear what it ended from memory, once its commit has landed. Also
    /// what a fresh open clears from the maps it loaded, before there is a
    /// State.
    pub(super) fn apply(
        &self,
        questions: &mut HashMap<MessageId, agentdocker_core::Question>,
        held: &mut HashMap<MessageId, HeldMessage>,
        blocked_defaults: &mut HashSet<MessageId>,
    ) {
        for question in &self.questions {
            Self::forget(question, false, questions, held, blocked_defaults);
        }
        for question in &self.held {
            held.remove(question);
        }
    }

    /// Clear one ended question from memory: its record, its held message
    /// and a refused default answer. `by_default` says the daemon's own
    /// default just answered it, which is what a refused default was
    /// waiting to do.
    pub(super) fn forget(
        question: &MessageId,
        by_default: bool,
        questions: &mut HashMap<MessageId, agentdocker_core::Question>,
        held: &mut HashMap<MessageId, HeldMessage>,
        blocked_defaults: &mut HashSet<MessageId>,
    ) {
        questions.remove(question);
        held.remove(question);
        if blocked_defaults.remove(question) {
            if by_default {
                info!(%question, "stale default answer published after it was refused");
            } else {
                info!(%question, "question closed before its refused stale default answer could be published");
            }
        }
    }
}

impl State {
    /// End these questions in one commit, `before` said ahead of them.
    /// Memory follows the commit; a failed or fenced write leaves it as it
    /// was.
    pub(super) fn close(
        &mut self,
        what: &str,
        before: Vec<EventKind>,
        closing: Closing,
        at: DateTime<Utc>,
    ) -> Persisted {
        if closing.is_empty() {
            return Persisted::Committed;
        }
        let events = closing.events(before, at);
        let delete = closing.deletes();
        self.commit(
            what,
            events,
            |store, events| store.documents_change(&[], &delete, events),
            |state, _, ()| {
                let State {
                    questions,
                    held_messages,
                    blocked_defaults,
                    ..
                } = state;
                closing.apply(questions, held_messages, blocked_defaults);
            },
        )
    }

    /// Drop every held message whose question is gone, attributed to
    /// recovery: what a fenced open leaves for the successor's first act
    /// after it accepts, since only then may it write.
    pub(super) fn drop_orphaned_held(&mut self) {
        let mut orphans: Vec<&HeldMessage> = self
            .held_messages
            .values()
            .filter(|held| !self.questions.contains_key(&held.question))
            .collect();
        if orphans.is_empty() {
            return;
        }
        orphans.sort_by(|a, b| a.question.cmp(&b.question));
        let mut closing = Closing::default();
        for held in orphans {
            closing.orphan(held);
        }
        let _ = self.close("held message recovery", Vec::new(), closing, Utc::now());
    }
}
