//! The human as an agent: `me`, `ask`, `answer`, and the notification
//! that makes a question visible.
//!
//! Orchestration needs an escalation path, and the cheapest correct place
//! for it is inside the model that already exists. The person at the
//! keyboard registers as an agent named `user` with runtime `human`. From
//! then on they are addressable like anything else — messages queue in
//! their inbox, `watch` streams them, the journal keeps their cursor —
//! and the one thing that is genuinely different, that a person is not
//! polling a socket, is handled by a desktop notification.
//!
//! `ask` is the blocking half. An agent that needs a decision sends a
//! `question` and waits on the same connection until an `answer` naming
//! it comes back, or until it gives up. The daemon keeps the outstanding
//! questions because an answer names a question by id, and only the
//! question knows who is waiting on it.

use std::time::Duration as StdDuration;

use super::*;
use agentdocker_core::{HUMAN, HUMAN_RUNTIME, Question};
use agentdocker_host::notify::{self, Notification};

/// Enough outstanding questions that a busy fleet is never refused, few
/// enough that a client looping on `ask` cannot grow the daemon without
/// bound. Expired questions are dropped before this is consulted.
const MAX_QUESTIONS: usize = 512;

/// One notification per sender per minute. A person being told something
/// is told it once; the message itself is never dropped, only the
/// interruption.
const NOTIFY_EVERY: StdDuration = StdDuration::from_secs(60);

/// How much of a message a notification shows.
const NOTICE_CHARS: usize = 180;

/// A pending notification. Built under the state lock and handed off
/// without blocking; posting it is another thread's problem.
#[derive(Clone, Debug)]
pub struct Notice {
    pub from: String,
    pub kind: String,
    pub text: String,
}

impl State {
    /// A message was routed. If any recipient is a person, ask for a
    /// notification — `try_send` so a full or absent channel costs
    /// nothing and never blocks the state lock.
    pub(super) fn notify_humans(&self, envelope: &Envelope, recipients: &[AgentId]) {
        let Some(notifier) = &self.notifier else {
            return;
        };
        if !recipients
            .iter()
            .any(|id| self.registry.get(id).is_some_and(is_human))
        {
            return;
        }
        let _ = notifier.try_send(Notice {
            from: self.display_name(&envelope.from),
            kind: envelope.kind.clone(),
            text: message_text(&envelope.payload),
        });
    }

    /// An agent's name where it has one, else whatever the sender called
    /// itself: `agentd` and `user` also send, and neither is looked up.
    fn display_name(&self, from: &str) -> String {
        self.registry
            .get(&AgentId::from(from))
            .map(|a| a.spec.name.clone())
            .unwrap_or_else(|| from.to_owned())
    }

    /// Remember a question so its answer can find the asker.
    fn remember_question(&mut self, question: Question) {
        let now = question.asked_at;
        self.questions.retain(|_, q| !q.expired(now));
        while self.questions.len() >= MAX_QUESTIONS {
            // Drop the one that expires soonest: it is the closest to
            // being nobody's business anyway.
            let Some(soonest) = self
                .questions
                .values()
                .min_by_key(|q| q.expires_at)
                .map(|q| q.id.clone())
            else {
                break;
            };
            self.questions.remove(&soonest);
        }
        self.questions.insert(question.id.clone(), question);
    }

    /// The questions still waiting, newest first.
    fn open_questions(&mut self, agent: Option<&AgentId>) -> Vec<Question> {
        let now = Utc::now();
        self.questions.retain(|_, q| !q.expired(now));
        let mut questions: Vec<Question> = self
            .questions
            .values()
            .filter(|q| agent.is_none_or(|id| q.addressed_to(id)))
            .cloned()
            .collect();
        questions.sort_by_key(|q| std::cmp::Reverse(q.asked_at));
        questions
    }
}

/// Whether a record is a person rather than a program.
pub fn is_human(record: &AgentRecord) -> bool {
    record.spec.runtime == HUMAN_RUNTIME
}

/// The text of a message payload. `{"text": "..."}` is the convention;
/// anything else is shown as the JSON it is.
fn message_text(payload: &Value) -> String {
    match payload.get("text").and_then(Value::as_str) {
        Some(text) => text.to_owned(),
        None => payload.to_string(),
    }
}

/// The title a notification carries, given who sent it and what kind of
/// message it was.
fn title(from: &str, kind: &str) -> String {
    match kind {
        "question" => format!("{from} asks"),
        "stale" => format!("{from}: your context is stale"),
        "handoff" => format!("{from} handed over"),
        _ => format!("{from} says"),
    }
}

/// Post notifications off the state lock, at most one per sender per
/// minute. Runs until the daemon drops its sender.
pub async fn notifier(mut notices: mpsc::Receiver<Notice>) {
    let mut last: HashMap<String, Instant> = HashMap::new();
    while let Some(notice) = notices.recv().await {
        let now = Instant::now();
        if last
            .get(&notice.from)
            .is_some_and(|at| now.duration_since(*at) < NOTIFY_EVERY)
        {
            continue;
        }
        last.insert(notice.from.clone(), now);
        // Forget senders that have gone quiet, so a long-lived daemon does
        // not keep an entry for every agent that ever spoke.
        last.retain(|_, at| now.duration_since(*at) < NOTIFY_EVERY * 10);

        let notification = Notification {
            title: title(&notice.from, &notice.kind),
            body: notify::summarise(&notice.text, NOTICE_CHARS),
        };
        // Posting spawns a process and waits for it; keep that off the
        // runtime's worker threads.
        let posted = tokio::task::spawn_blocking(move || notify::post(&notification)).await;
        if matches!(posted, Ok(false)) {
            debug!("no desktop notifier on this machine; the message is queued as usual");
        }
    }
}

impl Daemon {
    /// `me`: register the person at the keyboard, or hand back the record
    /// they already have. Idempotent, because it is what a shell profile
    /// or an app launch runs every time.
    pub(super) async fn me(self: &Arc<Self>, workdir: Option<PathBuf>) -> Response {
        let existing = {
            let state = lock(&self.state);
            state
                .registry
                .live()
                .find(|a| is_human(a))
                .map(|a| a.id.clone())
        };
        let Some(id) = existing else {
            let spec = AgentSpec {
                name: HUMAN.to_owned(),
                runtime: HUMAN_RUNTIME.to_owned(),
                workdir,
                ..AgentSpec::default()
            };
            // No pid: a person is not a process, so liveness has nothing
            // to check and the record is never expired.
            return self.register(spec, None).await;
        };
        // Follow the person to wherever they are now. A human moves
        // between projects far more often than an agent does, and the
        // journal digest they are shown depends on which project it is.
        if let Some(workdir) = workdir {
            let project = self.project_for(Some(workdir.clone()), true).await;
            let vcs = Self::vcs_for(Some(workdir.clone())).await;
            let mut state = lock(&self.state);
            if let Some(record) = state.registry.get_mut(&id) {
                record.spec.workdir = Some(workdir);
                record.project = project;
                record.vcs = vcs;
                let record = record.clone();
                state.persist("agent", |store| store.upsert_agent(&record));
            }
        }
        let mut state = lock(&self.state);
        state.registry.touch(&id, Utc::now());
        match state.registry.get(&id) {
            Some(record) => Response::Agent {
                agent: record.clone(),
            },
            None => Response::error(ErrorCode::NotFound, "the human agent is gone"),
        }
    }

    /// `ask`: send a question and wait on this connection for its answer.
    ///
    /// The answer is an ordinary message, so it also reaches the asker's
    /// inbox. That is deliberate: an `ask` that timed out still leaves the
    /// answer somewhere the asker can read it later.
    pub(super) async fn ask(
        self: &Arc<Self>,
        from: String,
        to: String,
        question: String,
        timeout_secs: u64,
    ) -> Response {
        if question.trim().is_empty() {
            return Response::error(ErrorCode::Invalid, "a question needs some text");
        }
        let timeout = StdDuration::from_secs(timeout_secs.clamp(1, 24 * 60 * 60));
        let (from, to) = match self.endpoints(from, &to).await {
            Ok(pair) => pair,
            Err(response) => return *response,
        };

        // Subscribe before sending, so an answer that arrives while the
        // question is still being routed cannot be missed.
        let mut answers = lock(&self.state).bus.subscribe();
        let asked_at = Utc::now();
        let sent = lock(&self.state).send(
            from.clone(),
            to.clone(),
            "question".to_owned(),
            json!({ "text": question }),
            None,
        );
        let Response::Sent { message, .. } = sent else {
            return sent;
        };
        lock(&self.state).remember_question(Question {
            id: message.clone(),
            from,
            to,
            text: question,
            asked_at,
            expires_at: asked_at + Duration::from_std(timeout).unwrap_or_else(|_| Duration::zero()),
        });

        let waited = tokio::time::timeout(timeout, async {
            loop {
                match answers.recv().await {
                    Ok(envelope) if envelope.reply_to.as_ref() == Some(&message) => {
                        return Some(envelope);
                    }
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return None,
                }
            }
        })
        .await;

        lock(&self.state).questions.remove(&message);
        match waited {
            Ok(Some(answer)) => Response::Answer {
                message: answer.id,
                from: answer.from,
                text: message_text(&answer.payload),
            },
            Ok(None) => Response::error(ErrorCode::Internal, "the message bus closed"),
            Err(_) => Response::error(
                ErrorCode::Timeout,
                format!("nobody answered {message} within {timeout_secs}s"),
            ),
        }
    }

    /// `answer`: reply to a question by its id. Who to reply to comes
    /// from the remembered question, not from the caller, so answering is
    /// the same one-argument act whether a person or an agent does it.
    pub(super) async fn answer(
        self: &Arc<Self>,
        from: Option<String>,
        message: MessageId,
        text: String,
    ) -> Response {
        let question = lock(&self.state).questions.get(&message).cloned();
        let Some(question) = question else {
            return Response::error(
                ErrorCode::NotFound,
                format!("no question {message} is waiting for an answer"),
            );
        };
        self.send(
            from.unwrap_or_else(|| HUMAN.to_owned()),
            &question.from,
            "answer".to_owned(),
            json!({ "text": text }),
            Some(message),
        )
        .await
    }

    /// `questions`: what is waiting, for whoever wants to answer it.
    pub(super) fn questions(self: &Arc<Self>, agent: Option<String>) -> Response {
        let mut state = lock(&self.state);
        let agent = match agent.as_deref() {
            Some(reference) => match state.resolve(reference) {
                Ok(id) => Some(id),
                Err(response) => return *response,
            },
            None => None,
        };
        Response::Questions {
            questions: state.open_questions(agent.as_ref()),
        }
    }
}
