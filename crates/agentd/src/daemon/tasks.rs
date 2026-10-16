//! The board of work: cards in lanes, pulled by agents one at a time.
//! The rules are core's (`agentdocker_core::task` and `board`); the
//! daemon's part is to hold every card under its one mutex — which is
//! what makes a pull atomic — to keep cards, boards, comments and each
//! card's history as documents, and to say what changed.
use super::*;
use agentdocker_core::board::{self, Board, Lane, rank};
use agentdocker_core::protocol::{BatchCard, TASKS_LIMIT, TASKS_LIMIT_MAX, TASKS_PAGE_BYTES};
use agentdocker_core::task::{
    Comment, NewTask, Task, TaskAction, TaskEdit, TaskEntry, TaskError, TaskId,
};
use agentdocker_core::{Column, HUMAN, LeaseMode, ResourceKey};
use chrono::DateTime;

#[cfg(test)]
mod board_tests;
mod catalog;
mod plans;
mod rules;

const DOCUMENT: &str = "task";
const BOARD: &str = "board";
const COMMENT: &str = "task_comment";
const ENTRY: &str = "task_entry";
/// The history a card's detail carries: its last hundred lines.
const HISTORY_SHOWN: usize = 100;
/// At most this many cards of one project are read at once, for a pull
/// with no card named, a filtered list, or a lane's order.
const PROJECT_CARDS: usize = 20_000;

/// How long a pull holds a card without a word from its holder. Long
/// enough for a working session, short enough that a card whose agent
/// silently went away comes back to the board the same day; `renew`
/// extends it, and an exit releases it at once.
pub(super) const TASK_LEASE_SECS: u64 = 4 * 60 * 60;

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

/// A document a change writes beside its card: (kind, id, JSON).
type Doc = (&'static str, String, String);

fn doc<T: serde::Serialize>(kind: &'static str, id: String, value: &T) -> Doc {
    (
        kind,
        id,
        serde_json::to_string(value).expect("board documents serialize"),
    )
}

fn board_doc(board: &Board) -> Doc {
    doc(BOARD, format!("{}/{}", board.project, board.slug), board)
}

fn entry_doc(entry: &TaskEntry) -> Doc {
    doc(ENTRY, entry.id.clone(), entry)
}

fn comment_doc(comment: &Comment) -> Doc {
    doc(COMMENT, comment.id.to_string(), comment)
}

fn invalid(message: impl Into<String>) -> Box<Response> {
    Box::new(Response::error(ErrorCode::Invalid, message))
}

fn not_found(message: impl Into<String>) -> Box<Response> {
    Box::new(Response::error(ErrorCode::NotFound, message))
}

fn board_error(error: board::BoardError) -> Box<Response> {
    let details = match &error {
        board::BoardError::NoLane { lanes, .. } => Some(serde_json::json!({ "lanes": lanes })),
        board::BoardError::Invalid(_) => None,
    };
    Box::new(Response::Error {
        code: ErrorCode::Invalid,
        message: error.to_string(),
        details,
    })
}

/// A lane's cards in board order.
fn in_lane<'c>(cards: &'c [Task], board: &Board, lane: &str) -> Vec<&'c Task> {
    let mut cards: Vec<&Task> = cards
        .iter()
        .filter(|t| {
            t.archived_at.is_none()
                && t.board.as_deref().unwrap_or(board::MAIN) == board.slug
                && board.lane_of(t).slug == lane
        })
        .collect();
    cards.sort_by(|a, b| board::order(a, b));
    cards
}

/// A rank at the end of a lane.
fn rank_last(cards: &[Task], board: &Board, lane: &str, skip: &TaskId) -> String {
    let last = in_lane(cards, board, lane)
        .into_iter()
        .rfind(|t| &t.id != skip)
        .map(|t| t.rank.clone())
        .filter(|r| !r.is_empty());
    rank::between(last.as_deref(), None)
}

impl State {
    /// A card by its key (`AD-12`), id or a unique prefix of its id.
    fn resolve_task(&mut self, reference: &str) -> Result<Task, Box<Response>> {
        let reference = reference.trim();
        if reference.is_empty() {
            return Err(invalid("name a card by its key or id"));
        }
        if let Some((prefix, number)) = reference.split_once('-')
            && !prefix.is_empty()
            && prefix.chars().all(|c| c.is_ascii_alphabetic())
            && !number.is_empty()
            && number.chars().all(|c| c.is_ascii_digit())
        {
            let key = format!("{}-{number}", prefix.to_ascii_uppercase());
            let found = self.store_read("task by key", |store| {
                store.documents_where::<Task>(DOCUMENT, "key", &key, 2)
            });
            let Some(found) = found else {
                return Err(Box::new(self.storage_failure().unwrap_or_else(|| {
                    Response::error(ErrorCode::StorageUnavailable, "the board could not be read")
                })));
            };
            let mut found = found.into_iter();
            return match (found.next(), found.next()) {
                (Some(task), None) => Ok(task),
                (None, _) => Err(not_found(format!("no card is {key}"))),
                (Some(_), Some(_)) => Err(Box::new(Response::error(
                    ErrorCode::Ambiguous,
                    format!("more than one project has a card {key}; give its id"),
                ))),
            };
        }
        if !reference.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(not_found(format!("no card matches {reference}")));
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
            (None, _) => Err(not_found(format!("no card matches {reference}"))),
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

    /// What a project is called, for its display keys: the name an agent
    /// in it has for it, else its id.
    fn project_name(&self, project: &ProjectId) -> String {
        self.registry
            .all()
            .filter_map(|a| a.project.as_ref())
            .find(|p| &p.id() == project)
            .map(ProjectRef::name)
            .unwrap_or_else(|| project.as_str().to_owned())
    }

    /// A project's boards: its main board (the classic lanes until
    /// somebody shapes it) first, then the others by slug.
    fn project_boards(&mut self, project: &ProjectId) -> Result<Vec<Board>, Box<Response>> {
        let Some(mut boards) = self.store_read("boards", |store| {
            store.documents_where::<Board>(BOARD, "project", project.as_str(), 64)
        }) else {
            return Err(Box::new(self.storage_failure().unwrap_or_else(|| {
                Response::error(
                    ErrorCode::StorageUnavailable,
                    "the boards could not be read",
                )
            })));
        };
        if !boards.iter().any(|b| b.slug == board::MAIN) {
            boards.push(Board::main(
                project.clone(),
                &self.project_name(project),
                Utc::now(),
            ));
        }
        boards.sort_by(|a, b| {
            (a.slug != board::MAIN)
                .cmp(&(b.slug != board::MAIN))
                .then(a.slug.cmp(&b.slug))
        });
        Ok(boards)
    }

    /// One of a project's boards, by slug or name; its main board when
    /// none is named.
    fn project_board(
        &mut self,
        project: &ProjectId,
        selector: Option<&str>,
    ) -> Result<Board, Box<Response>> {
        let boards = self.project_boards(project)?;
        let wanted = selector.map(str::trim).filter(|s| !s.is_empty());
        let found = match wanted {
            None => boards.iter().find(|b| b.slug == board::MAIN),
            Some(words) => boards.iter().find(|b| {
                b.slug == words
                    || b.name.eq_ignore_ascii_case(words)
                    || b.slug == board::slug_of(words)
            }),
        };
        found.cloned().ok_or_else(|| {
            let names: Vec<String> = boards.iter().map(|b| b.slug.clone()).collect();
            Box::new(Response::Error {
                code: ErrorCode::NotFound,
                message: format!(
                    "no board is called {}; the project's boards are {}",
                    wanted.unwrap_or(board::MAIN),
                    names.join(", ")
                ),
                details: Some(serde_json::json!({ "boards": names })),
            })
        })
    }

    fn board_of(&mut self, task: &Task) -> Result<Board, Box<Response>> {
        self.project_board(&task.project, task.board.as_deref())
    }

    /// Every card of a project, archived ones too.
    fn project_cards(&mut self, project: &ProjectId) -> Result<Vec<Task>, Box<Response>> {
        self.store_read("project cards", |store| {
            store.documents_where::<Task>(DOCUMENT, "project", project.as_str(), PROJECT_CARDS)
        })
        .ok_or_else(|| {
            Box::new(self.storage_failure().unwrap_or_else(|| {
                Response::error(ErrorCode::StorageUnavailable, "the board could not be read")
            }))
        })
    }

    fn card_comments(&mut self, task: &TaskId) -> Vec<Comment> {
        let mut comments = self
            .store_read("card comments", |store| {
                store.documents_where::<Comment>(COMMENT, "task", task.as_str(), 2000)
            })
            .unwrap_or_default();
        comments.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        comments
    }

    fn card_history(&mut self, task: &TaskId) -> Vec<TaskEntry> {
        let mut entries = self
            .store_read("card history", |store| {
                store.documents_where::<TaskEntry>(ENTRY, "task", task.as_str(), 10_000)
            })
            .unwrap_or_default();
        entries.sort_by(|a, b| a.at.cmp(&b.at).then(a.id.cmp(&b.id)));
        let skip = entries.len().saturating_sub(HISTORY_SHOWN);
        entries.split_off(skip)
    }

    /// A card in full, with the lease a pull took.
    fn card_detail(&mut self, task: Task, lease: Option<Lease>) -> Response {
        let board = match self.board_of(&task) {
            Ok(board) => board,
            Err(response) => return *response,
        };
        let comments = self.card_comments(&task.id);
        let history = self.card_history(&task.id);
        let cards = self.project_cards(&task.project).unwrap_or_default();
        let blockers = cards
            .iter()
            .filter(|t| task.blocked_by.contains(&t.id))
            .cloned()
            .collect();
        let children = cards
            .iter()
            .filter(|t| t.parent.as_ref() == Some(&task.id) && t.archived_at.is_none())
            .cloned()
            .collect();
        let plan = task
            .plan
            .as_deref()
            .and_then(|id| self.plan_by_id(id).ok())
            .map(|p| p.summary());
        let files = self.card_files(&task.id);
        Response::TaskDetail {
            task,
            board,
            comments,
            history,
            blockers,
            children,
            lease,
            plan,
            files,
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

    /// Write the card, the lease it took or renewed, the leases it ended,
    /// the documents beside it and every event, as one commit; then move
    /// memory and announce. Nothing changes in memory unless the store
    /// took all of it.
    fn commit_task(
        &mut self,
        task: &Task,
        holder: Option<AgentId>,
        claimed: Option<Lease>,
        released: Vec<Lease>,
        kinds: Vec<EventKind>,
        documents: Vec<Doc>,
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
            .map(|kind| Event::new(kind, now))
            .collect();
        let ended: Vec<LeaseId> = released.iter().map(|l| l.id.clone()).collect();
        let committed = self.commit(
            "task",
            events,
            |store, events| {
                let transition = crate::store::TaskTransition {
                    holder: record.as_ref(),
                    claimed: claimed.as_ref(),
                    released: &ended,
                    kind: DOCUMENT,
                    id: task.id.as_str(),
                    documents: &documents,
                    events,
                };
                store.task_transition(&transition, task)
            },
            |state, _, ()| {
                if let (Some(id), Some(record)) = (&holder, &record) {
                    *state
                        .registry
                        .get_mut(id)
                        .expect("holder identity retained") = record.clone();
                }
                for lease in &released {
                    let _ = state.leases.release(&lease.id, &lease.holder);
                }
                if let Some(lease) = &claimed {
                    state.leases.restore(lease.clone());
                }
            },
        );
        if committed != Persisted::Committed {
            return Some(self.write_failure().unwrap_or_else(|| {
                Response::error(ErrorCode::Internal, "the card was not recorded")
            }));
        }
        None
    }

    /// Write documents that are not a card — a comment changed, a board
    /// reshaped — with their events, as one commit.
    fn commit_documents(
        &mut self,
        what: &'static str,
        documents: Vec<Doc>,
        kinds: Vec<EventKind>,
    ) -> Option<Response> {
        let now = Utc::now();
        let events: Vec<Event> = kinds
            .into_iter()
            .map(|kind| Event::new(kind, now))
            .collect();
        let committed = self.commit(
            what,
            events,
            |store, events| store.documents_with_events(&documents, events),
            |_, _, ()| {},
        );
        if committed != Persisted::Committed {
            return Some(self.write_failure().unwrap_or_else(|| {
                Response::error(ErrorCode::Internal, "the change was not recorded")
            }));
        }
        None
    }

    /// Write some documents and delete others with their events, as one
    /// commit.
    fn commit_change(
        &mut self,
        what: &'static str,
        put: Vec<Doc>,
        delete: Vec<(&'static str, String)>,
        kinds: Vec<EventKind>,
    ) -> Option<Response> {
        let now = Utc::now();
        let events: Vec<Event> = kinds
            .into_iter()
            .map(|kind| Event::new(kind, now))
            .collect();
        let committed = self.commit(
            what,
            events,
            |store, events| store.documents_change(&put, &delete, events),
            |_, _, ()| {},
        );
        if committed != Persisted::Committed {
            return Some(self.write_failure().unwrap_or_else(|| {
                Response::error(ErrorCode::Internal, "the change was not recorded")
            }));
        }
        None
    }

    /// Tell an agent about a card the person did something to: handed
    /// it to them, moved it while they held it, or said something on it.
    /// The card goes with it as a link; nothing is sent to the person.
    fn tell_holder(&mut self, from: &str, to: &AgentId, task: &Task, text: String) {
        if self.registry.get(to).is_none_or(super::humans::is_human) {
            return;
        }
        let mut envelope = Envelope::new(
            from.to_owned(),
            Destination::Agent(to.clone()),
            "task".to_owned(),
            serde_json::json!({ "text": text }),
            None,
            Utc::now(),
        );
        envelope.links = vec![agentdocker_core::Link {
            kind: agentdocker_core::LinkKind::Task,
            target: task.id.to_string(),
            note: (!task.key.is_empty()).then(|| task.key.clone()),
        }];
        let _ = self.publish(envelope);
    }

    /// The cards a card would wait on or be part of, resolved to ids;
    /// refused when one is unknown or the edges would close a loop.
    fn card_edges(
        &mut self,
        task: &TaskId,
        cards: &[Task],
        parent: Option<&str>,
        blocked_by: &[String],
    ) -> Result<(Option<TaskId>, Vec<TaskId>), Box<Response>> {
        let mut resolve = |reference: &str| -> Result<TaskId, Box<Response>> {
            if let Some(found) = cards.iter().find(|t| t.key.eq_ignore_ascii_case(reference)) {
                return Ok(found.id.clone());
            }
            self.resolve_task(reference).map(|t| t.id)
        };
        let parent = match parent.map(str::trim).filter(|p| !p.is_empty()) {
            Some(reference) => Some(resolve(reference)?),
            None => None,
        };
        let mut blockers = Vec::new();
        for reference in blocked_by {
            let id = resolve(reference)?;
            if !blockers.contains(&id) {
                blockers.push(id);
            }
        }
        let edges_of = |id: &TaskId| -> Vec<TaskId> {
            cards
                .iter()
                .find(|t| &t.id == id)
                .map(|t| {
                    t.parent
                        .iter()
                        .chain(t.blocked_by.iter())
                        .cloned()
                        .collect()
                })
                .unwrap_or_default()
        };
        let all: Vec<TaskId> = parent.iter().chain(blockers.iter()).cloned().collect();
        if agentdocker_core::task::closes_cycle(task, &all, edges_of) {
            return Err(refused(TaskError::Cycle).into());
        }
        Ok((parent, blockers))
    }
}

fn refused(error: TaskError) -> Response {
    refused_with(error, None)
}

/// A refusal by the card's rule, with what the caller can do about a hold
/// that has lapsed: the plain pull that met it is told to name the holder.
fn refused_with(error: TaskError, lapsed: Option<&AgentId>) -> Response {
    let code = match &error {
        TaskError::Invalid(_) | TaskError::Cycle => ErrorCode::Invalid,
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
        TaskError::Cycle => (
            error.to_string(),
            Some(serde_json::json!({ "cycle": true })),
        ),
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

/// What filing a card takes, from `TaskCreate` or a batch.
pub(super) struct Filing {
    pub from: String,
    pub project: Option<String>,
    pub board: Option<String>,
    /// The lane a card goes to unless it names its own.
    pub lane: Option<String>,
    /// The category whose first lane a card goes to when no lane is said.
    pub column: Option<Column>,
    /// Tags every card gets.
    pub tags: Vec<String>,
    pub cards: Vec<BatchCard>,
}

impl Daemon {
    /// File one card or a batch, all or none. A batch's cards may name
    /// each other by `ref` as the card they are part of or wait on; every
    /// problem is listed at once.
    pub(super) async fn task_file(self: &Arc<Self>, filing: Filing, batch: bool) -> Response {
        let named = match &filing.project {
            Some(selector) => match self.resolve_project(selector).await {
                Ok(id) => Some(id),
                Err(response) => return *response,
            },
            None => None,
        };
        if filing.cards.is_empty() {
            return Response::error(ErrorCode::Invalid, "file at least one card");
        }
        if filing.cards.len() > agentdocker_core::task::BATCH_MAX {
            return Response::error(ErrorCode::Invalid, "a batch is at most fifty cards");
        }
        let mut state = lock(&self.state);
        let actor = match state.actor(&filing.from) {
            Ok(actor) => actor,
            Err(response) => return *response,
        };
        let Some(project) = named.or(actor.project.clone()) else {
            return Response::error(ErrorCode::Invalid, "the caller is in no project; name one");
        };
        let mut board = match state.project_board(&project, filing.board.as_deref()) {
            Ok(board) => board,
            Err(response) => return *response,
        };
        // Keys are the project's: one counter, on its main board.
        let mut keys = if board.slug == board::MAIN {
            None
        } else {
            match state.project_board(&project, None) {
                Ok(main) => Some(main),
                Err(response) => return *response,
            }
        };
        let existing = match state.project_cards(&project) {
            Ok(cards) => cards,
            Err(response) => return *response,
        };
        let now = Utc::now();
        let mut problems: Vec<String> = Vec::new();
        let mut filed: Vec<Task> = Vec::new();
        let mut refs: BTreeMap<String, String> = BTreeMap::new();
        let mut ids: BTreeMap<String, TaskId> = BTreeMap::new();
        let mut last_rank: BTreeMap<String, String> = BTreeMap::new();
        for (index, card) in filing.cards.iter().enumerate() {
            let label = card
                .reference
                .clone()
                .unwrap_or_else(|| format!("card {}", index + 1));
            let lane = match card.lane.as_deref().or(filing.lane.as_deref()) {
                Some(words) => match board.resolve(words) {
                    Ok(lane) => lane.clone(),
                    Err(error) => {
                        problems.push(format!("{label}: {error}"));
                        continue;
                    }
                },
                None => filing
                    .column
                    .and_then(|c| board.first(c))
                    .or_else(|| board.first(Column::Backlog))
                    .unwrap_or(&board.lanes[0])
                    .clone(),
            };
            let mut tags = filing.tags.clone();
            tags.extend(card.tags.iter().cloned());
            let mut task = match Task::file(
                project.clone(),
                NewTask {
                    title: card.title.clone(),
                    acceptance: card.acceptance.clone(),
                    description: card.description.clone(),
                    column: Some(lane.category),
                    lane: Some(lane.slug.clone()),
                    priority: card.priority.unwrap_or_default(),
                    size: card.size,
                    due: card.due,
                    external_blocker: card.external_blocker.clone(),
                    parent: None,
                    blocked_by: Vec::new(),
                    tags,
                    links: card.links.clone(),
                    plan: None,
                },
                &actor.id,
                now,
            ) {
                Ok(task) => task,
                Err(error) => {
                    problems.push(format!("{label}: {error}"));
                    continue;
                }
            };
            if let Some(words) = card.plan.as_deref().filter(|w| !w.trim().is_empty()) {
                match state.resolve_plan(&project, words) {
                    Ok(plan) => task.plan = Some(plan.id),
                    Err(response) => {
                        let message = match *response {
                            Response::Error { message, .. } => message,
                            _ => "no such plan".to_owned(),
                        };
                        problems.push(format!("{label}: {message}"));
                        continue;
                    }
                }
            }
            task.key = match keys.as_mut() {
                Some(main) => main.take_key(),
                None => board.take_key(),
            };
            if board.slug != board::MAIN {
                task.board = Some(board.slug.clone());
            }
            let before = last_rank.get(&lane.slug).cloned().or_else(|| {
                in_lane(&existing, &board, &lane.slug)
                    .last()
                    .map(|t| t.rank.clone())
                    .filter(|r| !r.is_empty())
            });
            task.rank = rank::between(before.as_deref(), None);
            last_rank.insert(lane.slug.clone(), task.rank.clone());
            if let Some(reference) = &card.reference {
                if ids.contains_key(reference) {
                    problems.push(format!("{label}: two cards in the batch use this ref"));
                }
                ids.insert(reference.clone(), task.id.clone());
                refs.insert(reference.clone(), task.key.clone());
            }
            filed.push(task);
        }
        // References, once every card has an id: a ref in the batch first,
        // else a card already on the board.
        let mut everything = existing.clone();
        everything.extend(filed.iter().cloned());
        for (index, card) in filing.cards.iter().enumerate() {
            let Some(task) = filed.get(index).filter(|_| problems.is_empty()) else {
                continue;
            };
            let id = task.id.clone();
            let to_reference = |words: &str| -> String {
                ids.get(words)
                    .map(|id| id.to_string())
                    .unwrap_or_else(|| words.to_owned())
            };
            let parent = card.parent.as_deref().map(to_reference);
            let blocked: Vec<String> = card.blocked_by.iter().map(|b| to_reference(b)).collect();
            match state.card_edges(&id, &everything, parent.as_deref(), &blocked) {
                Ok((parent, blocked_by)) => {
                    let task = filed.iter_mut().find(|t| t.id == id).expect("filed");
                    task.parent = parent;
                    task.blocked_by = blocked_by;
                    if let Some(row) = everything.iter_mut().find(|t| t.id == id) {
                        row.parent = task.parent.clone();
                        row.blocked_by = task.blocked_by.clone();
                    }
                }
                Err(response) => {
                    let label = card
                        .reference
                        .clone()
                        .unwrap_or_else(|| format!("card {}", index + 1));
                    let message = match *response {
                        Response::Error { message, .. } => message,
                        _ => "cannot be filed".to_owned(),
                    };
                    problems.push(format!("{label}: {message}"));
                }
            }
        }
        if !problems.is_empty() {
            return Response::Error {
                code: ErrorCode::Invalid,
                message: problems.join("; "),
                details: Some(serde_json::json!({ "problems": problems })),
            };
        }
        // Tags the project does not have: the person's become tags with the
        // cards; an agent's refuse the filing.
        let wanted: Vec<String> = filed.iter().flat_map(|t| t.tags.iter().cloned()).collect();
        let new_tags = match state.tags_for(&project, &actor, &wanted, now) {
            Ok(docs) => docs,
            Err(response) => return *response,
        };
        let mut kinds = Vec::new();
        let mut documents = vec![board_doc(&board)];
        if let Some(main) = &keys {
            documents.push(board_doc(main));
        }
        documents.extend(new_tags);
        for task in &filed {
            kinds.push(EventKind::TaskCreated {
                task: task.id.clone(),
                project: task.project.clone(),
                by: actor.id.clone(),
                title: task.title.clone(),
            });
            documents.push(entry_doc(&TaskEntry::new(
                task,
                &actor.id,
                TaskAction::Created {
                    lane: task.lane.clone().unwrap_or_default(),
                },
                now,
            )));
        }
        for task in filed.iter().skip(1) {
            documents.push(doc(DOCUMENT, task.id.to_string(), task));
        }
        if let Some(error) = state.commit_task(&filed[0], None, None, Vec::new(), kinds, documents)
        {
            return error;
        }
        // Rules on the lanes the cards came into act on them now.
        let mut starts = Vec::new();
        for task in &filed {
            let lane = task.lane.clone().unwrap_or_default();
            starts.extend(state.rules_on_enter(task, &lane));
        }
        drop(state);
        self.spawn_starts(starts);
        if batch {
            Response::TaskBatch { tasks: filed, refs }
        } else {
            Response::Task {
                task: filed.remove(0),
            }
        }
    }

    /// A pull takes the card and the `task:<id>` lease in one commit.
    /// The lease is the holding: while the holder's lease lives nobody
    /// else may take the card, and a pull of one's own held card renews
    /// it. Once the hold has lapsed — expired, released, or its agent
    /// exited — the card is taken over only by name: `take_over_from`
    /// says whom the caller expects to take it from, and a card that
    /// names somebody else, or whose hold is live after all, is refused.
    /// With no card named it takes the first ready one, or says there is
    /// none.
    pub(super) async fn task_pull(
        self: &Arc<Self>,
        reference: &str,
        task: Option<&str>,
        take_over_from: Option<&str>,
        project: Option<&str>,
        board_selector: Option<&str>,
    ) -> Response {
        let named_project = match project {
            Some(selector) => match self.resolve_project(selector).await {
                Ok(id) => Some(id),
                Err(response) => return *response,
            },
            None => None,
        };
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
        let mut task = match task {
            Some(reference) => match state.resolve_task(reference) {
                Ok(task) => task,
                Err(response) => return *response,
            },
            None => {
                let Some(project) =
                    named_project.or_else(|| record.project.as_ref().map(ProjectRef::id))
                else {
                    return Response::error(
                        ErrorCode::Invalid,
                        "the caller is in no project; name one or a card",
                    );
                };
                let board = match state.project_board(&project, board_selector) {
                    Ok(board) => board,
                    Err(response) => return *response,
                };
                let cards = match state.project_cards(&project) {
                    Ok(cards) => cards,
                    Err(response) => return *response,
                };
                let open = |id: &TaskId| cards.iter().any(|t| &t.id == id && t.open());
                let on_board: Vec<Task> = cards
                    .iter()
                    .filter(|t| t.board.as_deref().unwrap_or(board::MAIN) == board.slug)
                    .cloned()
                    .collect();
                let pick = board.next_ready(&on_board, &agent, open);
                match pick.card {
                    Some(card) => card.clone(),
                    None => {
                        return Response::NothingReady {
                            project,
                            board: board.slug.clone(),
                            source: board.pull_source.clone(),
                            total: pick.total,
                            held: pick.held,
                            blocked: pick.blocked,
                        };
                    }
                }
            }
        };
        let board = match state.board_of(&task) {
            Ok(board) => board,
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
        let lapsed = task
            .assignee
            .clone()
            .filter(|_| held.is_none() && task.column.working());
        let from_lane = task.lane_on(&board).slug.clone();
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
                    if let Some(error) = state.commit_task(
                        &task,
                        Some(agent),
                        Some(lease.clone()),
                        Vec::new(),
                        vec![kind],
                        Vec::new(),
                    ) {
                        return error;
                    }
                    return state.card_detail(task, Some(lease));
                }
                // A card that waits on an open card is not taken yet.
                if !task.blocked_by.is_empty() {
                    let cards = state.project_cards(&task.project).unwrap_or_default();
                    let open: Vec<String> = cards
                        .iter()
                        .filter(|t| task.blocked_by.contains(&t.id) && t.open())
                        .map(Task::handle)
                        .collect();
                    if !open.is_empty() {
                        return Response::Error {
                            code: ErrorCode::Conflict,
                            message: format!("the card waits on {}", open.join(", ")),
                            details: Some(serde_json::json!({ "blocked_by": open })),
                        };
                    }
                }
                if let Err(error) = task.pull(&agent, &board, now) {
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
        if task.rank.is_empty() || task.lane.as_deref() != Some(from_lane.as_str()) {
            let cards = state.project_cards(&task.project).unwrap_or_default();
            let lane = task.lane_on(&board).slug.clone();
            task.rank = rank_last(&cards, &board, &lane, &task.id);
        }
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
                from: from.clone(),
                lease: lease.id.clone(),
            },
        ];
        let entry = TaskEntry::new(
            &task,
            agent.as_str(),
            TaskAction::Pulled {
                lane: task.lane.clone().unwrap_or_default(),
                from: from.map(|f| f.to_string()),
            },
            now,
        );
        if let Some(error) = state.commit_task(
            &task,
            Some(agent),
            Some(lease.clone()),
            Vec::new(),
            kinds,
            vec![entry_doc(&entry)],
        ) {
            return error;
        }
        state.card_detail(task, Some(lease))
    }

    /// An agent's change to a card it holds requires the hold to be
    /// live while the card is being worked: a lapsed lease is told so,
    /// and the card is pulled again by name before it moves. A card that
    /// has ended, or waits, holds no lease to lapse. The person needs no
    /// hold.
    fn agent_hold(state: &State, task: &Task, actor: &Actor) -> Result<(), Box<Response>> {
        if actor.human || !task.column.working() {
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
        let working = after.column.working() && after.archived_at.is_none();
        state
            .task_holds(before)
            .into_iter()
            .filter(|l| !working || Some(&l.holder) != after.assignee.as_ref())
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn task_move(
        self: &Arc<Self>,
        reference: &str,
        task: &str,
        column: Option<Column>,
        to: Option<&str>,
        after: Option<&str>,
        before: Option<&str>,
        comment: Option<&str>,
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
        let board = match state.board_of(&task) {
            Ok(board) => board,
            Err(response) => return *response,
        };
        let neighbour = |state: &mut State, words: Option<&str>| match words {
            Some(words) => state.resolve_task(words).map(Some),
            None => Ok(None),
        };
        let after = match neighbour(&mut state, after) {
            Ok(card) => card,
            Err(response) => return *response,
        };
        let before_card = match neighbour(&mut state, before) {
            Ok(card) => card,
            Err(response) => return *response,
        };
        // Where to: a lane named, a category, or beside a card named.
        let lane = match (to, column, after.as_ref().or(before_card.as_ref())) {
            (Some(words), _, _) => match board.resolve(words) {
                Ok(lane) => lane.clone(),
                Err(error) => return *board_error(error),
            },
            (None, Some(column), _) => match board.first(column) {
                Some(lane) => lane.clone(),
                None => {
                    return *board_error(board::BoardError::NoLane {
                        asked: column.as_str().to_owned(),
                        lanes: board.lanes.iter().map(|l| l.slug.clone()).collect(),
                    });
                }
            },
            (None, None, Some(beside)) => board.lane_of(beside).clone(),
            (None, None, None) => {
                return Response::error(
                    ErrorCode::Invalid,
                    "say where: a lane, or a card to sit beside",
                );
            }
        };
        let previous = task.clone();
        let from_lane = previous.lane_on(&board).slug.clone();
        let moved = match task.move_to(&actor.id, actor.human, &lane, now) {
            Ok(moved) => moved,
            Err(error) => return refused(error),
        };
        // Its place in the lane: beside the card named, else the end of a
        // lane it has just come into.
        let cards = state.project_cards(&task.project).unwrap_or_default();
        let order: Vec<&Task> = in_lane(&cards, &board, &lane.slug)
            .into_iter()
            .filter(|t| t.id != task.id)
            .collect();
        let rank_now = if let Some(after) = &after {
            let at = order.iter().position(|t| t.id == after.id);
            let next = at.and_then(|i| order.get(i + 1)).map(|t| t.rank.as_str());
            Some(rank::between(Some(after.rank.as_str()), next))
        } else if let Some(before) = &before_card {
            let at = order.iter().position(|t| t.id == before.id);
            let previous = at
                .and_then(|i| i.checked_sub(1))
                .and_then(|i| order.get(i))
                .map(|t| t.rank.as_str());
            Some(rank::between(previous, Some(before.rank.as_str())))
        } else if moved || task.rank.is_empty() {
            Some(rank_last(&cards, &board, &lane.slug, &task.id))
        } else {
            None
        };
        let reranked = rank_now.as_ref().is_some_and(|r| *r != task.rank);
        if let Some(rank) = rank_now {
            task.rank = rank;
        }
        let words = comment.map(str::trim).filter(|c| !c.is_empty());
        if !moved && !reranked && words.is_none() {
            // Where it was asked to go is where it is: nothing changes.
            return Response::Task { task };
        }
        if reranked || moved {
            task.updated_at = now;
        }
        let released = Self::holds_ended(&state, &previous, &task);
        let mut kinds = Vec::new();
        let mut documents = Vec::new();
        if moved || reranked {
            kinds.push(EventKind::TaskMoved {
                task: task.id.clone(),
                project: task.project.clone(),
                by: actor.id.clone(),
                column: lane.category,
                lane: Some(lane.slug.clone()),
            });
        }
        if moved {
            documents.push(entry_doc(&TaskEntry::new(
                &task,
                &actor.id,
                TaskAction::Moved {
                    from: from_lane.clone(),
                    to: lane.slug.clone(),
                },
                now,
            )));
        }
        if let Some(words) = words {
            let note = match Comment::new(&task, &actor.id, words, None, now) {
                Ok(note) => note,
                Err(error) => return refused(error),
            };
            kinds.push(EventKind::TaskCommented {
                task: task.id.clone(),
                project: task.project.clone(),
                by: actor.id.clone(),
                comment: note.id.clone(),
            });
            documents.push(entry_doc(&TaskEntry::new(
                &task,
                &actor.id,
                TaskAction::Commented {
                    comment: note.id.clone(),
                },
                now,
            )));
            documents.push(comment_doc(&note));
        }
        kinds.extend(released.iter().map(|lease| EventKind::LeaseReleased {
            lease: lease.clone(),
        }));
        if let Some(error) = state.commit_task(&task, None, None, released, kinds, documents) {
            return error;
        }
        // The person moved a card an agent held, or sent it back: the
        // agent hears where it went and why.
        if actor.human
            && moved
            && let Some(holder) = previous.assignee.clone()
        {
            let mut text = format!("{} {} → {}", task.handle(), task.title, lane.name);
            if let Some(words) = words {
                text.push_str(&format!(": {words}"));
            }
            state.tell_holder(&actor.id, &holder, &task, text);
        }
        // Rules on the lane it came into act on it now.
        let starts = if moved {
            state.rules_on_enter(&task, &lane.slug)
        } else {
            Vec::new()
        };
        drop(state);
        self.spawn_starts(starts);
        Response::Task { task }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn task_update(
        self: &Arc<Self>,
        reference: &str,
        task: &str,
        change: TaskChange,
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
        // A holder named by id, name or prefix, or `me`; the empty string
        // takes the card away from whoever holds it.
        let assignee = match change.assignee.as_deref().map(str::trim) {
            None => None,
            Some("") | Some("none") => Some(None),
            Some("me") if !actor.human => Some(Some(AgentId::from(actor.id.clone()))),
            Some(who) => match state.resolve(who) {
                Ok(id) => Some(Some(id)),
                Err(response) => return *response,
            },
        };
        let size = match change.size.as_deref().map(str::trim) {
            None => None,
            Some("") => Some(None),
            Some(words) => match agentdocker_core::Size::parse(words) {
                Some(size) => Some(Some(size)),
                None => return Response::error(ErrorCode::Invalid, "a size is s, m or l"),
            },
        };
        let due = match change.due.as_deref().map(str::trim) {
            None => None,
            Some("") => Some(None),
            Some(words) => match chrono::NaiveDate::parse_from_str(words, "%Y-%m-%d") {
                Ok(day) => Some(Some(day)),
                Err(_) => return Response::error(ErrorCode::Invalid, "a due day is YYYY-MM-DD"),
            },
        };
        let (parent, blocked_by) = if change.parent.is_some() || change.blocked_by.is_some() {
            let cards = match state.project_cards(&task.project) {
                Ok(cards) => cards,
                Err(response) => return *response,
            };
            let parent_words = change.parent.as_deref();
            let blocked = change.blocked_by.clone().unwrap_or_default();
            let (parent, blockers) = match state.card_edges(
                &task.id,
                &cards,
                parent_words.or(task.parent.as_ref().map(TaskId::as_str)),
                &if change.blocked_by.is_some() {
                    blocked
                } else {
                    task.blocked_by.iter().map(TaskId::to_string).collect()
                },
            ) {
                Ok(edges) => edges,
                Err(response) => return *response,
            };
            (
                change.parent.as_ref().map(|words| {
                    if words.trim().is_empty() {
                        None
                    } else {
                        parent
                    }
                }),
                change.blocked_by.as_ref().map(|_| blockers),
            )
        } else {
            (None, None)
        };
        let new_tags = match change.tags.as_deref() {
            Some(tags) => match state.tags_for(&task.project, &actor, tags, now) {
                Ok(docs) => docs,
                Err(response) => return *response,
            },
            None => Vec::new(),
        };
        let plan = match change.plan.as_deref().map(str::trim) {
            None => None,
            Some("") => Some(None),
            Some(words) => match state.resolve_plan(&task.project, words) {
                Ok(plan) => Some(Some(plan.id)),
                Err(response) => return *response,
            },
        };
        let previous = task.clone();
        let fields = match task.update(
            &actor.id,
            actor.human,
            TaskEdit {
                title: change.title.as_deref(),
                acceptance: change.acceptance.as_deref(),
                description: change.description.as_deref(),
                assignee,
                links: change.links,
                priority: change.priority,
                size,
                due,
                external_blocker: change.external_blocker.as_deref(),
                parent,
                blocked_by,
                tags: change.tags,
                plan,
            },
            now,
        ) {
            Ok(fields) => fields,
            Err(error) => return refused(error),
        };
        if fields.is_empty() {
            return Response::Task { task };
        }
        let released = Self::holds_ended(&state, &previous, &task);
        // A hand to a running agent is its hold from now: the person's
        // confirmed reassignment takes the lease for them. Handed to an
        // agent that is not running, the card waits for it to pull by
        // name.
        let handed_to = task
            .assignee
            .clone()
            .filter(|a| previous.assignee.as_ref() != Some(a));
        let handed = handed_to.clone().filter(|a| {
            task.column.working()
                && state
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
            by: actor.id.clone(),
            fields: fields.iter().map(|f| (*f).to_owned()).collect(),
        }];
        kinds.extend(released.iter().map(|lease| EventKind::LeaseReleased {
            lease: lease.clone(),
        }));
        if let Some(lease) = &claimed {
            kinds.push(EventKind::LeaseClaimed {
                lease: lease.clone(),
            });
        }
        let mut documents = vec![entry_doc(&TaskEntry::new(
            &task,
            &actor.id,
            TaskAction::Updated {
                fields: fields.iter().map(|f| (*f).to_owned()).collect(),
            },
            now,
        ))];
        documents.extend(new_tags);
        if fields.contains(&"assignee") {
            documents.push(entry_doc(&TaskEntry::new(
                &task,
                &actor.id,
                TaskAction::Assigned {
                    to: task.assignee.as_ref().map(ToString::to_string),
                },
                now,
            )));
        }
        if let Some(error) = state.commit_task(&task, handed, claimed, released, kinds, documents) {
            return error;
        }
        // The person handed it to an agent: the agent hears so.
        if actor.human
            && let Some(holder) = handed_to
        {
            let text = format!("{} {} is yours", task.handle(), task.title);
            state.tell_holder(&actor.id, &holder, &task, text);
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
            by: actor.id.clone(),
        }];
        kinds.extend(released.iter().map(|lease| EventKind::LeaseReleased {
            lease: lease.clone(),
        }));
        let entry = TaskEntry::new(&task, &actor.id, TaskAction::Archived, now);
        if let Some(error) =
            state.commit_task(&task, None, None, released, kinds, vec![entry_doc(&entry)])
        {
            return error;
        }
        Response::Ok
    }

    pub(super) fn task_unarchive(self: &Arc<Self>, reference: &str, task: &str) -> Response {
        let mut state = lock(&self.state);
        let actor = match state.actor(reference) {
            Ok(actor) => actor,
            Err(response) => return *response,
        };
        if let Some(error) = state.write_failure() {
            return error;
        }
        let now = Utc::now();
        let mut task = match state.resolve_task(task) {
            Ok(task) => task,
            Err(response) => return *response,
        };
        let was = task.archived_at.is_some();
        if let Err(error) = task.unarchive(actor.human, now) {
            return refused(error);
        }
        if !was {
            return Response::Task { task };
        }
        let kinds = vec![EventKind::TaskUnarchived {
            task: task.id.clone(),
            project: task.project.clone(),
            by: actor.id.clone(),
        }];
        let entry = TaskEntry::new(&task, &actor.id, TaskAction::Unarchived, now);
        if let Some(error) = state.commit_task(
            &task,
            None,
            None,
            Vec::new(),
            kinds,
            vec![entry_doc(&entry)],
        ) {
            return error;
        }
        Response::Task { task }
    }

    /// Every card in a lane off the board, the person's to do.
    pub(super) async fn task_archive_lane(
        self: &Arc<Self>,
        from: &str,
        project: Option<&str>,
        board_selector: Option<&str>,
        lane: &str,
    ) -> Response {
        let named = match project {
            Some(selector) => match self.resolve_project(selector).await {
                Ok(id) => Some(id),
                Err(response) => return *response,
            },
            None => None,
        };
        let mut state = lock(&self.state);
        let actor = match state.actor(from) {
            Ok(actor) => actor,
            Err(response) => return *response,
        };
        if !actor.human {
            return refused(TaskError::NotYours);
        }
        let Some(project) = named.or(actor.project.clone()) else {
            return Response::error(ErrorCode::Invalid, "name the project");
        };
        let board = match state.project_board(&project, board_selector) {
            Ok(board) => board,
            Err(response) => return *response,
        };
        let lane = match board.resolve(lane) {
            Ok(lane) => lane.clone(),
            Err(error) => return *board_error(error),
        };
        let cards = match state.project_cards(&project) {
            Ok(cards) => cards,
            Err(response) => return *response,
        };
        let now = Utc::now();
        state.expire_leases_at(now);
        let going: Vec<Task> = in_lane(&cards, &board, &lane.slug)
            .into_iter()
            .cloned()
            .collect();
        if going.is_empty() {
            return Response::Archived { count: 0 };
        }
        let mut kinds = Vec::new();
        let mut documents = Vec::new();
        let mut released = Vec::new();
        let mut archived = Vec::new();
        for card in going {
            let mut task = card.clone();
            if task.archive(&actor.id, true, now).is_err() {
                continue;
            }
            released.extend(Self::holds_ended(&state, &card, &task));
            kinds.push(EventKind::TaskArchived {
                task: task.id.clone(),
                project: task.project.clone(),
                by: actor.id.clone(),
            });
            documents.push(entry_doc(&TaskEntry::new(
                &task,
                &actor.id,
                TaskAction::Archived,
                now,
            )));
            archived.push(task);
        }
        kinds.extend(released.iter().map(|lease| EventKind::LeaseReleased {
            lease: lease.clone(),
        }));
        for task in archived.iter().skip(1) {
            documents.push(doc(DOCUMENT, task.id.to_string(), task));
        }
        let count = archived.len();
        if let Some(error) = state.commit_task(&archived[0], None, None, released, kinds, documents)
        {
            return error;
        }
        Response::Archived { count }
    }

    pub(super) fn task_get(self: &Arc<Self>, task: &str) -> Response {
        let mut state = lock(&self.state);
        let task = match state.resolve_task(task) {
            Ok(task) => task,
            Err(response) => return *response,
        };
        state.card_detail(task, None)
    }

    /// Words on a card, from an agent or the person; a reply hangs under
    /// the comment it answers, one level deep. The person's words reach
    /// the agent holding the card.
    pub(super) fn task_comment(
        self: &Arc<Self>,
        from: &str,
        task: &str,
        body: &str,
        reply_to: Option<&str>,
    ) -> Response {
        let mut state = lock(&self.state);
        let actor = match state.actor(from) {
            Ok(actor) => actor,
            Err(response) => return *response,
        };
        if let Some(error) = state.write_failure() {
            return error;
        }
        let task = match state.resolve_task(task) {
            Ok(task) => task,
            Err(response) => return *response,
        };
        let reply_to = match reply_to.map(str::trim).filter(|r| !r.is_empty()) {
            Some(id) => {
                let thread = state.card_comments(&task.id);
                match thread.iter().find(|c| c.id.as_str() == id) {
                    // One level: a reply to a reply hangs under its root.
                    Some(found) => Some(found.reply_to.clone().unwrap_or_else(|| found.id.clone())),
                    None => {
                        return Response::error(
                            ErrorCode::NotFound,
                            format!("no comment {id} on the card"),
                        );
                    }
                }
            }
            None => None,
        };
        let now = Utc::now();
        let comment = match Comment::new(&task, &actor.id, body, reply_to, now) {
            Ok(comment) => comment,
            Err(error) => return refused(error),
        };
        let mut touched = task.clone();
        touched.updated_at = now;
        let kinds = vec![EventKind::TaskCommented {
            task: task.id.clone(),
            project: task.project.clone(),
            by: actor.id.clone(),
            comment: comment.id.clone(),
        }];
        let documents = vec![
            comment_doc(&comment),
            entry_doc(&TaskEntry::new(
                &task,
                &actor.id,
                TaskAction::Commented {
                    comment: comment.id.clone(),
                },
                now,
            )),
        ];
        if let Some(error) = state.commit_task(&touched, None, None, Vec::new(), kinds, documents) {
            return error;
        }
        if actor.human
            && let Some(holder) = task.assignee.clone()
        {
            let text = format!("{} {}: {}", task.handle(), task.title, comment.body);
            state.tell_holder(&actor.id, &holder, &task, text);
        }
        Response::Comment { comment }
    }

    /// Its author rewrites a comment, or its author or the person deletes
    /// it (`body: None`).
    pub(super) fn task_comment_change(
        self: &Arc<Self>,
        from: &str,
        comment: &str,
        body: Option<&str>,
    ) -> Response {
        let mut state = lock(&self.state);
        let actor = match state.actor(from) {
            Ok(actor) => actor,
            Err(response) => return *response,
        };
        if let Some(error) = state.write_failure() {
            return error;
        }
        let found = state.store_read("comment", |store| {
            store.document::<Comment>(COMMENT, comment.trim())
        });
        let Some(Some(mut comment)) = found else {
            return Response::error(ErrorCode::NotFound, format!("no comment {comment}"));
        };
        let now = Utc::now();
        let outcome = match body {
            Some(body) => comment.edit(&actor.id, body, now),
            None => comment.delete(&actor.id, actor.human, now),
        };
        if let Err(error) = outcome {
            return refused(error);
        }
        let kinds = vec![EventKind::TaskCommentChanged {
            task: comment.task.clone(),
            project: comment.project.clone(),
            by: actor.id.clone(),
            comment: comment.id.clone(),
            deleted: body.is_none(),
        }];
        if let Some(error) = state.commit_documents("comment", vec![comment_doc(&comment)], kinds) {
            return error;
        }
        match body {
            Some(_) => Response::Comment { comment },
            None => Response::Ok,
        }
    }

    pub(super) async fn boards(
        self: &Arc<Self>,
        project: Option<&str>,
        agent: Option<&str>,
    ) -> Response {
        let named = match project {
            Some(selector) => match self.resolve_project(selector).await {
                Ok(id) => Some(id),
                Err(response) => return *response,
            },
            None => None,
        };
        let mut state = lock(&self.state);
        let caller = agent
            .and_then(|a| state.actor(a).ok())
            .and_then(|a| a.project);
        let Some(project) = named.or(caller) else {
            return Response::error(ErrorCode::Invalid, "name the project");
        };
        match state.project_boards(&project) {
            Ok(boards) => Response::Boards { boards },
            Err(response) => *response,
        }
    }

    pub(super) async fn board_create(
        self: &Arc<Self>,
        from: &str,
        project: Option<&str>,
        name: &str,
        template: &str,
    ) -> Response {
        let named = match project {
            Some(selector) => match self.resolve_project(selector).await {
                Ok(id) => Some(id),
                Err(response) => return *response,
            },
            None => None,
        };
        let mut state = lock(&self.state);
        let actor = match state.actor(from) {
            Ok(actor) => actor,
            Err(response) => return *response,
        };
        if !actor.human {
            return refused(TaskError::NotYours);
        }
        let Some(project) = named.or(actor.project.clone()) else {
            return Response::error(ErrorCode::Invalid, "name the project");
        };
        let boards = match state.project_boards(&project) {
            Ok(boards) => boards,
            Err(response) => return *response,
        };
        let base = board::slug_of(name);
        let mut slug = base.clone();
        let mut n = 2;
        while boards.iter().any(|b| b.slug == slug) {
            slug = format!("{base}-{n}");
            n += 1;
        }
        let prefix = boards
            .iter()
            .find(|b| b.slug == board::MAIN)
            .map(|b| b.key_prefix.clone())
            .unwrap_or_else(|| board::key_prefix(&state.project_name(&project)));
        let board =
            match Board::from_template(project.clone(), &slug, name, template, &prefix, Utc::now())
            {
                Ok(board) => board,
                Err(error) => return *board_error(error),
            };
        let kinds = vec![EventKind::BoardChanged {
            project: project.clone(),
            board: board.slug.clone(),
            by: actor.id.clone(),
        }];
        if let Some(error) = state.commit_documents("board", vec![board_doc(&board)], kinds) {
            return error;
        }
        Response::Board { board }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn board_update(
        self: &Arc<Self>,
        from: &str,
        project: Option<&str>,
        selector: &str,
        name: Option<String>,
        lanes: Option<Vec<Lane>>,
        pull_source: Option<String>,
        pull_target: Option<String>,
    ) -> Response {
        let named = match project {
            Some(selector) => match self.resolve_project(selector).await {
                Ok(id) => Some(id),
                Err(response) => return *response,
            },
            None => None,
        };
        let mut state = lock(&self.state);
        let actor = match state.actor(from) {
            Ok(actor) => actor,
            Err(response) => return *response,
        };
        if !actor.human {
            return refused(TaskError::NotYours);
        }
        let Some(project) = named.or(actor.project.clone()) else {
            return Response::error(ErrorCode::Invalid, "name the project");
        };
        let mut board = match state.project_board(&project, Some(selector)) {
            Ok(board) => board,
            Err(response) => return *response,
        };
        if let Some(name) = name {
            board.name = name.trim().to_owned();
        }
        if let Some(lanes) = lanes {
            board.lanes = lanes;
        }
        if let Some(source) = pull_source {
            board.pull_source = source;
        }
        if let Some(target) = pull_target {
            board.pull_target = target;
        }
        board.updated_at = Utc::now();
        if let Err(error) = board.check() {
            return *board_error(error);
        }
        let kinds = vec![EventKind::BoardChanged {
            project: project.clone(),
            board: board.slug.clone(),
            by: actor.id.clone(),
        }];
        if let Some(error) = state.commit_documents("board", vec![board_doc(&board)], kinds) {
            return error;
        }
        Response::Board { board }
    }

    pub(super) async fn tasks(self: &Arc<Self>, query: TaskQuery) -> Response {
        let project = match &query.project {
            Some(selector) => match self.resolve_project(selector).await {
                Ok(id) => Some(id),
                Err(response) => return *response,
            },
            None => None,
        };
        let limit = if query.limit == 0 {
            TASKS_LIMIT
        } else {
            query.limit.min(TASKS_LIMIT_MAX)
        };
        let mut state = lock(&self.state);
        let filtered = query.board.is_some()
            || query.lane.is_some()
            || query.assignee.is_some()
            || query.open
            || query.query.is_some()
            || query.tag.is_some();
        if !filtered {
            let page = state.store_read("tasks", |store| {
                store.tasks_page(
                    project.as_ref().map(ProjectId::as_str),
                    query.column.map(|c| c.as_str()),
                    query.archived,
                    query.offset,
                    limit,
                    TASKS_PAGE_BYTES,
                )
            });
            return match page {
                Some((tasks, more)) => Response::Tasks { tasks, more },
                None => state.storage_failure().unwrap_or_else(|| {
                    Response::error(ErrorCode::StorageUnavailable, "the board could not be read")
                }),
            };
        }
        let cards = match &project {
            Some(project) => match state.project_cards(project) {
                Ok(cards) => cards,
                Err(response) => return *response,
            },
            None => state
                .store_read("all cards", |store| store.documents::<Task>(DOCUMENT, None))
                .unwrap_or_default(),
        };
        let board = match (
            &project,
            query
                .board
                .as_deref()
                .or(query.lane.as_ref().map(|_| board::MAIN)),
        ) {
            (Some(project), Some(selector)) => match state.project_board(project, Some(selector)) {
                Ok(board) => Some(board),
                Err(response) => return *response,
            },
            _ => None,
        };
        let lane = match (&board, query.lane.as_deref()) {
            (Some(board), Some(words)) => match board.resolve(words) {
                Ok(lane) => Some(lane.slug.clone()),
                Err(error) => return *board_error(error),
            },
            _ => None,
        };
        let me = query
            .agent
            .as_deref()
            .and_then(|a| state.actor(a).ok())
            .map(|a| a.id);
        let assignee = match query.assignee.as_deref().map(str::trim) {
            None => None,
            Some("none") | Some("") => Some(None),
            Some("me") => match &me {
                Some(me) => Some(Some(me.clone())),
                None => {
                    return Response::error(
                        ErrorCode::Invalid,
                        "say who you are to ask for your cards",
                    );
                }
            },
            Some(who) => match state.resolve(who) {
                Ok(id) => Some(Some(id.to_string())),
                Err(response) => return *response,
            },
        };
        let words = query.query.as_deref().map(str::to_lowercase);
        let tag = query.tag.as_deref().map(agentdocker_core::task::tag_of);
        let mut matching: Vec<Task> = cards
            .into_iter()
            .filter(|t| query.archived || t.archived_at.is_none())
            .filter(|t| !query.open || t.open())
            .filter(|t| query.column.is_none_or(|c| t.column == c))
            .filter(|t| {
                board.as_ref().is_none_or(|b| {
                    t.board.as_deref().unwrap_or(board::MAIN) == b.slug
                        && lane.as_ref().is_none_or(|l| &b.lane_of(t).slug == l)
                })
            })
            .filter(|t| match &assignee {
                None => true,
                Some(None) => t.assignee.is_none(),
                Some(Some(who)) => t.assignee.as_ref().is_some_and(|a| a.as_str() == who),
            })
            .filter(|t| tag.as_ref().is_none_or(|tag| t.tags.contains(tag)))
            .filter(|t| {
                words.as_ref().is_none_or(|w| {
                    t.key.to_lowercase() == *w
                        || t.title.to_lowercase().contains(w.as_str())
                        || t.description.to_lowercase().contains(w.as_str())
                        || t.acceptance.to_lowercase().contains(w.as_str())
                })
            })
            .collect();
        matching.sort_by(|a, b| {
            Column::CATEGORIES
                .iter()
                .position(|c| *c == a.column)
                .cmp(&Column::CATEGORIES.iter().position(|c| *c == b.column))
                .then(board::order(a, b))
        });
        let mut page = Vec::new();
        let mut more = false;
        let mut size = 0usize;
        for task in matching.into_iter().skip(query.offset) {
            let bytes = serde_json::to_string(&task).map_or(0, |j| j.len());
            if page.len() >= limit || (!page.is_empty() && size + bytes > TASKS_PAGE_BYTES) {
                more = true;
                break;
            }
            size += bytes;
            page.push(task);
        }
        Response::Tasks { tasks: page, more }
    }
}

/// What an update asks to change, as the request says it.
#[derive(Default)]
pub(super) struct TaskChange {
    pub title: Option<String>,
    pub acceptance: Option<String>,
    pub assignee: Option<String>,
    pub links: Option<Vec<agentdocker_core::Link>>,
    pub description: Option<String>,
    pub priority: Option<agentdocker_core::Priority>,
    pub size: Option<String>,
    pub due: Option<String>,
    pub external_blocker: Option<String>,
    pub parent: Option<String>,
    pub blocked_by: Option<Vec<String>>,
    pub tags: Option<Vec<String>>,
    pub plan: Option<String>,
}

/// What a listing asks for.
pub(super) struct TaskQuery {
    pub project: Option<String>,
    pub column: Option<Column>,
    pub archived: bool,
    pub offset: usize,
    pub limit: usize,
    pub board: Option<String>,
    pub lane: Option<String>,
    pub assignee: Option<String>,
    pub agent: Option<String>,
    pub open: bool,
    pub query: Option<String>,
    pub tag: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pull answers the card in full; these tests read the card.
    fn as_card(response: Response) -> Response {
        match response {
            Response::TaskDetail { task, .. } => Response::Task { task },
            other => other,
        }
    }
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
                        links: Vec::new(),
                        board: None,
                        lane: None,
                        description: String::new(),
                        priority: None,
                        size: None,
                        due: None,
                        external_blocker: String::new(),
                        parent: None,
                        blocked_by: Vec::new(),
                        tags: Vec::new(),
                        plan: None,
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
                    links: Vec::new(),
                    board: None,
                    lane: None,
                    description: String::new(),
                    priority: None,
                    size: None,
                    due: None,
                    external_blocker: String::new(),
                    parent: None,
                    blocked_by: Vec::new(),
                    tags: Vec::new(),
                    plan: None,
                })
                .await,
            Response::Error {
                code: ErrorCode::Invalid,
                ..
            }
        ));
        let first = create(&daemon, "Fix login", Some(Column::Ready)).await;
        let second = create(&daemon, "Write the release notes", None).await;
        // Links: a card is filed with them, checked for their kinds'
        // shapes, and an update replaces them whole or clears them.
        assert!(matches!(
            daemon
                .handle(Request::TaskCreate {
                    from: HUMAN.to_owned(),
                    project: Some(work.display().to_string()),
                    title: "Linked".to_owned(),
                    acceptance: String::new(),
                    column: None,
                    links: vec![agentdocker_core::Link::new(
                        agentdocker_core::LinkKind::Url,
                        "ftp://nope"
                    )],
                    board: None,
                    lane: None,
                    description: String::new(),
                    priority: None,
                    size: None,
                    due: None,
                    external_blocker: String::new(),
                    parent: None,
                    blocked_by: Vec::new(),
                    tags: Vec::new(),
                    plan: None,
                })
                .await,
            Response::Error {
                code: ErrorCode::Invalid,
                ..
            }
        ));
        let links = vec![
            agentdocker_core::Link::parse("pr:#176").unwrap(),
            agentdocker_core::Link::parse("path:crates/core/src/task.rs").unwrap(),
        ];
        let Response::Task { task: linked } = daemon
            .handle(Request::TaskCreate {
                from: HUMAN.to_owned(),
                project: Some(work.display().to_string()),
                title: "Linked".to_owned(),
                acceptance: String::new(),
                column: None,
                links: links.clone(),
                board: None,
                lane: None,
                description: String::new(),
                priority: None,
                size: None,
                due: None,
                external_blocker: String::new(),
                parent: None,
                blocked_by: Vec::new(),
                tags: Vec::new(),
                plan: None,
            })
            .await
        else {
            panic!("filed with links")
        };
        assert_eq!(linked.links, links);
        let Response::Task { task: relinked } = daemon
            .handle(Request::TaskUpdate {
                agent: HUMAN.to_owned(),
                task: linked.id.to_string(),
                title: None,
                acceptance: None,
                assignee: None,
                links: Some(vec![
                    agentdocker_core::Link::parse("commit:3fb678f").unwrap(),
                ]),
                description: None,
                priority: None,
                size: None,
                due: None,
                external_blocker: None,
                parent: None,
                blocked_by: None,
                tags: None,
                plan: None,
            })
            .await
        else {
            panic!("relinked")
        };
        assert_eq!(relinked.links.len(), 1);
        assert_eq!(relinked.links[0].to_string(), "commit:3fb678f");
        let Response::Task { task: unlinked } = daemon
            .handle(Request::TaskUpdate {
                agent: HUMAN.to_owned(),
                task: linked.id.to_string(),
                title: None,
                acceptance: None,
                assignee: None,
                links: Some(Vec::new()),
                description: None,
                priority: None,
                size: None,
                due: None,
                external_blocker: None,
                parent: None,
                blocked_by: None,
                tags: None,
                plan: None,
            })
            .await
        else {
            panic!("unlinked")
        };
        assert!(unlinked.links.is_empty());
        assert!(matches!(
            daemon
                .handle(Request::TaskArchive {
                    agent: HUMAN.to_owned(),
                    task: linked.id.to_string(),
                })
                .await,
            Response::Ok
        ));
        // A message carries its links to the reader, and a bad one is
        // refused before it is routed.
        assert!(matches!(
            daemon
                .handle(Request::Send {
                    from: HUMAN.to_owned(),
                    to: "alice".to_owned(),
                    kind: "chat".to_owned(),
                    payload: serde_json::json!({"text": "see"}),
                    reply_to: None,
                    links: vec![agentdocker_core::Link::new(
                        agentdocker_core::LinkKind::Task,
                        "zz"
                    )],
                })
                .await,
            Response::Error {
                code: ErrorCode::Invalid,
                ..
            }
        ));
        let card_link = agentdocker_core::Link::parse(&format!("task:{}", first.id)).unwrap();
        assert!(matches!(
            daemon
                .handle(Request::Send {
                    from: HUMAN.to_owned(),
                    to: "alice".to_owned(),
                    kind: "chat".to_owned(),
                    payload: serde_json::json!({"text": "this one is yours"}),
                    reply_to: None,
                    links: vec![card_link.clone()],
                })
                .await,
            Response::Sent { .. }
        ));
        let Response::Messages { messages } = daemon
            .handle(Request::Inbox {
                agent: "alice".to_owned(),
                drain: false,
            })
            .await
        else {
            panic!("inbox")
        };
        assert!(
            messages
                .iter()
                .any(|m| m.payload["text"] == "this one is yours"
                    && m.links == vec![card_link.clone()]),
            "{messages:?}"
        );
        assert_eq!(
            (first.column, first.created_by.as_str()),
            (Column::Ready, person.id.as_str()),
            "filed by the person's record"
        );
        assert_eq!(second.column, Column::Backlog);
        // A prefix resolves; two agents race for the card.
        let prefix = &first.id.as_str()[..6];
        let Response::Task { task: pulled } = as_card(
            daemon
                .handle(Request::TaskPull {
                    agent: "alice".to_owned(),
                    task: Some(prefix.to_owned()),
                    take_over_from: None,
                    project: None,
                    board: None,
                })
                .await,
        ) else {
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
        match as_card(
            daemon
                .handle(Request::TaskPull {
                    agent: "bob".to_owned(),
                    task: Some(first.id.to_string()),
                    take_over_from: None,
                    project: None,
                    board: None,
                })
                .await,
        ) {
            Response::Error {
                code: ErrorCode::Conflict,
                details,
                ..
            } => assert_eq!(details.unwrap()["assignee"], alice.id.as_str()),
            other => panic!("{other:?}"),
        }
        // Not ready: Backlog is the person's.
        assert!(matches!(
            as_card(
                daemon
                    .handle(Request::TaskPull {
                        agent: "bob".to_owned(),
                        task: Some(second.id.to_string()),
                        take_over_from: None,
                        project: None,
                        board: None,
                    })
                    .await
            ),
            Response::Error {
                code: ErrorCode::Conflict,
                ..
            }
        ));
        // The person does not pull; they assign.
        assert!(matches!(
            as_card(
                daemon
                    .handle(Request::TaskPull {
                        agent: HUMAN.to_owned(),
                        task: Some(second.id.to_string()),
                        take_over_from: None,
                        project: None,
                        board: None,
                    })
                    .await
            ),
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
                    column: Some(Column::Review),
                    to: None,
                    after: None,
                    before: None,
                    comment: None,
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
                column: Some(Column::Review),
                to: None,
                after: None,
                before: None,
                comment: None,
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
                column: Some(Column::Ready),
                to: None,
                after: None,
                before: None,
                comment: None,
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
                links: None,
                description: None,
                priority: None,
                size: None,
                due: None,
                external_blocker: None,
                parent: None,
                blocked_by: None,
                tags: None,
                plan: None,
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
                        board: None,
                        lane: None,
                        assignee: None,
                        agent: None,
                        open: false,
                        query: None,
                        tag: None,
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
        assert_eq!(
            list(&daemon, true).await.len(),
            3,
            "with the archived linked card"
        );
        drop(daemon);
        let daemon = open(&dir);
        let board = list(&daemon, true).await;
        assert_eq!(board.len(), 3);
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
                "created", "created", "created", "updated", "updated", "archived", "pulled",
                "moved", "moved", "updated", "archived"
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
                links: Vec::new(),
                board: None,
                lane: None,
                description: String::new(),
                priority: None,
                size: None,
                due: None,
                external_blocker: String::new(),
                parent: None,
                blocked_by: Vec::new(),
                tags: Vec::new(),
                plan: None,
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
                as_card(
                    daemon
                        .handle(Request::TaskPull {
                            agent: who,
                            task: Some(id),
                            take_over_from: from,
                            project: None,
                            board: None,
                        })
                        .await,
                )
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
                    column: Some(Column::Review),
                    to: None,
                    after: None,
                    before: None,
                    comment: None,
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
                column: Some(Column::Done),
                to: None,
                after: None,
                before: None,
                comment: None,
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
                links: None,
                description: None,
                priority: None,
                size: None,
                due: None,
                external_blocker: None,
                parent: None,
                blocked_by: None,
                tags: None,
                plan: None,
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
                    column: Some(Column::Ready),
                    to: None,
                    after: None,
                    before: None,
                    comment: None,
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
                column: Some(Column::Done),
                to: None,
                after: None,
                before: None,
                comment: None,
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
                    column: Some(Column::Review),
                    to: None,
                    after: None,
                    before: None,
                    comment: None,
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
                column: Some(Column::Done),
                to: None,
                after: None,
                before: None,
                comment: None,
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
                links: Vec::new(),
                board: None,
                lane: None,
                description: String::new(),
                priority: None,
                size: None,
                due: None,
                external_blocker: String::new(),
                parent: None,
                blocked_by: Vec::new(),
                tags: Vec::new(),
                plan: None,
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
            as_card(
                daemon
                    .handle(Request::TaskPull {
                        agent: "bob".to_owned(),
                        task: Some(task.id.to_string()),
                        take_over_from: None,
                        project: None,
                        board: None,
                    })
                    .await
            ),
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
            as_card(
                daemon
                    .handle(Request::TaskPull {
                        agent: "bob".to_owned(),
                        task: Some(task.id.to_string()),
                        take_over_from: None,
                        project: None,
                        board: None,
                    })
                    .await
            ),
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
                board: None,
                lane: None,
                assignee: None,
                agent: None,
                open: false,
                query: None,
                tag: None,
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
            as_card(
                daemon
                    .handle(Request::TaskPull {
                        agent: bob.id.to_string(),
                        task: Some(task.id.to_string()),
                        take_over_from: None,
                        project: None,
                        board: None,
                    })
                    .await
            ),
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
                links: Vec::new(),
                board: None,
                lane: None,
                description: String::new(),
                priority: None,
                size: None,
                due: None,
                external_blocker: String::new(),
                parent: None,
                blocked_by: Vec::new(),
                tags: Vec::new(),
                plan: None,
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
            as_card(
                daemon
                    .handle(Request::TaskPull {
                        agent: "bob".to_owned(),
                        task: Some(task.id.to_string()),
                        take_over_from: None,
                        project: None,
                        board: None,
                    })
                    .await
            ),
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
                        links: Vec::new(),
                        board: None,
                        lane: None,
                        description: String::new(),
                        priority: None,
                        size: None,
                        due: None,
                        external_blocker: String::new(),
                        parent: None,
                        blocked_by: Vec::new(),
                        tags: Vec::new(),
                        plan: None,
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
                        board: None,
                        lane: None,
                        assignee: None,
                        agent: None,
                        open: false,
                        query: None,
                        tag: None,
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
            as_card(
                daemon
                    .handle(Request::TaskPull {
                        agent: alice.id.to_string(),
                        task: Some("abc".to_owned()),
                        take_over_from: None,
                        project: None,
                        board: None,
                    })
                    .await
            ),
            Response::Error {
                code: ErrorCode::StorageUnavailable,
                ..
            }
        ));
    }
}
