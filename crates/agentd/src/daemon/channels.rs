//! Channels: the room agents share when they turn out to be on the same
//! work, and the reviews they trade in it.
//!
//! A lease keeps two agents out of one file. This is what happens when
//! they are in it anyway. The ledger already says which checkout changed
//! which path; the second checkout to touch a path is a collision, and the
//! daemon opens a room for the agents involved rather than wait for one of
//! them to notice. Inside it they talk (`send --to channel:<id>`) and
//! review each other (`review`). Reviews are the tie-break: a request for
//! changes blocks, approvals count, and only a reviewer's latest word
//! counts. When the work is final somebody closes the channel, and closed
//! channels are pruned.
//!
//! One open contested channel per project, not one per file: agents
//! colliding on `src/parser.rs` and then on `src/lexer.rs` are having one
//! conversation, so the paths accumulate on the same channel.

use super::*;
use agentdocker_core::channel::{Channel, ChannelId, ChannelSubject, Review, Verdict};

/// Contested paths remembered per project, so a second checkout touching
/// one is noticed without scanning the ledger.
const CONTESTED_PATHS: usize = 2_000;

impl State {
    /// A ledger row landed. If a second checkout of the project has now
    /// changed this path, the agents behind those checkouts belong in a
    /// room together.
    pub(super) fn note_contested(&mut self, change: &Change) {
        let Some(checkout) = change.checkout.clone() else {
            return;
        };
        let key = (change.project.clone(), change.path.clone());
        let contested: Option<Vec<PathBuf>> = {
            let seen = self.contested.entry(key).or_default();
            seen.insert(checkout);
            (seen.len() >= 2).then(|| seen.iter().cloned().collect())
        };
        if let Some(checkouts) = contested {
            self.open_contested(&change.project, &change.path, &checkouts);
        }
        // Bounded: the oldest paths are forgotten, which at worst means a
        // long-quiet collision is noticed again later.
        while self.contested.len() > CONTESTED_PATHS {
            let Some(oldest) = self.contested.keys().next().cloned() else {
                break;
            };
            self.contested.remove(&oldest);
        }
    }

    /// The live agents of a project working in any of these checkouts.
    fn agents_in(&self, project: &ProjectId, checkouts: &[PathBuf]) -> Vec<AgentRecord> {
        self.registry
            .live()
            .filter(|a| {
                a.project.as_ref().is_some_and(|p| {
                    p.id() == *project && checkouts.contains(&project::canonical(p.dir()))
                })
            })
            .cloned()
            .collect()
    }

    /// The project's open contested channel, if it has one.
    fn contested_channel(&self, project: &ProjectId) -> Option<ChannelId> {
        self.channels
            .values()
            .find(|c| {
                c.is_open()
                    && c.project == *project
                    && matches!(c.subject, ChannelSubject::Contested { .. })
            })
            .map(|c| c.id.clone())
    }

    /// Open the project's contested channel, or widen the one it has.
    fn open_contested(&mut self, project: &ProjectId, path: &Path, checkouts: &[PathBuf]) {
        let members = self.agents_in(project, checkouts);
        if members.len() < 2 {
            // One agent editing a path its own worktree also touched is
            // not a collision, and a room needs two.
            return;
        }
        let ids: Vec<AgentId> = members.iter().map(|m| m.id.clone()).collect();
        if let Some(id) = self.contested_channel(project) {
            let Some(channel) = self.channels.get_mut(&id) else {
                return;
            };
            let widened = channel.add_path(path.to_path_buf());
            let joined: Vec<AgentId> = ids
                .iter()
                .filter(|agent| !channel.has(agent))
                .cloned()
                .collect();
            for agent in &joined {
                channel.admit(agent.clone());
            }
            if !widened && joined.is_empty() {
                return;
            }
            let channel = channel.clone();
            self.persist("channel", |store| {
                store.put_document("channel", channel.id.as_str(), &channel)
            });
            for agent in joined {
                self.emit(EventKind::ChannelJoined {
                    channel: channel.id.clone(),
                    agent,
                });
            }
            if widened {
                self.tell_channel(
                    &channel,
                    format!(
                        "{} is contested too; it is part of this channel now.",
                        path.display()
                    ),
                );
            }
            return;
        }
        let channel = Channel {
            id: ChannelId::generate(),
            project: project.clone(),
            subject: ChannelSubject::Contested {
                paths: vec![path.to_path_buf()],
            },
            members: ids.clone(),
            opened_by: None,
            opened_at: Utc::now(),
            reviews: Vec::new(),
            closed_at: None,
            resolution: None,
        };
        self.install_channel(channel.clone(), &members);
        self.tell_channel(
            &channel,
            format!(
                "You are both changing {}. Talk here with `agentdocker send --to channel:{} \"…\"`, \
                 ask for review with `agentdocker review-request --as <you> {}`, and review with \
                 `agentdocker review --as <you> {} --approve`. Close it when the work is final.",
                path.display(),
                channel.id,
                channel.id,
                channel.id
            ),
        );
    }

    /// Store, announce and journal a new channel.
    fn install_channel(&mut self, channel: Channel, members: &[AgentRecord]) {
        self.channels.insert(channel.id.clone(), channel.clone());
        self.persist("channel", |store| {
            store.put_document("channel", channel.id.as_str(), &channel)
        });
        self.emit(EventKind::ChannelOpened {
            channel: channel.id.clone(),
            project: channel.project.clone(),
            title: channel.title(),
            members: channel.members.clone(),
        });
        let names: Vec<String> = members.iter().map(|m| m.spec.name.clone()).collect();
        if let Some(record) = members.first() {
            let summary = format!(
                "opened a channel on {} for {}",
                channel.title(),
                names.join(", ")
            );
            if let Some(mut entry) = self.plain_entry(
                record,
                JournalKind::Review,
                summary,
                SummarySource::Synthesised,
            ) {
                if channel.opened_by.is_none() {
                    entry.agent = None;
                    entry.agent_name = "agentd".to_owned();
                }
                self.append_journal(entry);
            }
        }
    }

    /// A notice from the daemon to everyone in a channel.
    fn tell_channel(&mut self, channel: &Channel, text: String) {
        self.send(
            "agentd".to_owned(),
            Destination::Channel(channel.id.clone()),
            "channel".to_owned(),
            json!({
                "channel": channel.id.as_str(),
                "title": channel.title(),
                "text": text,
            }),
            None,
        );
    }

    /// Members of a channel, for message routing.
    pub(super) fn channel_members(&self, id: &ChannelId) -> Vec<AgentId> {
        self.channels
            .get(id)
            .filter(|c| c.is_open())
            .map(|c| c.members.clone())
            .unwrap_or_default()
    }

    /// An agent finished: it leaves its channels, and a channel nobody is
    /// left in closes itself.
    pub(super) fn leave_channels(&mut self, agent: &AgentId) {
        let touched: Vec<ChannelId> = self
            .channels
            .values()
            .filter(|c| c.is_open() && c.has(agent))
            .map(|c| c.id.clone())
            .collect();
        let live: HashSet<AgentId> = self.registry.live().map(|a| a.id.clone()).collect();
        for id in touched {
            let Some(channel) = self.channels.get_mut(&id) else {
                continue;
            };
            let empty = !channel
                .members
                .iter()
                .any(|m| m != agent && live.contains(m));
            if empty {
                channel.closed_at = Some(Utc::now());
                channel.resolution = Some("everyone left".to_owned());
            }
            let channel = channel.clone();
            self.persist("channel", |store| {
                store.put_document("channel", channel.id.as_str(), &channel)
            });
            if empty {
                self.emit(EventKind::ChannelClosed {
                    channel: id,
                    resolution: channel.resolution.clone(),
                });
            }
        }
    }

    fn find_channel(&self, reference: &str) -> Option<&Channel> {
        self.channels.get(&ChannelId::from(reference)).or_else(|| {
            let mut matches = self
                .channels
                .values()
                .filter(|c| c.id.as_str().starts_with(reference));
            let first = matches.next()?;
            matches.next().is_none().then_some(first)
        })
    }
}

impl Daemon {
    /// A channel by id or unique prefix, and the agent asking.
    fn channel_of(
        &self,
        reference: &str,
        agent: &str,
    ) -> Result<(ChannelId, AgentId), Box<Response>> {
        let mut state = lock(&self.state);
        let id = state.resolve(agent)?;
        let Some(channel) = state.find_channel(reference) else {
            return Err(Box::new(Response::error(
                ErrorCode::NotFound,
                format!("no channel `{reference}`"),
            )));
        };
        let channel = channel.id.clone();
        if !state.channels.get(&channel).is_some_and(|c| c.has(&id)) {
            return Err(Box::new(Response::error(
                ErrorCode::Forbidden,
                "you are not in that channel",
            )));
        }
        Ok((channel, id))
    }

    pub(super) async fn channels(
        &self,
        project: &str,
        all: bool,
        agent: Option<String>,
    ) -> Response {
        let member = match agent.map(|reference| self.resolve(&reference)).transpose() {
            Ok(id) => id,
            Err(response) => return *response,
        };
        let project = if project.is_empty() {
            let own = member.as_ref().and_then(|id| {
                lock(&self.state)
                    .registry
                    .get(id)
                    .and_then(|r| r.project.as_ref().map(ProjectRef::id))
            });
            match own {
                Some(project) => project,
                None => {
                    return Response::error(
                        ErrorCode::Invalid,
                        "name a project, or an agent that works in one",
                    );
                }
            }
        } else {
            match self.resolve_project(project).await {
                Ok(id) => id,
                Err(response) => return *response,
            }
        };
        let state = lock(&self.state);
        let mut channels: Vec<Channel> = state
            .channels
            .values()
            .filter(|c| c.project == project)
            .filter(|c| all || c.is_open())
            .filter(|c| member.as_ref().is_none_or(|id| c.has(id)))
            .cloned()
            .collect();
        channels.sort_by(|a, b| a.opened_at.cmp(&b.opened_at).then_with(|| a.id.cmp(&b.id)));
        Response::Channels { channels }
    }

    /// Open a channel for a task, rather than wait for a collision.
    pub(super) fn channel_open(
        &self,
        reference: &str,
        task: String,
        members: Vec<String>,
    ) -> Response {
        let task = task.trim().to_owned();
        if task.is_empty() {
            return Response::error(ErrorCode::Invalid, "a channel needs a task");
        }
        let mut state = lock(&self.state);
        let opener = match state.resolve(reference) {
            Ok(id) => id,
            Err(e) => return *e,
        };
        let Some(record) = state.registry.get(&opener).cloned() else {
            return Response::error(ErrorCode::NotFound, "agent vanished");
        };
        let Some(project) = record.project.as_ref().map(ProjectRef::id) else {
            return Response::error(ErrorCode::Invalid, "the agent is in no project");
        };
        let mut ids = vec![opener.clone()];
        if members.is_empty() {
            // Everyone else working here.
            let others: Vec<AgentId> = state
                .registry
                .live()
                .filter(|a| a.id != opener)
                .filter(|a| a.project.as_ref().is_some_and(|p| p.id() == project))
                .map(|a| a.id.clone())
                .collect();
            ids.extend(others);
        } else {
            for reference in &members {
                match state.resolve(reference) {
                    Ok(id) if !ids.contains(&id) => ids.push(id),
                    Ok(_) => {}
                    Err(e) => return *e,
                }
            }
        }
        let records: Vec<AgentRecord> = ids
            .iter()
            .filter_map(|id| state.registry.get(id).cloned())
            .collect();
        let channel = Channel {
            id: ChannelId::generate(),
            project,
            subject: ChannelSubject::Task { task: task.clone() },
            members: ids,
            opened_by: Some(opener),
            opened_at: Utc::now(),
            reviews: Vec::new(),
            closed_at: None,
            resolution: None,
        };
        state.install_channel(channel.clone(), &records);
        state.tell_channel(
            &channel,
            format!("{} opened this channel: {task}", record.spec.name),
        );
        if let Some(error) = state.storage_failure() {
            return error;
        }
        Response::Channel { channel }
    }

    /// The work is final.
    pub(super) fn channel_close(
        &self,
        reference: &str,
        channel: &str,
        resolution: Option<String>,
    ) -> Response {
        let (id, agent) = match self.channel_of(channel, reference) {
            Ok(v) => v,
            Err(e) => return *e,
        };
        let mut state = lock(&self.state);
        let name = state
            .registry
            .get(&agent)
            .map(|r| r.spec.name.clone())
            .unwrap_or_else(|| agent.short().to_owned());
        let Some(channel) = state.channels.get_mut(&id) else {
            return Response::error(ErrorCode::NotFound, "channel vanished");
        };
        if !channel.is_open() {
            return Response::Channel {
                channel: channel.clone(),
            };
        }
        let resolution = resolution
            .map(|r| r.trim().to_owned())
            .filter(|r| !r.is_empty());
        channel.closed_at = Some(Utc::now());
        channel.resolution = resolution.clone();
        let channel = channel.clone();
        state.persist("channel", |store| {
            store.put_document("channel", channel.id.as_str(), &channel)
        });
        state.tell_channel(
            &channel,
            match &resolution {
                Some(text) => format!("{name} closed this channel: {text}"),
                None => format!("{name} closed this channel."),
            },
        );
        state.emit(EventKind::ChannelClosed {
            channel: id,
            resolution: resolution.clone(),
        });
        if let Some(record) = state.registry.get(&agent).cloned() {
            let summary = match &resolution {
                Some(text) => format!("closed the channel on {}: {text}", channel.title()),
                None => format!("closed the channel on {}", channel.title()),
            };
            if let Some(entry) = state.plain_entry(
                &record,
                JournalKind::Review,
                summary,
                SummarySource::Explicit,
            ) {
                state.append_journal(entry);
            }
        }
        if let Some(error) = state.storage_failure() {
            return error;
        }
        Response::Channel { channel }
    }

    /// Forget channels closed longer ago than `before_secs`.
    pub(super) async fn channel_prune(&self, project: &str, before_secs: u64) -> Response {
        // No project named is housekeeping across all of them.
        let project = if project.is_empty() {
            None
        } else {
            match self.resolve_project(project).await {
                Ok(id) => Some(id),
                Err(response) => return *response,
            }
        };
        let cutoff = Utc::now() - Duration::seconds(i64::try_from(before_secs).unwrap_or(i64::MAX));
        let mut state = lock(&self.state);
        let gone: Vec<ChannelId> = state
            .channels
            .values()
            .filter(|c| project.as_ref().is_none_or(|wanted| c.project == *wanted))
            .filter(|c| c.closed_at.is_some_and(|at| at <= cutoff))
            .map(|c| c.id.clone())
            .collect();
        for id in &gone {
            state.channels.remove(id);
        }
        let ids: Vec<String> = gone.iter().map(|id| id.to_string()).collect();
        state.persist("channel", |store| {
            for id in &ids {
                store.delete_document("channel", id)?;
            }
            Ok(())
        });
        if let Some(error) = state.storage_failure() {
            return error;
        }
        Response::Pruned {
            removed: gone.len(),
        }
    }

    /// Ask the channel's other members to look at this agent's work.
    pub(super) fn review_request(
        &self,
        reference: &str,
        channel: &str,
        note: Option<String>,
    ) -> Response {
        let (id, agent) = match self.channel_of(channel, reference) {
            Ok(v) => v,
            Err(e) => return *e,
        };
        let mut state = lock(&self.state);
        let Some(record) = state.registry.get(&agent).cloned() else {
            return Response::error(ErrorCode::NotFound, "agent vanished");
        };
        let Some(channel) = state.channels.get(&id).cloned() else {
            return Response::error(ErrorCode::NotFound, "channel vanished");
        };
        if !channel.is_open() {
            return Response::error(ErrorCode::Invalid, "that channel is closed");
        }
        let branch = record
            .vcs
            .as_ref()
            .and_then(|v| v.branch.clone())
            .unwrap_or_else(|| "its checkout".to_owned());
        let mut text = format!(
            "{} asks you to review its work on {branch}. See it with `agentdocker worktree-diff --as {}`, \
             then `agentdocker review --as <you> {id} --of {} --approve` or `--changes \"…\"`.",
            record.spec.name, record.spec.name, record.spec.name
        );
        if let Some(note) = note.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
            text.push_str(&format!(" They said: {note}"));
        }
        state.send(
            agent.to_string(),
            Destination::Channel(id),
            "review_request".to_owned(),
            json!({
                "channel": channel.id.as_str(),
                "of": agent.as_str(),
                "of_name": record.spec.name,
                "note": note,
                "text": text,
            }),
            None,
        );
        if let Some(error) = state.storage_failure() {
            return error;
        }
        Response::Channel { channel }
    }

    /// A verdict on another member's work. This is the tie-break.
    pub(super) fn review(
        &self,
        reference: &str,
        channel: &str,
        of: Option<String>,
        verdict: &str,
        note: Option<String>,
    ) -> Response {
        let Some(verdict) = Verdict::parse(verdict) else {
            return Response::error(
                ErrorCode::Invalid,
                "verdict must be approve, changes, or comment",
            );
        };
        let (id, reviewer) = match self.channel_of(channel, reference) {
            Ok(v) => v,
            Err(e) => return *e,
        };
        let mut state = lock(&self.state);
        let author = match of {
            Some(reference) => match state.resolve(&reference) {
                Ok(id) => id,
                Err(e) => return *e,
            },
            None => {
                let others = state
                    .channels
                    .get(&id)
                    .map(|c| c.others(&reviewer))
                    .unwrap_or_default();
                match others.as_slice() {
                    [only] => only.clone(),
                    _ => {
                        return Response::error(
                            ErrorCode::Invalid,
                            "say whose work with --of: the channel has more than one other member",
                        );
                    }
                }
            }
        };
        if author == reviewer {
            return Response::error(ErrorCode::Invalid, "an agent cannot review its own work");
        }
        let Some(reviewer_record) = state.registry.get(&reviewer).cloned() else {
            return Response::error(ErrorCode::NotFound, "agent vanished");
        };
        let author_record = state.registry.get(&author).cloned();
        let head = author_record
            .as_ref()
            .and_then(|r| r.vcs.as_ref().and_then(|v| v.head.clone()));
        let Some(channel) = state.channels.get_mut(&id) else {
            return Response::error(ErrorCode::NotFound, "channel vanished");
        };
        if !channel.is_open() {
            return Response::error(ErrorCode::Invalid, "that channel is closed");
        }
        if !channel.has(&author) {
            return Response::error(ErrorCode::Invalid, "that agent is not in the channel");
        }
        let note = note
            .map(|n| n.trim().to_owned())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| verdict.to_string());
        let review = Review {
            by: reviewer.clone(),
            by_name: reviewer_record.spec.name.clone(),
            of: author.clone(),
            of_name: author_record
                .map(|r| r.spec.name)
                .unwrap_or_else(|| author.short().to_owned()),
            verdict,
            note,
            at: Utc::now(),
            head,
        };
        channel.reviews.push(review.clone());
        let channel = channel.clone();
        state.persist("channel", |store| {
            store.put_document("channel", channel.id.as_str(), &channel)
        });
        state.emit(EventKind::ReviewSubmitted {
            channel: id.clone(),
            by: reviewer,
            of: author.clone(),
            verdict,
        });
        let decision = channel.decision(&author, 1);
        state.send(
            review.by.to_string(),
            Destination::Channel(id),
            "review".to_owned(),
            json!({
                "channel": channel.id.as_str(),
                "of": author.as_str(),
                "verdict": verdict.to_string(),
                "note": review.note,
                "decision": decision,
                "text": review.line(),
            }),
            None,
        );
        if let Some(entry) = state.plain_entry(
            &reviewer_record,
            JournalKind::Review,
            review.summary(),
            SummarySource::Explicit,
        ) {
            state.append_journal(entry);
        }
        if let Some(error) = state.storage_failure() {
            return error;
        }
        Response::Channel { channel }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::Decision;

    async fn fixture(tmp: &tempfile::TempDir) -> (Arc<Daemon>, PathBuf) {
        let root = tmp.path().join("checkout");
        std::fs::create_dir(&root).unwrap();
        let daemon =
            Arc::new(Daemon::open(tmp.path().join("state"), tmp.path().join("sock")).unwrap());
        for name in ["writer", "reviewer", "bystander"] {
            daemon
                .handle(Request::Register {
                    spec: AgentSpec {
                        name: name.into(),
                        workdir: Some(root.clone()),
                        ..AgentSpec::default()
                    },
                    pid: None,
                })
                .await;
        }
        (daemon, root)
    }

    async fn inbox(daemon: &Arc<Daemon>, agent: &str) -> Vec<Envelope> {
        match daemon
            .handle(Request::Inbox {
                agent: agent.into(),
                drain: true,
            })
            .await
        {
            Response::Messages { messages } => messages,
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn a_channel_carries_talk_and_reviews_and_closes_when_the_work_is_final() {
        let tmp = tempfile::tempdir().unwrap();
        let (daemon, _root) = fixture(&tmp).await;

        let Response::Channel { channel } = daemon
            .handle(Request::ChannelOpen {
                agent: "writer".into(),
                task: "settle the parser".into(),
                members: vec!["reviewer".into()],
            })
            .await
        else {
            panic!("open failed");
        };
        assert_eq!(channel.members.len(), 2, "just the two named");
        assert_eq!(channel.title(), "settle the parser");
        assert!(channel.is_open());
        // The other member was told, the opener was not messaged by itself.
        let told = inbox(&daemon, "reviewer").await;
        assert_eq!(told.len(), 1);
        assert_eq!(told[0].kind, "channel");
        assert!(inbox(&daemon, "bystander").await.is_empty(), "not a member");

        // Talking to the channel reaches members only.
        daemon
            .handle(Request::Send {
                from: "writer".into(),
                to: format!("channel:{}", channel.id),
                kind: "chat".into(),
                payload: serde_json::json!({"text": "taking src/parser.rs"}),
                reply_to: None,
            })
            .await;
        assert_eq!(inbox(&daemon, "reviewer").await.len(), 1);
        assert!(inbox(&daemon, "bystander").await.is_empty());

        // A request for review reaches the others.
        assert!(matches!(
            daemon
                .handle(Request::ReviewRequest {
                    agent: "writer".into(),
                    channel: channel.id.to_string(),
                    note: Some("the lexer is untouched".into()),
                })
                .await,
            Response::Channel { .. }
        ));
        let asked = inbox(&daemon, "reviewer").await;
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0].kind, "review_request");

        // Changes block; a later approval from the same reviewer clears it.
        let review = |verdict: &str, note: &str| Request::Review {
            agent: "reviewer".into(),
            channel: channel.id.to_string(),
            of: None,
            verdict: verdict.into(),
            note: Some(note.into()),
        };
        let Response::Channel { channel: after } = daemon
            .handle(review("changes", "handle the empty input"))
            .await
        else {
            panic!("review failed");
        };
        let writer = after.members[0].clone();
        assert!(matches!(
            after.decision(&writer, 1),
            Decision::Blocked { .. }
        ));
        let Response::Channel { channel: after } =
            daemon.handle(review("approve", "good now")).await
        else {
            panic!("review failed");
        };
        assert_eq!(
            after.decision(&writer, 1),
            Decision::Approved { approvals: 1 }
        );
        assert_eq!(after.reviews.len(), 2, "both are kept as history");

        // Nobody reviews their own work, and a stranger cannot review here.
        assert!(matches!(
            daemon
                .handle(Request::Review {
                    agent: "writer".into(),
                    channel: channel.id.to_string(),
                    of: Some("writer".into()),
                    verdict: "approve".into(),
                    note: None,
                })
                .await,
            Response::Error {
                code: ErrorCode::Invalid,
                ..
            }
        ));
        assert!(matches!(
            daemon
                .handle(Request::Review {
                    agent: "bystander".into(),
                    channel: channel.id.to_string(),
                    of: Some("writer".into()),
                    verdict: "approve".into(),
                    note: None,
                })
                .await,
            Response::Error {
                code: ErrorCode::Forbidden,
                ..
            }
        ));

        // Closing says why, tells the members, and journals it.
        let Response::Channel { channel: closed } = daemon
            .handle(Request::ChannelClose {
                agent: "writer".into(),
                channel: channel.id.to_string(),
                resolution: Some("writer's version landed".into()),
            })
            .await
        else {
            panic!("close failed");
        };
        assert!(!closed.is_open());
        assert_eq!(
            closed.resolution.as_deref(),
            Some("writer's version landed")
        );
        assert!(matches!(
            daemon
                .handle(Request::Review {
                    agent: "reviewer".into(),
                    channel: channel.id.to_string(),
                    of: None,
                    verdict: "approve".into(),
                    note: None,
                })
                .await,
            Response::Error {
                code: ErrorCode::Invalid,
                ..
            }
        ));

        // Open channels are the default listing; closed ones need --all,
        // and pruning forgets them.
        let listed = |all: bool| {
            let daemon = daemon.clone();
            async move {
                match daemon
                    .handle(Request::Channels {
                        project: String::new(),
                        all,
                        agent: Some("writer".into()),
                    })
                    .await
                {
                    Response::Channels { channels } => channels,
                    other => panic!("{other:?}"),
                }
            }
        };
        assert!(listed(false).await.is_empty());
        assert_eq!(listed(true).await.len(), 1);
        assert!(matches!(
            daemon
                .handle(Request::ChannelPrune {
                    project: String::new(),
                    before_secs: 0,
                })
                .await,
            Response::Pruned { removed: 1 }
        ));
        assert!(listed(true).await.is_empty());

        // Survives a restart: reopen and check a fresh channel is there.
        let Response::Channel { channel: fresh } = daemon
            .handle(Request::ChannelOpen {
                agent: "writer".into(),
                task: "second round".into(),
                members: vec![],
            })
            .await
        else {
            panic!("open failed");
        };
        assert_eq!(fresh.members.len(), 3, "everyone in the project by default");
        drop(daemon);
        let daemon =
            Arc::new(Daemon::open(tmp.path().join("state"), tmp.path().join("sock")).unwrap());
        let Response::Channels { channels } = daemon
            .handle(Request::Channels {
                project: String::new(),
                all: false,
                agent: Some("writer".into()),
            })
            .await
        else {
            panic!("list failed");
        };
        assert_eq!(channels.len(), 1);
        assert_eq!(channels[0].id, fresh.id);
    }
}
