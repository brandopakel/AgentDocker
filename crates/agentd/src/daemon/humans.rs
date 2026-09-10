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
/// bound. Expired questions are dropped before this is consulted, and a
/// new question is refused rather than an old one evicted: entries may
/// have a caller blocked on them or reconnecting after a restart. Dropping
/// one would prevent its answer from finding the caller.
pub(super) const MAX_QUESTIONS: usize = 512;

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

    pub(super) fn expire_questions(&mut self, now: DateTime<Utc>) {
        let mut expired: Vec<_> = self
            .questions
            .values()
            .filter(|question| question.expired(now))
            .map(|question| question.id.clone())
            .collect();
        if expired.is_empty() || self.storage_error.is_some() {
            return;
        }
        expired.sort();
        let events: Vec<_> = expired
            .iter()
            .enumerate()
            .map(|(index, question)| {
                let mut event = Event::new(
                    EventKind::QuestionClosed {
                        question: question.clone(),
                        answer: None,
                    },
                    now,
                );
                event.seq = self.next_seq + index as u64;
                event
            })
            .collect();
        self.persist("question expiration", |store| {
            store.close_questions(&expired, &events)
        });
        if self.storage_error.is_some() {
            return;
        }
        for question in expired {
            self.questions.remove(&question);
        }
        self.next_seq += events.len() as u64;
        for event in events {
            let _ = self.events.send(event);
        }
    }

    /// The questions still waiting, newest first.
    fn open_questions(&mut self, agent: Option<&AgentId>) -> Vec<Question> {
        let now = Utc::now();
        self.expire_questions(now);
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
            let registered = self.register(spec, None, None).await;
            // Registering is not atomic with the look-up above — it does
            // project discovery in between — so two `me` calls can both
            // find nobody and both try. The second is refused for the
            // name, and the right answer to an idempotent call is the
            // record that won, not an error.
            if matches!(
                registered,
                Response::Error {
                    code: ErrorCode::NameTaken,
                    ..
                }
            ) {
                let state = lock(&self.state);
                if let Some(record) = state.registry.live().find(|a| is_human(a)) {
                    return Response::Agent {
                        agent: record.clone(),
                    };
                }
            }
            return registered;
        };
        // Follow the person to wherever they are now. A human moves
        // between projects far more often than an agent does, and the
        // journal digest they are shown depends on which project it is.
        //
        // Except to nowhere. The filesystem root is what a client that
        // has no working directory reports — an app started from a Dock
        // or a launcher inherits it — and taking it at face value moves
        // the person out of whatever project they were in and into a
        // "project" that is the whole disk. Keeping the record they had
        // is the better wrong answer of the two.
        let workdir = workdir.filter(|dir| dir.parent().is_some());
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
        // A question needs somebody who can answer it. A topic delivers
        // only to whoever happens to be subscribed and queues for nobody,
        // so a question put to one can sit unanswerable for its whole
        // timeout without ever appearing in `questions`.
        if !matches!(to, Destination::Agent(_) | Destination::Broadcast) {
            return Response::error(
                ErrorCode::Invalid,
                "ask needs one agent, or `all`; a question has to reach somebody who can answer it",
            );
        }

        // Subscribe before sending, so an answer that arrives while the
        // question is still being routed cannot be missed.
        let mut answers = lock(&self.state).bus.subscribe();
        let asked_at = Utc::now();
        // Built here rather than by `send`, so the question is recorded
        // under its own id *before* it goes out. Publishing first would
        // leave a window where a recipient answers a question the daemon
        // has not heard of, and the asker waits out its timeout.
        let envelope = Envelope::new(
            from.clone(),
            to.clone(),
            "question",
            json!({ "text": question }),
            None,
            asked_at,
        );
        let message = envelope.id.clone();
        let sent = {
            let mut state = lock(&self.state);
            let pending = Question {
                id: message.clone(),
                from: from.clone(),
                to,
                text: question,
                asked_at,
                expires_at: asked_at
                    + Duration::from_std(timeout).unwrap_or_else(|_| Duration::zero()),
            };
            state.publish_question(envelope, Some(pending))
        };
        if !matches!(sent, Response::Sent { .. }) {
            return sent;
        }

        let waited = tokio::time::timeout(timeout, async {
            loop {
                match answers.recv().await {
                    Ok(envelope) if envelope.reply_to.as_ref() == Some(&message)
                        && matches!(&envelope.to, Destination::Agent(id) if id.as_str() == from) => {
                        return Some(envelope);
                    }
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return None,
                }
            }
        })
        .await;

        lock(&self.state).expire_questions(Utc::now());
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
        let question = lock(&self.state)
            .questions
            .get(&message)
            .cloned()
            .filter(|question| !question.expired(Utc::now()));
        let Some(question) = question else {
            return Response::error(
                ErrorCode::NotFound,
                format!("no question {message} is waiting for an answer"),
            );
        };
        let (from, to) = match self
            .endpoints(from.unwrap_or_else(|| HUMAN.to_owned()), &question.from)
            .await
        {
            Ok(pair) => pair,
            Err(response) => return *response,
        };
        let mut state = lock(&self.state);
        state.expire_questions(Utc::now());
        if let Some(error) = state.storage_failure() {
            return error;
        }
        if !state.questions.contains_key(&message) {
            return Response::error(ErrorCode::NotFound, "the question was answered or expired");
        }
        let sender = AgentId::from(from.as_str());
        if state.registry.get(&sender).is_some() {
            let action = format!("send:{to}");
            let ruling = state.permits(&sender, &action);
            if !ruling.is_allowed() {
                return state.refuse(&sender, &action, ruling);
            }
        }
        state.send(
            from,
            to,
            "answer".to_owned(),
            json!({ "text": text }),
            Some(message),
        )
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// A daemon with nothing in it, for the pure parts of this module.
    fn state(dir: &TempDir) -> Arc<Daemon> {
        let home = dir.path().to_path_buf();
        Arc::new(Daemon::open(home.clone(), home.join("sock")).unwrap())
    }

    fn question(seconds: i64) -> Question {
        let now = Utc::now();
        Question {
            id: MessageId::generate(),
            from: "asker".to_owned(),
            to: Destination::Broadcast,
            text: "?".to_owned(),
            asked_at: now,
            expires_at: now + Duration::seconds(seconds),
        }
    }

    fn remember(state: &mut State, question: Question) -> bool {
        let mut envelope = Envelope::new(
            question.from.clone(),
            question.to.clone(),
            "question",
            json!({"text": question.text}),
            None,
            question.asked_at,
        );
        envelope.id = question.id.clone();
        matches!(
            state.publish_question(envelope, Some(question)),
            Response::Sent { .. }
        )
    }

    async fn register(daemon: &Arc<Daemon>, name: &str) -> AgentRecord {
        let Response::Agent { agent } = daemon
            .handle(Request::Register {
                spec: AgentSpec {
                    name: name.into(),
                    ..AgentSpec::default()
                },
                pid: None,
                session: None,
            })
            .await
        else {
            panic!("registration failed")
        };
        agent
    }

    #[tokio::test]
    async fn disconnected_questions_survive_restart_and_accept_exactly_one_answer() {
        let dir = TempDir::new().unwrap();
        let daemon = state(&dir);
        let asker = register(&daemon, "asker").await;
        register(&daemon, "recipient").await;
        let waiting = tokio::spawn({
            let daemon = daemon.clone();
            async move {
                daemon
                    .handle(Request::Ask {
                        from: "asker".into(),
                        to: "recipient".into(),
                        question: "continue after restart?".into(),
                        timeout_secs: 300,
                    })
                    .await
            }
        });
        let pending = tokio::time::timeout(StdDuration::from_secs(5), async {
            loop {
                if let Some(question) = lock(&daemon.state).questions.values().next().cloned() {
                    break question;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        waiting.abort();
        let _ = waiting.await;
        drop(daemon);

        let daemon = state(&dir);
        assert_eq!(
            lock(&daemon.state).open_questions(None),
            vec![pending.clone()]
        );
        let request = || Request::Answer {
            from: Some("recipient".into()),
            message: pending.id.clone(),
            text: "continue".into(),
        };
        let (first, second) = tokio::join!(daemon.handle(request()), daemon.handle(request()));
        assert_eq!(
            [&first, &second]
                .iter()
                .filter(|response| matches!(response, Response::Sent { .. }))
                .count(),
            1
        );
        assert_eq!(
            [&first, &second]
                .iter()
                .filter(|response| matches!(
                    response,
                    Response::Error {
                        code: ErrorCode::NotFound,
                        ..
                    }
                ))
                .count(),
            1
        );
        drop(daemon);

        let daemon = state(&dir);
        let state = lock(&daemon.state);
        assert!(state.questions.is_empty());
        assert!(
            state
                .store
                .documents::<Question>("question", None)
                .unwrap()
                .is_empty()
        );
        let replies = state.inboxes.get(&asker.id).unwrap();
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].reply_to, Some(pending.id));
        assert_eq!(replies[0].payload["text"], "continue");
    }

    #[tokio::test]
    async fn failed_question_publication_leaves_no_inbox_route_or_live_event() {
        for rejected in ["message_sent", "question_opened"] {
            let dir = TempDir::new().unwrap();
            let daemon = state(&dir);
            let recipient = register(&daemon, "recipient").await;
            let mut events = daemon.subscribe_events();
            let mut messages = lock(&daemon.state).bus.subscribe();
            let mut state = lock(&daemon.state);
            let before = state.store.max_event_seq().unwrap();
            state.store.reject_event_for_test(rejected);
            let mut pending = question(300);
            pending.to = Destination::Agent(recipient.id);
            assert!(!remember(&mut state, pending));
            assert!(state.storage_failure().is_some());
            assert!(state.questions.is_empty());
            assert!(state.inboxes.values().all(VecDeque::is_empty));
            assert!(state.store.load_inboxes().unwrap().is_empty());
            assert!(
                state
                    .store
                    .documents::<Question>("question", None)
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(state.store.max_event_seq().unwrap(), before);
            assert!(events.try_recv().is_err());
            assert!(messages.try_recv().is_err());
        }
    }

    #[tokio::test]
    async fn failed_answer_preserves_route_and_does_not_queue_an_uncommitted_reply() {
        let dir = TempDir::new().unwrap();
        let daemon = state(&dir);
        let asker = register(&daemon, "asker").await;
        let mut pending = question(300);
        pending.from = asker.id.to_string();
        assert!(remember(&mut lock(&daemon.state), pending.clone()));
        lock(&daemon.state)
            .store
            .reject_event_for_test("question_closed");
        let response = daemon
            .handle(Request::Answer {
                from: None,
                message: pending.id.clone(),
                text: "reply".into(),
            })
            .await;
        assert!(matches!(
            response,
            Response::Error {
                code: ErrorCode::StorageUnavailable,
                ..
            }
        ));
        drop(daemon);
        let daemon = state(&dir);
        let state = lock(&daemon.state);
        assert!(state.questions.contains_key(&pending.id));
        assert!(state.inboxes.get(&asker.id).is_none_or(VecDeque::is_empty));
    }

    #[test]
    fn expiration_is_durable_and_storage_failure_does_not_forget_the_route() {
        let dir = TempDir::new().unwrap();
        let daemon = state(&dir);
        let pending = question(300);
        assert!(remember(&mut lock(&daemon.state), pending.clone()));
        {
            let mut state = lock(&daemon.state);
            state.store.reject_event_for_test("question_closed");
            state.expire_questions(pending.expires_at);
            assert!(state.storage_failure().is_some());
            assert!(state.questions.contains_key(&pending.id));
            assert_eq!(
                state.store.documents::<Question>("question", None).unwrap(),
                vec![pending.clone()]
            );
        }
        drop(daemon);
        let daemon = state(&dir);
        lock(&daemon.state).expire_questions(pending.expires_at);
        assert!(lock(&daemon.state).questions.is_empty());
        drop(daemon);
        assert!(lock(&state(&dir).state).questions.is_empty());
    }

    #[test]
    fn restart_expires_old_questions_once() {
        let dir = TempDir::new().unwrap();
        let daemon = state(&dir);
        let pending = question(-1);
        assert!(remember(&mut lock(&daemon.state), pending.clone()));
        drop(daemon);
        let daemon = state(&dir);
        assert!(lock(&daemon.state).questions.is_empty());
        let count = daemon
            .recent_events(10)
            .iter()
            .filter(|event| {
                matches!(&event.kind,
            EventKind::QuestionClosed { question, answer: None } if question == &pending.id)
            })
            .count();
        assert_eq!(count, 1);
        drop(daemon);
        let daemon = state(&dir);
        assert_eq!(
            daemon
                .recent_events(10)
                .iter()
                .filter(|event| matches!(&event.kind,
            EventKind::QuestionClosed { question, answer: None } if question == &pending.id))
                .count(),
            1
        );
    }

    /// A full table refuses rather than evicts. Every entry has a caller
    /// blocked on it, and dropping one would leave that caller waiting
    /// out its whole timeout for an answer nobody can deliver.
    #[test]
    fn a_full_table_refuses_instead_of_dropping_somebody_elses_question() {
        let dir = TempDir::new().unwrap();
        let daemon = state(&dir);
        let mut state = lock(&daemon.state);
        for _ in 0..MAX_QUESTIONS {
            assert!(remember(&mut state, question(300)));
        }
        let refused = question(300);
        assert!(!remember(&mut state, refused.clone()));
        assert_eq!(state.questions.len(), MAX_QUESTIONS);
        assert!(!state.questions.contains_key(&refused.id));
    }

    /// Expired questions are nobody's business, so they make room.
    #[test]
    fn expired_questions_make_room_for_new_ones() {
        let dir = TempDir::new().unwrap();
        let daemon = state(&dir);
        let mut state = lock(&daemon.state);
        for _ in 0..MAX_QUESTIONS {
            assert!(remember(&mut state, question(-1)));
        }
        let fresh = question(300);
        assert!(remember(&mut state, fresh.clone()));
        assert_eq!(state.questions.len(), 1);
        assert!(state.questions.contains_key(&fresh.id));
    }

    #[test]
    fn a_notification_title_says_what_kind_of_message_arrived() {
        assert_eq!(title("alpha", "question"), "alpha asks");
        assert_eq!(title("alpha", "stale"), "alpha: your context is stale");
        assert_eq!(title("alpha", "chat"), "alpha says");
    }

    #[test]
    fn message_text_prefers_the_conventional_field() {
        assert_eq!(message_text(&json!({ "text": "hello" })), "hello");
        assert_eq!(message_text(&json!({ "n": 1 })), r#"{"n":1}"#);
    }
}
