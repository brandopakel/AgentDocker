//! Inbound trust: what a project does with a remote sender's word.
//!
//! A session of a Claude or ChatGPT account reaches a project through the
//! remote connector as an agent with no process here. Its messages are
//! attributed input like any agent's, and a project may want to see them
//! before its agents do — or not at all. The rule is pure and in core
//! (`policy::inbound`: sender class and setting → accept, hold or refuse);
//! this is the part that acts on it.
//!
//! **Refuse** is a refusal at the send, `forbidden`, through the same path
//! as a `[[rule]]` denial, so `policy_denied` names the setting. **Hold**
//! keeps the message whole as a `held_message` document and puts a
//! question to the person about it — the card the desktop's Needs you and
//! `agentdocker questions` already show — whose answer `Release` sends it
//! as it was and `Drop` discards it. The question and the held copy land
//! in one transaction, and so do the release and the message going out,
//! so there is never a card without its message or a message without its
//! card. A held question is dropped unanswered after a week.

use super::*;
use agentdocker_core::policy::InboundRuling;
use agentdocker_core::{HUMAN, HeldDecision, HeldMessage, Ruling, Trust};

/// The document kind a held message is kept as, under its question's id.
pub(super) const DOCUMENT: &str = "held_message";
/// `held_message_dropped.by` when nobody decided in time.
pub(super) const EXPIRY: &str = "expiry";

impl Daemon {
    /// What the sender's project does with this send: nothing special
    /// (`Ok(None)`), hold it (`Ok(Some(ruling))`), or refuse it, announced
    /// as a policy denial of `send:<to>` and answered `forbidden`.
    /// A sender with no record (`agentd`, a bare `user`) is not classed.
    pub(super) async fn inbound_ruling(
        &self,
        from: &str,
        to: &str,
    ) -> Result<Option<InboundRuling>, Box<Response>> {
        let mut state = lock(&self.state);
        let sender = match state.registry.resolve(from) {
            Ok(id) => id,
            Err(RegistryError::NotFound(_)) => return Ok(None),
            Err(err) => return Err(Box::new(registry_error(err))),
        };
        let Some(record) = state.registry.get(&sender) else {
            return Ok(None);
        };
        let project = record
            .project
            .as_ref()
            .and_then(|p| state.project_policies.get(p.dir()))
            .map(|loaded| &loaded.policy);
        let ruling = agentdocker_core::policy::inbound(&state.host_policy.policy, project, record);
        match ruling.trust {
            Trust::Accept => Ok(None),
            Trust::Hold => Ok(Some(ruling)),
            Trust::Refuse => {
                let action = format!("send:{to}");
                Err(Box::new(state.refuse(
                    &sender,
                    &action,
                    Ruling::Deny {
                        rule: ruling.rule,
                        reason: "this project refuses what this sender sends".to_owned(),
                    },
                )))
            }
        }
    }
}

impl State {
    /// Keep a message for the person instead of delivering it: the held
    /// copy and the card asking about it, in one commit. Answers `held`
    /// with the message's id and the card's.
    pub(super) fn hold_message(&mut self, envelope: Envelope, ruling: InboundRuling) -> Response {
        if let Some(error) = self.write_failure() {
            return error;
        }
        let (name, runtime) = self
            .registry
            .get(&AgentId::from(envelope.from.as_str()))
            .map(|record| (record.spec.name.clone(), record.spec.runtime.clone()))
            .unwrap_or_else(|| (envelope.from.clone(), "unknown runtime".to_owned()));
        // The person's record when they have one, so the card is theirs in
        // `questions --me` and the desktop; the bare name otherwise, which
        // every listing of questions still shows.
        let human = self
            .registry
            .live()
            .find(|a| humans::is_human(a))
            .map(|a| a.id.clone())
            .unwrap_or_else(|| AgentId::from(HUMAN));
        let to = self.describe_destination(&envelope.to);
        let now = Utc::now();
        let mut question =
            HeldMessage::question(&envelope, &name, &runtime, &to, human, &ruling.rule, now);
        let card = Envelope::new(
            question.from.clone(),
            question.to.clone(),
            "question",
            json!({ "text": question.text }),
            None,
            now,
        );
        question.id = card.id.clone();
        let held = HeldMessage {
            question: question.id.clone(),
            envelope: envelope.clone(),
            rule: ruling.rule.clone(),
            held_at: now,
        };
        let transition = DocumentTransition {
            kind: DOCUMENT,
            id: question.id.to_string(),
            value: Some(serde_json::to_value(&held).expect("a held message serialises")),
            event: EventKind::MessageHeld {
                message: envelope.id.clone(),
                question: question.id.clone(),
                from: envelope.from.clone(),
                to: envelope.to.clone(),
                rule: ruling.rule,
            },
            closes: None,
        };
        let question_id = question.id.clone();
        match self.publish_with_channel(card, Some(question), None, Some(transition)) {
            Response::Sent { .. } => {
                self.held_messages.insert(question_id.clone(), held);
                Response::Held {
                    message: envelope.id,
                    question: question_id,
                }
            }
            other => other,
        }
    }

    /// A destination as the person reads it on the card.
    fn describe_destination(&self, to: &Destination) -> String {
        match to {
            Destination::Agent(id) => self.display_name(id.as_str()),
            Destination::Project(project) => {
                format!("everyone in {}", self.project_name(project))
            }
            Destination::Channel(id) => self
                .channels
                .get(id)
                .and_then(|channel| channel.name.clone())
                .map(|name| format!("#{name}"))
                .unwrap_or_else(|| format!("channel:{id}")),
            Destination::Broadcast => "everyone".to_owned(),
            Destination::Topic(topic) => format!("topic:{topic}"),
        }
    }

    /// The person's word on a held message. Release sends it as it was,
    /// closing its card in the same commit; Drop discards it and closes
    /// the card. `by` is who decided, for the event.
    pub(super) fn settle_held(
        &mut self,
        question: &MessageId,
        decision: HeldDecision,
        by: &str,
    ) -> Response {
        if let Some(error) = self.write_failure() {
            return error;
        }
        let Some(held) = self.held_messages.get(question).cloned() else {
            return Response::error(
                ErrorCode::NotFound,
                format!("no held message waits on question {question}"),
            );
        };
        match decision {
            HeldDecision::Release => {
                let transition = DocumentTransition {
                    kind: DOCUMENT,
                    id: question.to_string(),
                    value: None,
                    event: EventKind::HeldMessageReleased {
                        message: held.envelope.id.clone(),
                        question: question.clone(),
                        by: by.to_owned(),
                    },
                    closes: Some(question.clone()),
                };
                let response =
                    self.publish_with_channel(held.envelope, None, None, Some(transition));
                if matches!(response, Response::Sent { .. }) {
                    self.held_messages.remove(question);
                }
                response
            }
            HeldDecision::Drop => {
                self.drop_held(
                    &[(question.clone(), held.envelope.id.clone())],
                    by,
                    Some(EventKind::QuestionClosed {
                        question: question.clone(),
                        answer: None,
                    }),
                );
                self.write_failure().unwrap_or(Response::Ok)
            }
        }
    }

    /// Discard held messages with their cards: the question documents and
    /// the held copies go, with `closing` (the card's own closure, when the
    /// caller has not already committed one) and one `held_message_dropped`
    /// each. Memory follows the commit.
    pub(super) fn drop_held(
        &mut self,
        dropped: &[(MessageId, MessageId)],
        by: &str,
        closing: Option<EventKind>,
    ) {
        if dropped.is_empty() {
            return;
        }
        let now = Utc::now();
        let mut events: Vec<Event> = closing
            .into_iter()
            .map(|kind| Event::new(kind, now))
            .collect();
        events.extend(dropped.iter().map(|(question, message)| {
            Event::new(
                EventKind::HeldMessageDropped {
                    message: message.clone(),
                    question: question.clone(),
                    by: by.to_owned(),
                },
                now,
            )
        }));
        let delete: Vec<(&str, String)> = dropped
            .iter()
            .flat_map(|(question, _)| {
                [
                    ("question", question.as_str().to_owned()),
                    (DOCUMENT, question.as_str().to_owned()),
                ]
            })
            .collect();
        let _ = self.commit(
            "held message drop",
            events,
            |store, events| store.documents_change(&[], &delete, events),
            |state, _, ()| {
                for (question, _) in dropped {
                    state.questions.remove(question);
                    state.held_messages.remove(question);
                }
            },
        );
    }

    /// Whether this question carries a held message.
    pub(super) fn holds_message(&self, question: &MessageId) -> bool {
        self.held_messages.contains_key(question)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::{HUMAN_RUNTIME, HeldMessage};
    use tempfile::TempDir;

    fn open(dir: &TempDir) -> Arc<Daemon> {
        let home = dir.path().to_path_buf();
        Arc::new(Daemon::open(home.clone(), home.join("sock")).unwrap())
    }

    /// A checkout with a project policy, and a daemon whose home holds a
    /// host policy, so both files can be tried.
    fn checkout(dir: &TempDir, project_policy: &str) -> PathBuf {
        let root = dir.path().join("checkout");
        std::fs::create_dir_all(root.join(".agentdocker")).unwrap();
        std::fs::write(root.join(".agentdocker/policy.toml"), project_policy).unwrap();
        root.canonicalize().unwrap()
    }

    async fn register(daemon: &Arc<Daemon>, spec: AgentSpec, pid: Option<u32>) -> AgentRecord {
        match daemon
            .handle(Request::Register {
                spec,
                pid,
                session: None,
            })
            .await
        {
            Response::Agent { agent } => agent,
            other => panic!("{other:?}"),
        }
    }

    /// The person, a local agent and a connector sender, all in the checkout.
    async fn cast(daemon: &Arc<Daemon>, root: &Path) -> (AgentRecord, AgentRecord, AgentRecord) {
        let person = register(
            daemon,
            AgentSpec {
                name: HUMAN.into(),
                runtime: HUMAN_RUNTIME.into(),
                workdir: Some(root.to_path_buf()),
                ..AgentSpec::default()
            },
            None,
        )
        .await;
        let local = register(
            daemon,
            AgentSpec {
                name: "local".into(),
                workdir: Some(root.to_path_buf()),
                ..AgentSpec::default()
            },
            Some(std::process::id()),
        )
        .await;
        let remote = register(
            daemon,
            AgentSpec {
                name: "claude-connector-7f3a".into(),
                runtime: "claude-browser".into(),
                workdir: Some(root.to_path_buf()),
                labels: [(
                    agentdocker_core::agent::CONNECTOR_LABEL.to_owned(),
                    "true".to_owned(),
                )]
                .into(),
                ..AgentSpec::default()
            },
            None,
        )
        .await;
        assert!(remote.via_connector());
        (person, local, remote)
    }

    fn send(from: &AgentRecord, to: &str, text: &str) -> Request {
        Request::Send {
            from: from.id.to_string(),
            to: to.to_owned(),
            kind: "message".into(),
            payload: json!({ "text": text }),
            reply_to: None,
            links: Vec::new(),
        }
    }

    async fn inbox(daemon: &Arc<Daemon>, agent: &AgentRecord) -> Vec<Envelope> {
        match daemon
            .handle(Request::Inbox {
                agent: agent.id.to_string(),
                drain: false,
            })
            .await
        {
            Response::Messages { messages } => messages,
            other => panic!("{other:?}"),
        }
    }

    fn kinds(daemon: &Arc<Daemon>) -> Vec<EventKind> {
        daemon
            .recent_events(200)
            .into_iter()
            .map(|event| event.kind)
            .collect()
    }

    /// With nothing written a connector sender's word is delivered like
    /// anyone's: the setting is opt-in.
    #[tokio::test]
    async fn nothing_written_accepts_a_connector_sender() {
        let dir = TempDir::new().unwrap();
        let root = checkout(&dir, "");
        let daemon = open(&dir);
        let (_, local, remote) = cast(&daemon, &root).await;
        let response = daemon.handle(send(&remote, "local", "hello")).await;
        assert!(matches!(response, Response::Sent { .. }), "{response:?}");
        assert_eq!(inbox(&daemon, &local).await.len(), 1);
    }

    /// `refuse` is a refusal at the send, through the policy path: nothing
    /// is queued, the sender hears `forbidden` naming the setting, and the
    /// stream says which rule refused.
    #[tokio::test]
    async fn a_refused_sender_is_told_at_the_send_and_nothing_is_queued() {
        let dir = TempDir::new().unwrap();
        let root = checkout(&dir, "[inbound]\nconnector = \"refuse\"\n");
        let daemon = open(&dir);
        let (person, local, remote) = cast(&daemon, &root).await;
        let response = daemon.handle(send(&remote, "local", "hello")).await;
        let Response::Error {
            code: ErrorCode::Forbidden,
            message,
            details,
        } = response
        else {
            panic!("{response:?}")
        };
        assert!(message.contains("project: inbound.connector"), "{message}");
        assert_eq!(details.unwrap()["rule"], "project: inbound.connector");
        assert!(inbox(&daemon, &local).await.is_empty());
        assert!(inbox(&daemon, &person).await.is_empty());
        assert!(kinds(&daemon).iter().any(|kind| matches!(kind,
            EventKind::PolicyDenied { agent, action, rule }
                if *agent == remote.id && action == "send:local" && rule == "project: inbound.connector")));
        // A question from it is refused the same way.
        let response = daemon
            .handle(Request::PostQuestion {
                from: remote.id.to_string(),
                to: HUMAN.into(),
                question: "may I?".into(),
                presentation: None,
                timeout_secs: 60,
            })
            .await;
        assert!(
            matches!(
                response,
                Response::Error {
                    code: ErrorCode::Forbidden,
                    ..
                }
            ),
            "{response:?}"
        );
        // A local agent is not a remote sender and is untouched.
        let response = daemon
            .handle(send(&local, "claude-connector-7f3a", "hi"))
            .await;
        assert!(matches!(response, Response::Sent { .. }), "{response:?}");
    }

    /// `hold` keeps the message whole and puts a card to the person; the
    /// recipient has nothing until the person answers Release, and then has
    /// the message as it was sent. The card closes with the release.
    #[tokio::test]
    async fn a_held_message_waits_on_a_card_and_release_delivers_it_as_sent() {
        let dir = TempDir::new().unwrap();
        let root = checkout(&dir, "[inbound]\nconnector = \"hold\"\n");
        let daemon = open(&dir);
        let (person, local, remote) = cast(&daemon, &root).await;
        let response = daemon
            .handle(send(
                &remote,
                "local",
                "the docs page says the flag was renamed",
            ))
            .await;
        let Response::Held { message, question } = response else {
            panic!("{response:?}")
        };
        assert!(
            inbox(&daemon, &local).await.is_empty(),
            "nothing reaches the recipient yet"
        );

        // The card is the person's: among their questions, from the
        // sender, with the two answers, quoting the message.
        let Response::Questions { questions } = daemon
            .handle(Request::Questions {
                agent: Some(HUMAN.into()),
            })
            .await
        else {
            panic!("questions")
        };
        assert_eq!(questions.len(), 1);
        let card = &questions[0];
        assert_eq!(card.id, question);
        assert_eq!(card.from, remote.id.as_str());
        assert_eq!(card.to, Destination::Agent(person.id.clone()));
        assert!(
            card.text.contains(
                "claude-connector-7f3a (claude-browser) sent a `message` message to local"
            )
        );
        assert!(card.text.contains("(project: inbound.connector)"));
        assert!(
            card.text
                .contains("the docs page says the flag was renamed")
        );
        assert!(
            card.presentation
                .as_ref()
                .unwrap()
                .permits_choice(agentdocker_core::message::RELEASE)
        );
        // The card is in the person's inbox as a question message.
        let notices = inbox(&daemon, &person).await;
        assert_eq!(notices.len(), 1);
        assert_eq!(
            (notices[0].id.clone(), notices[0].kind.as_str()),
            (question.clone(), "question")
        );
        // Held durably, under the card's id, in the same transaction.
        let held: Vec<HeldMessage> = lock(&daemon.state).store.documents(DOCUMENT, None).unwrap();
        assert_eq!(held.len(), 1);
        assert_eq!(
            (held[0].question.clone(), held[0].envelope.id.clone()),
            (question.clone(), message.clone())
        );
        assert!(kinds(&daemon).iter().any(|kind| matches!(kind,
            EventKind::MessageHeld { message: m, question: q, from, to, rule }
                if *m == message && *q == question && from == remote.id.as_str()
                    && *to == Destination::Agent(local.id.clone()) && rule == "project: inbound.connector")));

        // Somebody else's word is not a decision.
        let response = daemon
            .handle(Request::Answer {
                from: Some(local.id.to_string()),
                message: question.clone(),
                text: "Release".into(),
            })
            .await;
        assert!(
            matches!(
                response,
                Response::Error {
                    code: ErrorCode::Forbidden,
                    ..
                }
            ),
            "{response:?}"
        );
        // Nor is an essay from the person.
        let response = daemon
            .handle(Request::Answer {
                from: None,
                message: question.clone(),
                text: "sure, go ahead".into(),
            })
            .await;
        assert!(
            matches!(
                response,
                Response::Error {
                    code: ErrorCode::Invalid,
                    ..
                }
            ),
            "{response:?}"
        );
        assert!(inbox(&daemon, &local).await.is_empty());

        // Release: the message goes out as it was, the card closes, and
        // the person's record of it is the released id.
        let response = daemon
            .handle(Request::Answer {
                from: None,
                message: question.clone(),
                text: "release".into(),
            })
            .await;
        let Response::Sent {
            message: released, ..
        } = response
        else {
            panic!("{response:?}")
        };
        assert_eq!(released, message);
        let delivered = inbox(&daemon, &local).await;
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].id, message);
        assert_eq!(delivered[0].from, remote.id.as_str());
        assert_eq!(delivered[0].kind, "message");
        assert_eq!(
            delivered[0].payload["text"],
            "the docs page says the flag was renamed"
        );
        let Response::Questions { questions } =
            daemon.handle(Request::Questions { agent: None }).await
        else {
            panic!("questions")
        };
        assert!(questions.is_empty(), "{questions:?}");
        assert!(lock(&daemon.state).held_messages.is_empty());
        let held: Vec<HeldMessage> = lock(&daemon.state).store.documents(DOCUMENT, None).unwrap();
        assert!(held.is_empty());
        let events = kinds(&daemon);
        let released_at = events
            .iter()
            .position(|kind| {
                matches!(kind, EventKind::HeldMessageReleased { message: m, question: q, by }
                if *m == message && *q == question && by == person.id.as_str())
            })
            .expect("released");
        let closed_at = events
            .iter()
            .position(|kind| matches!(kind, EventKind::QuestionClosed { question: q, answer: None } if *q == question))
            .expect("closed");
        let sent_at = events
            .iter()
            .rposition(
                |kind| matches!(kind, EventKind::MessageSent { message: m, .. } if *m == message),
            )
            .expect("sent");
        assert!(released_at < sent_at && closed_at < sent_at, "{events:?}");
        // Answering again finds no question.
        let response = daemon
            .handle(Request::Answer {
                from: None,
                message: question,
                text: "Drop".into(),
            })
            .await;
        assert!(
            matches!(
                response,
                Response::Error {
                    code: ErrorCode::NotFound,
                    ..
                }
            ),
            "{response:?}"
        );
    }

    /// Drop discards the message: nobody gets it, the card closes, and the
    /// stream says the person dropped it.
    #[tokio::test]
    async fn drop_discards_a_held_message_and_closes_its_card() {
        let dir = TempDir::new().unwrap();
        let root = checkout(&dir, "[inbound]\nconnector = \"hold\"\n");
        let daemon = open(&dir);
        let (person, local, remote) = cast(&daemon, &root).await;
        let Response::Held { message, question } =
            daemon.handle(send(&remote, "local", "spam")).await
        else {
            panic!("held")
        };
        let response = daemon
            .handle(Request::Answer {
                from: None,
                message: question.clone(),
                text: "Drop".into(),
            })
            .await;
        assert!(matches!(response, Response::Ok), "{response:?}");
        assert!(inbox(&daemon, &local).await.is_empty());
        assert!(lock(&daemon.state).questions.is_empty());
        assert!(lock(&daemon.state).held_messages.is_empty());
        let held: Vec<HeldMessage> = lock(&daemon.state).store.documents(DOCUMENT, None).unwrap();
        assert!(held.is_empty());
        let events = kinds(&daemon);
        assert!(
            events.iter().any(
                |kind| matches!(kind, EventKind::HeldMessageDropped { message: m, question: q, by }
            if *m == message && *q == question && by == person.id.as_str())
            ),
            "{events:?}"
        );
        assert!(events.iter().any(|kind| matches!(kind, EventKind::QuestionClosed { question: q, answer: None } if *q == question)));
        assert!(!events.iter().any(
            |kind| matches!(kind, EventKind::MessageSent { message: m, .. } if *m == message)
        ));
    }

    /// A held `ask` does not wait: the asker hears `held` at once, and the
    /// released question reaches its recipient as an ordinary message.
    #[tokio::test]
    async fn a_held_ask_answers_held_at_once_and_is_released_as_a_message() {
        let dir = TempDir::new().unwrap();
        let root = checkout(&dir, "[inbound]\nconnector = \"hold\"\n");
        let daemon = open(&dir);
        let (person, _, remote) = cast(&daemon, &root).await;
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            daemon.handle(Request::Ask {
                from: remote.id.to_string(),
                to: HUMAN.into(),
                question: "may I merge?".into(),
                timeout_secs: 60,
            }),
        )
        .await
        .expect("a held ask does not wait");
        let Response::Held { message, question } = response else {
            panic!("{response:?}")
        };
        // The person has the card, and not the question itself.
        let notices = inbox(&daemon, &person).await;
        assert_eq!(
            notices.iter().map(|m| &m.id).collect::<Vec<_>>(),
            [&question]
        );
        let Response::Sent {
            message: released, ..
        } = daemon
            .handle(Request::Answer {
                from: None,
                message: question,
                text: "Release".into(),
            })
            .await
        else {
            panic!("released")
        };
        assert_eq!(released, message);
        let notices = inbox(&daemon, &person).await;
        let released = notices.iter().find(|m| m.id == message).expect("delivered");
        assert_eq!(released.kind, "question");
        assert_eq!(released.payload["text"], "may I merge?");
        // No durable question waits on it: there is no ask to answer.
        assert!(lock(&daemon.state).questions.is_empty());
    }

    /// The sender withdrawing its card drops the message with it.
    #[tokio::test]
    async fn the_sender_cancelling_its_card_drops_the_held_message() {
        let dir = TempDir::new().unwrap();
        let root = checkout(&dir, "[inbound]\nconnector = \"hold\"\n");
        let daemon = open(&dir);
        let (_, local, remote) = cast(&daemon, &root).await;
        let Response::Held { message, question } =
            daemon.handle(send(&remote, "local", "oops")).await
        else {
            panic!("held")
        };
        let response = daemon
            .handle(Request::CancelQuestion {
                agent: remote.id.to_string(),
                message: question.clone(),
            })
            .await;
        assert!(matches!(response, Response::Ok), "{response:?}");
        assert!(lock(&daemon.state).held_messages.is_empty());
        assert!(inbox(&daemon, &local).await.is_empty());
        let held: Vec<HeldMessage> = lock(&daemon.state).store.documents(DOCUMENT, None).unwrap();
        assert!(held.is_empty());
        assert!(kinds(&daemon).iter().any(
            |kind| matches!(kind, EventKind::HeldMessageDropped { message: m, question: q, by }
            if *m == message && *q == question && by == remote.id.as_str())
        ));
    }

    /// A held message outlives a restart with its card, and an expired
    /// card drops its message as expired.
    #[tokio::test]
    async fn a_held_message_survives_a_restart_and_expires_with_its_card() {
        let dir = TempDir::new().unwrap();
        let root = checkout(&dir, "[inbound]\nconnector = \"hold\"\n");
        let daemon = open(&dir);
        let (_, local, remote) = cast(&daemon, &root).await;
        let Response::Held { message, question } =
            daemon.handle(send(&remote, "local", "later")).await
        else {
            panic!("held")
        };
        let Response::Held {
            message: second,
            question: second_card,
        } = daemon.handle(send(&remote, "local", "much later")).await
        else {
            panic!("held")
        };
        drop(daemon);
        let daemon = open(&dir);
        {
            let state = lock(&daemon.state);
            assert_eq!(state.held_messages.len(), 2);
            assert_eq!(state.held_messages[&question].envelope.id, message);
            assert!(state.questions.contains_key(&question));
        }
        // Release after the restart still delivers the original.
        let Response::Sent {
            message: released, ..
        } = daemon
            .handle(Request::Answer {
                from: None,
                message: question,
                text: "Release".into(),
            })
            .await
        else {
            panic!("released")
        };
        assert_eq!(released, message);
        assert_eq!(inbox(&daemon, &local).await.len(), 1);

        // The other card expires: its message goes with it.
        {
            let mut state = lock(&daemon.state);
            state.questions.get_mut(&second_card).unwrap().expires_at =
                Utc::now() - Duration::seconds(1);
            state.expire_questions(Utc::now());
            assert!(state.held_messages.is_empty());
            assert!(state.questions.is_empty());
        }
        let held: Vec<HeldMessage> = lock(&daemon.state).store.documents(DOCUMENT, None).unwrap();
        assert!(held.is_empty());
        assert!(kinds(&daemon).iter().any(
            |kind| matches!(kind, EventKind::HeldMessageDropped { message: m, question: q, by }
            if *m == second && *q == second_card && by == EXPIRY)
        ));
        assert_eq!(
            inbox(&daemon, &local).await.len(),
            1,
            "an expired message reaches nobody"
        );
    }

    /// The host's file holds what a project would accept: a repository
    /// cannot vote a remote sender in.
    #[tokio::test]
    async fn the_host_policy_is_not_widened_by_the_project() {
        let dir = TempDir::new().unwrap();
        let root = checkout(&dir, "[inbound]\nconnector = \"accept\"\n");
        let daemon = open(&dir);
        std::fs::write(
            daemon.home.join("policy.toml"),
            "[inbound]\nconnector = \"hold\"\n",
        )
        .unwrap();
        daemon.reload_policies();
        let (_, _, remote) = cast(&daemon, &root).await;
        let response = daemon.handle(send(&remote, "local", "hello")).await;
        let Response::Held { .. } = response else {
            panic!("{response:?}")
        };
        assert!(kinds(&daemon).iter().any(|kind| matches!(kind, EventKind::MessageHeld { rule, .. } if rule == "host: inbound.connector")));
    }

    /// A sender entry picks one account out of the class.
    #[tokio::test]
    async fn a_sender_entry_accepts_one_account_while_the_class_is_held() {
        let dir = TempDir::new().unwrap();
        let root = checkout(
            &dir,
            "[inbound]\nconnector = \"hold\"\n[[inbound.sender]]\nname = \"mine\"\nagent = \"claude-connector-*\"\ntrust = \"accept\"\n",
        );
        let daemon = open(&dir);
        let (_, local, remote) = cast(&daemon, &root).await;
        let response = daemon.handle(send(&remote, "local", "hello")).await;
        assert!(matches!(response, Response::Sent { .. }), "{response:?}");
        assert_eq!(inbox(&daemon, &local).await.len(), 1);
        let other = register(
            &daemon,
            AgentSpec {
                name: "chatgpt-connector-1".into(),
                runtime: "chatgpt-browser".into(),
                workdir: Some(root.clone()),
                labels: [(
                    agentdocker_core::agent::CONNECTOR_LABEL.to_owned(),
                    "true".to_owned(),
                )]
                .into(),
                ..AgentSpec::default()
            },
            None,
        )
        .await;
        let response = daemon.handle(send(&other, "local", "hello")).await;
        assert!(matches!(response, Response::Held { .. }), "{response:?}");
    }
}
