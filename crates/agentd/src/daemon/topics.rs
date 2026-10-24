//! Topics keep their messages: `send` keeps a topic message in the topic log
//! beside its other effects (`publish_with_channel`), `subscribe` replays
//! what a reader has not acknowledged before the live stream, an
//! acknowledgement moves a reader's cursor on a topic, and the minute tick
//! keeps every topic within its count and age. The rules — who may read a
//! kept message, where a reader starts, how far a cursor moves, how much
//! one subscribe replays — are `agentdocker_core::topic`'s.
use agentdocker_core::config::RETENTION_BATCH;
use agentdocker_core::topic::{self, REPLAY_PER_TOPIC, REPLAY_TOTAL, TopicCursor};

use super::*;
use crate::store::topics::{ReplayRequest, TopicReplay};

/// Acknowledged ids, split: the kept topic messages the agent may see, as
/// topic and sequence, and the rest for its queue.
type TopicAcks = (Vec<(String, u64)>, Vec<MessageId>);

impl Daemon {
    /// Once a minute: drop kept topic messages past `[messages]
    /// topic_retention` and past `topic_limit` per topic, in one bounded
    /// batch. The file is read outside the lock; a value that cannot be
    /// read is said once and the defaults hold, so a topic is never kept
    /// unbounded because of a typo.
    pub fn apply_topic_retention(&self) {
        let read = crate::config::read(&self.home).and_then(|config| config.topic_retention());
        let mut state = lock(&self.state);
        let retention = match read {
            Ok(retention) => {
                state.topic_config_notice = None;
                retention
            }
            Err(notice) => {
                if state.topic_config_notice.as_ref() != Some(&notice) {
                    warn!(%notice, "topic retention keeps its defaults");
                    state.topic_config_notice = Some(notice);
                }
                agentdocker_core::config::TopicRetention::default()
            }
        };
        let cutoff = Utc::now().checked_sub_signed(retention.window);
        state.prune_topic_log(cutoff, retention.limit, RETENTION_BATCH);
    }
}

impl State {
    /// Commit topic retention and its event before publishing the transition.
    pub(super) fn prune_topic_log(
        &mut self,
        cutoff: Option<DateTime<Utc>>,
        limit: usize,
        batch: usize,
    ) {
        let _ = self.commit_produced(
            "topic retention",
            |store, seq| {
                let event =
                    store.prune_topic_messages_with_event(cutoff, limit, batch, seq, Utc::now())?;
                Ok((event.into_iter().collect(), ()))
            },
            |_, events, ()| {
                for event in events {
                    if let EventKind::TopicMessagesPruned { removed } = &event.kind {
                        info!(removed, "pruned kept topic messages");
                    }
                }
            },
        );
    }

    /// What a subscription replays from the topic log, read under the lock
    /// the subscription is opened under. A read through a coordinator
    /// transfer is served, as every read is.
    pub(super) fn topic_replay(
        &self,
        patterns: &[String],
        scope: Option<&ProjectId>,
        reader: Option<&AgentId>,
        after: Option<u64>,
    ) -> Result<TopicReplay, Box<Response>> {
        if let Some(error) = self.storage_failure() {
            return Err(Box::new(error));
        }
        self.store
            .topic_replay(&ReplayRequest {
                patterns,
                scope,
                reader,
                after,
                per_topic: REPLAY_PER_TOPIC,
                total: REPLAY_TOTAL,
            })
            .map_err(|error| {
                Box::new(Response::error(
                    ErrorCode::StorageUnavailable,
                    format!("cannot read kept topic messages: {error}"),
                ))
            })
    }

    /// Of the ids an agent acknowledges, those that are kept topic
    /// messages it may see — as topic and sequence — and the rest, in their
    /// order, for its queue. A kept message of another project's is
    /// neither: acknowledging it moves nothing.
    pub(super) fn split_topic_acks(
        &mut self,
        reference: &str,
        messages: &[MessageId],
    ) -> Result<TopicAcks, Box<Response>> {
        let id = self.resolve(reference)?;
        if messages.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }
        let posts = self.store.topic_posts(messages).map_err(|error| {
            Box::new(Response::error(
                ErrorCode::StorageUnavailable,
                format!("cannot read kept topic messages: {error}"),
            ))
        })?;
        if posts.is_empty() {
            return Ok((Vec::new(), messages.to_vec()));
        }
        let scope = topic::scope(self.registry.get(&id));
        let kept: HashSet<&MessageId> = posts.iter().map(|(id, _, _)| id).collect();
        let rest = messages
            .iter()
            .filter(|message| !kept.contains(message))
            .cloned()
            .collect();
        let topics = posts
            .iter()
            .filter(|(_, _, post)| topic::visible(post.project.as_ref(), scope.as_ref()))
            .map(|(_, topic, post)| (topic.clone(), post.seq))
            .collect();
        Ok((topics, rest))
    }

    /// Move an agent's topic cursors past what it acknowledged, with one
    /// `topic_cursors_advanced` event, in one transaction. A cursor already
    /// there moves nothing and says nothing.
    pub(super) fn ack_topics(
        &mut self,
        reference: &str,
        acknowledged: &[(String, u64)],
    ) -> Result<(), Box<Response>> {
        if acknowledged.is_empty() {
            return Ok(());
        }
        let id = self.resolve(reference)?;
        let current: HashMap<String, u64> = self
            .store
            .topic_cursors(&id)
            .map_err(|error| {
                Box::new(Response::error(
                    ErrorCode::StorageUnavailable,
                    format!("cannot read topic cursors: {error}"),
                ))
            })?
            .into_iter()
            .collect();
        let cursors: Vec<TopicCursor> = topic::advance(
            |topic| current.get(topic).copied(),
            acknowledged.iter().cloned(),
        );
        if cursors.is_empty() {
            return Ok(());
        }
        let event = Event::new(
            EventKind::TopicCursorsAdvanced {
                agent: id.clone(),
                cursors: cursors.clone(),
            },
            Utc::now(),
        );
        let _ = self.transition(
            "topic acknowledgement",
            event,
            |store, event| store.advance_topic_cursors(&id, &cursors, event),
            |_| {},
        );
        match self.write_failure() {
            Some(error) => Err(Box::new(error)),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::AgentSpec;
    use std::path::Path;
    use tempfile::TempDir;

    fn open(dir: &TempDir) -> Arc<Daemon> {
        let home = dir.path().to_path_buf();
        Arc::new(Daemon::open(home.clone(), home.join("sock")).unwrap())
    }

    /// Each name in its own directory, so its own project.
    async fn register(daemon: &Arc<Daemon>, name: &str, workdir: &Path) -> AgentRecord {
        std::fs::create_dir_all(workdir).unwrap();
        match daemon
            .handle(Request::Register {
                spec: AgentSpec {
                    name: name.into(),
                    workdir: Some(workdir.to_path_buf()),
                    ..AgentSpec::default()
                },
                pid: None,
                session: None,
            })
            .await
        {
            Response::Agent { agent } => agent,
            other => panic!("{other:?}"),
        }
    }

    async fn post(daemon: &Arc<Daemon>, from: &str, topic: &str, text: &str) -> MessageId {
        match daemon
            .handle(Request::Send {
                from: from.into(),
                to: format!("topic:{topic}"),
                kind: "notice".into(),
                payload: serde_json::json!({ "text": text }),
                reply_to: None,
                links: Vec::new(),
            })
            .await
        {
            Response::Sent { message, .. } => message,
            other => panic!("{other:?}"),
        }
    }

    async fn ack(daemon: &Arc<Daemon>, agent: &str, messages: &[MessageId]) -> Response {
        daemon
            .handle(Request::AckInbox {
                agent: agent.into(),
                messages: messages.to_vec(),
            })
            .await
    }

    /// What a subscribe replays from the topic log: ids, oldest first, and
    /// how many it left out.
    fn replayed(
        daemon: &Arc<Daemon>,
        agent: Option<&str>,
        patterns: &[&str],
        after: Option<u64>,
    ) -> (Vec<MessageId>, u64) {
        let (mut subscription, _) = daemon
            .subscribe(
                agent,
                patterns.iter().map(|p| (*p).to_owned()).collect(),
                after,
            )
            .unwrap();
        let replay = subscription.take_topic_replay();
        let seqs: Vec<u64> = replay.messages.iter().map(|(seq, _)| *seq).collect();
        assert!(seqs.is_sorted(), "oldest first: {seqs:?}");
        (
            replay.messages.into_iter().map(|(_, m)| m.id).collect(),
            replay.skipped,
        )
    }

    fn events_of(events: &mut broadcast::Receiver<Event>) -> Vec<EventKind> {
        let mut kinds = Vec::new();
        while let Ok(event) = events.try_recv() {
            kinds.push(event.kind);
        }
        kinds
    }

    /// A message said on a topic before anyone subscribed reaches the
    /// first agent that subscribes, from the oldest kept; somebody with no
    /// agent and no point to start from is live only, as before.
    #[tokio::test]
    async fn a_topic_message_sent_before_a_subscriber_joins_reaches_its_first_subscribe() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        register(&daemon, "reader", &dir.path().join("work")).await;
        let first = post(&daemon, "user", "repo/reviews", "first").await;
        let second = post(&daemon, "user", "repo/builds", "second").await;
        post(&daemon, "user", "elsewhere", "not matched").await;

        assert_eq!(
            replayed(&daemon, Some("reader"), &["repo/#"], None),
            (vec![first.clone(), second.clone()], 0)
        );
        assert_eq!(
            replayed(&daemon, Some("reader"), &["repo/+"], None),
            (vec![first.clone(), second.clone()], 0),
            "nothing acknowledged, so the same again"
        );
        assert_eq!(replayed(&daemon, None, &["repo/#"], None), (vec![], 0));
        // An anonymous watcher may name where to start.
        let (mut stream, _) = daemon
            .subscribe(None, vec!["repo/#".into()], Some(0))
            .unwrap();
        let replay = stream.take_topic_replay();
        assert_eq!(replay.messages.len(), 2);
        let after_first = replay.messages[0].0;
        assert_eq!(
            replayed(&daemon, None, &["repo/#"], Some(after_first)),
            (vec![second], 0)
        );
    }

    /// An agent that acknowledged some messages resumes after them, on
    /// reconnect and after the daemon restarts; acknowledging moves each
    /// topic's cursor, says so once, and a repeat says nothing.
    #[tokio::test]
    async fn a_reader_resumes_after_what_it_acknowledged_across_a_restart() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let reader = register(&daemon, "reader", &dir.path().join("work")).await;
        let mut said = Vec::new();
        for text in ["one", "two", "three"] {
            said.push(post(&daemon, "user", "t/a", text).await);
        }
        let other = post(&daemon, "user", "t/b", "other").await;
        let (replay, _) = replayed(&daemon, Some("reader"), &["t/#"], None);
        assert_eq!(replay, [&said[..], std::slice::from_ref(&other)].concat());

        let mut events = daemon.subscribe_events();
        assert!(matches!(
            ack(&daemon, "reader", &said[..2]).await,
            Response::Ok
        ));
        let kinds = events_of(&mut events);
        assert!(
            matches!(&kinds[..], [EventKind::TopicCursorsAdvanced { agent, cursors }]
                if *agent == reader.id && cursors.len() == 1 && cursors[0].topic == "t/a"),
            "{kinds:?}"
        );
        assert!(matches!(
            ack(&daemon, "reader", &said[..1]).await,
            Response::Ok
        ));
        assert!(
            events_of(&mut events).is_empty(),
            "a cursor never moves back"
        );

        let later = post(&daemon, "user", "t/a", "four").await;
        let expected = vec![said[2].clone(), other.clone(), later.clone()];
        assert_eq!(
            replayed(&daemon, Some("reader"), &["t/#"], None).0,
            expected
        );
        drop(daemon);
        let daemon = open(&dir);
        assert_eq!(
            replayed(&daemon, Some("reader"), &["t/#"], None).0,
            expected,
            "the cursor and the log outlive the daemon"
        );
        // An inbox message and a topic message acknowledged together each
        // go their own way.
        let queued = match daemon
            .handle(Request::Send {
                from: "user".into(),
                to: "reader".into(),
                kind: "chat".into(),
                payload: serde_json::json!({"text": "queued"}),
                reply_to: None,
                links: Vec::new(),
            })
            .await
        {
            Response::Sent { message, .. } => message,
            other => panic!("{other:?}"),
        };
        assert!(matches!(
            ack(&daemon, "reader", &[queued, other]).await,
            Response::Ok
        ));
        assert!(lock(&daemon.state).inboxes[&reader.id].is_empty());
        assert_eq!(
            replayed(&daemon, Some("reader"), &["t/#"], None).0,
            [said[2].clone(), later.clone()],
            "each topic keeps its own cursor"
        );
        // A cursor is a place, not a set: acknowledging the newest on a
        // topic passes what came before it there, acknowledged or not.
        assert!(matches!(
            ack(&daemon, "reader", &[later]).await,
            Response::Ok
        ));
        assert!(
            replayed(&daemon, Some("reader"), &["t/#"], None)
                .0
                .is_empty()
        );
        // A forgotten agent's cursors go with it.
        for request in [
            Request::Deregister {
                agent: "reader".into(),
            },
            Request::Remove {
                agent: reader.id.to_string(),
            },
        ] {
            let response = daemon.handle(request).await;
            assert!(!matches!(response, Response::Error { .. }), "{response:?}");
        }
        assert!(
            lock(&daemon.state)
                .store
                .topic_cursors(&reader.id)
                .unwrap()
                .is_empty()
        );
    }

    /// Each topic keeps its newest `limit` per project, oldest out first,
    /// with the exact count announced.
    #[tokio::test]
    async fn a_topic_keeps_at_most_its_limit() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        register(&daemon, "reader", &dir.path().join("work")).await;
        let mut said = Vec::new();
        for n in 0..5 {
            said.push(post(&daemon, "user", "busy", &n.to_string()).await);
        }
        let quiet = post(&daemon, "user", "quiet", "kept").await;
        let mut events = daemon.subscribe_events();
        lock(&daemon.state).prune_topic_log(None, 2, RETENTION_BATCH);
        let kinds = events_of(&mut events);
        assert!(
            matches!(&kinds[..], [EventKind::TopicMessagesPruned { removed: 3 }]),
            "{kinds:?}"
        );
        assert_eq!(
            replayed(&daemon, Some("reader"), &["#"], None).0,
            [said[3].clone(), said[4].clone(), quiet]
        );
        // Within the budget, and again on the next tick for the rest.
        for n in 0..4 {
            post(&daemon, "user", "busy", &format!("more {n}")).await;
        }
        lock(&daemon.state).prune_topic_log(None, 2, 3);
        assert_eq!(lock(&daemon.state).store.kept_on_topic("busy").unwrap(), 3);
        lock(&daemon.state).prune_topic_log(None, 2, 3);
        assert_eq!(lock(&daemon.state).store.kept_on_topic("busy").unwrap(), 2);
        lock(&daemon.state).prune_topic_log(None, 2, 3);
        let pruned: Vec<usize> = events_of(&mut events)
            .into_iter()
            .filter_map(|kind| match kind {
                EventKind::TopicMessagesPruned { removed } => Some(removed),
                _ => None,
            })
            .collect();
        assert_eq!(pruned, [3, 1], "nothing pruned, nothing said");
    }

    /// A message older than the window goes; a reader whose cursor stood
    /// on a pruned message still resumes after it.
    #[tokio::test]
    async fn a_topic_keeps_nothing_older_than_its_window() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        register(&daemon, "reader", &dir.path().join("work")).await;
        let old = post(&daemon, "user", "t", "old").await;
        let older = post(&daemon, "user", "t", "older").await;
        assert!(matches!(ack(&daemon, "reader", &[old]).await, Response::Ok));
        let cutoff = Utc::now() + Duration::seconds(1);
        lock(&daemon.state).prune_topic_log(Some(cutoff), 1_000, RETENTION_BATCH);
        assert_eq!(lock(&daemon.state).store.kept_on_topic("t").unwrap(), 0);
        assert!(replayed(&daemon, Some("reader"), &["t"], None).0.is_empty());
        let fresh = post(&daemon, "user", "t", "fresh").await;
        lock(&daemon.state).prune_topic_log(
            Utc::now().checked_sub_signed(Duration::days(7)),
            1_000,
            RETENTION_BATCH,
        );
        assert_eq!(
            replayed(&daemon, Some("reader"), &["t"], None).0,
            [fresh],
            "the sequence is never reused, so the cursor keeps its meaning"
        );
        drop(older);
    }

    /// One subscribe replays at most the newest per topic and the newest
    /// overall, and says how many it left out.
    #[tokio::test]
    async fn a_bounded_replay_says_what_it_left_out() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        register(&daemon, "reader", &dir.path().join("work")).await;
        let total = REPLAY_PER_TOPIC + 7;
        let mut said = Vec::new();
        for n in 0..total {
            said.push(post(&daemon, "user", "flood", &n.to_string()).await);
        }
        let (replay, skipped) = replayed(&daemon, Some("reader"), &["flood"], None);
        assert_eq!(replay, said[7..]);
        assert_eq!(skipped, 7);
        // The stream says so before the replay.
        let (mut subscription, _) = daemon
            .subscribe(Some("reader"), vec!["flood".into()], None)
            .unwrap();
        assert_eq!(subscription.take_topic_replay().skipped, 7);
        drop(subscription);
        // Acknowledging the newest moves past the ones left out.
        assert!(matches!(
            ack(&daemon, "reader", &said[total - 1..]).await,
            Response::Ok
        ));
        assert_eq!(
            replayed(&daemon, Some("reader"), &["flood"], None),
            (vec![], 0)
        );
    }

    /// A kept message is for whoever could have had it live: another
    /// project's agent gets nothing of it, live or replayed, and
    /// acknowledging it moves nothing; the sender's project, the person
    /// and an anonymous watcher get it.
    #[tokio::test]
    async fn another_projects_subscriber_gets_nothing() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let alpha = dir.path().join("alpha");
        std::fs::create_dir_all(alpha.join("src")).unwrap();
        std::fs::write(alpha.join("Agentfile.toml"), "").unwrap();
        let writer = register(&daemon, "writer", &alpha).await;
        let colleague = register(&daemon, "colleague-2", &alpha.join("src")).await;
        let stranger = register(&daemon, "stranger", &dir.path().join("beta")).await;
        assert!(writer.project.is_some());
        assert_eq!(writer.project, colleague.project, "one project");
        assert_ne!(writer.project, stranger.project, "another");

        let (mut live_stranger, mut stranger_bus) = daemon
            .subscribe(Some("stranger"), vec!["build/#".into()], None)
            .unwrap();
        let (mut live_colleague, mut colleague_bus) = daemon
            .subscribe(Some("colleague-2"), vec!["build/#".into()], None)
            .unwrap();
        let word = post(&daemon, "writer", "build/status", "green").await;
        let posted = stranger_bus.try_recv().unwrap();
        assert_eq!(posted.envelope.id, word);
        assert!(!live_stranger.wants(&posted), "not live to another project");
        let posted = colleague_bus.try_recv().unwrap();
        assert!(live_colleague.wants(&posted));
        assert!(live_stranger.take_topic_replay().messages.is_empty());
        assert!(live_colleague.take_topic_replay().messages.is_empty());
        drop((live_stranger, live_colleague));

        assert!(
            replayed(&daemon, Some("stranger"), &["build/#"], None)
                .0
                .is_empty()
        );
        assert!(
            replayed(&daemon, Some("stranger"), &["#"], Some(0))
                .0
                .is_empty()
        );
        assert_eq!(
            replayed(&daemon, Some("colleague-2"), &["build/#"], None).0,
            std::slice::from_ref(&word)
        );
        assert_eq!(
            replayed(&daemon, None, &["#"], Some(0)).0,
            std::slice::from_ref(&word)
        );
        let person = match daemon.handle(Request::Me { workdir: None }).await {
            Response::Agent { agent } => agent,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            replayed(&daemon, Some(person.id.as_str()), &["#"], None).0,
            std::slice::from_ref(&word)
        );
        // The stranger's acknowledgement of a message it could not see
        // moves no cursor of its own.
        let mut events = daemon.subscribe_events();
        assert!(matches!(
            ack(&daemon, "stranger", std::slice::from_ref(&word)).await,
            Response::Ok
        ));
        assert!(events_of(&mut events).is_empty());
        // The person's word on a topic is anyone's.
        let open_word = post(&daemon, "user", "build/status", "announced").await;
        assert_eq!(
            replayed(&daemon, Some("stranger"), &["build/#"], None).0,
            [open_word]
        );
    }

    /// Messages sent while subscriptions open reach each one exactly
    /// once, in order: in its replay or on its live stream, never both
    /// and never neither.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn nothing_is_missed_or_repeated_between_replay_and_live() {
        const SENT: usize = 300;
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        for n in 0..6 {
            register(&daemon, &format!("reader-{n}"), &dir.path().join("work")).await;
        }
        let sender = {
            let daemon = daemon.clone();
            tokio::spawn(async move {
                let mut said = Vec::new();
                for n in 0..SENT {
                    said.push(post(&daemon, "user", "race/t", &n.to_string()).await);
                }
                said
            })
        };
        let mut opened = Vec::new();
        for n in 0..6 {
            tokio::time::sleep(std::time::Duration::from_millis(2 * n)).await;
            let (mut subscription, receiver) = daemon
                .subscribe(Some(&format!("reader-{n}")), vec!["race/#".into()], None)
                .unwrap();
            let replay = subscription.take_topic_replay();
            opened.push((subscription, receiver, replay));
        }
        let said = sender.await.unwrap();
        for (subscription, mut receiver, replay) in opened {
            assert_eq!(replay.skipped, 0);
            let mut seen: Vec<(u64, MessageId)> = replay
                .messages
                .into_iter()
                .map(|(seq, message)| (seq, message.id))
                .collect();
            while let Ok(posted) = receiver.try_recv() {
                if subscription.wants(&posted) {
                    seen.push((posted.topic.unwrap().seq, posted.envelope.id));
                }
            }
            assert!(
                seen.windows(2).all(|pair| pair[0].0 < pair[1].0),
                "in order, none twice"
            );
            assert_eq!(
                seen.into_iter().map(|(_, id)| id).collect::<Vec<_>>(),
                said,
                "every one of them"
            );
        }
    }
}
