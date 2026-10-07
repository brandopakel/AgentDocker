//! Volatile secret handoff. No envelope, SQLite write or durable event contains
//! a question, capability or answer from this route. Socket loss never replays
//! a consumed answer; the controller must reconcile its metadata-only fence.
use super::*;
use agentdocker_core::ProcessIdentity;
use agentdocker_core::secret::{
    SecretAnswers, SecretReply, SecretReview, SecretReviewSpec, SecretText,
};
use sha2::{Digest, Sha256};

const MAX_ROUTES: usize = 32;
const ROUTE_LIFETIME: std::time::Duration = std::time::Duration::from_secs(300);
const OWNER_HEARTBEAT: std::time::Duration = std::time::Duration::from_secs(15);

pub(super) struct Entry {
    view: SecretReview,
    capability_hash: [u8; 32],
    answer: Option<SecretAnswers>,
    expires: Instant,
    owner_deadline: Instant,
}

fn capability(token: &SecretText) -> Option<[u8; 32]> {
    let text = token.expose();
    (text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| Sha256::digest(text.as_bytes()).into())
}

impl State {
    fn secret_owner_current(&self, agent: &AgentId, owner: &ProcessIdentity, thread: &str) -> bool {
        self.registry.get(agent).is_some_and(|a| {
            agentdocker_host::provider_input::is_codex_input(a)
                && a.status.is_live()
                && a.input_binding.is_none()
                && a.pid == Some(owner.pid)
                && a.process_started_at == Some(owner.started_at)
                && a.spec.labels.get("session_id").is_some_and(|s| s == thread)
        }) && procinfo::alive(owner.pid)
            && procinfo::start_time(owner.pid) == Some(owner.started_at)
    }

    fn secret_changed(&self, review: &SecretReview) {
        // seq:0 is explicitly live-only. A reconnect lists metadata again.
        let _ = self.events.send(Event::new(
            EventKind::SecretReviewChanged {
                review: review.id.clone(),
                agent: review.agent.clone(),
            },
            Utc::now(),
        ));
    }

    pub(super) fn expire_secret_reviews(&mut self) {
        let now = Instant::now();
        let wall_now = Utc::now();
        let expired: Vec<_> = self
            .secret_reviews
            .iter()
            .filter(|(_, entry)| {
                now >= entry.expires
                    || wall_now >= entry.view.expires_at
                    || now >= entry.owner_deadline
                    || !self.secret_owner_current(
                        &entry.view.agent,
                        &entry.view.owner,
                        &entry.view.request.thread,
                    )
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            if let Some(entry) = self.secret_reviews.remove(&id) {
                self.secret_changed(&entry.view);
            }
        }
    }

    pub(super) fn open_secret_review(
        &mut self,
        agent: &str,
        owner: ProcessIdentity,
        recipient: &str,
        request: SecretReviewSpec,
        token: SecretText,
    ) -> Response {
        self.expire_secret_reviews();
        let Some(hash) = capability(&token) else {
            return Response::error(ErrorCode::Invalid, "invalid secret capability");
        };
        if !request.valid() {
            return Response::error(ErrorCode::Invalid, "invalid secret review bundle");
        }
        let agent = match self.resolve(agent) {
            Ok(id) => id,
            Err(e) => return *e,
        };
        let recipient = match self.resolve(recipient) {
            Ok(id) => id,
            Err(e) => return *e,
        };
        if !self.secret_owner_current(&agent, &owner, &request.thread)
            || !self.registry.get(&recipient).is_some_and(humans::is_human)
        {
            return Response::error(
                ErrorCode::Forbidden,
                "secret review requires its exact live managed owner and human recipient",
            );
        }
        if self.secret_reviews.len() >= MAX_ROUTES
            || self.secret_reviews.values().any(|e| e.view.agent == agent)
        {
            return Response::error(ErrorCode::Conflict, "secret review capacity is occupied");
        }
        // The daemon creates a fresh opaque route: a stale human answer can
        // never target a caller-reused ID after closure or daemon replacement.
        let view = SecretReview {
            id: uuid::Uuid::new_v4().simple().to_string(),
            agent,
            recipient,
            owner,
            request,
            expires_at: Utc::now() + Duration::seconds(300),
        };
        let now = Instant::now();
        let entry = Entry {
            view: view.clone(),
            capability_hash: hash,
            answer: None,
            expires: now + ROUTE_LIFETIME,
            owner_deadline: now + OWNER_HEARTBEAT,
        };
        self.secret_reviews.insert(view.id.clone(), entry);
        self.secret_changed(&view);
        Response::SecretReview { review: view }
    }

    pub(super) fn list_secret_reviews(&mut self, recipient: &str) -> Response {
        self.expire_secret_reviews();
        let id = match self.resolve(recipient) {
            Ok(id) => id,
            Err(e) => return *e,
        };
        if !self.registry.get(&id).is_some_and(humans::is_human) {
            return Response::error(
                ErrorCode::Forbidden,
                "secret reviews require their human recipient",
            );
        }
        let mut reviews: Vec<_> = self
            .secret_reviews
            .values()
            .filter(|e| e.view.recipient == id && e.answer.is_none())
            .map(|e| e.view.clone())
            .collect();
        reviews.sort_by(|a, b| a.id.cmp(&b.id));
        Response::SecretReviews { reviews }
    }

    pub(super) fn answer_secret_review(
        &mut self,
        from: &str,
        id: &str,
        answers: SecretAnswers,
        retention_acknowledged: bool,
    ) -> Response {
        self.expire_secret_reviews();
        let from = match self.resolve(from) {
            Ok(id) => id,
            Err(e) => return *e,
        };
        let Some(entry) = self.secret_reviews.get_mut(id) else {
            return Response::error(ErrorCode::NotFound, "secret review is closed");
        };
        if entry.view.recipient != from {
            return Response::error(ErrorCode::Forbidden, "secret review has another recipient");
        }
        if !retention_acknowledged {
            return Response::error(
                ErrorCode::Invalid,
                "acknowledge the provider retention notice before submitting",
            );
        }
        if !answers.matches(&entry.view.request.fields) {
            return Response::error(
                ErrorCode::Invalid,
                "secret answer fields do not match the complete bundle",
            );
        }
        if entry.answer.is_some() {
            return Response::error(
                ErrorCode::Conflict,
                "secret answer was already submitted; do not resend it",
            );
        }
        entry.answer = Some(answers);
        let view = entry.view.clone();
        self.secret_changed(&view);
        Response::Ok
    }

    pub(super) fn cancel_secret_review(&mut self, from: &str, id: &str) -> Response {
        self.expire_secret_reviews();
        let from = match self.resolve(from) {
            Ok(id) => id,
            Err(e) => return *e,
        };
        let Some(entry) = self.secret_reviews.get(id) else {
            return Response::Ok;
        };
        if entry.view.recipient != from {
            return Response::error(ErrorCode::Forbidden, "secret review has another recipient");
        }
        let entry = self.secret_reviews.remove(id).expect("checked route");
        self.secret_changed(&entry.view);
        Response::Ok
    }

    pub(super) fn poll_secret_review(
        &mut self,
        agent: &str,
        owner: &ProcessIdentity,
        id: &str,
        token: &SecretText,
        close: bool,
    ) -> Response {
        self.expire_secret_reviews();
        let agent = match self.resolve(agent) {
            Ok(id) => id,
            Err(e) => return *e,
        };
        let Some(hash) = capability(token) else {
            return Response::error(ErrorCode::Forbidden, "invalid secret owner capability");
        };
        let Some(entry) = self.secret_reviews.get_mut(id) else {
            return Response::SecretReply {
                reply: SecretReply::Closed,
            };
        };
        let different = hash
            .iter()
            .zip(entry.capability_hash)
            .fold(0u8, |diff, (a, b)| diff | (*a ^ b));
        if different != 0 || entry.view.agent != agent || &entry.view.owner != owner {
            return Response::error(ErrorCode::Forbidden, "secret review has another owner");
        }
        entry.owner_deadline = Instant::now() + OWNER_HEARTBEAT;
        if !close && entry.answer.is_none() {
            return Response::SecretReply {
                reply: SecretReply::Waiting,
            };
        }
        // Remove before responding. A lost IPC reply cannot retrieve it twice.
        let entry = self.secret_reviews.remove(id).expect("checked route");
        self.secret_changed(&entry.view);
        Response::SecretReply {
            reply: if close {
                SecretReply::Closed
            } else {
                SecretReply::Answered {
                    answers: entry.answer.expect("checked answer"),
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::secret::SecretField;

    fn fixture() -> (
        tempfile::TempDir,
        Arc<Daemon>,
        AgentId,
        AgentId,
        ProcessIdentity,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let daemon =
            Arc::new(Daemon::open(dir.path().into(), dir.path().join("daemon.sock")).unwrap());
        let owner = ProcessIdentity {
            pid: std::process::id(),
            started_at: procinfo::start_time(std::process::id()).unwrap(),
        };
        let mut actor = AgentRecord::new(
            AgentSpec {
                name: "managed-secret-owner".into(),
                runtime: "codex".into(),
                env: BTreeMap::from([("AGENTDOCKER_CODEX_INPUT".into(), "1".into())]),
                labels: BTreeMap::from([("session_id".into(), "thread".into())]),
                ..Default::default()
            },
            true,
            Utc::now(),
        );
        actor.status = AgentStatus::Running;
        actor.pid = Some(owner.pid);
        actor.process_started_at = Some(owner.started_at);
        let mut human = AgentRecord::new(
            AgentSpec {
                name: "human".into(),
                runtime: "human".into(),
                ..Default::default()
            },
            false,
            Utc::now(),
        );
        human.status = AgentStatus::Running;
        let agent = actor.id.clone();
        let recipient = human.id.clone();
        {
            let mut s = lock(&daemon.state);
            s.registry.insert(actor).unwrap();
            s.registry.insert(human).unwrap();
        }
        (dir, daemon, agent, recipient, owner)
    }
    fn token() -> SecretText {
        SecretText::new("ab".repeat(32)).unwrap()
    }
    fn spec() -> SecretReviewSpec {
        SecretReviewSpec {
            thread: "thread".into(),
            turn: "turn".into(),
            fields: vec![
                SecretField {
                    id: "secret".into(),
                    question: "Temporary value?".into(),
                    is_secret: true,
                },
                SecretField {
                    id: "ordinary".into(),
                    question: "Label?".into(),
                    is_secret: false,
                },
            ],
        }
    }
    fn answers() -> SecretAnswers {
        SecretAnswers::new(BTreeMap::from([
            (
                "secret".into(),
                SecretText::new("invented-secret-canary".into()).unwrap(),
            ),
            (
                "ordinary".into(),
                SecretText::new("also-volatile".into()).unwrap(),
            ),
        ]))
        .unwrap()
    }
    async fn open(
        daemon: &Arc<Daemon>,
        agent: &AgentId,
        human: &AgentId,
        owner: &ProcessIdentity,
    ) -> SecretReview {
        let Response::SecretReview { review } = daemon
            .handle(Request::OpenSecretReview {
                agent: agent.to_string(),
                owner: owner.clone(),
                recipient: human.to_string(),
                request: spec(),
                token: token(),
            })
            .await
        else {
            panic!("private review not opened");
        };
        review
    }
    async fn take(
        daemon: &Arc<Daemon>,
        agent: &AgentId,
        owner: &ProcessIdentity,
        id: &str,
    ) -> Response {
        daemon
            .handle(Request::PollSecretReview {
                agent: agent.to_string(),
                owner: owner.clone(),
                review: id.into(),
                token: token(),
                close: false,
            })
            .await
    }

    #[tokio::test]
    async fn a_mixed_bundle_is_consumed_once_without_durable_messages_questions_or_events() {
        let (dir, daemon, agent, human, owner) = fixture();
        let mut events = daemon.subscribe_events();
        let view = open(&daemon, &agent, &human, &owner).await;
        assert!(matches!(
            take(&daemon, &agent, &owner, &view.id).await,
            Response::SecretReply {
                reply: SecretReply::Waiting
            }
        ));
        let submit = || Request::AnswerSecretReview {
            from: human.to_string(),
            review: view.id.clone(),
            answers: answers(),
            retention_acknowledged: true,
        };
        assert!(matches!(daemon.handle(submit()).await, Response::Ok));
        assert!(matches!(
            daemon.handle(submit()).await,
            Response::Error {
                code: ErrorCode::Conflict,
                ..
            }
        ));
        let listed = daemon
            .handle(Request::SecretReviews {
                recipient: human.to_string(),
            })
            .await;
        assert!(matches!(listed,Response::SecretReviews{reviews} if reviews.is_empty()));
        let result = take(&daemon, &agent, &owner, &view.id).await;
        assert!(
            matches!(&result,Response::SecretReply{reply:SecretReply::Answered{answers:got}} if got==&answers())
        );
        assert!(!format!("{result:?}").contains("invented-secret-canary"));
        assert!(matches!(
            take(&daemon, &agent, &owner, &view.id).await,
            Response::SecretReply {
                reply: SecretReply::Closed
            }
        ));
        assert!(daemon.recent_events(100).is_empty());
        assert!(lock(&daemon.state).questions.is_empty());
        while let Ok(event) = events.try_recv() {
            assert_eq!(event.seq, 0);
            assert!(matches!(event.kind, EventKind::SecretReviewChanged { .. }));
            let text = serde_json::to_string(&event).unwrap();
            assert!(
                !text.contains("invented-secret-canary")
                    && !text.contains("also-volatile")
                    && !text.contains("Temporary value")
            );
        }
        for name in ["state.db", "state.db-wal"] {
            if let Ok(bytes) = std::fs::read(dir.path().join(name)) {
                assert!(
                    !bytes
                        .windows(b"invented-secret-canary".len())
                        .any(|b| b == b"invented-secret-canary")
                );
            }
        }
        let next = open(&daemon, &agent, &human, &owner).await;
        assert_ne!(
            next.id, view.id,
            "a stale answer cannot target a new request"
        );
        assert!(matches!(
            daemon.handle(submit()).await,
            Response::Error {
                code: ErrorCode::NotFound,
                ..
            }
        ));
        drop(daemon);
        let reopened = Daemon::open(dir.path().into(), dir.path().join("daemon.sock")).unwrap();
        assert!(lock(&reopened.state).secret_reviews.is_empty());
    }

    #[tokio::test]
    async fn wrong_recipient_capability_generation_and_partial_bundle_never_consume_an_answer() {
        let (_dir, daemon, agent, human, owner) = fixture();
        let view = open(&daemon, &agent, &human, &owner).await;
        let wrong = daemon
            .handle(Request::AnswerSecretReview {
                from: agent.to_string(),
                review: view.id.clone(),
                answers: answers(),
                retention_acknowledged: true,
            })
            .await;
        assert!(matches!(
            wrong,
            Response::Error {
                code: ErrorCode::Forbidden,
                ..
            }
        ));
        assert!(matches!(
            daemon
                .handle(Request::AnswerSecretReview {
                    from: human.to_string(),
                    review: view.id.clone(),
                    answers: answers(),
                    retention_acknowledged: false
                })
                .await,
            Response::Error {
                code: ErrorCode::Invalid,
                ..
            }
        ));
        let partial = SecretAnswers::new(BTreeMap::from([(
            "secret".into(),
            SecretText::new("private".into()).unwrap(),
        )]))
        .unwrap();
        assert!(matches!(
            daemon
                .handle(Request::AnswerSecretReview {
                    from: human.to_string(),
                    review: view.id.clone(),
                    answers: partial,
                    retention_acknowledged: true
                })
                .await,
            Response::Error {
                code: ErrorCode::Invalid,
                ..
            }
        ));
        assert!(matches!(
            daemon
                .handle(Request::AnswerSecretReview {
                    from: human.to_string(),
                    review: view.id.clone(),
                    answers: answers(),
                    retention_acknowledged: true
                })
                .await,
            Response::Ok
        ));
        assert!(matches!(
            daemon
                .handle(Request::PollSecretReview {
                    agent: agent.to_string(),
                    owner: owner.clone(),
                    review: view.id.clone(),
                    token: SecretText::new("cd".repeat(32)).unwrap(),
                    close: false
                })
                .await,
            Response::Error {
                code: ErrorCode::Forbidden,
                ..
            }
        ));
        let mut wrong = owner.clone();
        wrong.started_at -= Duration::seconds(1);
        assert!(matches!(
            take(&daemon, &agent, &wrong, &view.id).await,
            Response::Error {
                code: ErrorCode::Forbidden,
                ..
            }
        ));
        assert!(matches!(
            take(&daemon, &agent, &owner, &view.id).await,
            Response::SecretReply {
                reply: SecretReply::Answered { .. }
            }
        ));
    }

    #[tokio::test]
    async fn owner_loss_expiry_and_cancellation_remove_pending_and_submitted_values() {
        let (_dir, daemon, agent, human, owner) = fixture();
        for mode in 0..4 {
            let view = open(&daemon, &agent, &human, &owner).await;
            assert!(matches!(
                daemon
                    .handle(Request::AnswerSecretReview {
                        from: human.to_string(),
                        review: view.id.clone(),
                        answers: answers(),
                        retention_acknowledged: true
                    })
                    .await,
                Response::Ok
            ));
            match mode {
                0 => {
                    lock(&daemon.state)
                        .secret_reviews
                        .get_mut(&view.id)
                        .unwrap()
                        .owner_deadline = Instant::now() - std::time::Duration::from_secs(1);
                }
                1 => {
                    lock(&daemon.state)
                        .secret_reviews
                        .get_mut(&view.id)
                        .unwrap()
                        .expires = Instant::now() - std::time::Duration::from_secs(1);
                }
                2 => {
                    assert!(matches!(
                        daemon
                            .handle(Request::CancelSecretReview {
                                from: human.to_string(),
                                review: view.id.clone()
                            })
                            .await,
                        Response::Ok
                    ));
                }
                _ => {
                    lock(&daemon.state)
                        .registry
                        .get_mut(&agent)
                        .unwrap()
                        .process_started_at = Some(owner.started_at - Duration::seconds(1));
                }
            }
            assert!(matches!(
                take(&daemon, &agent, &owner, &view.id).await,
                Response::SecretReply {
                    reply: SecretReply::Closed
                }
            ));
            assert!(lock(&daemon.state).secret_reviews.is_empty());
        }
    }
    #[tokio::test]
    async fn displayed_expiry_rejects_new_answers_and_drops_unclaimed_answers() {
        let (_dir, daemon, agent, human, owner) = fixture();
        for submitted in [false, true] {
            let view = open(&daemon, &agent, &human, &owner).await;
            if submitted {
                assert!(matches!(
                    daemon
                        .handle(Request::AnswerSecretReview {
                            from: human.to_string(),
                            review: view.id.clone(),
                            answers: answers(),
                            retention_acknowledged: true
                        })
                        .await,
                    Response::Ok
                ));
            }
            {
                let mut state = lock(&daemon.state);
                let entry = state.secret_reviews.get_mut(&view.id).unwrap();
                // Simulate sleep or a clock jump past the displayed deadline,
                // while both monotonic deadlines are still in the future.
                entry.expires = Instant::now() + ROUTE_LIFETIME;
                entry.owner_deadline = Instant::now() + OWNER_HEARTBEAT;
                entry.view.expires_at = Utc::now() - Duration::seconds(1);
            }
            if !submitted {
                assert!(matches!(
                    daemon
                        .handle(Request::AnswerSecretReview {
                            from: human.to_string(),
                            review: view.id.clone(),
                            answers: answers(),
                            retention_acknowledged: true
                        })
                        .await,
                    Response::Error {
                        code: ErrorCode::NotFound,
                        ..
                    }
                ));
            }
            assert!(matches!(
                take(&daemon, &agent, &owner, &view.id).await,
                Response::SecretReply {
                    reply: SecretReply::Closed
                }
            ));
            assert!(lock(&daemon.state).secret_reviews.is_empty());
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn secret_notifications_do_not_break_the_checked_question_event_stream() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let (_dir, daemon, agent, human, owner) = fixture();
        let listener = crate::server::bind(&daemon).await.unwrap();
        let serving = tokio::spawn(crate::server::serve(daemon.clone(), listener));
        let stream = agentdocker_host::ipc::Stream::connect(&daemon.socket)
            .await
            .unwrap();
        let mut client = BufReader::new(stream);
        client
            .get_mut()
            .write_all(b"{\"op\":\"resume_events\",\"after\":null}\n")
            .await
            .unwrap();
        async fn next(client: &mut BufReader<agentdocker_host::ipc::Stream>) -> Response {
            let mut text = String::new();
            tokio::time::timeout(
                std::time::Duration::from_secs(2),
                client.read_line(&mut text),
            )
            .await
            .unwrap()
            .unwrap();
            serde_json::from_str(&text).unwrap()
        }
        let Response::EventsReadyAt { cursor } = next(&mut client).await else {
            panic!("no checked cursor")
        };
        assert!(matches!(
            next(&mut client).await,
            Response::EventsCaughtUp { .. }
        ));
        let _review = open(&daemon, &agent, &human, &owner).await;
        assert!(matches!(
            daemon
                .handle(Request::Register {
                    spec: AgentSpec {
                        name: "after-secret-notification".into(),
                        ..Default::default()
                    },
                    pid: None,
                    session: None
                })
                .await,
            Response::Agent { .. }
        ));
        let response = next(&mut client).await;
        drop(client);
        serving.abort();
        let _ = serving.await;
        assert!(
            matches!(response,Response::EventAt { cursor:next,.. } if next.seq==cursor.seq+1),
            "a volatile notification must not masquerade as missing durable history"
        );
    }
}
