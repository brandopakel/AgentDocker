//! The board of work: cards in columns, pulled by agents one at a time.
//! The rules are core's (`agentdocker_core::task`); the daemon's part is
//! to hold every card under its one mutex — which is what makes a pull
//! atomic — to keep them as documents, and to say what changed.
use super::*;
use agentdocker_core::task::{Task, TaskError, TaskId};
use agentdocker_core::{Column, HUMAN};

const DOCUMENT: &str = "task";

/// Who is acting on a card: an agent by its record, or the person — by
/// their record, or as the bare `user` a shell speaks as.
struct Actor {
    id: String,
    human: bool,
    project: Option<ProjectId>,
}

impl State {
    fn task(&mut self, id: &TaskId) -> Option<Task> {
        self.store_read("task", |store| store.document(DOCUMENT, id.as_str()))
            .flatten()
    }

    /// A card by its id or a unique prefix of it, on the board.
    fn resolve_task(&mut self, reference: &str) -> Result<Task, Box<Response>> {
        let reference = reference.trim();
        if reference.is_empty() {
            return Err(Box::new(Response::error(
                ErrorCode::Invalid,
                "name a card by its id",
            )));
        }
        let all: Vec<Task> = self
            .store_read("tasks", |store| store.documents::<Task>(DOCUMENT, None))
            .unwrap_or_default();
        let mut matching = all
            .into_iter()
            .filter(|t| t.id.as_str().starts_with(reference));
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
            return Response::error(
                ErrorCode::Invalid,
                "the caller is in no project; name one",
            );
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

    pub(super) fn task_pull(self: &Arc<Self>, reference: &str, task: &str) -> Response {
        let mut state = lock(&self.state);
        let agent = match state.resolve(reference) {
            Ok(id) => id,
            Err(response) => return *response,
        };
        if !state
            .registry
            .get(&agent)
            .is_some_and(|a| a.status.is_live() && !super::humans::is_human(a))
        {
            return Response::error(
                ErrorCode::Invalid,
                "a running agent pulls a card; the person assigns one with task_update",
            );
        }
        let mut task = match state.resolve_task(task) {
            Ok(task) => task,
            Err(response) => return *response,
        };
        if let Err(error) = task.pull(&agent, Utc::now()) {
            return refused(error);
        }
        let kind = EventKind::TaskPulled {
            task: task.id.clone(),
            project: task.project.clone(),
            agent,
        };
        if let Some(error) = state.commit_task(&task, kind) {
            return error;
        }
        Response::Task { task }
    }

    pub(super) fn task_move(self: &Arc<Self>, reference: &str, task: &str, column: Column) -> Response {
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
    ) -> Response {
        let project = match project {
            Some(selector) => match self.resolve_project(&selector).await {
                Ok(id) => Some(id),
                Err(response) => return *response,
            },
            None => None,
        };
        let mut state = lock(&self.state);
        let mut tasks: Vec<Task> = state
            .store_read("tasks", |store| store.documents::<Task>(DOCUMENT, None))
            .unwrap_or_default()
            .into_iter()
            .filter(|t| project.as_ref().is_none_or(|p| &t.project == p))
            .filter(|t| column.is_none_or(|c| t.column == c))
            .filter(|t| archived || t.archived_at.is_none())
            .collect();
        // Backlog to Done, the oldest first within a column: what was
        // filed first is at the top.
        tasks.sort_by(|a, b| {
            a.column
                .cmp(&b.column)
                .then(a.created_at.cmp(&b.created_at))
                .then(a.id.cmp(&b.id))
        });
        Response::Tasks { tasks }
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
        assert_eq!((first.column, first.created_by.as_str()), (Column::Ready, "user"));
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
        assert_eq!((pulled.assignee.as_ref(), pulled.column), (Some(&alice.id), Column::InProgress));
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
        assert_eq!((handed.assignee.as_ref(), handed.acceptance.as_str()), (Some(&bob.id), "Notes for 0.1.1"));
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
                    })
                    .await
                {
                    Response::Tasks { tasks } => tasks,
                    other => panic!("{other:?}"),
                }
            }
        };
        let board = list(&daemon, false).await;
        assert_eq!(
            board.iter().map(|t| (t.column, t.title.as_str())).collect::<Vec<_>>(),
            vec![(Column::Backlog, "Write the release notes"), (Column::Ready, "Fix login")]
        );
        assert!(matches!(
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
        ), "bob holds it but it is not done");
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
        assert!(board.iter().any(|t| t.id == first.id && t.column == Column::Ready && t.assignee.is_none()));
        assert!(board.iter().any(|t| t.id == second.id && t.archived_at.is_some()));
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
        assert_eq!(kinds, ["created", "created", "pulled", "moved", "moved", "updated", "archived"]);
        let _ = project;
    }
}
