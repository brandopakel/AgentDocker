//! Handoff bundles: a checkpoint addressed to someone, with everything the
//! daemon already knows about the sender bundled around it. Acceptance is
//! `resume` (in `recovery`), which is where ownership moves.
use super::recovery::internal;
use super::*;
use agentdocker_core::handoff::{BUNDLE_ROWS, HANDOFF_SCHEMA};
use agentdocker_core::{Checkpoint, HandoffBundle};
use agentdocker_host::command;

/// The tracked diff of a checkout against HEAD; empty when there is none.
async fn tracked_diff(root: PathBuf) -> anyhow::Result<String> {
    tokio::task::spawn_blocking(move || {
        let argv: Vec<String> = ["git", "diff", "--no-ext-diff", "--patch", "HEAD", "--"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        command::run(&root, &argv, std::time::Duration::from_secs(30))
    })
    .await?
    .map_err(anyhow::Error::from)
    .and_then(|output| {
        if output.success {
            Ok(output.text)
        } else {
            anyhow::bail!("{}", output.text.trim())
        }
    })
}

impl Daemon {
    /// Make a bundle for `agent`'s work, addressed to `to` when given. The
    /// checkpoint underneath is made through the checkpoint path, so it
    /// has the same barrier, idempotency and release semantics; the bundle
    /// document, the `handoff` message, and the journal entry follow.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn handoff(
        &self,
        reference: &str,
        to: Option<&str>,
        task: Option<String>,
        note: Option<String>,
        links: Vec<agentdocker_core::Link>,
        transfer_leases: bool,
        key: Option<String>,
    ) -> Response {
        let (from, record) = {
            let mut state = lock(&self.state);
            let id = match state.resolve(reference) {
                Ok(id) => id,
                Err(e) => return *e,
            };
            let Some(record) = state.registry.get(&id).cloned() else {
                return Response::error(ErrorCode::NotFound, "agent vanished");
            };
            (id, record)
        };
        let key = key.unwrap_or_else(|| format!("handoff-{}", MessageId::generate()));
        let id = format!("{}:{key}", from.as_str());
        // A retry names the first durable bundle, even if roles, liveness or
        // project membership changed after an uncertain response. Validate the
        // current participants only when creating a new handoff.
        match lock(&self.state).store_read("handoff retry", |store| {
            store.document::<HandoffBundle>("handoff", &id)
        }) {
            Some(Some(bundle)) => return Response::Handoff { bundle },
            Some(None) => {}
            None => {
                return Response::error(ErrorCode::StorageUnavailable, "handoff could not be read");
            }
        }
        if record.status != AgentStatus::Running {
            return Response::error(ErrorCode::Forbidden, "a handoff needs a running sender");
        }
        let recipient = match to {
            Some(reference) => {
                let mut state = lock(&self.state);
                let id = match state.resolve_from(Some(&from), reference) {
                    Ok(id) => id,
                    Err(e) => return *e,
                };
                let Some(other) = state.registry.get(&id).cloned() else {
                    return Response::error(ErrorCode::NotFound, "recipient vanished");
                };
                if id == from {
                    return Response::error(
                        ErrorCode::Invalid,
                        "an agent cannot hand off to itself",
                    );
                }
                if !other.status.is_live() {
                    return Response::error(ErrorCode::Invalid, "the recipient has finished");
                }
                let same_project = match (&record.project, &other.project) {
                    (Some(mine), Some(theirs)) => mine.id() == theirs.id(),
                    (None, None) => true,
                    _ => false,
                };
                if !same_project {
                    return Response::error(
                        ErrorCode::Invalid,
                        "the recipient works in another project",
                    );
                }
                Some(other)
            }
            None => None,
        };
        if recipient.is_none() && transfer_leases {
            return Response::error(
                ErrorCode::Invalid,
                "leases can only move to a named recipient; an export releases them",
            );
        }
        // What the sender holds now: released by the checkpoint unless it
        // is to move at acceptance, and listed either way.
        let leases: Vec<Lease> = lock(&self.state)
            .leases
            .by_holder(&from)
            .into_iter()
            .cloned()
            .collect();
        let diff = match record.project.as_ref().and_then(|p| p.worktree.clone()) {
            Some(worktree) => match tracked_diff(project::canonical(&worktree)).await {
                Ok(patch) if patch.trim().is_empty() => None,
                Ok(patch) => Some(HandoffBundle::cap_diff(patch, worktree)),
                Err(err) => {
                    warn!(agent = %from.short(), %err, "handoff diff unavailable");
                    None
                }
            },
            None => None,
        };
        let task = task
            .map(|t| t.trim().to_owned())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| "continue the work".to_owned());
        let note = note.map(|n| n.trim().to_owned()).filter(|n| !n.is_empty());
        let checkpoint = match self
            .checkpoint(
                reference,
                key,
                task,
                Vec::new(),
                Vec::new(),
                links,
                !transfer_leases,
            )
            .await
        {
            Response::Checkpoint { checkpoint } => checkpoint,
            other => return other,
        };
        let mut state = lock(&self.state);
        // Two retries with one key may both have passed the probe above
        // while nobody held the lock; the first to get here wins and the
        // other returns what it wrote.
        match state.store.document::<HandoffBundle>("handoff", &id) {
            Ok(Some(bundle)) => return Response::Handoff { bundle },
            Ok(None) => {}
            Err(e) => return internal(e),
        }
        let project = record.project.as_ref().map(ProjectRef::id);
        let changes = match &project {
            Some(project) => state
                .store
                .changes(&ChangesQuery {
                    project: project.clone(),
                    since_seq: None,
                    path: None,
                    agent: Some(from.clone()),
                    limit: BUNDLE_ROWS,
                    after: Some(record.created_at),
                    before_seq: None,
                })
                .unwrap_or_default(),
            None => Vec::new(),
        };
        let journal = match &project {
            Some(project) => {
                let mut query = JournalQuery::new(project.clone(), BUNDLE_ROWS);
                query.agent = Some(from.clone());
                state.store.journal(&query).unwrap_or_default()
            }
            None => Vec::new(),
        };
        let journal_cursor = project
            .as_ref()
            .and_then(|project| state.cursor(from.as_str(), project));
        let unread_inbox: Vec<Envelope> = state
            .inboxes
            .get(&from)
            .map(|queue| queue.iter().take(BUNDLE_ROWS).cloned().collect())
            .unwrap_or_default();
        let bundle = HandoffBundle {
            schema: HANDOFF_SCHEMA,
            id: id.clone(),
            from: from.clone(),
            from_name: record.spec.name.clone(),
            to: recipient.as_ref().map(|r| r.id.clone()),
            project: project.clone(),
            task: checkpoint.task.clone(),
            note,
            assumptions: checkpoint.assumptions.clone(),
            next_steps: checkpoint.next_steps.clone(),
            links: checkpoint.links.clone(),
            checkout: checkpoint.checkout.clone(),
            version: checkpoint.version.clone(),
            environment: checkpoint.environment.clone(),
            vcs: record.vcs.clone(),
            leases,
            transfer_leases,
            read_set: checkpoint.reads.clone(),
            changes,
            diff,
            unread_inbox,
            journal,
            journal_cursor,
            created_at: checkpoint.created_at,
            imported_at: None,
        };
        let mut event = Event::new(
            EventKind::HandoffSent {
                from: from.clone(),
                to: bundle.to.clone(),
                handoff: id.clone(),
            },
            Utc::now(),
        );
        event.seq = state.next_seq;
        let _ = state.persist("handoff", |store| {
            store.put_document_with_event("handoff", &id, &bundle, &event)
        });
        if let Some(error) = state.write_failure() {
            return error;
        }
        state.next_seq += 1;
        let _ = state.events.send(event);
        let headline = bundle.headline(recipient.as_ref().map(|r| r.spec.name.as_str()));
        if let Some(recipient) = &recipient {
            state.send(
                from.to_string(),
                Destination::Agent(recipient.id.clone()),
                "handoff".to_owned(),
                json!({
                    "handoff": id,
                    "task": bundle.task,
                    "note": bundle.note,
                    "from": bundle.from_name,
                    "transfer_leases": transfer_leases,
                    "text": format!(
                        "{} handed you its work: {}. Review with `agentdocker resume --as <you> {id}` and accept with `--acknowledge`.",
                        bundle.from_name, bundle.task
                    ),
                }),
                None,
            );
        }
        if let Some(entry) = state.plain_entry(
            &record,
            JournalKind::Handoff,
            headline,
            SummarySource::Explicit,
        ) {
            state.append_journal(entry);
        }
        Response::Handoff { bundle }
    }

    pub(super) fn handoffs(&self, reference: Option<&str>) -> Response {
        let mut state = lock(&self.state);
        let agent = match reference.map(|r| state.resolve(r)).transpose() {
            Ok(a) => a,
            Err(e) => return *e,
        };
        match state.store.handoffs(agent.as_ref()) {
            Ok(bundles) => Response::Handoffs { bundles },
            Err(e) => internal(e),
        }
    }

    /// Bring a bundle exported elsewhere here: re-homed to `agent`'s
    /// checkout and addressed to it, with the checkpoint `resume` accepts
    /// it under. Leases cannot follow across hosts; content must match
    /// exactly for acceptance, as ever.
    pub(super) async fn import(&self, reference: &str, bundle: HandoffBundle) -> Response {
        if bundle.schema != HANDOFF_SCHEMA {
            return Response::error(
                ErrorCode::Invalid,
                format!(
                    "bundle schema {} is not the {} this daemon reads",
                    bundle.schema, HANDOFF_SCHEMA
                ),
            );
        }
        if bundle.id.is_empty() || bundle.id.len() > 256 || bundle.task.is_empty() {
            return Response::error(ErrorCode::Invalid, "bundle needs an id and a task");
        }
        // Links in a bundle from elsewhere — its own, and those on every
        // message it carries — are held to the same shape and count as
        // links made here, before anything is written.
        if let Err(reason) = agentdocker_core::link::check(&bundle.links) {
            return Response::error(ErrorCode::Invalid, format!("bundle links: {reason}"));
        }
        if let Some(reason) = bundle
            .unread_inbox
            .iter()
            .find_map(|message| agentdocker_core::link::check(&message.links).err())
        {
            return Response::error(
                ErrorCode::Invalid,
                format!("bundle message links: {reason}"),
            );
        }
        // The bounds this daemon's own bundles and checkpoints keep, before
        // anything is written.
        if bundle.from_name.len() > 256 || bundle.version.len() > 256 || !bundle.fits_import_limit()
        {
            return Response::error(
                ErrorCode::Invalid,
                "bundle sender/version or total serialized size exceeds the import limit",
            );
        }
        let notes = bundle.task.len()
            + bundle.note.as_ref().map_or(0, String::len)
            + bundle
                .assumptions
                .iter()
                .chain(&bundle.next_steps)
                .map(String::len)
                .sum::<usize>();
        if notes > 65536 {
            return Response::error(
                ErrorCode::Invalid,
                "bundle carries more than 64 KiB of notes",
            );
        }
        if bundle.read_set.len() > 1000
            || bundle.changes.len() > BUNDLE_ROWS
            || bundle.journal.len() > BUNDLE_ROWS
            || bundle.unread_inbox.len() > BUNDLE_ROWS
            || bundle.leases.len() > BUNDLE_ROWS
        {
            return Response::error(
                ErrorCode::Invalid,
                "bundle carries more rows than this daemon accepts",
            );
        }
        if bundle
            .diff
            .as_ref()
            .is_some_and(|d| d.patch.len() > agentdocker_core::handoff::DIFF_CAP)
        {
            return Response::error(ErrorCode::Invalid, "bundle diff exceeds the cap");
        }
        let (agent, checkout, _) = match self.reader_checkout(reference) {
            Ok(v) => v,
            Err(e) => return *e,
        };
        let mut state = lock(&self.state);
        match state.store.document::<Checkpoint>("checkpoint", &bundle.id) {
            Ok(Some(_)) => {
                return Response::error(
                    ErrorCode::Conflict,
                    "a checkpoint with this bundle's id already exists here",
                );
            }
            Ok(None) => {}
            Err(e) => return internal(e),
        }
        let now = Utc::now();
        let project = state
            .registry
            .get(&agent)
            .and_then(|r| r.project.as_ref().map(ProjectRef::id));
        let mut bundle = match bundle.imported(agent.clone(), checkout.clone(), now) {
            Ok(bundle) => bundle,
            Err(reason) => return Response::error(ErrorCode::Invalid, reason),
        };
        bundle.project = project;
        let checkpoint = Checkpoint {
            id: bundle.id.clone(),
            from: bundle.from.clone(),
            checkout,
            created_at: bundle.created_at,
            task: bundle.task.clone(),
            assumptions: bundle.assumptions.clone(),
            next_steps: bundle.next_steps.clone(),
            links: bundle.links.clone(),
            reads: bundle.read_set.clone(),
            version: bundle.version.clone(),
            environment: bundle.environment.clone(),
            accepted_by: None,
            release_leases: true,
        };
        let mut event = Event::new(
            EventKind::HandoffImported {
                agent: agent.clone(),
                handoff: bundle.id.clone(),
            },
            now,
        );
        event.seq = state.next_seq;
        let _ = state.persist("handoff import", |store| {
            store.import_handoff(&checkpoint, &bundle, &event)
        });
        if let Some(error) = state.write_failure() {
            return error;
        }
        state.next_seq += 1;
        let _ = state.events.send(event);
        state.send(
            USER_READER.to_owned(),
            Destination::Agent(agent.clone()),
            "handoff".to_owned(),
            json!({
                "handoff": bundle.id,
                "task": bundle.task,
                "note": bundle.note,
                "from": bundle.from_name,
                "imported": true,
                "text": format!(
                    "A handoff from {} was imported for you: {}. Review with `agentdocker resume --as <you> {}` and accept with `--acknowledge`.",
                    bundle.from_name, bundle.task, bundle.id
                ),
            }),
            None,
        );
        Response::Handoff { bundle }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::LeaseMode;

    async fn fixture(tmp: &tempfile::TempDir) -> (Arc<Daemon>, PathBuf) {
        let root = tmp.path().join("checkout");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("file"), "one").unwrap();
        let daemon =
            Arc::new(Daemon::open(tmp.path().join("state"), tmp.path().join("sock")).unwrap());
        for name in ["sender", "recipient", "other"] {
            daemon
                .handle(Request::Register {
                    spec: AgentSpec {
                        name: name.into(),
                        workdir: Some(root.clone()),
                        ..AgentSpec::default()
                    },
                    pid: None,
                    session: None,
                })
                .await;
        }
        (daemon, root)
    }

    async fn claim(daemon: &Arc<Daemon>, agent: &str, resource: &str) -> Lease {
        match daemon
            .handle(Request::Claim {
                agent: agent.into(),
                resource: resource.into(),
                mode: LeaseMode::Exclusive,
                amount: None,
                ttl_secs: 60,
                note: Some("mine".into()),
                wait_secs: 0,
                automatic: false,
            })
            .await
        {
            Response::Lease { lease } => lease,
            other => panic!("{other:?}"),
        }
    }

    async fn holders(daemon: &Arc<Daemon>, agent: &str) -> Vec<Lease> {
        match daemon
            .handle(Request::Leases {
                agent: Some(agent.into()),
                resource: None,
            })
            .await
        {
            Response::Leases { leases } => leases,
            other => panic!("{other:?}"),
        }
    }

    async fn role(daemon: &Arc<Daemon>, agent: &str, role: Option<&str>) -> Response {
        daemon
            .handle(Request::Role {
                agent: agent.into(),
                role: role.map(str::to_owned),
            })
            .await
    }

    async fn send_to(daemon: &Arc<Daemon>, from: &str, to: &str) -> Response {
        daemon
            .handle(Request::Send {
                from: from.into(),
                to: to.into(),
                kind: "chat".into(),
                payload: json!({"text": "for the reviewer"}),
                reply_to: None,
                links: Vec::new(),
            })
            .await
    }

    /// A role is a label a message or a hand-off can name: `role:<name>`
    /// is the one live agent holding it in the sender's project. The
    /// same role again is nothing; none is not found and two are
    /// ambiguous; a finished agent has no role; a role is one word.
    /// A person renames a live agent: the id stays, the name is unique among
    /// live agents, reserved spellings are refused, and the name survives a
    /// restart.
    #[tokio::test]
    async fn a_live_agent_can_be_renamed_once_uniquely_and_it_persists() {
        let tmp = tempfile::tempdir().unwrap();
        let (daemon, _root) = fixture(&tmp).await;
        let rename = |agent: &'static str, name: &'static str| {
            let daemon = daemon.clone();
            async move {
                daemon
                    .handle(Request::Rename {
                        agent: agent.into(),
                        name: name.into(),
                    })
                    .await
            }
        };
        let Response::Agent { agent: before } = daemon
            .handle(Request::Inspect {
                agent: "sender".into(),
            })
            .await
        else {
            panic!("sender is registered")
        };
        let Response::Agent { agent } = rename("sender", "Release helper").await else {
            panic!("renamed")
        };
        assert_eq!(agent.id, before.id, "the id stays");
        assert_eq!(agent.spec.name, "Release helper");
        assert!(!agent.name_is_generated());
        // The new name resolves; another live agent cannot take it.
        assert!(matches!(
            daemon
                .handle(Request::Inspect {
                    agent: "Release helper".into()
                })
                .await,
            Response::Agent { .. }
        ));
        assert!(matches!(
            rename("recipient", "Release helper").await,
            Response::Error {
                code: ErrorCode::NameTaken,
                ..
            }
        ));
        for reserved in ["project", "role:reviewer", "topic:x", " padded", ""] {
            assert!(
                matches!(
                    daemon
                        .handle(Request::Rename {
                            agent: "recipient".into(),
                            name: reserved.into(),
                        })
                        .await,
                    Response::Error {
                        code: ErrorCode::Invalid,
                        ..
                    }
                ),
                "{reserved:?} refused"
            );
        }
        // The same name again changes nothing.
        assert!(matches!(
            rename("Release helper", "Release helper").await,
            Response::Agent { .. }
        ));
        // A chosen name spelled like the adapter's own form stays chosen,
        // and choosing it again is nothing.
        let pid = std::process::id();
        let legacy_name = format!("codex-{pid}");
        let Response::Agent { agent: legacy } = daemon
            .handle(Request::Register {
                spec: AgentSpec {
                    name: legacy_name.clone(),
                    runtime: "codex".into(),
                    ..AgentSpec::default()
                },
                pid: Some(pid),
                session: None,
            })
            .await
        else {
            panic!("registered")
        };
        assert!(legacy.name_is_generated(), "the adapter's own spelling reads generated");
        let Response::Agent { agent: chosen } = daemon
            .handle(Request::Rename {
                agent: legacy.id.to_string(),
                name: legacy_name.clone(),
            })
            .await
        else {
            panic!("renamed")
        };
        assert!(!chosen.name_is_generated(), "chosen overrides the shape");
        let Response::Agent { agent: again } = daemon
            .handle(Request::Rename {
                agent: legacy.id.to_string(),
                name: legacy_name.clone(),
            })
            .await
        else {
            panic!("renamed again")
        };
        assert_eq!(again, chosen, "choosing it again changes nothing");
        drop(daemon);
        let reopened =
            Arc::new(Daemon::open(tmp.path().join("state"), tmp.path().join("sock")).unwrap());
        let Response::Agent { agent } = reopened
            .handle(Request::Inspect {
                agent: before.id.to_string(),
            })
            .await
        else {
            panic!("still registered")
        };
        assert_eq!(agent.spec.name, "Release helper", "the name was committed");
    }

    #[tokio::test]
    async fn a_role_is_set_once_and_names_the_projects_holder() {
        let tmp = tempfile::tempdir().unwrap();
        let (daemon, _root) = fixture(&tmp).await;
        // Another project's reviewer never answers for this one.
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        daemon
            .handle(Request::Register {
                spec: AgentSpec {
                    name: "far".into(),
                    workdir: Some(elsewhere),
                    ..AgentSpec::default()
                },
                pid: None,
                session: None,
            })
            .await;
        let mut events = daemon.subscribe_events();
        for bad in ["Reviewer", "re viewer", "", "-reviewer", &"r".repeat(41)] {
            assert!(
                matches!(
                    role(&daemon, "recipient", Some(bad)).await,
                    Response::Error {
                        code: ErrorCode::Invalid,
                        ..
                    }
                ),
                "{bad:?}"
            );
        }
        // A name spelled like a role would be reached as one, or shadow it.
        assert!(matches!(
            daemon
                .handle(Request::Register {
                    spec: AgentSpec {
                        name: "role:reviewer".into(),
                        ..AgentSpec::default()
                    },
                    pid: None,
                    session: None,
                })
                .await,
            Response::Error {
                code: ErrorCode::Invalid,
                ..
            }
        ));
        let Response::Agent { agent } = role(&daemon, "recipient", Some("reviewer")).await else {
            panic!("a role answers with the record");
        };
        assert_eq!(agent.role(), Some("reviewer"));
        match events.recv().await {
            Ok(Event {
                kind:
                    EventKind::RoleSet {
                        agent: who,
                        project,
                        role,
                    },
                ..
            }) => {
                assert_eq!(who, agent.id);
                assert!(project.is_some(), "a role is said with its project");
                assert_eq!(role.as_deref(), Some("reviewer"));
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            role(&daemon, "recipient", Some("reviewer")).await,
            Response::Agent { .. }
        ));
        assert!(events.try_recv().is_err(), "the same role again is nothing");
        assert!(matches!(
            role(&daemon, "far", Some("reviewer")).await,
            Response::Agent { .. }
        ));
        // Both the queue and readiness advice name this project's resolved
        // reviewer, never a same-role agent elsewhere or the literal role.
        let Response::Sent {
            message,
            recipient_readiness: Some(readiness),
            ..
        } = send_to(&daemon, "sender", "role:reviewer").await
        else {
            panic!("a role-addressed send includes its recipient readiness");
        };
        assert_eq!(readiness.recipients, 1);
        assert_eq!(readiness.needs_attention, 1);
        assert_eq!(readiness.details.len(), 1);
        assert_eq!(readiness.details[0].agent, agent.id);
        assert_eq!(
            readiness.details[0].issue,
            agentdocker_core::SendIssue::NoReceiver
        );
        let mail = inbox(&daemon, "recipient").await;
        assert_eq!(mail.len(), 1);
        assert_eq!(mail[0].id, message);
        assert_eq!(mail[0].to, Destination::Agent(agent.id.clone()));
        assert!(inbox(&daemon, "far").await.is_empty());
        // Unscoped — no sender to take a project from — two hold it.
        assert!(matches!(
            daemon
                .handle(Request::Inspect {
                    agent: "role:reviewer".into()
                })
                .await,
            Response::Error {
                code: ErrorCode::Ambiguous,
                ..
            }
        ));
        assert!(matches!(
            send_to(&daemon, "sender", "role:implementer").await,
            Response::Error {
                code: ErrorCode::NotFound,
                ..
            }
        ));
        // A hand-off names the reviewer the same way.
        match daemon
            .handle(Request::Handoff {
                agent: "sender".into(),
                to: Some("role:reviewer".into()),
                task: Some("review it".into()),
                note: None,
                transfer_leases: false,
                key: None,
                links: Vec::new(),
            })
            .await
        {
            Response::Handoff { bundle } => assert_eq!(bundle.to, Some(agent.id.clone())),
            other => panic!("{other:?}"),
        }
        // Two holders here: the role names nobody until one lets go.
        assert!(matches!(
            role(&daemon, "other", Some("reviewer")).await,
            Response::Agent { .. }
        ));
        assert!(matches!(
            send_to(&daemon, "sender", "role:reviewer").await,
            Response::Error {
                code: ErrorCode::Ambiguous,
                ..
            }
        ));
        let Response::Agent { agent: other } = role(&daemon, "other", None).await else {
            panic!("clearing answers with the record");
        };
        assert_eq!(other.role(), None);
        assert!(matches!(
            send_to(&daemon, "sender", "role:reviewer").await,
            Response::Sent { .. }
        ));
        // A finished agent has no role to take.
        daemon
            .handle(Request::Deregister {
                agent: "other".into(),
            })
            .await;
        assert!(matches!(
            role(&daemon, "other", Some("reviewer")).await,
            Response::Error {
                code: ErrorCode::Invalid,
                ..
            }
        ));
    }

    async fn inbox(daemon: &Arc<Daemon>, agent: &str) -> Vec<Envelope> {
        match daemon
            .handle(Request::Inbox {
                agent: agent.into(),
                drain: false,
            })
            .await
        {
            Response::Messages { messages } => messages,
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn handoff_retry_returns_the_saved_recipient_after_role_and_liveness_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let (daemon, _) = fixture(&tmp).await;
        let Response::Agent { agent: recipient } =
            role(&daemon, "recipient", Some("reviewer")).await
        else {
            panic!("role was not set")
        };
        let sender = daemon.resolve("sender").unwrap();
        let request = Request::Handoff {
            agent: sender.to_string(),
            to: Some("role:reviewer".into()),
            task: Some("review the original work".into()),
            note: None,
            transfer_leases: false,
            key: Some("one-handoff".into()),
            links: Vec::new(),
        };
        let Response::Handoff { bundle } = daemon.handle(request.clone()).await else {
            panic!("first handoff failed")
        };
        assert_eq!(bundle.to, Some(recipient.id.clone()));
        let queued = inbox(&daemon, "recipient").await;
        assert_eq!(queued.len(), 1);

        async fn retry(daemon: &Arc<Daemon>, request: &Request, expected: &HandoffBundle) {
            let seq = lock(&daemon.state).next_seq;
            assert_eq!(
                daemon.handle(request.clone()).await,
                Response::Handoff {
                    bundle: expected.clone()
                }
            );
            assert_eq!(lock(&daemon.state).next_seq, seq, "retry emits nothing");
        }
        assert!(matches!(
            role(&daemon, "recipient", None).await,
            Response::Agent { .. }
        ));
        retry(&daemon, &request, &bundle).await; // The role no longer exists.
        let mut fresh = request.clone();
        if let Request::Handoff { key, .. } = &mut fresh {
            *key = Some("fresh".into());
        }
        assert!(matches!(
            daemon.handle(fresh).await,
            Response::Error {
                code: ErrorCode::NotFound,
                ..
            }
        ));

        assert!(matches!(
            role(&daemon, "other", Some("reviewer")).await,
            Response::Agent { .. }
        ));
        retry(&daemon, &request, &bundle).await; // Reassignment cannot redirect it.
        assert!(matches!(
            role(&daemon, "recipient", Some("reviewer")).await,
            Response::Agent { .. }
        ));
        retry(&daemon, &request, &bundle).await; // Nor can ambiguity hide it.
        assert!(matches!(
            daemon
                .handle(Request::Deregister {
                    agent: recipient.id.to_string()
                })
                .await,
            Response::Agent { agent } if !agent.status.is_live()
        ));
        assert!(matches!(
            daemon
                .handle(Request::Deregister {
                    agent: sender.to_string()
                })
                .await,
            Response::Agent { agent } if !agent.status.is_live()
        ));
        retry(&daemon, &request, &bundle).await;
        assert_eq!(inbox(&daemon, recipient.id.as_str()).await, queued);
        assert!(inbox(&daemon, "other").await.is_empty());
        drop(daemon);
        let restored =
            Arc::new(Daemon::open(tmp.path().join("state"), tmp.path().join("sock")).unwrap());
        retry(&restored, &request, &bundle).await;
        assert_eq!(inbox(&restored, recipient.id.as_str()).await, queued);
    }

    #[tokio::test]
    async fn imported_image_handoff_preserves_provenance_and_refuses_native_acceptance() {
        use agentdocker_core::container::{ContainerEnvironment, ContainerNetwork, ImageInputs};
        let tmp = tempfile::tempdir().unwrap();
        let (daemon, _) = fixture(&tmp).await;
        let Response::Handoff { mut bundle } = daemon
            .handle(Request::Handoff {
                agent: "sender".into(),
                to: None,
                task: None,
                note: None,
                transfer_leases: false,
                key: None,
                links: Vec::new(),
            })
            .await
        else {
            panic!("export failed")
        };
        bundle.environment = Some(ContainerEnvironment {
            inputs: Some(ImageInputs {
                context_version: "input".into(),
                recipe_version: "recipe".into(),
                os: "linux".into(),
                architecture: "arm64".into(),
                variant: None,
            }),
            image_id: "sha256:immutable".into(),
            build: "foreign-id".into(),
            engine: agentdocker_core::ContainerEngine::Podman,
            connection: Some("foreign-connection".into()),
            network: ContainerNetwork::None,
            user: Some("1000:1000".into()),
            env: Default::default(),
        });
        let other = tempfile::tempdir().unwrap();
        let (elsewhere, _) = fixture(&other).await;
        let Response::Handoff { bundle: imported } =
            elsewhere.import("recipient", bundle.clone()).await
        else {
            panic!("import failed")
        };
        assert_eq!(imported.environment, bundle.environment);
        let Response::Recovery { recovery } =
            elsewhere.resume("recipient", &imported.id, false).await
        else {
            panic!("preview failed")
        };
        assert_eq!(recovery.checkpoint.environment, bundle.environment);
        assert!(recovery.checkout_matches);
        assert!(!recovery.environment_matches);
        assert!(matches!(
            elsewhere.resume("recipient", &imported.id, true).await,
            Response::Error {
                code: ErrorCode::Conflict,
                ..
            }
        ));
        drop(elsewhere);
        let restored =
            Arc::new(Daemon::open(other.path().join("state"), other.path().join("sock")).unwrap());
        let saved: Checkpoint = lock(&restored.state)
            .store
            .document("checkpoint", &imported.id)
            .unwrap()
            .unwrap();
        assert_eq!(saved.environment, bundle.environment);
        assert!(saved.accepted_by.is_none());
    }

    #[tokio::test]
    async fn handoff_moves_leases_reads_and_cursor_at_acceptance() {
        let tmp = tempfile::tempdir().unwrap();
        let (daemon, _root) = fixture(&tmp).await;
        daemon.observe("sender", vec!["file".into()]).await;
        let held = claim(&daemon, "sender", "task:parse").await;
        daemon
            .handle(Request::JournalAdd {
                agent: "sender".into(),
                summary: "parser half done".into(),
            })
            .await;
        // The sender has read the journal up to here.
        let Response::Digest { .. } = daemon
            .handle(Request::Journal {
                project: String::new(),
                since_seq: None,
                until_seq: None,
                agent: None,
                branch: None,
                kind: None,
                path: None,
                grep: None,
                limit: 50,
                digest: Some(DigestRequest {
                    reader: "sender".into(),
                    max_entries: 20,
                    max_chars: 2000,
                    all_branches: false,
                    advance: true,
                }),
            })
            .await
        else {
            panic!("digest failed");
        };

        // A link that is not the shape of its kind refuses the hand-off
        // before anything is written.
        assert!(matches!(
            daemon
                .handle(Request::Handoff {
                    agent: "sender".into(),
                    to: Some("recipient".into()),
                    task: None,
                    note: None,
                    transfer_leases: false,
                    key: None,
                    links: vec![agentdocker_core::Link::new(
                        agentdocker_core::LinkKind::Commit,
                        "not-a-hash"
                    )],
                })
                .await,
            Response::Error {
                code: ErrorCode::Invalid,
                ..
            }
        ));
        let links = vec![
            agentdocker_core::Link::parse("path:src/parser.rs").unwrap(),
            agentdocker_core::Link::parse(
                "memory:the grammar file is generated; edit the .peg, not the .rs",
            )
            .unwrap(),
        ];
        let request = Request::Handoff {
            agent: "sender".into(),
            to: Some("recipient".into()),
            task: Some("finish the parser".into()),
            note: Some("tests are in src/parser.rs".into()),
            transfer_leases: true,
            key: Some("parser".into()),
            links: links.clone(),
        };
        let Response::Handoff { bundle } = daemon.handle(request.clone()).await else {
            panic!("handoff failed");
        };
        assert_eq!(bundle.from_name, "sender");
        assert_eq!(bundle.task, "finish the parser");
        assert_eq!(bundle.note.as_deref(), Some("tests are in src/parser.rs"));
        assert_eq!(bundle.links, links, "the links travel with the bundle");
        assert_eq!(bundle.leases.len(), 1);
        assert_eq!(bundle.read_set.len(), 1);
        assert!(
            bundle
                .journal
                .iter()
                .any(|e| e.summary == "parser half done")
        );
        assert!(bundle.journal_cursor.is_some());
        assert!(bundle.transfer_leases);
        // Idempotent: the same key is the same bundle.
        assert_eq!(
            daemon.handle(request).await,
            Response::Handoff {
                bundle: bundle.clone()
            }
        );
        // Leases stay with the sender until acceptance; the recipient was told.
        assert_eq!(holders(&daemon, "sender").await.len(), 1);
        let mail = inbox(&daemon, "recipient").await;
        assert_eq!(mail.len(), 1);
        assert_eq!(mail[0].kind, "handoff");
        assert_eq!(mail[0].payload["handoff"], bundle.id);
        // Journaled, once, as a handoff.
        let Response::Journal { entries, .. } = daemon
            .handle(Request::Journal {
                project: bundle.project.as_ref().unwrap().as_str().to_owned(),
                since_seq: None,
                until_seq: None,
                agent: None,
                branch: None,
                kind: Some("handoff".into()),
                path: None,
                grep: None,
                limit: 50,
                digest: None,
            })
            .await
        else {
            panic!("journal failed");
        };
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].summary,
            "handed off to recipient: finish the parser"
        );
        assert!(
            entries[0]
                .line()
                .ends_with("sender handed off to recipient: finish the parser")
        );
        // Listed for both parties, not for a stranger.
        for (who, expect) in [("sender", 1), ("recipient", 1), ("other", 0)] {
            let Response::Handoffs { bundles } = daemon.handoffs(Some(who)) else {
                panic!("handoffs failed");
            };
            assert_eq!(bundles.len(), expect, "{who}");
        }

        // Somebody else cannot accept it.
        assert!(matches!(
            daemon.resume("other", &bundle.id, true).await,
            Response::Error {
                code: ErrorCode::Forbidden,
                ..
            }
        ));
        let later = claim(&daemon, "sender", "task:acquired-after-handoff").await;
        // The recipient can: leases move, reads seed, cursor follows.
        let Response::Recovery { recovery } = daemon.resume("recipient", &bundle.id, true).await
        else {
            panic!("resume failed");
        };
        let accepted_by = recovery.checkpoint.accepted_by.clone().expect("accepted");
        let moved = holders(&daemon, "recipient").await;
        assert_eq!(moved.len(), 1);
        assert_eq!(moved[0].holder, accepted_by);
        assert_eq!(moved[0].id, held.id);
        assert_eq!(moved[0].note.as_deref(), Some("mine"));
        assert_eq!(
            holders(&daemon, "sender")
                .await
                .iter()
                .map(|l| &l.id)
                .collect::<Vec<_>>(),
            vec![&later.id]
        );
        assert!(matches!(daemon.reads("recipient"), Response::Reads { reads } if reads.len() == 1));
        let events = daemon.recent_events(100);
        assert!(
            events
                .iter()
                .any(|e| matches!(&e.kind, EventKind::HandoffSent { to: Some(_), .. }))
        );
        assert!(events.iter().any(
            |e| matches!(&e.kind, EventKind::LeaseTransferred { lease, .. } if lease.id == held.id)
        ));
        assert!(events.iter().any(|e| matches!(&e.kind, EventKind::JournalRead { reader, .. } if reader == accepted_by.as_str())));
        // Nothing new for the recipient since the sender's cursor, except
        // what happened after it: the handoff line itself.
        let Response::Digest { digest, .. } = daemon
            .handle(Request::Journal {
                project: String::new(),
                since_seq: None,
                until_seq: None,
                agent: None,
                branch: None,
                kind: None,
                path: None,
                grep: None,
                limit: 50,
                digest: Some(DigestRequest {
                    reader: "recipient".into(),
                    max_entries: 20,
                    max_chars: 2000,
                    all_branches: false,
                    advance: false,
                }),
            })
            .await
        else {
            panic!("digest failed");
        };
        assert!(!digest.text.contains("parser half done"), "{}", digest.text);
        assert!(
            digest.text.contains("handed off to recipient"),
            "{}",
            digest.text
        );

        // Leases survive a restart under their new holder.
        drop(daemon);
        let daemon =
            Arc::new(Daemon::open(tmp.path().join("state"), tmp.path().join("sock")).unwrap());
        let after = holders(&daemon, "recipient").await;
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].id, held.id);
        assert_eq!(holders(&daemon, "sender").await[0].id, later.id);
    }

    #[tokio::test]
    async fn handoff_without_transfer_releases_and_export_import_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let (daemon, root) = fixture(&tmp).await;
        claim(&daemon, "sender", "task:parse").await;
        let Response::Handoff { bundle } = daemon
            .handle(Request::Handoff {
                agent: "sender".into(),
                to: None,
                task: None,
                note: None,
                transfer_leases: false,
                key: None,
                links: Vec::new(),
            })
            .await
        else {
            panic!("export failed");
        };
        assert_eq!(bundle.to, None);
        assert_eq!(bundle.task, "continue the work");
        // Leases cannot be asked to move to nobody.
        assert!(matches!(
            daemon
                .handle(Request::Handoff {
                    agent: "sender".into(),
                    to: None,
                    task: None,
                    note: None,
                    transfer_leases: true,
                    key: None,
                    links: Vec::new(),
                })
                .await,
            Response::Error {
                code: ErrorCode::Invalid,
                ..
            }
        ));
        assert_eq!(bundle.leases.len(), 1, "what was held when it was made");
        assert!(
            holders(&daemon, "sender").await.is_empty(),
            "released with the checkpoint"
        );
        assert!(!bundle.transfer_leases);
        assert!(
            inbox(&daemon, "recipient").await.is_empty(),
            "nobody addressed, nobody told"
        );
        assert!(matches!(
            daemon
                .handle(Request::Handoff {
                    agent: "sender".into(),
                    to: Some("sender".into()),
                    task: None,
                    note: None,
                    transfer_leases: false,
                    key: None,
                    links: Vec::new(),
                })
                .await,
            Response::Error {
                code: ErrorCode::Invalid,
                ..
            }
        ));

        // Another host: same content, so the import is acceptable; the id
        // cannot be imported twice; changed content refuses acceptance.
        let other = tempfile::tempdir().unwrap();
        let (elsewhere, far_root) = fixture(&other).await;
        assert!(matches!(
            elsewhere
                .import(
                    "recipient",
                    HandoffBundle {
                        schema: 99,
                        ..bundle.clone()
                    }
                )
                .await,
            Response::Error {
                code: ErrorCode::Invalid,
                ..
            }
        ));
        // Oversized or escaping bundles are refused before anything is
        // written.
        let mark = |path: PathBuf| agentdocker_core::ReadMark {
            path,
            at: Utc::now(),
            version: "v".into(),
            head: None,
        };
        let mut huge = bundle.clone();
        huge.read_set = (0..1001)
            .map(|i| mark(root.join(format!("f{i}"))))
            .collect();
        assert!(matches!(
            elsewhere.import("recipient", huge).await,
            Response::Error {
                code: ErrorCode::Invalid,
                ..
            }
        ));
        for field in ["sender", "version", "total"] {
            let mut oversized = bundle.clone();
            match field {
                "sender" => oversized.from_name = "x".repeat(257),
                "version" => oversized.version = "x".repeat(257),
                _ => oversized.note = Some("x".repeat(agentdocker_core::handoff::IMPORT_BYTES + 1)),
            }
            assert!(matches!(
                elsewhere.import("recipient", oversized).await,
                Response::Error {
                    code: ErrorCode::Invalid,
                    ..
                }
            ));
        }
        let mut escaping = bundle.clone();
        escaping.read_set = vec![mark(bundle.checkout.join("../../etc/passwd"))];
        assert!(matches!(
            elsewhere.import("recipient", escaping).await,
            Response::Error {
                code: ErrorCode::Invalid,
                ..
            }
        ));
        // A bundle's links are held to the shape and count of links made
        // here, whatever daemon wrote them.
        let mut mislinked = bundle.clone();
        mislinked.links = vec![agentdocker_core::Link {
            kind: agentdocker_core::LinkKind::Url,
            target: "not a url".into(),
            note: None,
        }];
        assert!(matches!(
            elsewhere.import("recipient", mislinked).await,
            Response::Error {
                code: ErrorCode::Invalid,
                ..
            }
        ));
        let mut overlinked = bundle.clone();
        overlinked.links = vec![agentdocker_core::Link::parse("pr:#1").unwrap(); 17];
        assert!(matches!(
            elsewhere.import("recipient", overlinked).await,
            Response::Error {
                code: ErrorCode::Invalid,
                ..
            }
        ));
        // A message the bundle carries is held to it too.
        let mut carried = bundle.clone();
        let mut message = Envelope::new(
            "sender",
            Destination::Agent("recipient".into()),
            "chat",
            json!({"text": "see"}),
            None,
            Utc::now(),
        );
        message.links = vec![agentdocker_core::Link {
            kind: agentdocker_core::LinkKind::Commit,
            target: "nothex".into(),
            note: None,
        }];
        carried.unread_inbox = vec![message];
        assert!(matches!(
            elsewhere.import("recipient", carried).await,
            Response::Error {
                code: ErrorCode::Invalid,
                ..
            }
        ));
        let Response::Handoff { bundle: imported } =
            elsewhere.import("recipient", bundle.clone()).await
        else {
            panic!("import failed");
        };
        let importer = imported.to.clone().expect("addressed to the importer");
        let mail = inbox(&elsewhere, "recipient").await;
        assert_eq!(mail.len(), 1);
        assert_eq!(mail[0].to, Destination::Agent(importer));
        assert_eq!(imported.checkout, project::canonical(&far_root));
        assert!(!imported.transfer_leases);
        assert!(imported.imported_at.is_some());
        assert_eq!(inbox(&elsewhere, "recipient").await[0].kind, "handoff");
        assert!(matches!(
            elsewhere.import("recipient", bundle.clone()).await,
            Response::Error {
                code: ErrorCode::Conflict,
                ..
            }
        ));
        let Response::Recovery { recovery } =
            elsewhere.resume("recipient", &imported.id, true).await
        else {
            panic!("resume failed");
        };
        assert!(recovery.checkout_matches);
        assert!(recovery.checkpoint.accepted_by.is_some());
        assert!(
            holders(&elsewhere, "recipient").await.is_empty(),
            "leases never cross hosts"
        );

        // Back home, changed content blocks acceptance of a bundle too.
        std::fs::write(root.join("file"), "two").unwrap();
        let Response::Handoff { bundle: second } = daemon
            .handle(Request::Handoff {
                agent: "sender".into(),
                to: Some("recipient".into()),
                task: Some("again".into()),
                note: None,
                transfer_leases: false,
                key: None,
                links: Vec::new(),
            })
            .await
        else {
            panic!("handoff failed");
        };
        std::fs::write(root.join("file"), "three").unwrap();
        assert!(matches!(
            daemon.resume("recipient", &second.id, true).await,
            Response::Error {
                code: ErrorCode::Conflict,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn failed_acceptance_puts_back_only_the_transferred_leases() {
        let tmp = tempfile::tempdir().unwrap();
        let (daemon, _root) = fixture(&tmp).await;
        let own = claim(&daemon, "recipient", "task:own").await;
        let moving = claim(&daemon, "sender", "task:parse").await;
        let Response::Handoff { bundle } = daemon
            .handle(Request::Handoff {
                agent: "sender".into(),
                to: Some("recipient".into()),
                task: Some("x".into()),
                note: None,
                transfer_leases: true,
                key: Some("rollback".into()),
                links: Vec::new(),
            })
            .await
        else {
            panic!("handoff failed");
        };
        // Force a unique-index failure at event insertion, as the recovery
        // tests do.
        let next = lock(&daemon.state).next_seq;
        lock(&daemon.state).next_seq = next - 1;
        assert!(matches!(
            daemon.resume("recipient", &bundle.id, true).await,
            Response::Error {
                code: ErrorCode::StorageUnavailable,
                ..
            }
        ));
        let state = lock(&daemon.state);
        let sender = state.registry.resolve("sender").unwrap();
        let recipient = state.registry.resolve("recipient").unwrap();
        let held = |who: &AgentId| -> Vec<LeaseId> {
            state
                .leases
                .by_holder(who)
                .into_iter()
                .map(|l| l.id.clone())
                .collect()
        };
        assert_eq!(
            held(&sender),
            vec![moving.id.clone()],
            "the moved lease went back"
        );
        assert_eq!(
            held(&recipient),
            vec![own.id.clone()],
            "what the recipient already held stayed"
        );
    }
}
