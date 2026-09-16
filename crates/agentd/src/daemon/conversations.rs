//! Conversations for readers: the archive and the cursors, over the queues
//! agents consume. `send` archives every message beside the inbox rows it
//! writes (`publish_question`); this module answers what a reader sees of
//! a conversation, hands out history and threads, moves read cursors, and
//! keeps the archive bounded on the minute tick.
use std::collections::{BTreeMap, BTreeSet, HashMap};

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

    pub(super) fn thread(
        &self,
        message: MessageId,
        after_seq: Option<u64>,
        limit: usize,
    ) -> Response {
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
            .thread_replies(&root.envelope.id, &root.conversation, after_seq, limit)
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
        reader: Option<String>,
        before_seq: Option<u64>,
        limit: usize,
    ) -> Response {
        let query = query.trim().to_owned();
        if query.is_empty() || query.len() > 256 {
            return Response::error(ErrorCode::Invalid, "a search needs 1 to 256 characters");
        }
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
        // What the reader could list is what the reader can search: the
        // same visibility, in the project or everywhere.
        let scope: Vec<ConversationId> = match state.conversations_for(&reader, project.as_ref()) {
            Ok(summaries) => summaries.into_iter().map(|s| s.conversation).collect(),
            Err(response) => return *response,
        };
        match state
            .store
            .search_messages(&query, Some(&scope), before_seq, limit)
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
        // Live records only: a finished agent's conversations are in the
        // archive, whose heads are added below, and pairing every record
        // ever registered is quadratic in a long-lived project.
        let in_project: Vec<&AgentRecord> = self
            .registry
            .live()
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
        // A former identity reads as the record it was retired into.
        let name_of = |id: &AgentId| {
            let id = self.registry.canonical_id(id);
            self.registry
                .get(id)
                .map(|r| r.spec.name.clone())
                .unwrap_or_else(|| id.short().to_owned())
        };
        let reader = self.registry.canonical_id(reader);
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
                let other = if self.registry.canonical_id(&a) == reader {
                    &b
                } else {
                    &a
                };
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

    /// The person: their record, or the bare `user` id a reader falls back
    /// to before anyone has registered them.
    fn is_human_id(&self, id: &AgentId) -> bool {
        id.as_str() == HUMAN || self.registry.get(id).is_some_and(super::humans::is_human)
    }

    /// Whether an archived direct conversation or notices belong to the
    /// project: a party of it has, or had, a record there.
    fn archived_in_project(&self, id: &ConversationId, project: &ProjectId) -> bool {
        let in_project = |agent: &str| {
            self.registry
                .get(self.registry.canonical_id(&AgentId::from(agent.to_owned())))
                .is_some_and(|r| r.project.as_ref().is_some_and(|p| p.id() == *project))
        };
        match id.kind() {
            Some(ConversationKind::Dm) => id
                .dm_parties()
                .is_some_and(|(a, b)| in_project(a) || in_project(b)),
            Some(ConversationKind::Notices) => {
                id.notices_agent().is_some_and(|a| in_project(a.as_str()))
            }
            _ => false,
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
        // The reader is every identity it has had: a record retired into
        // it by an identity repair or a resumed provider session keeps its
        // archived conversations, and they are the reader's to see.
        let reader = self.registry.canonical_id(reader);
        let identities = self.registry.identity_ids(reader);
        let is_reader = |id: &ConversationId| identities.iter().any(|me| id.is_party(me.as_str()));
        let heads: HashMap<ConversationId, ArchivedMessage> = self
            .store
            .conversation_heads()
            .map_err(storage)?
            .into_iter()
            .map(|m| (m.conversation.clone(), m))
            .collect();
        // Read through the furthest cursor any of those identities left.
        let mut cursors: BTreeMap<ConversationId, u64> = BTreeMap::new();
        for me in &identities {
            for cursor in self.store.read_cursors(me.as_str()).map_err(storage)? {
                let through = cursors.entry(cursor.conversation).or_insert(0);
                *through = (*through).max(cursor.through);
            }
        }
        let human = self.is_human_id(reader);
        // Which conversations to list at all.
        let mut candidates: BTreeSet<ConversationId> = match project {
            Some(project) => self
                .conversation_ids_in(project)
                .into_iter()
                .filter(|id| {
                    // Direct conversations of others are theirs; a reader
                    // sees its own, plus every room and broadcast. The
                    // person sees the project's, to read.
                    match id.kind() {
                        Some(ConversationKind::Dm) | Some(ConversationKind::Notices) => {
                            is_reader(id) || human
                        }
                        _ => true,
                    }
                })
                .collect(),
            None => BTreeSet::new(),
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
                    // Membership is checked below; being the reader's own
                    // does not move a conversation into every project.
                    _ => self.archived_in_project(id, project),
                },
                None => {
                    is_reader(id)
                        || human
                        || !matches!(
                            id.kind(),
                            Some(ConversationKind::Dm) | Some(ConversationKind::Notices)
                        )
                }
            };
            if in_scope {
                candidates.insert(id.clone());
            }
        }
        // Without a project: the reader's own direct conversations and the
        // broadcasts of every project, plus every room.
        if project.is_none() {
            candidates.insert(ConversationId::all());
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
                        is_reader(id) || human
                    }
                    _ => true,
                });
                candidates.extend(ids);
            }
        }
        let mut summaries = Vec::new();
        for id in candidates {
            let Some((kind, name, title, members)) = self.describe(&id, reader) else {
                continue;
            };
            // Only what the reader is in: direct ones by party, rooms by
            // membership. The person is in no room and sees every one of
            // the project's, as the Channels screen does; nothing lets an
            // agent outside a room read it because it has an archive.
            let mine = match kind {
                ConversationKind::Dm | ConversationKind::Notices => is_reader(&id) || human,
                ConversationKind::Channel | ConversationKind::Collision => {
                    members.contains(reader) || human
                }
                ConversationKind::Everyone | ConversationKind::All => true,
            };
            if !mine {
                continue;
            }
            let after = cursors.get(&id).copied().unwrap_or(0);
            let unread = self
                .store
                .unread_after(&id, after, &identities)
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
        // The reader is every identity it has had, as the list counts
        // unread: a mark below what any of them already read is a no-op.
        let reader = self.registry.canonical_id(reader).clone();
        let mut current = 0;
        for me in self.registry.identity_ids(&reader) {
            match self.store.read_cursors(me.as_str()) {
                Ok(cursors) => {
                    current = current.max(
                        cursors
                            .into_iter()
                            .find(|c| c.conversation == *conversation)
                            .map(|c| c.through)
                            .unwrap_or(0),
                    );
                }
                Err(error) => {
                    return Response::error(ErrorCode::StorageUnavailable, error.to_string());
                }
            }
        }
        if through <= current {
            // Read already: a cursor never moves back.
            return Response::Ok;
        }
        let reader = &reader;
        // A cursor is the reader's; the queue may not be. A queue with an
        // owner (a bound controller, the daemon's own bridge) is drained
        // only by that owner's receipts, so reading moves the cursor and
        // leaves every row where it is.
        let covered = if self.input_owner(reader).is_some() {
            Vec::new()
        } else {
            match self.store.message_ids_through(conversation, through) {
                Ok(ids) => ids,
                Err(error) => {
                    return Response::error(ErrorCode::StorageUnavailable, error.to_string());
                }
            }
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

    #[tokio::test]
    async fn project_conversation_list_keeps_archived_direct_messages_in_their_project() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let first = dir.path().join("first");
        let second = dir.path().join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let alice = register(&daemon, "alice", &first).await;
        let bob = register(&daemon, "bob", &second).await;
        let Response::Agent { agent: human } = daemon.me(None).await else {
            panic!("human registration failed");
        };
        send(&daemon, HUMAN, "alice", "first project", None).await;
        send(&daemon, HUMAN, "bob", "second project", None).await;
        daemon
            .handle(Request::Deregister {
                agent: "alice".into(),
            })
            .await;
        let first_id = alice.project.as_ref().unwrap().id();
        let second_id = bob.project.as_ref().unwrap().id();
        assert_ne!(first_id, second_id);
        for (project, included, excluded) in [
            (first_id, &alice.id, &bob.id),
            (second_id, &bob.id, &alice.id),
        ] {
            let Response::Conversations { conversations } = daemon
                .handle(Request::Conversations {
                    project: Some(project.to_string()),
                    reader: Some(human.id.to_string()),
                })
                .await
            else {
                panic!("conversation list failed");
            };
            assert!(conversations.iter().any(
                |c| c.conversation == ConversationId::dm(human.id.as_str(), included.as_str())
            ));
            assert!(
                !conversations
                    .iter()
                    .any(|c| c.conversation
                        == ConversationId::dm(human.id.as_str(), excluded.as_str())),
                "another project's direct messages entered the selected project"
            );
        }
    }

    #[tokio::test]
    async fn project_search_retains_finished_agents_direct_messages_and_notices() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let work = dir.path().join("work");
        let other = dir.path().join("other");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let alice = register(&daemon, "alice", &work).await;
        register(&daemon, "bob", &other).await;
        let Response::Agent { .. } = daemon.me(None).await else {
            panic!("human registration failed");
        };
        let direct = send(&daemon, HUMAN, "alice", "retainedneedle direct", None).await;
        send(&daemon, HUMAN, "bob", "retainedneedle other project", None).await;
        lock(&daemon.state).send(
            "agentd".into(),
            Destination::Agent(alice.id.clone()),
            "notice".into(),
            serde_json::json!({"text": "retainedneedle notice"}),
            None,
        );
        daemon
            .handle(Request::Deregister {
                agent: "alice".into(),
            })
            .await;
        drop(daemon);
        let reopened = open(&dir);
        let Response::History { messages } = reopened
            .handle(Request::SearchMessages {
                query: "retainedneedle".into(),
                project: Some(alice.project.clone().unwrap().id().to_string()),
                reader: None,
                before_seq: None,
                limit: 50,
            })
            .await
        else {
            panic!("project search failed");
        };
        assert_eq!(
            messages.len(),
            2,
            "finished records stay searchable without another project's messages"
        );
        assert!(messages.iter().any(|m| m.envelope.id == direct));
        assert!(
            messages
                .iter()
                .any(|m| m.conversation == ConversationId::notices(&alice.id))
        );
        // Another agent searches only what it could list: bob is in
        // neither of those conversations, so it finds nothing of them.
        let bob = register(&reopened, "carol", &work).await;
        let Response::History { messages } = reopened
            .handle(Request::SearchMessages {
                query: "retainedneedle".into(),
                project: Some(alice.project.unwrap().id().to_string()),
                reader: Some(bob.id.to_string()),
                before_seq: None,
                limit: 50,
            })
            .await
        else {
            panic!("project search failed");
        };
        assert!(
            messages.is_empty(),
            "an agent's search does not reach conversations it is not in"
        );
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
                after_seq: None,
                limit: 100,
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
        let Response::Thread { replies, .. } = daemon
            .handle(Request::Thread {
                message: first,
                after_seq: None,
                limit: 100,
            })
            .await
        else {
            panic!()
        };
        assert_eq!(replies.len(), 1, "still one reply in the thread");
        // A topic is a stream, not a conversation.
        send(&daemon, "alice", "topic:t/x", "streamed", None).await;
        assert!(matches!(
            daemon
                .handle(Request::Thread {
                    message: MessageId::generate(),
                    after_seq: None,
                    limit: 100,
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
    /// A record retired into the reader is the reader: what it said and
    /// read counts as the reader's own, and its conversations are the
    /// reader's to see, under their old ids.
    #[tokio::test]
    async fn a_retired_identity_is_read_as_the_reader_it_became() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let alice = register(&daemon, "alice", &work).await;
        let early = register(&daemon, "early", &work).await;
        let later = register(&daemon, "later", &work).await;
        // Alice and early talk; early is retired into later.
        send(&daemon, "alice", "early", "hello early", None).await;
        send(&daemon, "early", "alice", "hello alice", None).await;
        let old_dm = ConversationId::dm(alice.id.as_str(), early.id.as_str());
        let archived = history(&daemon, &old_dm).await;
        assert_eq!(archived.len(), 2);
        assert!(matches!(
            daemon
                .handle(Request::MarkRead {
                    conversation: old_dm.clone(),
                    through: archived[0].seq,
                    reader: Some(early.id.to_string()),
                })
                .await,
            Response::Ok
        ));
        {
            let mut state = lock(&daemon.state);
            state.registry.retire_into(&early.id, &later.id).unwrap();
        }
        let list = conversations(&daemon, later.id.as_str()).await;
        let old = list
            .iter()
            .find(|c| c.conversation == old_dm)
            .expect("the retired identity's conversation is the reader's");
        assert_eq!(old.title, "alice", "the other party, not the reader");
        assert_eq!(
            old.unread, 0,
            "alice's first was read under the old identity and the second is the reader's own words"
        );
        // A mark below what the old identity already read is a no-op for
        // the reader it became: the cursor never moves back, nothing is
        // written for the reader that is, and no read is recorded.
        let cursors_of = |id: &AgentId| {
            lock(&daemon.state)
                .store
                .read_cursors(id.as_str())
                .unwrap()
                .into_iter()
                .filter(|c| c.conversation == old_dm)
                .map(|c| c.through)
                .collect::<Vec<_>>()
        };
        assert_eq!(cursors_of(&early.id), vec![archived[0].seq]);
        assert!(cursors_of(&later.id).is_empty());
        let seq_before = lock(&daemon.state).next_seq;
        assert!(matches!(
            daemon
                .handle(Request::MarkRead {
                    conversation: old_dm.clone(),
                    through: archived[0].seq,
                    reader: Some(later.id.to_string()),
                })
                .await,
            Response::Ok
        ));
        assert_eq!(lock(&daemon.state).next_seq, seq_before, "no read recorded");
        assert!(
            !daemon
                .recent_events(8)
                .iter()
                .any(|e| matches!(&e.kind, EventKind::ConversationRead { reader, .. } if *reader == later.id)),
            "no ConversationRead for the reader that is"
        );
        assert_eq!(
            cursors_of(&early.id),
            vec![archived[0].seq],
            "durable cursor unchanged"
        );
        assert!(
            cursors_of(&later.id).is_empty(),
            "nothing written for the new identity"
        );
        assert!(
            !conversations(&daemon, later.id.as_str())
                .await
                .iter()
                .any(|c| c.conversation == old_dm && c.unread != 0),
            "nothing regressed"
        );
        // New words go to the record that is: its own conversation, with
        // the old one still listed and still read.
        send(&daemon, "alice", "later", "still there?", None).await;
        let list = conversations(&daemon, later.id.as_str()).await;
        assert_eq!(
            list.iter()
                .find(|c| c.conversation == old_dm)
                .unwrap()
                .unread,
            0
        );
        let new_dm = ConversationId::dm(alice.id.as_str(), later.id.as_str());
        assert_eq!(
            list.iter()
                .find(|c| c.conversation == new_dm)
                .unwrap()
                .unread,
            1
        );
        let alices = conversations(&daemon, alice.id.as_str()).await;
        assert_eq!(
            alices
                .iter()
                .find(|c| c.conversation == old_dm)
                .unwrap()
                .title,
            "later",
            "alice sees the old conversation under the name the record has now"
        );
    }

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
                reader: None,
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

    /// A reader whose queue has an owner keeps its cursor like anyone, but
    /// reading drains nothing: the owner's receipts do that.
    #[tokio::test]
    async fn reading_never_drains_a_queue_that_has_an_owner() {
        use agentdocker_core::{ProcessIdentity, ProviderGeneration};
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let alice = register(&daemon, "alice", &work).await;
        let mut spec = AgentSpec {
            name: "receiver".into(),
            runtime: "custom".into(),
            workdir: Some(work.clone()),
            ..Default::default()
        };
        spec.labels.insert("session_id".into(), "sess".into());
        let Response::Agent { agent: receiver } = daemon
            .handle(Request::Register {
                spec,
                pid: Some(std::process::id()),
                session: None,
            })
            .await
        else {
            panic!("register failed");
        };
        let me = std::process::id();
        let identity = ProcessIdentity {
            pid: me,
            started_at: agentdocker_host::procinfo::start_time(me).unwrap(),
        };
        assert!(matches!(
            daemon
                .handle(Request::BindInput {
                    agent: receiver.id.to_string(),
                    provider: ProviderGeneration {
                        process: identity.clone(),
                        session: "sess".into(),
                        profile: "/etc/hosts".into(),
                    },
                    controller: identity,
                    token: "0123456789abcdef0123456789abcdef".into(),
                    launch: None,
                })
                .await,
            Response::InputBound { .. }
        ));
        send(&daemon, "alice", "receiver", "for the model", None).await;
        let dm = ConversationId::dm(alice.id.as_str(), receiver.id.as_str());
        let seq = history(&daemon, &dm).await[0].seq;
        assert!(matches!(
            daemon
                .handle(Request::MarkRead {
                    conversation: dm.clone(),
                    through: seq,
                    reader: Some("receiver".into()),
                })
                .await,
            Response::Ok
        ));
        assert_eq!(
            lock(&daemon.state).inboxes[&receiver.id].len(),
            1,
            "the bound queue keeps its row for the controller's receipt"
        );
        let mine = conversations(&daemon, "receiver")
            .await
            .into_iter()
            .find(|c| c.conversation == dm)
            .unwrap();
        assert_eq!(mine.unread, 0, "the cursor moved all the same");
    }
}
