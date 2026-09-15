//! Conversations for readers: the archive and the cursors, over the queues
//! agents consume. `send` archives every message beside the inbox rows it
//! writes (`publish_question`); this module answers what a reader sees of
//! a conversation, hands out history and threads, moves read cursors, and
//! keeps the archive bounded on the minute tick.
use std::collections::{BTreeMap, HashMap};

use agentdocker_core::channel::ChannelSubject;
use agentdocker_core::config::{DaemonConfig, FILE_NAME, RETENTION_BATCH};
use agentdocker_core::conversation::line_of;
use agentdocker_core::{
    ArchivedMessage, ConversationId, ConversationKind, ConversationSummary, ErrorCode, EventKind,
    HUMAN, ReadCursor, Response,
};
use chrono::Utc;

use super::*;
use crate::store::CONVERSATION_CAP;

impl Daemon {
    /// Who is reading: the named agent, or the person.
    fn reader_id(&self, reader: Option<&str>) -> Result<AgentId, Box<Response>> {
        let state = lock(&self.state);
        match reader {
            Some(reference) => state
                .registry
                .resolve(reference)
                .map_err(|e| Box::new(registry_error(e))),
            None => Ok(state
                .registry
                .resolve(HUMAN)
                .unwrap_or_else(|_| AgentId::from(HUMAN.to_owned()))),
        }
    }

    pub(super) async fn conversations(
        &self,
        project: Option<String>,
        reader: Option<String>,
    ) -> Response {
        let reader = match self.reader_id(reader.as_deref()) {
            Ok(id) => id,
            Err(response) => return *response,
        };
        let project = match project {
            Some(selector) => match self.resolve_project(&selector).await {
                Ok(id) => Some(id),
                Err(response) => return *response,
            },
            None => None,
        };
        let state = lock(&self.state);
        match state.conversations_for(&reader, project.as_ref()) {
            Ok(conversations) => Response::Conversations { conversations },
            Err(error) => *error,
        }
    }

    pub(super) fn history(
        &self,
        conversation: ConversationId,
        before_seq: Option<u64>,
        limit: usize,
    ) -> Response {
        let state = lock(&self.state);
        match state.store.history(&conversation, before_seq, limit) {
            Ok(messages) => Response::History { messages },
            Err(error) => Response::error(ErrorCode::StorageUnavailable, error.to_string()),
        }
    }

    pub(super) fn thread(&self, message: MessageId) -> Response {
        let state = lock(&self.state);
        let root = match state.store.archived(&message) {
            Ok(Some(root)) => root,
            Ok(None) => {
                return Response::error(
                    ErrorCode::NotFound,
                    "no archived message has that id; it may have been pruned",
                );
            }
            Err(error) => return Response::error(ErrorCode::StorageUnavailable, error.to_string()),
        };
        match state
            .store
            .thread_replies(&root.envelope.id, &root.conversation, 500)
        {
            Ok(replies) => Response::Thread { root, replies },
            Err(error) => Response::error(ErrorCode::StorageUnavailable, error.to_string()),
        }
    }

    pub(super) fn mark_read(
        &self,
        conversation: ConversationId,
        through: u64,
        reader: Option<String>,
    ) -> Response {
        let reader = match self.reader_id(reader.as_deref()) {
            Ok(id) => id,
            Err(response) => return *response,
        };
        lock(&self.state).mark_read(&reader, &conversation, through, Utc::now())
    }

    pub(super) async fn search_messages(
        &self,
        query: String,
        project: Option<String>,
        before_seq: Option<u64>,
        limit: usize,
    ) -> Response {
        let query = query.trim().to_owned();
        if query.is_empty() || query.len() > 256 {
            return Response::error(ErrorCode::Invalid, "a search needs 1 to 256 characters");
        }
        let project = match project {
            Some(selector) => match self.resolve_project(&selector).await {
                Ok(id) => Some(id),
                Err(response) => return *response,
            },
            None => None,
        };
        let state = lock(&self.state);
        let scope: Option<Vec<ConversationId>> = project
            .as_ref()
            .map(|project| state.conversation_ids_in(project).into_iter().collect());
        match state
            .store
            .search_messages(&query, scope.as_deref(), before_seq, limit)
        {
            Ok(messages) => Response::History { messages },
            Err(error) => Response::error(ErrorCode::StorageUnavailable, error.to_string()),
        }
    }

    /// Once a minute: drop archived messages past `[messages] retention`
    /// and past the per-conversation cap, in bounded batches.
    pub fn apply_message_retention(&self) {
        let path = self.home.join(FILE_NAME);
        let window = match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<DaemonConfig>(&text)
                .map_err(|e| e.to_string())
                .and_then(|config| config.messages_retention())
            {
                Ok(window) => window,
                // The journal tick reports a broken file; this one stays quiet.
                Err(_) => return,
            },
            Err(_) => None,
        };
        let cutoff = window.and_then(|window| Utc::now().checked_sub_signed(window));
        let mut state = lock(&self.state);
        let removed = state.store_op("messages", |store| {
            store.prune_messages(cutoff, CONVERSATION_CAP, RETENTION_BATCH)
        });
        if let Some(removed) = removed
            && removed > 0
        {
            info!(removed, "pruned the message archive");
            state.emit(EventKind::MessagesPruned { removed });
        }
    }
}

impl State {
    /// Every conversation of a project that a reader could be shown: the
    /// project's broadcast, its open channels, the reader's direct
    /// conversations with its live agents, and anything in the archive.
    pub(super) fn conversation_ids_in(&self, project: &ProjectId) -> Vec<ConversationId> {
        let mut ids: Vec<ConversationId> = vec![ConversationId::everyone(project)];
        for channel in self.channels.values() {
            if channel.project == *project {
                ids.push(ConversationId::channel(&channel.id));
            }
        }
        let in_project: Vec<&AgentRecord> = self
            .registry
            .all()
            .filter(|a| a.project.as_ref().is_some_and(|p| p.id() == *project))
            .collect();
        for a in &in_project {
            ids.push(ConversationId::notices(&a.id));
        }
        for (i, a) in in_project.iter().enumerate() {
            for b in in_project.iter().skip(i + 1) {
                ids.push(ConversationId::dm(a.id.as_str(), b.id.as_str()));
            }
        }
        // A person without a project record still talks to everyone in it.
        if let Ok(human) = self.registry.resolve(HUMAN) {
            for a in &in_project {
                if a.id != human {
                    ids.push(ConversationId::dm(a.id.as_str(), human.as_str()));
                }
            }
        }
        ids.sort();
        ids.dedup();
        ids
    }

    /// The name a conversation shows and who is in it.
    fn describe(
        &self,
        conversation: &ConversationId,
        reader: &AgentId,
    ) -> Option<(ConversationKind, Option<String>, String, Vec<AgentId>)> {
        let name_of = |id: &AgentId| {
            self.registry
                .get(id)
                .map(|r| r.spec.name.clone())
                .unwrap_or_else(|| id.short().to_owned())
        };
        match conversation.kind()? {
            ConversationKind::Everyone => {
                let project = conversation.everyone_project()?;
                let members: Vec<AgentId> = self
                    .registry
                    .live()
                    .filter(|a| a.project.as_ref().is_some_and(|p| p.id() == project))
                    .map(|a| a.id.clone())
                    .collect();
                let title = self
                    .registry
                    .all()
                    .find_map(|a| {
                        a.project
                            .as_ref()
                            .filter(|p| p.id() == project)
                            .map(ProjectRef::name)
                    })
                    .unwrap_or_else(|| project.short().to_owned());
                Some((
                    ConversationKind::Everyone,
                    Some("everyone".to_owned()),
                    title,
                    members,
                ))
            }
            ConversationKind::All => Some((
                ConversationKind::All,
                Some("all".to_owned()),
                "Every project".to_owned(),
                self.registry.live().map(|a| a.id.clone()).collect(),
            )),
            ConversationKind::Channel | ConversationKind::Collision => {
                let channel = self.channels.get(&conversation.channel_id()?)?;
                let kind = match channel.subject {
                    ChannelSubject::Contested { .. } => ConversationKind::Collision,
                    ChannelSubject::Task { .. } => ConversationKind::Channel,
                };
                Some((
                    kind,
                    channel.name.clone(),
                    channel.title(),
                    channel.members.clone(),
                ))
            }
            ConversationKind::Dm => {
                let (a, b) = conversation.dm_parties()?;
                let (a, b) = (AgentId::from(a.to_owned()), AgentId::from(b.to_owned()));
                let other = if a == *reader { &b } else { &a };
                Some((
                    ConversationKind::Dm,
                    None,
                    name_of(other),
                    vec![a.clone(), b.clone()],
                ))
            }
            ConversationKind::Notices => {
                let agent = conversation.notices_agent()?;
                Some((
                    ConversationKind::Notices,
                    None,
                    format!("AgentDocker to {}", name_of(&agent)),
                    vec![agent],
                ))
            }
        }
    }

    /// The conversations a reader sees, newest activity first; those with
    /// nothing said yet after the rest, so the broadcast and the live
    /// direct conversations are always there to start.
    pub(super) fn conversations_for(
        &self,
        reader: &AgentId,
        project: Option<&ProjectId>,
    ) -> Result<Vec<ConversationSummary>, Box<Response>> {
        let storage = |e: anyhow::Error| {
            Box::new(Response::error(
                ErrorCode::StorageUnavailable,
                e.to_string(),
            ))
        };
        let heads: HashMap<ConversationId, ArchivedMessage> = self
            .store
            .conversation_heads()
            .map_err(storage)?
            .into_iter()
            .map(|m| (m.conversation.clone(), m))
            .collect();
        let cursors: BTreeMap<ConversationId, u64> = self
            .store
            .read_cursors(reader.as_str())
            .map_err(storage)?
            .into_iter()
            .map(|c| (c.conversation, c.through))
            .collect();
        // Which conversations to list at all.
        let mut candidates: Vec<ConversationId> = match project {
            Some(project) => self
                .conversation_ids_in(project)
                .into_iter()
                .filter(|id| {
                    // Direct conversations of others are theirs; a reader
                    // sees its own, plus every room and broadcast.
                    match id.kind() {
                        Some(ConversationKind::Dm) | Some(ConversationKind::Notices) => {
                            id.is_party(reader.as_str()) || heads.contains_key(id)
                        }
                        _ => true,
                    }
                })
                .collect(),
            None => Vec::new(),
        };
        for id in heads.keys() {
            let in_scope = match project {
                Some(project) => match id.kind() {
                    Some(ConversationKind::Everyone) => {
                        id.everyone_project().as_ref() == Some(project)
                    }
                    Some(ConversationKind::All) => true,
                    Some(ConversationKind::Channel) | Some(ConversationKind::Collision) => id
                        .channel_id()
                        .and_then(|c| self.channels.get(&c))
                        .is_some_and(|c| c.project == *project),
                    _ => candidates.contains(id),
                },
                None => true,
            };
            if in_scope && !candidates.contains(id) {
                candidates.push(id.clone());
            }
        }
        // Without a project: the reader's own direct conversations and the
        // broadcasts of every project, plus every room.
        if project.is_none() {
            candidates.push(ConversationId::all());
            let mut projects: Vec<ProjectId> = self
                .registry
                .live()
                .filter_map(|a| a.project.as_ref().map(ProjectRef::id))
                .collect();
            projects.sort();
            projects.dedup();
            for project in projects {
                let mut ids = self.conversation_ids_in(&project);
                ids.retain(|id| match id.kind() {
                    Some(ConversationKind::Dm) | Some(ConversationKind::Notices) => {
                        id.is_party(reader.as_str()) || heads.contains_key(id)
                    }
                    _ => true,
                });
                candidates.extend(ids);
            }
            candidates.sort();
            candidates.dedup();
        }
        let mut summaries = Vec::new();
        for id in candidates {
            let Some((kind, name, title, members)) = self.describe(&id, reader) else {
                continue;
            };
            // Only what the reader is in: rooms and broadcasts by
            // membership or project, direct ones by party.
            let mine = match kind {
                ConversationKind::Dm | ConversationKind::Notices => id.is_party(reader.as_str()),
                ConversationKind::Channel | ConversationKind::Collision => {
                    members.contains(reader) || heads.contains_key(&id)
                }
                ConversationKind::Everyone | ConversationKind::All => true,
            };
            if !mine {
                continue;
            }
            let after = cursors.get(&id).copied().unwrap_or(0);
            let unread = self
                .store
                .unread_after(&id, after, reader.as_str())
                .map_err(storage)?;
            let head = heads.get(&id);
            summaries.push(ConversationSummary {
                conversation: id,
                kind,
                name,
                title,
                members,
                unread,
                last_seq: head.map(|m| m.seq),
                last_at: head.map(|m| m.envelope.sent_at),
                last_from: head.map(|m| m.envelope.from.clone()),
                last_line: head.map(|m| {
                    let line = line_of(&m.envelope);
                    line.chars().take(160).collect()
                }),
            });
        }
        summaries.sort_by(|a, b| {
            b.last_at
                .cmp(&a.last_at)
                .then_with(|| a.title.cmp(&b.title))
        });
        Ok(summaries)
    }

    /// Move a reader's cursor to `through`, which must be an archived seq
    /// of that conversation and not before the cursor already there;
    /// acknowledge the reader's own queued rows the cursor covers.
    pub(super) fn mark_read(
        &mut self,
        reader: &AgentId,
        conversation: &ConversationId,
        through: u64,
        now: DateTime<Utc>,
    ) -> Response {
        if let Some(error) = self.storage_failure() {
            return error;
        }
        match self.store.seq_in_conversation(conversation, through) {
            Ok(true) => {}
            Ok(false) => {
                return Response::error(
                    ErrorCode::Invalid,
                    "that seq is not an archived message of this conversation",
                );
            }
            Err(error) => return Response::error(ErrorCode::StorageUnavailable, error.to_string()),
        }
        let current = match self.store.read_cursors(reader.as_str()) {
            Ok(cursors) => cursors
                .into_iter()
                .find(|c| c.conversation == *conversation)
                .map(|c| c.through)
                .unwrap_or(0),
            Err(error) => return Response::error(ErrorCode::StorageUnavailable, error.to_string()),
        };
        if through <= current {
            // Read already: a cursor never moves back.
            return Response::Ok;
        }
        let covered = match self.store.message_ids_through(conversation, through) {
            Ok(ids) => ids,
            Err(error) => return Response::error(ErrorCode::StorageUnavailable, error.to_string()),
        };
        let acknowledged: Vec<MessageId> = self
            .inboxes
            .get(reader)
            .into_iter()
            .flatten()
            .filter(|m| covered.contains(&m.id))
            .map(|m| m.id.clone())
            .collect();
        let cursor = ReadCursor {
            reader: reader.clone(),
            conversation: conversation.clone(),
            through,
            updated_at: now,
        };
        let mut event = Event::new(
            EventKind::ConversationRead {
                reader: reader.clone(),
                conversation: conversation.clone(),
                through,
            },
            now,
        );
        event.seq = self.next_seq;
        self.persist("read cursor", |store| {
            store.mark_read(&cursor, &acknowledged, &event)
        });
        if let Some(error) = self.storage_failure() {
            return error;
        }
        if !acknowledged.is_empty()
            && let Some(queue) = self.inboxes.get_mut(reader)
        {
            let bytes = self.inbox_bytes.entry(reader.clone()).or_default();
            queue.retain(|message| {
                if acknowledged.contains(&message.id) {
                    *bytes = bytes.saturating_sub(message_bytes(message));
                    false
                } else {
                    true
                }
            });
            self.forget_delivered(reader, &acknowledged);
        }
        self.next_seq += 1;
        let _ = self.events.send(event);
        Response::Ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::AgentSpec;
    use tempfile::TempDir;

    fn open(dir: &TempDir) -> Arc<Daemon> {
        let home = dir.path().to_path_buf();
        Arc::new(Daemon::open(home.clone(), home.join("sock")).unwrap())
    }

    async fn register(daemon: &Arc<Daemon>, name: &str, workdir: &std::path::Path) -> AgentRecord {
        let Response::Agent { agent } = daemon
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
        else {
            panic!("registration failed")
        };
        agent
    }

    async fn send(
        daemon: &Arc<Daemon>,
        from: &str,
        to: &str,
        text: &str,
        reply_to: Option<MessageId>,
    ) -> MessageId {
        match daemon
            .handle(Request::Send {
                from: from.into(),
                to: to.into(),
                kind: "chat".into(),
                payload: serde_json::json!({ "text": text }),
                reply_to,
            })
            .await
        {
            Response::Sent { message, .. } => message,
            other => panic!("{other:?}"),
        }
    }

    async fn conversations(daemon: &Arc<Daemon>, reader: &str) -> Vec<ConversationSummary> {
        match daemon
            .handle(Request::Conversations {
                project: None,
                reader: Some(reader.into()),
            })
            .await
        {
            Response::Conversations { conversations } => conversations,
            other => panic!("{other:?}"),
        }
    }

    async fn history(daemon: &Arc<Daemon>, conversation: &ConversationId) -> Vec<ArchivedMessage> {
        match daemon
            .handle(Request::History {
                conversation: conversation.clone(),
                before_seq: None,
                limit: 100,
            })
            .await
        {
            Response::History { messages } => messages,
            other => panic!("{other:?}"),
        }
    }

    /// Every message is archived beside the queue it is delivered to, in
    /// its one conversation; history reads it back oldest first with reply
    /// counts, and a thread is its root and replies.
    #[tokio::test]
    async fn messages_are_archived_in_their_conversation_with_threads() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let alice = register(&daemon, "alice", &work).await;
        let bob = register(&daemon, "bob", &work).await;
        let first = send(&daemon, "alice", "bob", "hello bob", None).await;
        let reply = send(&daemon, "bob", "alice", "hello alice", Some(first.clone())).await;
        let dm = ConversationId::dm(alice.id.as_str(), bob.id.as_str());
        let archived = history(&daemon, &dm).await;
        assert_eq!(
            archived
                .iter()
                .map(|m| m.envelope.id.clone())
                .collect::<Vec<_>>(),
            vec![first.clone(), reply.clone()],
            "one conversation whichever way it went, oldest first"
        );
        assert_eq!(archived[0].replies, 1, "the root counts its reply");
        assert!(archived[0].seq < archived[1].seq);
        let Response::Thread { root, replies } = daemon
            .handle(Request::Thread {
                message: first.clone(),
            })
            .await
        else {
            panic!("thread failed");
        };
        assert_eq!(root.envelope.id, first);
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].envelope.id, reply);
        // A reply from another conversation is not part of the thread.
        let project = alice.project.clone().unwrap();
        let elsewhere = send(
            &daemon,
            "alice",
            &format!("project:{}", project.id()),
            "also this",
            Some(first.clone()),
        )
        .await;
        let everyone = history(&daemon, &ConversationId::everyone(&project.id())).await;
        assert_eq!(everyone.len(), 1);
        assert_eq!(everyone[0].envelope.id, elsewhere);
        assert_eq!(
            everyone[0].envelope.reply_to,
            Some(first.clone()),
            "kept as data"
        );
        let Response::Thread { replies, .. } =
            daemon.handle(Request::Thread { message: first }).await
        else {
            panic!()
        };
        assert_eq!(replies.len(), 1, "still one reply in the thread");
        // A topic is a stream, not a conversation.
        send(&daemon, "alice", "topic:t/x", "streamed", None).await;
        assert!(matches!(
            daemon
                .handle(Request::Thread {
                    message: MessageId::generate()
                })
                .await,
            Response::Error {
                code: ErrorCode::NotFound,
                ..
            }
        ));
    }

    /// A reader sees each conversation once with the count past its own
    /// cursor; reading through a seq moves the cursor forward, never back,
    /// takes only that reader's queued rows in that conversation, and a
    /// seq of another conversation is refused.
    #[tokio::test]
    async fn read_cursors_count_unread_per_conversation_and_never_regress() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let alice = register(&daemon, "alice", &work).await;
        let bob = register(&daemon, "bob", &work).await;
        let carol = register(&daemon, "carol", &work).await;
        let project = alice.project.clone().unwrap().id();
        send(&daemon, "alice", "bob", "one", None).await;
        send(&daemon, "alice", "bob", "two", None).await;
        let own = send(&daemon, "bob", "alice", "mine", None).await;
        send(
            &daemon,
            "carol",
            &format!("project:{}", project),
            "hi all",
            None,
        )
        .await;
        let list = conversations(&daemon, "bob").await;
        let dm = ConversationId::dm(alice.id.as_str(), bob.id.as_str());
        let with_alice = list
            .iter()
            .find(|c| c.conversation == dm)
            .expect("bob's conversation with alice");
        assert_eq!(with_alice.kind, ConversationKind::Dm);
        assert_eq!(with_alice.title, "alice");
        assert_eq!(with_alice.unread, 2, "own words do not count");
        assert_eq!(with_alice.last_line.as_deref(), Some("mine"));
        let everyone = list
            .iter()
            .find(|c| c.conversation == ConversationId::everyone(&project))
            .expect("the project's broadcast");
        assert_eq!(everyone.name.as_deref(), Some("everyone"));
        assert_eq!(everyone.unread, 1);
        assert!(
            !list.iter().any(|c| c.conversation == ConversationId::dm(alice.id.as_str(), carol.id.as_str())),
            "another pair's direct conversation is not bob's to see"
        );
        // Bob's queue holds alice's two and carol's broadcast.
        let queued = |daemon: &Arc<Daemon>| lock(&daemon.state).inboxes[&bob.id].len();
        assert_eq!(queued(&daemon), 3);
        let archived = history(&daemon, &dm).await;
        let first_seq = archived[0].seq;
        let last_seq = archived[2].seq;
        assert!(matches!(
            daemon
                .handle(Request::MarkRead {
                    conversation: dm.clone(),
                    through: first_seq,
                    reader: Some("bob".into()),
                })
                .await,
            Response::Ok
        ));
        assert_eq!(
            queued(&daemon),
            2,
            "the row the cursor covers left bob's queue"
        );
        let with_alice = conversations(&daemon, "bob")
            .await
            .into_iter()
            .find(|c| c.conversation == dm)
            .unwrap();
        assert_eq!(with_alice.unread, 1);
        // Through the end: the second row goes, bob's own was never queued.
        assert!(matches!(
            daemon
                .handle(Request::MarkRead {
                    conversation: dm.clone(),
                    through: last_seq,
                    reader: Some("bob".into()),
                })
                .await,
            Response::Ok
        ));
        assert_eq!(
            queued(&daemon),
            1,
            "carol's broadcast is another conversation"
        );
        let with_alice = conversations(&daemon, "bob")
            .await
            .into_iter()
            .find(|c| c.conversation == dm)
            .unwrap();
        assert_eq!(with_alice.unread, 0);
        // Backwards is a no-op; a foreign seq is refused.
        let seq_before = lock(&daemon.state).next_seq;
        assert!(matches!(
            daemon
                .handle(Request::MarkRead {
                    conversation: dm.clone(),
                    through: first_seq,
                    reader: Some("bob".into()),
                })
                .await,
            Response::Ok
        ));
        assert_eq!(lock(&daemon.state).next_seq, seq_before, "nothing recorded");
        let broadcast_seq = history(&daemon, &ConversationId::everyone(&project)).await[0].seq;
        assert!(matches!(
            daemon
                .handle(Request::MarkRead {
                    conversation: dm.clone(),
                    through: broadcast_seq,
                    reader: Some("bob".into()),
                })
                .await,
            Response::Error {
                code: ErrorCode::Invalid,
                ..
            }
        ));
        // Later arrivals stay unread.
        send(&daemon, "alice", "bob", "three", None).await;
        let with_alice = conversations(&daemon, "bob")
            .await
            .into_iter()
            .find(|c| c.conversation == dm)
            .unwrap();
        assert_eq!(with_alice.unread, 1);
        let _ = own;
        // Search finds by text, scoped to the project.
        let Response::History { messages } = daemon
            .handle(Request::SearchMessages {
                query: "three".into(),
                project: Some(work.display().to_string()),
                before_seq: None,
                limit: 10,
            })
            .await
        else {
            panic!("search failed");
        };
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].conversation, dm);
    }

    /// A named channel is a conversation people can find by name; a
    /// collision room is listed apart; and the daemon's own notices to an
    /// agent are that agent's conversation with AgentDocker.
    #[tokio::test]
    async fn channels_have_names_and_notices_are_their_own_conversation() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let alice = register(&daemon, "alice", &work).await;
        register(&daemon, "bob", &work).await;
        let Response::Channel { channel } = daemon
            .handle(Request::ChannelOpen {
                agent: "alice".into(),
                task: "Plan the release".into(),
                members: vec!["bob".into()],
                name: None,
            })
            .await
        else {
            panic!("open failed");
        };
        assert_eq!(
            channel.name.as_deref(),
            Some("plan-the-release"),
            "named from the task"
        );
        assert!(matches!(
            daemon
                .handle(Request::ChannelOpen {
                    agent: "alice".into(),
                    task: "Another".into(),
                    members: vec!["bob".into()],
                    name: Some("plan-the-release".into()),
                })
                .await,
            Response::Error {
                code: ErrorCode::Conflict,
                ..
            }
        ));
        assert!(matches!(
            daemon
                .handle(Request::ChannelOpen {
                    agent: "alice".into(),
                    task: "Another".into(),
                    members: vec!["bob".into()],
                    name: Some("Not A Slug".into()),
                })
                .await,
            Response::Error {
                code: ErrorCode::Invalid,
                ..
            }
        ));
        send(
            &daemon,
            "alice",
            &format!("channel:{}", channel.id),
            "first in the room",
            None,
        )
        .await;
        let list = conversations(&daemon, "bob").await;
        let room = list
            .iter()
            .find(|c| c.conversation == ConversationId::channel(&channel.id))
            .expect("the channel");
        assert_eq!(room.kind, ConversationKind::Channel);
        assert_eq!(room.name.as_deref(), Some("plan-the-release"));
        assert_eq!(room.unread, 2, "the opening notice and alice's message");
        // The daemon's own words to an agent: a notices conversation.
        lock(&daemon.state).send(
            "agentd".into(),
            Destination::Agent(alice.id.clone()),
            "stale".into(),
            serde_json::json!({ "text": "x changed" }),
            None,
        );
        let list = conversations(&daemon, "alice").await;
        let notices = list
            .iter()
            .find(|c| c.conversation == ConversationId::notices(&alice.id))
            .expect("notices");
        assert_eq!(notices.kind, ConversationKind::Notices);
        assert_eq!(notices.unread, 1);
        assert!(
            !conversations(&daemon, "bob")
                .await
                .iter()
                .any(|c| c.conversation == ConversationId::notices(&alice.id)),
            "alice's notices are not bob's"
        );
    }
}
