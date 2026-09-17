//! The board of work: cards in columns, pulled by agents one at a time.
//! The rules are core's (`agentdocker_core::task`); the daemon's part is
//! to hold every card under its one mutex — which is what makes a pull
//! atomic — to keep them as documents, and to say what changed.
use super::*;
use agentdocker_core::protocol::{TASKS_LIMIT, TASKS_LIMIT_MAX};
use agentdocker_core::task::{Task, TaskError};
use agentdocker_core::{Column, HUMAN, LeaseMode, ResourceKey};

const DOCUMENT: &str = "task";

/// How long a pull holds a card without a word from its holder. Long
/// enough for a working session, short enough that a card whose agent
/// silently went away comes back to the board the same day; `renew`
/// extends it, and an exit releases it at once.
const TASK_LEASE_SECS: u64 = 4 * 60 * 60;

/// The lease a pull takes: the holding itself, so `leases` shows who
/// has which card and every lease rule — expiry, release on exit,
/// refusal of a second claimant — applies to cards too.
fn task_resource(task: &Task) -> ResourceKey {
    ResourceKey::new(format!("task:{}", task.id))
}

/// Who is acting on a card: an agent by its record, or the person — by
/// their record, or as the bare `user` a shell speaks as.
struct Actor {
    id: String,
    human: bool,
    project: Option<ProjectId>,
}

impl State {
    /// A card by its id or a unique prefix of it, on the board.
    fn resolve_task(&mut self, reference: &str) -> Result<Task, Box<Response>> {
        let reference = reference.trim();
        if reference.is_empty() {
            return Err(Box::new(Response::error(
                ErrorCode::Invalid,
                "name a card by its id",
            )));
        }
        if !reference.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(Box::new(Response::error(
                ErrorCode::NotFound,
                format!("no card matches {reference}"),
            )));
        }
        // Two is enough to tell one match from several, and no more of
        // the board is read for a lookup.
        let Some(matching) = self.store_read("task lookup", |store| {
            store.documents_with_prefix::<Task>(DOCUMENT, reference, 2)
        }) else {
            return Err(Box::new(self.storage_failure().unwrap_or_else(|| {
                Response::error(ErrorCode::StorageUnavailable, "the board could not be read")
            })));
        };
        let mut matching = matching.into_iter();
        match (matching.next(), matching.next()) {
            (Some(task), None) => Ok(task),
            (None, _) => Err(Box::new(Response::error(
                ErrorCode::NotFound,
                format!("no card matches {reference}"),
            ))),
            (Some(_), Some(_)) => Err(Box::new(Response::error(
                ErrorCode::Ambiguous,
                format!("more than one card matches {reference}; give more of the id"),
            ))),
        }
    }

    fn actor(&mut self, reference: &str) -> Result<Actor, Box<Response>> {
        match self.registry.resolve(reference) {
            Ok(id) => {
                let record = self.registry.get(&id).cloned();
                Ok(Actor {
                    human: record.as_ref().is_some_and(super::humans::is_human),
                    project: record.and_then(|r| r.project.as_ref().map(ProjectRef::id)),
                    id: id.to_string(),
                })
            }
            Err(_) if reference == HUMAN => Ok(Actor {
                id: HUMAN.to_owned(),
                human: true,
                project: None,
            }),
            Err(err) => Err(Box::new(registry_error(err))),
        }
    }

    /// Write the card and announce it, as one commit; memory has nothing
    /// of its own to keep.
    fn commit_task(&mut self, task: &Task, kind: EventKind) -> Option<Response> {
        let mut event = Event::new(kind, task.updated_at);
        event.seq = self.next_seq;
        if self.persist("task", |store| {
            store.put_document_with_event(DOCUMENT, task.id.as_str(), task, &event)
        }) != Persisted::Committed
        {
            return Some(self.write_failure().unwrap_or_else(|| {
                Response::error(ErrorCode::Internal, "the card was not recorded")
            }));
        }
        self.next_seq += 1;
        let _ = self.events.send(event);
        None
    }
}

fn refused(error: TaskError) -> Response {
    let code = match &error {
        TaskError::Invalid(_) => ErrorCode::Invalid,
        TaskError::Taken { .. } => ErrorCode::Conflict,
        TaskError::NotYours => ErrorCode::Forbidden,
        TaskError::Archived => ErrorCode::Invalid,
    };
    let details = match &error {
        TaskError::Taken { assignee, column } => Some(serde_json::json!({
            "assignee": assignee,
            "column": column,
        })),
        _ => None,
    };
    Response::Error {
        code,
        message: error.to_string(),
        details,
    }
}

impl Daemon {
    pub(super) async fn task_create(
        self: &Arc<Self>,
        from: String,
        project: Option<String>,
        title: String,
        acceptance: String,
        column: Option<Column>,
    ) -> Response {
        let named = match project {
            Some(selector) => match self.resolve_project(&selector).await {
                Ok(id) => Some(id),
                Err(response) => return *response,
            },
            None => None,
        };
        let mut state = lock(&self.state);
        let actor = match state.actor(&from) {
            Ok(actor) => actor,
            Err(response) => return *response,
        };
        let Some(project) = named.or(actor.project) else {
            return Response::error(ErrorCode::Invalid, "the caller is in no project; name one");
        };
        let task = match Task::new(project, &title, &acceptance, column, &actor.id, Utc::now()) {
            Ok(task) => task,
            Err(error) => return refused(error),
        };
        let kind = EventKind::TaskCreated {
            task: task.id.clone(),
            project: task.project.clone(),
            by: actor.id,
            title: task.title.clone(),
        };
        if let Some(error) = state.commit_task(&task, kind) {
            return error;
        }
        Response::Task { task }
    }

    /// A pull takes the card and the `task:<id>` lease in one commit.
    /// The lease is the holding: while the holder's lease lives nobody
    /// else may take the card; once it is gone — expired, released, or
    /// its agent exited — the card passes to the next taker where it
    /// sits, and the event says from whom.
    pub(super) fn task_pull(self: &Arc<Self>, reference: &str, task: &str) -> Response {
        let mut state = lock(&self.state);
        let agent = match state.resolve(reference) {
            Ok(id) => id,
            Err(response) => return *response,
        };
        let Some(record) = state
            .registry
            .get(&agent)
            .filter(|a| a.status.is_live() && !super::humans::is_human(a))
            .cloned()
        else {
            return Response::error(
                ErrorCode::Invalid,
                "a running agent pulls a card; the person assigns one with task_update",
            );
        };
        if let Some(error) = state.write_failure() {
            return error;
        }
        let now = Utc::now();
        state.expire_leases_at(now);
        if let Some(error) = state.write_failure() {
            return error;
        }
        let mut task = match state.resolve_task(task) {
            Ok(task) => task,
            Err(response) => return *response,
        };
        let resource = task_resource(&task);
        let holders: Vec<Lease> = state
            .leases
            .holders_of(&resource)
            .into_iter()
            .cloned()
            .collect();
        // The holder is whoever the card names, while they hold its lease.
        let holder_gone = !task
            .assignee
            .as_ref()
            .is_some_and(|a| holders.iter().any(|l| &l.holder == a));
        // A lease held by somebody the card does not name — a claim made
        // by hand — keeps it as any lease keeps a resource; the card's
        // own holder is answered by the card's rule, with its column.
        let held_by: Vec<Lease> = holders
            .iter()
            .filter(|l| l.holder != agent && Some(&l.holder) != task.assignee.as_ref())
            .cloned()
            .collect();
        if !held_by.is_empty() {
            let error = LeaseError::Conflict {
                resource: resource.clone(),
                held_by: held_by.clone(),
            };
            return Response::Error {
                code: ErrorCode::Conflict,
                message: error.to_string(),
                details: Some(serde_json::json!({ "resource": resource, "held_by": held_by })),
            };
        }
        let from = match task.pull(&agent, holder_gone, now) {
            Ok(from) => from,
            Err(error) => return refused(error),
        };
        let mut lease = match state.leases.clone().claim(
            resource.clone(),
            agent.clone(),
            LeaseMode::Exclusive,
            ttl(TASK_LEASE_SECS),
            Some(task.title.clone()),
            now,
        ) {
            Ok(Claimed::New(lease) | Claimed::Renewed(lease)) => lease,
            Err(error) => return lease_error(error),
        };
        lease.change_seq =
            state.store_read("lease ledger boundary", |store| store.change_watermark());
        let mut record = record;
        record.last_seen = now;
        let mut claimed = Event::new(
            EventKind::LeaseClaimed {
                lease: lease.clone(),
            },
            now,
        );
        claimed.seq = state.next_seq;
        let mut pulled = Event::new(
            EventKind::TaskPulled {
                task: task.id.clone(),
                project: task.project.clone(),
                agent: agent.clone(),
                from,
                lease: lease.id.clone(),
            },
            now,
        );
        pulled.seq = state.next_seq + 1;
        let committed = state.persist("task pull", |store| {
            store.lease_with_document(
                &record,
                &lease,
                DOCUMENT,
                task.id.as_str(),
                &task,
                &[claimed.clone(), pulled.clone()],
            )
        });
        if committed != Persisted::Committed {
            return state.write_failure().unwrap_or_else(|| {
                Response::error(ErrorCode::Internal, "the pull was not recorded")
            });
        }
        *state
            .registry
            .get_mut(&agent)
            .expect("pull identity retained") = record;
        state.leases.restore(lease);
        state.next_seq += 2;
        let _ = state.events.send(claimed);
        let _ = state.events.send(pulled);
        Response::Task { task }
    }

    pub(super) fn task_move(
        self: &Arc<Self>,
        reference: &str,
        task: &str,
        column: Column,
    ) -> Response {
        let mut state = lock(&self.state);
        let actor = match state.actor(reference) {
            Ok(actor) => actor,
            Err(response) => return *response,
        };
        let mut task = match state.resolve_task(task) {
            Ok(task) => task,
            Err(response) => return *response,
        };
        if let Err(error) = task.move_to(&actor.id, actor.human, column, Utc::now()) {
            return refused(error);
        }
        let kind = EventKind::TaskMoved {
            task: task.id.clone(),
            project: task.project.clone(),
            by: actor.id,
            column,
        };
        if let Some(error) = state.commit_task(&task, kind) {
            return error;
        }
        Response::Task { task }
    }

    pub(super) fn task_update(
        self: &Arc<Self>,
        reference: &str,
        task: &str,
        title: Option<String>,
        acceptance: Option<String>,
        assignee: Option<String>,
    ) -> Response {
        let mut state = lock(&self.state);
        let actor = match state.actor(reference) {
            Ok(actor) => actor,
            Err(response) => return *response,
        };
        let mut task = match state.resolve_task(task) {
            Ok(task) => task,
            Err(response) => return *response,
        };
        // A holder named by id, name or prefix; the empty string takes
        // the card away from whoever holds it.
        let assignee = match assignee.as_deref().map(str::trim) {
            None => None,
            Some("") => Some(None),
            Some(who) => match state.resolve(who) {
                Ok(id) => Some(Some(id)),
                Err(response) => return *response,
            },
        };
        if let Err(error) = task.update(
            &actor.id,
            actor.human,
            title.as_deref(),
            acceptance.as_deref(),
            assignee,
            Utc::now(),
        ) {
            return refused(error);
        }
        let kind = EventKind::TaskUpdated {
            task: task.id.clone(),
            project: task.project.clone(),
            by: actor.id,
        };
        if let Some(error) = state.commit_task(&task, kind) {
            return error;
        }
        Response::Task { task }
    }

    pub(super) fn task_archive(self: &Arc<Self>, reference: &str, task: &str) -> Response {
        let mut state = lock(&self.state);
        let actor = match state.actor(reference) {
            Ok(actor) => actor,
            Err(response) => return *response,
        };
        let mut task = match state.resolve_task(task) {
            Ok(task) => task,
            Err(response) => return *response,
        };
        let already = task.archived_at.is_some();
        if let Err(error) = task.archive(&actor.id, actor.human, Utc::now()) {
            return refused(error);
        }
        if already {
            return Response::Ok;
        }
        let kind = EventKind::TaskArchived {
            task: task.id.clone(),
            project: task.project.clone(),
            by: actor.id,
        };
        if let Some(error) = state.commit_task(&task, kind) {
            return error;
        }
        Response::Ok
    }

    pub(super) async fn tasks(
        self: &Arc<Self>,
        project: Option<String>,
        column: Option<Column>,
        archived: bool,
        limit: usize,
    ) -> Response {
        let project = match project {
            Some(selector) => match self.resolve_project(&selector).await {
                Ok(id) => Some(id),
                Err(response) => return *response,
            },
            None => None,
        };
        let limit = if limit == 0 {
            TASKS_LIMIT
        } else {
            limit.min(TASKS_LIMIT_MAX)
        };
        let mut state = lock(&self.state);
        let page = state.store_read("tasks", |store| {
            store.tasks_page(
                project.as_ref().map(ProjectId::as_str),
                column.map(|c| c.as_str()),
                archived,
                limit,
            )
        });
        match page {
            Some((tasks, more)) => Response::Tasks { tasks, more },
            None => state.storage_failure().unwrap_or_else(|| {
                Response::error(ErrorCode::StorageUnavailable, "the board could not be read")
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn open(dir: &TempDir) -> Arc<Daemon> {
        let home = dir.path().to_path_buf();
        Arc::new(Daemon::open(home.clone(), home.join("sock")).unwrap())
    }

    async fn register(daemon: &Arc<Daemon>, name: &str, workdir: &std::path::Path) -> AgentRecord {
        match daemon
            .handle(Request::Register {
                spec: AgentSpec {
                    name: name.to_owned(),
                    workdir: Some(workdir.to_owned()),
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

    /// The person files a card; two agents pull it and only the first
    /// gets it, the second told who holds it; the holder moves it on and
    /// nobody else can; the person can reassign, move back (releasing
    /// it) and archive; the board lists by column, oldest first, and
    /// leaves archived cards out unless asked; it all survives a reopen.
    #[tokio::test]
    async fn a_card_is_pulled_once_and_moved_by_its_holder_or_the_person() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let alice = register(&daemon, "alice", &work).await;
        let bob = register(&daemon, "bob", &work).await;
        let Response::Agent { agent: person } = daemon
            .handle(Request::Me {
                workdir: Some(work.clone()),
            })
            .await
        else {
            panic!("the person")
        };
        let project = alice.project.clone().unwrap().id();
        let create = |daemon: &Arc<Daemon>, title: &str, column: Option<Column>| {
            let daemon = daemon.clone();
            let title = title.to_owned();
            let work = work.clone();
            async move {
                match daemon
                    .handle(Request::TaskCreate {
                        from: HUMAN.to_owned(),
                        project: Some(work.display().to_string()),
                        title,
                        acceptance: "it works".to_owned(),
                        column,
                    })
                    .await
                {
                    Response::Task { task } => task,
                    other => panic!("{other:?}"),
                }
            }
        };
        assert!(matches!(
            daemon
                .handle(Request::TaskCreate {
                    from: HUMAN.to_owned(),
                    project: Some(work.display().to_string()),
                    title: "  ".to_owned(),
                    acceptance: String::new(),
                    column: None,
                })
                .await,
            Response::Error {
                code: ErrorCode::Invalid,
                ..
            }
        ));
        let first = create(&daemon, "Fix login", Some(Column::Ready)).await;
        let second = create(&daemon, "Write the release notes", None).await;
        assert_eq!(
            (first.column, first.created_by.as_str()),
            (Column::Ready, person.id.as_str()),
            "filed by the person's record"
        );
        assert_eq!(second.column, Column::Backlog);
        // A prefix resolves; two agents race for the card.
        let prefix = &first.id.as_str()[..6];
        let Response::Task { task: pulled } = daemon
            .handle(Request::TaskPull {
                agent: "alice".to_owned(),
                task: prefix.to_owned(),
            })
            .await
        else {
            panic!("alice pulls")
        };
        assert_eq!(
            (pulled.assignee.as_ref(), pulled.column),
            (Some(&alice.id), Column::InProgress)
        );
        // The pull is a lease on the card, with its title as the note:
        // `leases` shows who holds which card.
        let Response::Leases { leases } = daemon
            .handle(Request::Leases {
                agent: None,
                resource: Some(format!("task:{}", first.id)),
            })
            .await
        else {
            panic!("leases")
        };
        assert_eq!(
            leases
                .iter()
                .map(|l| (l.holder.clone(), l.note.clone()))
                .collect::<Vec<_>>(),
            vec![(alice.id.clone(), Some("Fix login".to_owned()))]
        );
        match daemon
            .handle(Request::TaskPull {
                agent: "bob".to_owned(),
                task: first.id.to_string(),
            })
            .await
        {
            Response::Error {
                code: ErrorCode::Conflict,
                details,
                ..
            } => assert_eq!(details.unwrap()["assignee"], alice.id.as_str()),
            other => panic!("{other:?}"),
        }
        // Not ready: Backlog is the person's.
        assert!(matches!(
            daemon
                .handle(Request::TaskPull {
                    agent: "bob".to_owned(),
                    task: second.id.to_string(),
                })
                .await,
            Response::Error {
                code: ErrorCode::Conflict,
                ..
            }
        ));
        // The person does not pull; they assign.
        assert!(matches!(
            daemon
                .handle(Request::TaskPull {
                    agent: HUMAN.to_owned(),
                    task: second.id.to_string(),
                })
                .await,
            Response::Error {
                code: ErrorCode::Invalid,
                ..
            }
        ));
        // Moves: bob cannot, alice can, the person can.
        assert!(matches!(
            daemon
                .handle(Request::TaskMove {
                    agent: "bob".to_owned(),
                    task: first.id.to_string(),
                    column: Column::Review,
                })
                .await,
            Response::Error {
                code: ErrorCode::Forbidden,
                ..
            }
        ));
        let Response::Task { task: reviewed } = daemon
            .handle(Request::TaskMove {
                agent: "alice".to_owned(),
                task: first.id.to_string(),
                column: Column::Review,
            })
            .await
        else {
            panic!("alice moves")
        };
        assert_eq!(reviewed.column, Column::Review);
        let Response::Task { task: released } = daemon
            .handle(Request::TaskMove {
                agent: HUMAN.to_owned(),
                task: first.id.to_string(),
                column: Column::Ready,
            })
            .await
        else {
            panic!("the person moves")
        };
        assert_eq!((released.assignee, released.column), (None, Column::Ready));
        // The person hands the other card to bob and moves it on.
        let Response::Task { task: handed } = daemon
            .handle(Request::TaskUpdate {
                agent: HUMAN.to_owned(),
                task: second.id.to_string(),
                title: None,
                acceptance: Some("Notes for 0.1.1".to_owned()),
                assignee: Some("bob".to_owned()),
            })
            .await
        else {
            panic!("the person assigns")
        };
        assert_eq!(
            (handed.assignee.as_ref(), handed.acceptance.as_str()),
            (Some(&bob.id), "Notes for 0.1.1")
        );
        // Listed by column, archived left out; a reopen keeps it all.
        let list = |daemon: &Arc<Daemon>, archived: bool| {
            let daemon = daemon.clone();
            let work = work.clone();
            async move {
                match daemon
                    .handle(Request::Tasks {
                        project: Some(work.display().to_string()),
                        column: None,
                        archived,
                        limit: 0,
                    })
                    .await
                {
                    Response::Tasks { tasks, more: false } => tasks,
                    other => panic!("{other:?}"),
                }
            }
        };
        let board = list(&daemon, false).await;
        assert_eq!(
            board
                .iter()
                .map(|t| (t.column, t.title.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (Column::Backlog, "Write the release notes"),
                (Column::Ready, "Fix login")
            ]
        );
        assert!(
            matches!(
                daemon
                    .handle(Request::TaskArchive {
                        agent: "bob".to_owned(),
                        task: second.id.to_string(),
                    })
                    .await,
                Response::Error {
                    code: ErrorCode::Forbidden,
                    ..
                }
            ),
            "bob holds it but it is not done"
        );
        assert!(matches!(
            daemon
                .handle(Request::TaskArchive {
                    agent: HUMAN.to_owned(),
                    task: second.id.to_string(),
                })
                .await,
            Response::Ok
        ));
        assert_eq!(list(&daemon, false).await.len(), 1);
        assert_eq!(list(&daemon, true).await.len(), 2);
        drop(daemon);
        let daemon = open(&dir);
        let board = list(&daemon, true).await;
        assert_eq!(board.len(), 2);
        assert!(
            board
                .iter()
                .any(|t| t.id == first.id && t.column == Column::Ready && t.assignee.is_none())
        );
        assert!(
            board
                .iter()
                .any(|t| t.id == second.id && t.archived_at.is_some())
        );
        let kinds: Vec<String> = daemon
            .recent_events(30)
            .iter()
            .filter_map(|e| match &e.kind {
                EventKind::TaskCreated { .. } => Some("created"),
                EventKind::TaskPulled { .. } => Some("pulled"),
                EventKind::TaskMoved { .. } => Some("moved"),
                EventKind::TaskUpdated { .. } => Some("updated"),
                EventKind::TaskArchived { .. } => Some("archived"),
                _ => None,
            })
            .map(str::to_owned)
            .collect();
        assert_eq!(
            kinds,
            [
                "created", "created", "pulled", "moved", "moved", "updated", "archived"
            ]
        );
        let _ = project;
    }

    /// The holding is the lease. While alice holds `task:<id>` bob is
    /// refused; once her lease is gone — released here, as an exit or
    /// expiry would — bob's pull takes the card over where it sits, the
    /// event says from whom, and alice can no longer move it. A hand
    /// claim on the card by somebody else keeps it like any lease.
    #[tokio::test]
    async fn a_card_whose_holders_lease_lapsed_passes_to_the_next_puller() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let alice = register(&daemon, "alice", &work).await;
        let bob = register(&daemon, "bob", &work).await;
        let carol = register(&daemon, "carol", &work).await;
        let Response::Task { task } = daemon
            .handle(Request::TaskCreate {
                from: HUMAN.to_owned(),
                project: Some(work.display().to_string()),
                title: "Port the parser".to_owned(),
                acceptance: "tests pass".to_owned(),
                column: Some(Column::Ready),
            })
            .await
        else {
            panic!("filed")
        };
        let pull = |daemon: &Arc<Daemon>, who: &str| {
            let daemon = daemon.clone();
            let who = who.to_owned();
            let id = task.id.to_string();
            async move {
                daemon
                    .handle(Request::TaskPull {
                        agent: who,
                        task: id,
                    })
                    .await
            }
        };
        assert!(matches!(
            pull(&daemon, "alice").await,
            Response::Task { .. }
        ));
        assert!(matches!(
            pull(&daemon, "bob").await,
            Response::Error {
                code: ErrorCode::Conflict,
                ..
            }
        ));
        // Alice moves it to review, then her lease goes.
        assert!(matches!(
            daemon
                .handle(Request::TaskMove {
                    agent: "alice".to_owned(),
                    task: task.id.to_string(),
                    column: Column::Review,
                })
                .await,
            Response::Task { .. }
        ));
        assert!(matches!(
            daemon
                .handle(Request::ReleaseAll {
                    agent: "alice".to_owned(),
                    summary: Some("stepping away".to_owned()),
                    summary_source: Default::default(),
                })
                .await,
            Response::Leases { .. }
        ));
        // A pull by hand of the card's lease by carol keeps it from bob.
        let Response::Lease { lease: by_hand } = daemon
            .handle(Request::Claim {
                agent: "carol".to_owned(),
                resource: format!("task:{}", task.id),
                mode: LeaseMode::Exclusive,
                amount: None,
                ttl_secs: 60,
                note: None,
                wait_secs: 0,
            })
            .await
        else {
            panic!("carol claims by hand")
        };
        match pull(&daemon, "bob").await {
            Response::Error {
                code: ErrorCode::Conflict,
                details,
                ..
            } => assert_eq!(details.unwrap()["held_by"][0]["holder"], carol.id.as_str()),
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            daemon
                .handle(Request::Release {
                    agent: "carol".to_owned(),
                    lease: by_hand.id,
                    summary: None,
                    summary_source: Default::default(),
                })
                .await,
            Response::Lease { .. } | Response::Ok | Response::Leases { .. }
        ));
        // Now bob takes it over, where it sits.
        let Response::Task { task: taken } = pull(&daemon, "bob").await else {
            panic!("bob takes over")
        };
        assert_eq!(
            (taken.assignee.as_ref(), taken.column),
            (Some(&bob.id), Column::Review)
        );
        assert!(
            daemon.recent_events(10).iter().any(|e| matches!(
                &e.kind,
                EventKind::TaskPulled { agent, from: Some(from), .. }
                    if agent == &bob.id && from == &alice.id
            )),
            "the event says from whom"
        );
        assert!(matches!(
            daemon
                .handle(Request::TaskMove {
                    agent: "alice".to_owned(),
                    task: task.id.to_string(),
                    column: Column::Done,
                })
                .await,
            Response::Error {
                code: ErrorCode::Forbidden,
                ..
            }
        ));
        // Bob pulling his own card again gains nothing and is told so.
        assert!(matches!(
            pull(&daemon, "bob").await,
            Response::Error {
                code: ErrorCode::Conflict,
                ..
            }
        ));
    }

    /// The board is read a page at a time and says when it goes on; a
    /// lookup by prefix reads two cards, not the board; and when storage
    /// has failed neither pretends the board is empty.
    #[tokio::test]
    async fn the_board_is_paged_and_a_failed_store_is_not_an_empty_board() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let alice = register(&daemon, "alice", &work).await;
        for (i, column) in [Column::Done, Column::Backlog, Column::Ready]
            .into_iter()
            .enumerate()
        {
            assert!(matches!(
                daemon
                    .handle(Request::TaskCreate {
                        from: HUMAN.to_owned(),
                        project: Some(work.display().to_string()),
                        title: format!("card {i}"),
                        acceptance: "x".repeat(agentdocker_core::task::ACCEPTANCE_CHARS),
                        column: Some(column),
                    })
                    .await,
                Response::Task { .. }
            ));
        }
        let list = |limit: usize, column: Option<Column>| {
            let daemon = daemon.clone();
            let work = work.clone();
            async move {
                daemon
                    .handle(Request::Tasks {
                        project: Some(work.display().to_string()),
                        column,
                        archived: false,
                        limit,
                    })
                    .await
            }
        };
        match list(2, None).await {
            Response::Tasks { tasks, more } => {
                assert!(more, "the board goes on");
                assert_eq!(
                    tasks.iter().map(|t| t.column).collect::<Vec<_>>(),
                    [Column::Backlog, Column::Ready],
                    "Backlog to Done, so Done is what a short page leaves out"
                );
            }
            other => panic!("{other:?}"),
        }
        match list(2, Some(Column::Done)).await {
            Response::Tasks { tasks, more } => {
                assert_eq!((tasks.len(), more), (1, false));
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            list(0, None).await,
            Response::Tasks { more: false, .. }
        ));
        {
            let mut state = lock(&daemon.state);
            state.storage_error = Some("disk gone".to_owned());
        }
        assert!(matches!(
            list(0, None).await,
            Response::Error {
                code: ErrorCode::StorageUnavailable,
                ..
            }
        ));
        assert!(matches!(
            daemon
                .handle(Request::TaskPull {
                    agent: alice.id.to_string(),
                    task: "abc".to_owned(),
                })
                .await,
            Response::Error {
                code: ErrorCode::StorageUnavailable,
                ..
            }
        ));
    }
}
