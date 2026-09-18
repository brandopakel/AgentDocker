//! The board of work: cards in columns, pulled by agents one at a time.
//! The rules are core's (`agentdocker_core::task`); the daemon's part is
//! to hold every card under its one mutex — which is what makes a pull
//! atomic — to keep them as documents, and to say what changed.
use super::*;
use agentdocker_core::protocol::{TASKS_LIMIT, TASKS_LIMIT_MAX, TASKS_PAGE_BYTES};
use agentdocker_core::task::{Task, TaskError};
use agentdocker_core::{Column, HUMAN, LeaseMode, ResourceKey};
use chrono::DateTime;

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

    /// The live exclusive `task:<id>` lease on a card, by its holder:
    /// the holding. A shared claim on the card, made by hand, is not a
    /// hold — it excludes nobody, so it entitles nobody to the card.
    fn task_hold(&self, task: &Task, holder: &AgentId) -> Option<Lease> {
        let resource = task_resource(task);
        self.leases
            .holders_of(&resource)
            .into_iter()
            .find(|l| &l.holder == holder && l.mode == LeaseMode::Exclusive)
            .cloned()
    }

    /// Every live lease on a card: its holder's, and any taken by hand.
    fn task_holds(&self, task: &Task) -> Vec<Lease> {
        self.leases
            .holders_of(&task_resource(task))
            .into_iter()
            .cloned()
            .collect()
    }

    /// Take the card's lease for `holder`, or renew the one it has,
    /// with the leases the same transition ends already out of the
    /// way; the lease is not in memory or the store until the
    /// transition commits.
    fn take_hold(
        &mut self,
        task: &Task,
        holder: &AgentId,
        released: &[Lease],
        now: DateTime<Utc>,
    ) -> Result<Lease, Box<Response>> {
        let mut table = self.leases.clone();
        for lease in released {
            let _ = table.release(&lease.id, &lease.holder);
        }
        let claimed = table
            .claim(
                task_resource(task),
                holder.clone(),
                LeaseMode::Exclusive,
                ttl(TASK_LEASE_SECS),
                Some(task.title.clone()),
                now,
            )
            .map_err(|error| Box::new(lease_error(error)))?;
        let mut lease = claimed.into_lease();
        lease.change_seq =
            self.store_read("lease ledger boundary", |store| store.change_watermark());
        Ok(lease)
    }

    /// Write the card, the lease it took or renewed, the leases it ended
    /// and every event, as one commit; then move memory and announce.
    /// Nothing changes in memory unless the store took all of it.
    fn commit_task(
        &mut self,
        task: &Task,
        holder: Option<AgentId>,
        claimed: Option<Lease>,
        released: Vec<Lease>,
        kinds: Vec<EventKind>,
    ) -> Option<Response> {
        let now = task.updated_at;
        let record = holder.as_ref().map(|id| {
            let mut record = self
                .registry
                .get(id)
                .expect("holder identity retained")
                .clone();
            record.last_seen = now;
            record
        });
        let events: Vec<Event> = kinds
            .into_iter()
            .enumerate()
            .map(|(i, kind)| {
                let mut event = Event::new(kind, now);
                event.seq = self.next_seq + i as u64;
                event
            })
            .collect();
        let ended: Vec<LeaseId> = released.iter().map(|l| l.id.clone()).collect();
        let transition = crate::store::TaskTransition {
            holder: record.as_ref(),
            claimed: claimed.as_ref(),
            released: &ended,
            kind: DOCUMENT,
            id: task.id.as_str(),
            events: &events,
        };
        if self.persist("task", |store| store.task_transition(&transition, task))
            != Persisted::Committed
        {
            return Some(self.write_failure().unwrap_or_else(|| {
                Response::error(ErrorCode::Internal, "the card was not recorded")
            }));
        }
        if let (Some(id), Some(record)) = (holder, record) {
            *self
                .registry
                .get_mut(&id)
                .expect("holder identity retained") = record;
        }
        for lease in &released {
            let _ = self.leases.release(&lease.id, &lease.holder);
        }
        if let Some(lease) = claimed {
            self.leases.restore(lease);
        }
        self.next_seq += events.len() as u64;
        for event in events {
            let _ = self.events.send(event);
        }
        None
    }
}

fn refused(error: TaskError) -> Response {
    refused_with(error, None)
}

/// A refusal by the card's rule, with what the caller can do about a hold
/// that has lapsed: the plain pull that met it is told to name the holder.
fn refused_with(error: TaskError, lapsed: Option<&AgentId>) -> Response {
    let code = match &error {
        TaskError::Invalid(_) => ErrorCode::Invalid,
        TaskError::Taken { .. } => ErrorCode::Conflict,
        TaskError::NotYours => ErrorCode::Forbidden,
        TaskError::Archived => ErrorCode::Invalid,
    };
    let (message, details) = match &error {
        TaskError::Taken { assignee, column } => {
            let mut details = serde_json::json!({
                "assignee": assignee,
                "column": column,
            });
            let message = match lapsed {
                Some(holder) => {
                    details["hold"] = serde_json::json!("lapsed");
                    format!(
                        "{error}, whose hold has lapsed; to take it over, pull again with take_over_from naming {holder}"
                    )
                }
                None => {
                    if assignee.is_some() {
                        details["hold"] = serde_json::json!("live");
                    }
                    error.to_string()
                }
            };
            (message, Some(details))
        }
        _ => (error.to_string(), None),
    };
    Response::Error {
        code,
        message,
        details,
    }
}

/// An agent's change to a card it no longer holds: the lease lapsed.
fn hold_lapsed(task: &Task) -> Response {
    Response::Error {
        code: ErrorCode::Forbidden,
        message: "your hold on the card has lapsed; pull it again with take_over_from naming yourself before moving it".to_string(),
        details: Some(serde_json::json!({
            "assignee": task.assignee,
            "column": task.column,
            "hold": "lapsed",
        })),
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
        if let Some(error) = state.commit_task(&task, None, None, Vec::new(), vec![kind]) {
            return error;
        }
        Response::Task { task }
    }

    /// A pull takes the card and the `task:<id>` lease in one commit.
    /// The lease is the holding: while the holder's lease lives nobody
    /// else may take the card, and a pull of one's own held card renews
    /// it. Once the hold has lapsed — expired, released, or its agent
    /// exited — the card is taken over only by name: `take_over_from`
    /// says whom the caller expects to take it from, and a card that
    /// names somebody else, or whose hold is live after all, is refused.
    pub(super) fn task_pull(
        self: &Arc<Self>,
        reference: &str,
        task: &str,
        take_over_from: Option<&str>,
    ) -> Response {
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
        // Paused: nothing new is taken, as with any lease.
        if let Some(pause) = state.pause_holding(&agent) {
            return super::pause_refusal(pause);
        }
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
        let held = task
            .assignee
            .as_ref()
            .and_then(|a| state.task_hold(&task, a));
        // A lease held by somebody the card does not name — a claim made
        // by hand — keeps it as any lease keeps a resource.
        let by_hand: Vec<Lease> = state
            .task_holds(&task)
            .into_iter()
            .filter(|l| l.holder != agent && Some(&l.holder) != task.assignee.as_ref())
            .collect();
        if !by_hand.is_empty() {
            return lease_error(LeaseError::Conflict {
                resource: task_resource(&task),
                held_by: by_hand,
            });
        }
        let lapsed = task.assignee.clone().filter(|_| {
            held.is_none() && matches!(task.column, Column::InProgress | Column::Review)
        });
        let from = match take_over_from {
            None => {
                if task.assignee.as_ref() == Some(&agent) && held.is_some() {
                    // Its own held card: the pull is a renewal, so a
                    // retry after an uncertain reply changes nothing.
                    let lease = match state.take_hold(&task, &agent, &[], now) {
                        Ok(lease) => lease,
                        Err(response) => return *response,
                    };
                    let kind = EventKind::LeaseRenewed {
                        lease: lease.clone(),
                    };
                    task.updated_at = now;
                    if let Some(error) =
                        state.commit_task(&task, Some(agent), Some(lease), Vec::new(), vec![kind])
                    {
                        return error;
                    }
                    return Response::Task { task };
                }
                if let Err(error) = task.pull(&agent, now) {
                    return refused_with(error, lapsed.as_ref());
                }
                None
            }
            Some(named) => {
                let from = match state.resolve(named) {
                    Ok(id) => id,
                    Err(response) => return *response,
                };
                if lapsed.as_ref() != Some(&from) {
                    // The hold is live, or the card names somebody else,
                    // or sits where a take-over does not apply.
                    return refused(TaskError::Taken {
                        assignee: task.assignee.clone(),
                        column: task.column,
                    });
                }
                if let Err(error) = task.take_over(&agent, &from, now) {
                    return refused(error);
                }
                Some(from)
            }
        };
        let lease = match state.take_hold(&task, &agent, &[], now) {
            Ok(lease) => lease,
            Err(response) => return *response,
        };
        let kinds = vec![
            EventKind::LeaseClaimed {
                lease: lease.clone(),
            },
            EventKind::TaskPulled {
                task: task.id.clone(),
                project: task.project.clone(),
                agent: agent.clone(),
                from,
                lease: lease.id.clone(),
            },
        ];
        if let Some(error) = state.commit_task(&task, Some(agent), Some(lease), Vec::new(), kinds) {
            return error;
        }
        Response::Task { task }
    }

    /// An agent's change to a card it holds requires the hold to be
    /// live: a lapsed lease is told so, and the card is pulled again by
    /// name before it moves. The person needs no hold.
    fn agent_hold(state: &State, task: &Task, actor: &Actor) -> Result<(), Box<Response>> {
        if actor.human {
            return Ok(());
        }
        let holder = AgentId::from(actor.id.clone());
        if task.assignee.as_ref() == Some(&holder) && state.task_hold(task, &holder).is_none() {
            return Err(Box::new(hold_lapsed(task)));
        }
        Ok(())
    }

    /// The leases a card's new state ends: whoever it named and no
    /// longer does, and everybody's once it leaves the working columns
    /// or the board — a hand's old holder, a release, done, archived.
    fn holds_ended(state: &State, before: &Task, after: &Task) -> Vec<Lease> {
        let working = matches!(after.column, Column::InProgress | Column::Review)
            && after.archived_at.is_none();
        state
            .task_holds(before)
            .into_iter()
            .filter(|l| !working || Some(&l.holder) != after.assignee.as_ref())
            .collect()
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
        if let Some(error) = state.write_failure() {
            return error;
        }
        let now = Utc::now();
        state.expire_leases_at(now);
        let mut task = match state.resolve_task(task) {
            Ok(task) => task,
            Err(response) => return *response,
        };
        if let Err(response) = Self::agent_hold(&state, &task, &actor) {
            return *response;
        }
        let before = task.clone();
        if let Err(error) = task.move_to(&actor.id, actor.human, column, now) {
            return refused(error);
        }
        let released = Self::holds_ended(&state, &before, &task);
        let mut kinds = vec![EventKind::TaskMoved {
            task: task.id.clone(),
            project: task.project.clone(),
            by: actor.id,
            column,
        }];
        kinds.extend(released.iter().map(|lease| EventKind::LeaseReleased {
            lease: lease.clone(),
        }));
        if let Some(error) = state.commit_task(&task, None, None, released, kinds) {
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
        if let Some(error) = state.write_failure() {
            return error;
        }
        let now = Utc::now();
        state.expire_leases_at(now);
        let mut task = match state.resolve_task(task) {
            Ok(task) => task,
            Err(response) => return *response,
        };
        if let Err(response) = Self::agent_hold(&state, &task, &actor) {
            return *response;
        }
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
        let before = task.clone();
        if let Err(error) = task.update(
            &actor.id,
            actor.human,
            title.as_deref(),
            acceptance.as_deref(),
            assignee,
            now,
        ) {
            return refused(error);
        }
        let released = Self::holds_ended(&state, &before, &task);
        // A hand to a running agent is its hold from now: the person's
        // confirmed reassignment takes the lease for them. Handed to an
        // agent that is not running, the card waits for it to pull by
        // name.
        let handed = task
            .assignee
            .clone()
            .filter(|a| before.assignee.as_ref() != Some(a))
            .filter(|a| {
                state
                    .registry
                    .get(a)
                    .is_some_and(|r| r.status.is_live() && !super::humans::is_human(r))
            });
        let claimed = match handed.as_ref() {
            Some(holder) => match state.take_hold(&task, holder, &released, now) {
                Ok(lease) => Some(lease),
                Err(response) => return *response,
            },
            None => None,
        };
        let mut kinds = vec![EventKind::TaskUpdated {
            task: task.id.clone(),
            project: task.project.clone(),
            by: actor.id,
        }];
        kinds.extend(released.iter().map(|lease| EventKind::LeaseReleased {
            lease: lease.clone(),
        }));
        if let Some(lease) = &claimed {
            kinds.push(EventKind::LeaseClaimed {
                lease: lease.clone(),
            });
        }
        if let Some(error) = state.commit_task(&task, handed, claimed, released, kinds) {
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
        if let Some(error) = state.write_failure() {
            return error;
        }
        let now = Utc::now();
        state.expire_leases_at(now);
        let mut task = match state.resolve_task(task) {
            Ok(task) => task,
            Err(response) => return *response,
        };
        if let Err(response) = Self::agent_hold(&state, &task, &actor) {
            return *response;
        }
        let already = task.archived_at.is_some();
        let before = task.clone();
        if let Err(error) = task.archive(&actor.id, actor.human, now) {
            return refused(error);
        }
        if already {
            return Response::Ok;
        }
        let released = Self::holds_ended(&state, &before, &task);
        let mut kinds = vec![EventKind::TaskArchived {
            task: task.id.clone(),
            project: task.project.clone(),
            by: actor.id,
        }];
        kinds.extend(released.iter().map(|lease| EventKind::LeaseReleased {
            lease: lease.clone(),
        }));
        if let Some(error) = state.commit_task(&task, None, None, released, kinds) {
            return error;
        }
        Response::Ok
    }

    pub(super) async fn tasks(
        self: &Arc<Self>,
        project: Option<String>,
        column: Option<Column>,
        archived: bool,
        offset: usize,
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
                offset,
                limit,
                TASKS_PAGE_BYTES,
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
                take_over_from: None,
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
                take_over_from: None,
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
                    take_over_from: None,
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
                    take_over_from: None,
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
                        offset: 0,
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
    /// refused and alice's own pull renews; once her lease is gone —
    /// released here, as an exit or expiry would — a plain pull is still
    /// refused (hold lapsed), alice can no longer move the card, and bob
    /// takes it over only by naming her, where it sits, the event saying
    /// from whom. A hand claim on the card by somebody else keeps it like
    /// any lease; the person's moves and hands end and take leases in
    /// the same commit; a failed store writes none of it.
    #[tokio::test]
    async fn a_lapsed_hold_is_recovered_by_name_and_the_persons_hands_move_the_lease() {
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
        let pull = |daemon: &Arc<Daemon>, who: &str, from: Option<&str>| {
            let daemon = daemon.clone();
            let who = who.to_owned();
            let from = from.map(str::to_owned);
            let id = task.id.to_string();
            async move {
                daemon
                    .handle(Request::TaskPull {
                        agent: who,
                        task: id,
                        take_over_from: from,
                    })
                    .await
            }
        };
        let holds = |daemon: &Arc<Daemon>| {
            let daemon = daemon.clone();
            let resource = format!("task:{}", task.id);
            async move {
                match daemon
                    .handle(Request::Leases {
                        agent: None,
                        resource: Some(resource),
                    })
                    .await
                {
                    Response::Leases { leases } => {
                        leases.into_iter().map(|l| l.holder).collect::<Vec<_>>()
                    }
                    other => panic!("{other:?}"),
                }
            }
        };
        assert!(matches!(
            pull(&daemon, "alice", None).await,
            Response::Task { .. }
        ));
        assert_eq!(holds(&daemon).await, vec![alice.id.clone()]);
        // Alice's own pull renews; bob's is refused with a live hold.
        assert!(matches!(
            pull(&daemon, "alice", None).await,
            Response::Task { .. }
        ));
        assert_eq!(holds(&daemon).await, vec![alice.id.clone()]);
        match pull(&daemon, "bob", None).await {
            Response::Error {
                code: ErrorCode::Conflict,
                details,
                ..
            } => assert_eq!(details.unwrap()["hold"], "live"),
            other => panic!("{other:?}"),
        }
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
                    only_automatic: false,
                })
                .await,
            Response::Leases { .. }
        ));
        assert!(holds(&daemon).await.is_empty());
        // A plain pull is refused and told the hold lapsed; alice cannot
        // move the card she no longer holds; a wrong name takes nothing.
        match pull(&daemon, "bob", None).await {
            Response::Error {
                code: ErrorCode::Conflict,
                details,
                message,
            } => {
                assert_eq!(details.unwrap()["hold"], "lapsed");
                assert!(message.contains("take_over_from"), "{message}");
            }
            other => panic!("{other:?}"),
        }
        match daemon
            .handle(Request::TaskMove {
                agent: "alice".to_owned(),
                task: task.id.to_string(),
                column: Column::Done,
            })
            .await
        {
            Response::Error {
                code: ErrorCode::Forbidden,
                details,
                ..
            } => assert_eq!(details.unwrap()["hold"], "lapsed"),
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            pull(&daemon, "bob", Some("carol")).await,
            Response::Error {
                code: ErrorCode::Conflict,
                ..
            }
        ));
        // A claim by hand of the card's lease by carol keeps it from bob.
        let Response::Lease { lease: by_hand } = daemon
            .handle(Request::Claim {
                agent: "carol".to_owned(),
                resource: format!("task:{}", task.id),
                mode: LeaseMode::Exclusive,
                amount: None,
                ttl_secs: 60,
                note: None,
                wait_secs: 0,
                automatic: false,
            })
            .await
        else {
            panic!("carol claims by hand")
        };
        match pull(&daemon, "bob", Some("alice")).await {
            Response::Error {
                code: ErrorCode::Conflict,
                message,
                ..
            } => assert!(
                message.contains(carol.id.short()) || message.contains("carol"),
                "{message}"
            ),
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
        // Alice re-takes her own lapsed hold by name, then bob takes it
        // over from her by name, where it sits.
        let Response::Task { task: retaken } = pull(&daemon, "alice", Some("alice")).await else {
            panic!("alice re-takes")
        };
        assert_eq!(retaken.assignee.as_ref(), Some(&alice.id));
        assert_eq!(holds(&daemon).await, vec![alice.id.clone()]);
        assert!(matches!(
            daemon
                .handle(Request::ReleaseAll {
                    agent: "alice".to_owned(),
                    summary: None,
                    summary_source: Default::default(),
                    only_automatic: false,
                })
                .await,
            Response::Leases { .. }
        ));
        let Response::Task { task: taken } = pull(&daemon, "bob", Some("alice")).await else {
            panic!("bob takes over")
        };
        assert_eq!(
            (taken.assignee.as_ref(), taken.column),
            (Some(&bob.id), Column::Review)
        );
        assert_eq!(holds(&daemon).await, vec![bob.id.clone()]);
        assert!(
            daemon.recent_events(10).iter().any(|e| matches!(
                &e.kind,
                EventKind::TaskPulled { agent, from: Some(from), .. }
                    if agent == &bob.id && from == &alice.id
            )),
            "the event says from whom"
        );
        // The person hands it to carol: bob's lease ends and carol's
        // begins in the one commit; a move back to Ready ends carol's;
        // done by its holder ends the hold but keeps the name.
        let Response::Task { task: handed } = daemon
            .handle(Request::TaskUpdate {
                agent: HUMAN.to_owned(),
                task: task.id.to_string(),
                title: None,
                acceptance: None,
                assignee: Some("carol".to_owned()),
            })
            .await
        else {
            panic!("handed")
        };
        assert_eq!(handed.assignee.as_ref(), Some(&carol.id));
        assert_eq!(holds(&daemon).await, vec![carol.id.clone()]);
        assert!(matches!(
            daemon
                .handle(Request::TaskMove {
                    agent: HUMAN.to_owned(),
                    task: task.id.to_string(),
                    column: Column::Ready,
                })
                .await,
            Response::Task { .. }
        ));
        assert!(holds(&daemon).await.is_empty(), "released with the move");
        assert!(matches!(
            pull(&daemon, "carol", None).await,
            Response::Task { .. }
        ));
        let Response::Task { task: done } = daemon
            .handle(Request::TaskMove {
                agent: "carol".to_owned(),
                task: task.id.to_string(),
                column: Column::Done,
            })
            .await
        else {
            panic!("done")
        };
        assert_eq!(
            (done.assignee.as_ref(), done.column),
            (Some(&carol.id), Column::Done)
        );
        assert!(holds(&daemon).await.is_empty(), "done ends the hold");
        // A shared claim on the card by hand is no hold: carol, whose
        // exclusive hold ended with Done, cannot move the card on the
        // strength of it, and is told the hold lapsed.
        assert!(matches!(
            daemon
                .handle(Request::TaskMove {
                    agent: HUMAN.to_owned(),
                    task: task.id.to_string(),
                    column: Column::Review,
                })
                .await,
            Response::Task { .. }
        ));
        let Response::Lease { lease: shared } = daemon
            .handle(Request::Claim {
                agent: "carol".to_owned(),
                resource: format!("task:{}", task.id),
                mode: LeaseMode::Shared,
                amount: None,
                ttl_secs: 60,
                note: None,
                wait_secs: 0,
                automatic: false,
            })
            .await
        else {
            panic!("carol claims shared by hand")
        };
        match daemon
            .handle(Request::TaskMove {
                agent: "carol".to_owned(),
                task: task.id.to_string(),
                column: Column::Done,
            })
            .await
        {
            Response::Error {
                code: ErrorCode::Forbidden,
                details,
                ..
            } => assert_eq!(details.unwrap()["hold"], "lapsed"),
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            daemon
                .handle(Request::Release {
                    agent: "carol".to_owned(),
                    lease: shared.id,
                    summary: None,
                    summary_source: Default::default(),
                })
                .await,
            Response::Lease { .. } | Response::Ok | Response::Leases { .. }
        ));
    }

    /// A transition is one commit: when the store fails in the middle of
    /// a pull — the card and the lease written, the pull's event refused
    /// — nothing of it survives a reopen: the card is as it was, no lease
    /// is held, no event was announced, and the daemon said so. The
    /// same pull behind a coordinator fence is refused as transferring
    /// and writes nothing.
    #[tokio::test]
    async fn a_pull_the_store_fails_midway_or_a_fence_refuses_leaves_nothing_behind() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let bob = register(&daemon, "bob", &work).await;
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
        let resource = task_resource(&task);
        // Behind a fence nothing is written and the caller is told so
        // (the offer itself is announced; the pull must add nothing).
        let seq_before = {
            let mut state = lock(&daemon.state);
            state.offer_transfer(1).unwrap();
            state.next_seq
        };
        assert!(matches!(
            daemon
                .handle(Request::TaskPull {
                    agent: "bob".to_owned(),
                    task: task.id.to_string(),
                    take_over_from: None,
                })
                .await,
            Response::Error {
                code: ErrorCode::Transferring,
                ..
            }
        ));
        {
            let mut state = lock(&daemon.state);
            assert!(state.leases.holders_of(&resource).is_empty());
            assert_eq!(state.next_seq, seq_before);
            assert!(state.abort_transfer("test"));
        }
        // The pull's second event is refused inside the transaction.
        let (seq_before, mut events) = {
            let state = lock(&daemon.state);
            state.store.reject_event_for_test("task_pulled");
            (state.next_seq, state.events.subscribe())
        };
        assert!(matches!(
            daemon
                .handle(Request::TaskPull {
                    agent: "bob".to_owned(),
                    task: task.id.to_string(),
                    take_over_from: None,
                })
                .await,
            Response::Error {
                code: ErrorCode::StorageUnavailable,
                ..
            }
        ));
        {
            let state = lock(&daemon.state);
            assert!(
                state.leases.holders_of(&resource).is_empty(),
                "no lease in memory"
            );
            assert_eq!(state.next_seq, seq_before, "no sequence spent");
            assert!(state.storage_error.is_some(), "storage failure latched");
        }
        assert!(
            events.try_recv().is_err(),
            "nothing announced for a pull that did not land"
        );
        // Reopened: the card is as filed, nobody holds it, and no
        // lease_claimed or task_pulled was ever stored.
        drop(daemon);
        let daemon = open(&dir);
        let Response::Tasks { tasks: reread, .. } = daemon
            .handle(Request::Tasks {
                project: Some(work.display().to_string()),
                column: None,
                archived: false,
                offset: 0,
                limit: 0,
            })
            .await
        else {
            panic!("reread")
        };
        assert_eq!(
            reread
                .iter()
                .map(|t| (t.id.clone(), t.assignee.clone(), t.column))
                .collect::<Vec<_>>(),
            vec![(task.id.clone(), None, Column::Ready)]
        );
        let Response::Leases { leases } = daemon
            .handle(Request::Leases {
                agent: None,
                resource: Some(resource.to_string()),
            })
            .await
        else {
            panic!("leases")
        };
        assert!(leases.is_empty());
        assert!(!daemon.recent_events(50).iter().any(|e| matches!(
            &e.kind,
            EventKind::TaskPulled { .. } | EventKind::LeaseClaimed { .. }
        )));
        // And the pull lands once the store is back.
        assert!(matches!(
            daemon
                .handle(Request::TaskPull {
                    agent: bob.id.to_string(),
                    task: task.id.to_string(),
                    take_over_from: None,
                })
                .await,
            Response::Task { .. }
        ));
    }

    /// The preflight: once storage has failed, a pull is refused before
    /// anything is attempted and no lease appears in memory.
    #[tokio::test]
    async fn a_pull_after_a_storage_failure_is_refused_before_it_starts() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        register(&daemon, "bob", &work).await;
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
        {
            let mut state = lock(&daemon.state);
            state.storage_error = Some("disk gone".to_owned());
        }
        assert!(matches!(
            daemon
                .handle(Request::TaskPull {
                    agent: "bob".to_owned(),
                    task: task.id.to_string(),
                    take_over_from: None,
                })
                .await,
            Response::Error {
                code: ErrorCode::StorageUnavailable,
                ..
            }
        ));
        {
            let state = lock(&daemon.state);
            assert!(state.leases.holders_of(&task_resource(&task)).is_empty());
        }
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
        let list = |offset: usize, limit: usize, column: Option<Column>| {
            let daemon = daemon.clone();
            let work = work.clone();
            async move {
                daemon
                    .handle(Request::Tasks {
                        project: Some(work.display().to_string()),
                        column,
                        archived: false,
                        offset,
                        limit,
                    })
                    .await
            }
        };
        match list(0, 2, None).await {
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
        // The next page starts where the last said it went on.
        match list(2, 2, None).await {
            Response::Tasks { tasks, more } => {
                assert_eq!(
                    (tasks.iter().map(|t| t.column).collect::<Vec<_>>(), more),
                    (vec![Column::Done], false)
                );
            }
            other => panic!("{other:?}"),
        }
        match list(0, 2, Some(Column::Done)).await {
            Response::Tasks { tasks, more } => {
                assert_eq!((tasks.len(), more), (1, false));
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            list(0, 0, None).await,
            Response::Tasks { more: false, .. }
        ));
        // A page is bounded in bytes too: three long cards do not fit a
        // small budget together, and a page still holds one.
        {
            let state = lock(&daemon.state);
            let (page, more) = state
                .store
                .tasks_page(None, None, false, 0, 100, 1)
                .unwrap();
            assert_eq!((page.len(), more), (1, true));
            let (page, more) = state
                .store
                .tasks_page(None, None, false, 1, 100, 6_000)
                .unwrap();
            assert_eq!(
                (page.len(), more),
                (1, true),
                "one long card per small page"
            );
        }
        {
            let mut state = lock(&daemon.state);
            state.storage_error = Some("disk gone".to_owned());
        }
        assert!(matches!(
            list(0, 0, None).await,
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
                    take_over_from: None,
                })
                .await,
            Response::Error {
                code: ErrorCode::StorageUnavailable,
                ..
            }
        ));
    }
}
