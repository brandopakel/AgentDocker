//! Rules on a board, and agents started on cards. A rule acts when a card
//! is filed into or moved into its lane (never on a rule's own move, so
//! rules cannot chase each other), or, on the minute, once a card has sat
//! in its lane for its days. Starting an agent runs a process, so it is
//! done off the state lock: the lock hands back a [`Start`], and the
//! daemon launches, then hands the card to the new agent.
use super::*;
use agentdocker_core::rule::{self, Rule, RuleAction, Trigger};
use std::path::PathBuf;

pub(in crate::daemon) const RULE: &str = "board_rule";
/// At most this many rule actions on one minute's sweep; the rest wait
/// for the next.
const SWEEP_ACTIONS: usize = 50;

/// An agent to start on a card, once the state lock is let go.
pub(in crate::daemon) struct Start {
    /// The rule that asked; the person when absent.
    pub rule: Option<Rule>,
    pub task: TaskId,
    pub runtime: String,
    pub args: Vec<String>,
    /// Who asked: the person, for a rule.
    pub from: String,
    /// Where to start it, when the caller said.
    pub workdir: Option<PathBuf>,
}

fn rule_doc(rule: &Rule) -> Doc {
    doc(RULE, rule.id.clone(), rule)
}

/// The author a rule writes as on a card's comments and history.
fn rule_author(rule: &Rule) -> String {
    format!("rule:{}", rule.name)
}

impl State {
    pub(super) fn board_rules(&mut self, project: &ProjectId) -> Vec<Rule> {
        self.store_read("board rules", |store| {
            store.documents_where::<Rule>(RULE, "project", project.as_str(), 2000)
        })
        .unwrap_or_default()
    }

    /// The directory a project's agents work in: its main checkout as an
    /// agent there knows it, else any checkout of it.
    pub(in crate::daemon) fn project_dir(&self, project: &ProjectId) -> Option<PathBuf> {
        let mut found: Option<PathBuf> = None;
        for record in self.registry.all() {
            let Some(p) = record.project.as_ref().filter(|p| &p.id() == project) else {
                continue;
            };
            if p.worktree.is_none() {
                return Some(p.dir().to_path_buf());
            }
            found.get_or_insert_with(|| p.dir().to_path_buf());
        }
        found
    }

    /// Live agents a rule started, and cards it is starting one on.
    /// Why an agent may not be started on `task` now, or `None` when it
    /// may: what a pull would refuse, its blockers looked up among its
    /// project's cards.
    pub(in crate::daemon) fn start_refusal(&mut self, task: &Task) -> Option<Response> {
        let cards = if task.blocked_by.is_empty() {
            Vec::new()
        } else {
            match self.project_cards(&task.project) {
                Ok(cards) => cards,
                Err(response) => return Some(*response),
            }
        };
        let open = |id: &TaskId| cards.iter().any(|t| &t.id == id && t.open());
        if rule::startable(task, open) {
            return None;
        }
        let waits: Vec<String> = cards
            .iter()
            .filter(|t| task.blocked_by.contains(&t.id) && t.open())
            .map(Task::handle)
            .collect();
        Some(if task.assignee.is_some() || !task.open() {
            Response::error(
                ErrorCode::Conflict,
                "the card is held, ended or archived; an agent starts on a card nobody holds",
            )
        } else if !waits.is_empty() {
            Response::Error {
                code: ErrorCode::Conflict,
                message: format!("the card waits on {}", waits.join(", ")),
                details: Some(serde_json::json!({ "blocked_by": waits })),
            }
        } else {
            Response::error(
                ErrorCode::Conflict,
                "the card waits on something off the board",
            )
        })
    }

    fn started_by(&self, rule: &Rule) -> usize {
        let live = self
            .registry
            .all()
            .filter(|r| {
                r.status.is_live() && r.spec.labels.get(rule::STARTED_BY_LABEL) == Some(&rule.id)
            })
            .count();
        let starting = self
            .starting_cards
            .values()
            .filter(|r| r.as_deref() == Some(rule.id.as_str()))
            .count();
        live + starting
    }

    /// A card came into `lane` by a filing or a move that was not a
    /// rule's: every enabled enter rule of that lane acts on it now, and
    /// the agents to start are handed back.
    pub(super) fn rules_on_enter(&mut self, task: &Task, lane: &str) -> Vec<Start> {
        let rules: Vec<Rule> = self
            .board_rules(&task.project)
            .into_iter()
            .filter(|r| r.on_enter(task, lane))
            .collect();
        let mut starts = Vec::new();
        for rule in rules {
            // Read it again: an earlier rule may have moved or archived it.
            let Ok(card) = self.resolve_task(task.id.as_str()) else {
                break;
            };
            if card.archived_at.is_some() || card.lane_on_board_slug() != lane {
                break;
            }
            match self.apply_rule(&rule, card) {
                Ok(Some(start)) => starts.push(start),
                Ok(None) => {}
                Err(response) => warn!(rule = %rule.id, ?response, "a board rule could not act"),
            }
        }
        starts
    }

    /// One rule's action on a card, in one commit. An agent to start is
    /// handed back instead, and the card is marked as being started on.
    fn apply_rule(&mut self, rule: &Rule, mut task: Task) -> Result<Option<Start>, Box<Response>> {
        let now = Utc::now();
        let by = rule_author(rule);
        let board = self.board_of(&task)?;
        let mut kinds = vec![EventKind::RuleFired {
            project: task.project.clone(),
            rule: rule.id.clone(),
            task: task.id.clone(),
        }];
        let mut documents = Vec::new();
        let mut released = Vec::new();
        match &rule.action {
            RuleAction::Comment { text } => {
                let note = Comment::new(&task, &by, text, None, now)
                    .map_err(|error| Box::new(refused(error)))?;
                kinds.push(EventKind::TaskCommented {
                    task: task.id.clone(),
                    project: task.project.clone(),
                    by: by.clone(),
                    comment: note.id.clone(),
                });
                documents.push(entry_doc(&TaskEntry::new(
                    &task,
                    &by,
                    TaskAction::Commented {
                        comment: note.id.clone(),
                    },
                    now,
                )));
                documents.push(comment_doc(&note));
                task.fired.push(rule.id.clone());
            }
            RuleAction::Move { lane } => {
                let to = board.lane(lane).cloned().ok_or_else(|| {
                    board_error(board::BoardError::NoLane {
                        asked: lane.clone(),
                        lanes: board.lanes.iter().map(|l| l.slug.clone()).collect(),
                    })
                })?;
                let previous = task.clone();
                let from = previous.lane_on(&board).slug.clone();
                if !task
                    .move_to(&by, true, &to, now)
                    .map_err(|error| Box::new(refused(error)))?
                {
                    return Ok(None);
                }
                let cards = self.project_cards(&task.project)?;
                task.rank = rank_last(&cards, &board, &to.slug, &task.id);
                released = Daemon::holds_ended(self, &previous, &task);
                kinds.push(EventKind::TaskMoved {
                    task: task.id.clone(),
                    project: task.project.clone(),
                    by: by.clone(),
                    column: to.category,
                    lane: Some(to.slug.clone()),
                });
                documents.push(entry_doc(&TaskEntry::new(
                    &task,
                    &by,
                    TaskAction::Moved {
                        from,
                        to: to.slug.clone(),
                    },
                    now,
                )));
            }
            RuleAction::Archive => {
                let before = task.clone();
                task.archive(&by, true, now)
                    .map_err(|error| Box::new(refused(error)))?;
                released = Daemon::holds_ended(self, &before, &task);
                kinds.push(EventKind::TaskArchived {
                    task: task.id.clone(),
                    project: task.project.clone(),
                    by: by.clone(),
                });
                documents.push(entry_doc(&TaskEntry::new(
                    &task,
                    &by,
                    TaskAction::Archived,
                    now,
                )));
            }
            RuleAction::Start { runtime, args, max } => {
                if self.start_refusal(&task).is_some()
                    || self.starting_cards.contains_key(&task.id)
                    || self.started_by(rule) >= *max as usize
                {
                    // Full, waiting, or somebody has it: the minute's
                    // sweep tries again while it waits.
                    return Ok(None);
                }
                self.starting_cards
                    .insert(task.id.clone(), Some(rule.id.clone()));
                return Ok(Some(Start {
                    rule: Some(rule.clone()),
                    task: task.id.clone(),
                    runtime: runtime.clone(),
                    args: args.clone(),
                    from: HUMAN.to_owned(),
                    workdir: None,
                }));
            }
        }
        kinds.extend(released.iter().map(|lease| EventKind::LeaseReleased {
            lease: lease.clone(),
        }));
        if let Some(error) = self.commit_task(&task, None, None, released, kinds, documents) {
            return Err(Box::new(error));
        }
        Ok(None)
    }

    /// The minute's sweep: idle rules that are due act, and start rules
    /// take the cards still waiting in their lanes, up to their limits.
    pub(super) fn sweep_rules(&mut self, now: DateTime<Utc>) -> Vec<Start> {
        let Some(rules) =
            self.store_read("board rules", |store| store.documents::<Rule>(RULE, None))
        else {
            return Vec::new();
        };
        let mut by_project: BTreeMap<ProjectId, Vec<Rule>> = BTreeMap::new();
        for rule in rules.into_iter().filter(|r| r.enabled) {
            let sweeps = matches!(rule.trigger, Trigger::Idle { .. })
                || matches!(rule.action, RuleAction::Start { .. });
            if sweeps {
                by_project
                    .entry(rule.project.clone())
                    .or_default()
                    .push(rule);
            }
        }
        let mut starts = Vec::new();
        let mut acted = 0;
        for (project, rules) in by_project {
            let Ok(cards) = self.project_cards(&project) else {
                continue;
            };
            let Ok(boards) = self.project_boards(&project) else {
                continue;
            };
            for card in cards.iter().filter(|t| t.archived_at.is_none()) {
                let slug = card.board.as_deref().unwrap_or(board::MAIN);
                let Some(board) = boards.iter().find(|b| b.slug == slug) else {
                    continue;
                };
                let lane = board.lane_of(card).slug.clone();
                for rule in &rules {
                    if acted >= SWEEP_ACTIONS {
                        return starts;
                    }
                    let due = match (&rule.trigger, &rule.action) {
                        (Trigger::Idle { .. }, _) => rule.idle_due(card, &lane, now),
                        // A start rule keeps its lane worked: cards that
                        // came in while it was full, or before it was made.
                        (Trigger::Enter { lane: l }, RuleAction::Start { .. }) => {
                            rule.on_enter(card, &lane)
                                && *l == lane
                                && rule::startable(card, |id| {
                                    cards.iter().any(|t| &t.id == id && t.open())
                                })
                        }
                        _ => false,
                    };
                    if !due {
                        continue;
                    }
                    let Ok(fresh) = self.resolve_task(card.id.as_str()) else {
                        continue;
                    };
                    if fresh.fired.contains(&rule.id) || fresh.archived_at.is_some() {
                        continue;
                    }
                    match self.apply_rule(rule, fresh) {
                        Ok(Some(start)) => {
                            acted += 1;
                            starts.push(start);
                        }
                        Ok(None) => acted += 1,
                        Err(response) => {
                            warn!(rule = %rule.id, ?response, "a board rule could not act");
                        }
                    }
                    // One rule a card a minute: the next sees what it did.
                    break;
                }
            }
        }
        starts
    }
}

trait LaneSlug {
    fn lane_on_board_slug(&self) -> String;
}

impl LaneSlug for Task {
    /// Its lane as stored, or its category's name for a card from before
    /// lanes: what an enter rule compares with the lane it came into.
    fn lane_on_board_slug(&self) -> String {
        self.lane
            .clone()
            .unwrap_or_else(|| self.column.as_str().to_owned())
    }
}

/// A rule's lanes as the person named them, made slugs of `board`.
fn named_lanes(
    board: &Board,
    trigger: Trigger,
    action: RuleAction,
) -> Result<(Trigger, RuleAction), Box<Response>> {
    let slug = |words: &str| {
        board
            .resolve(words)
            .map(|l| l.slug.clone())
            .map_err(board_error)
    };
    let trigger = match trigger {
        Trigger::Enter { lane } => Trigger::Enter { lane: slug(&lane)? },
        Trigger::Idle { lane, days } => Trigger::Idle {
            lane: slug(&lane)?,
            days,
        },
    };
    let action = match action {
        RuleAction::Move { lane } => RuleAction::Move { lane: slug(&lane)? },
        other => other,
    };
    Ok((trigger, action))
}

impl Daemon {
    /// A board's rules, or every board's of the project.
    pub(in crate::daemon) async fn rules(
        self: &Arc<Self>,
        project: Option<String>,
        board: Option<String>,
    ) -> Response {
        let named = match &project {
            Some(selector) => match self.resolve_project(selector).await {
                Ok(id) => Some(id),
                Err(response) => return *response,
            },
            None => None,
        };
        let mut state = lock(&self.state);
        let Some(project) = named else {
            return Response::error(ErrorCode::Invalid, "name a project");
        };
        let slug = match board.as_deref() {
            Some(words) => match state.project_board(&project, Some(words)) {
                Ok(board) => Some(board.slug),
                Err(response) => return *response,
            },
            None => None,
        };
        let mut rules: Vec<Rule> = state
            .board_rules(&project)
            .into_iter()
            .filter(|r| slug.as_ref().is_none_or(|s| *s == r.board))
            .collect();
        rules.sort_by(|a, b| a.board.cmp(&b.board).then(a.created_at.cmp(&b.created_at)));
        Response::Rules { rules }
    }

    #[allow(clippy::too_many_arguments)]
    pub(in crate::daemon) async fn rule_create(
        self: &Arc<Self>,
        from: &str,
        project: Option<String>,
        board: Option<String>,
        name: &str,
        trigger: Trigger,
        action: RuleAction,
    ) -> Response {
        let named = match &project {
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
            return Response::error(
                ErrorCode::Forbidden,
                "a board's rules are the person's to set",
            );
        }
        if let Some(error) = state.write_failure() {
            return error;
        }
        let Some(project) = named.or(actor.project.clone()) else {
            return Response::error(ErrorCode::Invalid, "name a project");
        };
        let board = match state.project_board(&project, board.as_deref()) {
            Ok(board) => board,
            Err(response) => return *response,
        };
        let (trigger, action) = match named_lanes(&board, trigger, action) {
            Ok(named) => named,
            Err(response) => return *response,
        };
        if state
            .board_rules(&project)
            .iter()
            .filter(|r| r.board == board.slug)
            .count()
            >= rule::RULES_MAX
        {
            return Response::error(ErrorCode::Invalid, "a board has at most 32 rules");
        }
        let rule = match Rule::new(
            project.clone(),
            &board,
            name,
            trigger,
            action,
            &actor.id,
            Utc::now(),
        ) {
            Ok(rule) => rule,
            Err(error) => return *board_error(error),
        };
        let kinds = vec![EventKind::RuleChanged {
            project,
            board: board.slug.clone(),
            rule: rule.id.clone(),
            by: actor.id.clone(),
            deleted: false,
        }];
        if let Some(error) = state.commit_documents("rule", vec![rule_doc(&rule)], kinds) {
            return error;
        }
        Response::Rule { rule }
    }

    pub(in crate::daemon) fn rule_update(
        self: &Arc<Self>,
        from: &str,
        id: &str,
        name: Option<String>,
        trigger: Option<Trigger>,
        action: Option<RuleAction>,
        enabled: Option<bool>,
    ) -> Response {
        let mut state = lock(&self.state);
        let actor = match state.actor(from) {
            Ok(actor) => actor,
            Err(response) => return *response,
        };
        if !actor.human {
            return Response::error(
                ErrorCode::Forbidden,
                "a board's rules are the person's to set",
            );
        }
        if let Some(error) = state.write_failure() {
            return error;
        }
        let Some(Some(mut rule)) =
            state.store_read("rule", |store| store.document::<Rule>(RULE, id))
        else {
            return Response::error(ErrorCode::NotFound, format!("no rule is {id}"));
        };
        let board = match state.project_board(&rule.project, Some(&rule.board)) {
            Ok(board) => board,
            Err(response) => return *response,
        };
        let (trigger, action) = match named_lanes(
            &board,
            trigger.unwrap_or_else(|| rule.trigger.clone()),
            action.unwrap_or_else(|| rule.action.clone()),
        ) {
            Ok(named) => named,
            Err(response) => return *response,
        };
        rule.trigger = trigger;
        rule.action = action;
        if let Some(name) = name {
            rule.name = name.trim().to_owned();
        }
        if let Some(enabled) = enabled {
            rule.enabled = enabled;
        }
        rule.updated_at = Utc::now();
        if let Err(error) = rule.check(&board) {
            return *board_error(error);
        }
        let kinds = vec![EventKind::RuleChanged {
            project: rule.project.clone(),
            board: rule.board.clone(),
            rule: rule.id.clone(),
            by: actor.id.clone(),
            deleted: false,
        }];
        if let Some(error) = state.commit_documents("rule", vec![rule_doc(&rule)], kinds) {
            return error;
        }
        Response::Rule { rule }
    }

    pub(in crate::daemon) fn rule_delete(self: &Arc<Self>, from: &str, id: &str) -> Response {
        let mut state = lock(&self.state);
        let actor = match state.actor(from) {
            Ok(actor) => actor,
            Err(response) => return *response,
        };
        if !actor.human {
            return Response::error(
                ErrorCode::Forbidden,
                "a board's rules are the person's to set",
            );
        }
        if let Some(error) = state.write_failure() {
            return error;
        }
        let Some(Some(rule)) = state.store_read("rule", |store| store.document::<Rule>(RULE, id))
        else {
            return Response::error(ErrorCode::NotFound, format!("no rule is {id}"));
        };
        let kinds = vec![EventKind::RuleChanged {
            project: rule.project.clone(),
            board: rule.board.clone(),
            rule: rule.id.clone(),
            by: actor.id.clone(),
            deleted: true,
        }];
        if let Some(error) =
            state.commit_change("rule", Vec::new(), vec![(RULE, rule.id.clone())], kinds)
        {
            return error;
        }
        Response::Ok
    }

    /// The minute's sweep of every board's rules.
    pub fn run_board_rules(self: &Arc<Self>) {
        let starts = {
            let mut state = lock(&self.state);
            if state.fenced() || state.write_failure().is_some() {
                return;
            }
            state.sweep_rules(Utc::now())
        };
        self.spawn_starts(starts);
    }

    /// Start the agents a rule asked for, each in its own task.
    pub(in crate::daemon) fn spawn_starts(self: &Arc<Self>, starts: Vec<Start>) {
        for start in starts {
            let daemon = self.clone();
            tokio::spawn(async move {
                let task = start.task.clone();
                if let Response::Error { message, .. } = daemon.start_on_card(start).await {
                    warn!(%task, %message, "could not start an agent on a card");
                }
            });
        }
    }

    /// The person starts an agent on a card nobody holds.
    pub(in crate::daemon) async fn task_start(
        self: &Arc<Self>,
        from: &str,
        task: &str,
        runtime: String,
        args: Vec<String>,
        workdir: Option<PathBuf>,
    ) -> Response {
        let id = {
            let mut state = lock(&self.state);
            let actor = match state.actor(from) {
                Ok(actor) => actor,
                Err(response) => return *response,
            };
            if !actor.human {
                return Response::error(
                    ErrorCode::Forbidden,
                    "starting an agent on a card is the person's to do",
                );
            }
            if let Some(error) = state.write_failure() {
                return error;
            }
            let task = match state.resolve_task(task) {
                Ok(task) => task,
                Err(response) => return *response,
            };
            if let Some(refusal) = state.start_refusal(&task) {
                return refusal;
            }
            if state.starting_cards.contains_key(&task.id) {
                return Response::error(ErrorCode::Conflict, "an agent is starting on it already");
            }
            state.starting_cards.insert(task.id.clone(), None);
            task.id
        };
        self.start_on_card(Start {
            rule: None,
            task: id,
            runtime,
            args,
            from: from.to_owned(),
            workdir,
        })
        .await
    }

    /// Launch the agent, then hand it the card — moved into the board's
    /// pull target and held by its lease, as a pull would — and tell it
    /// what to do. The card stays marked as being started on until then.
    pub(in crate::daemon) async fn start_on_card(self: &Arc<Self>, start: Start) -> Response {
        let response = self.start_on_card_inner(&start).await;
        lock(&self.state).starting_cards.remove(&start.task);
        response
    }

    async fn start_on_card_inner(self: &Arc<Self>, start: &Start) -> Response {
        let (task, workdir, git) = {
            let mut state = lock(&self.state);
            let task = match state.resolve_task(start.task.as_str()) {
                Ok(task) => task,
                Err(response) => return *response,
            };
            let workdir = start
                .workdir
                .clone()
                .or_else(|| state.project_dir(&task.project));
            let git = state.registry.all().any(|r| {
                r.project
                    .as_ref()
                    .is_some_and(|p| p.id() == task.project && p.source == ProjectSource::Git)
            });
            (task, workdir, git)
        };
        let Some(workdir) = workdir else {
            return Response::error(
                ErrorCode::Invalid,
                "no checkout of the card's project is known here; start the agent from its folder",
            );
        };
        let roots = agentdocker_host::runtimes::Roots::from_env();
        let inventory = tokio::task::spawn_blocking(move || {
            agentdocker_host::runtimes::inventory(&roots, "agentdocker")
        })
        .await;
        let runtimes = match inventory {
            Ok(Ok(runtimes)) => runtimes,
            Ok(Err(error)) => return Response::error(ErrorCode::Unavailable, error.to_string()),
            Err(_) => {
                return Response::error(ErrorCode::Unavailable, "runtime inventory worker failed");
            }
        };
        let Some(cli) = runtimes
            .iter()
            .find(|r| r.name == start.runtime)
            .and_then(|r| r.cli.clone())
        else {
            return Response::error(
                ErrorCode::NotFound,
                format!(
                    "no installed command-line agent is called {}",
                    start.runtime
                ),
            );
        };
        // The inventory took a while: a card taken or blocked since is not
        // started on.
        {
            let mut state = lock(&self.state);
            let fresh = match state.resolve_task(start.task.as_str()) {
                Ok(task) => task,
                Err(response) => return *response,
            };
            if let Some(refusal) = state.start_refusal(&fresh) {
                return refusal;
            }
        }
        let mut command = vec![cli.to_string_lossy().into_owned()];
        command.extend(start.args.iter().cloned());
        let mut labels = BTreeMap::from([(
            agentdocker_core::agent::NAME_LABEL.to_owned(),
            agentdocker_core::agent::GENERATED_NAME.to_owned(),
        )]);
        if let Some(rule) = &start.rule {
            labels.insert(rule::STARTED_BY_LABEL.to_owned(), rule.id.clone());
        }
        let handle = task.handle().to_lowercase();
        let spec = AgentSpec {
            name: format!("{}-{}", start.runtime, handle),
            runtime: start.runtime.clone(),
            command,
            workdir: Some(workdir),
            labels,
            isolate: git,
            tty: true,
            ..AgentSpec::default()
        };
        let agent = match self.run(spec).await {
            Response::Agent { agent } => agent,
            other => return other,
        };
        let mut state = lock(&self.state);
        let now = Utc::now();
        state.expire_leases_at(now);
        let mut task = match state.resolve_task(start.task.as_str()) {
            Ok(task) => task,
            Err(response) => return *response,
        };
        if state.start_refusal(&task).is_some() {
            // Taken or blocked while it started: it says so, and finds
            // other work.
            let why = if task.assignee.is_none() && task.open() {
                "is blocked now"
            } else {
                "was taken before you started"
            };
            let text = format!("{} {} {why}; pull another card.", task.handle(), task.title);
            state.tell_holder(&start.from, &agent.id, &task, text);
            return Response::Agent { agent };
        }
        let board = match state.board_of(&task) {
            Ok(board) => board,
            Err(response) => return *response,
        };
        let previous = task.clone();
        let from_lane = previous.lane_on(&board).slug.clone();
        let mut kinds = Vec::new();
        let mut documents = Vec::new();
        if !task.column.working()
            && let Some(target) = board.lane(&board.pull_target).cloned()
            && task
                .move_to(&start.from, true, &target, now)
                .unwrap_or(false)
        {
            let cards = state.project_cards(&task.project).unwrap_or_default();
            task.rank = rank_last(&cards, &board, &target.slug, &task.id);
            kinds.push(EventKind::TaskMoved {
                task: task.id.clone(),
                project: task.project.clone(),
                by: start.from.clone(),
                column: target.category,
                lane: Some(target.slug.clone()),
            });
            documents.push(entry_doc(&TaskEntry::new(
                &task,
                &start.from,
                TaskAction::Moved {
                    from: from_lane,
                    to: target.slug.clone(),
                },
                now,
            )));
        }
        task.assignee = Some(agent.id.clone());
        task.updated_at = now;
        if let Some(rule) = &start.rule {
            task.fired.push(rule.id.clone());
            kinds.push(EventKind::RuleFired {
                project: task.project.clone(),
                rule: rule.id.clone(),
                task: task.id.clone(),
            });
        }
        let claimed = match state.take_hold(&task, &agent.id, &[], now) {
            Ok(lease) => lease,
            Err(response) => return *response,
        };
        kinds.insert(
            0,
            EventKind::TaskUpdated {
                task: task.id.clone(),
                project: task.project.clone(),
                by: start.from.clone(),
                fields: vec!["assignee".to_owned()],
            },
        );
        kinds.push(EventKind::LeaseClaimed {
            lease: claimed.clone(),
        });
        documents.push(entry_doc(&TaskEntry::new(
            &task,
            &start.from,
            TaskAction::Assigned {
                to: Some(agent.id.to_string()),
            },
            now,
        )));
        if let Some(error) = state.commit_task(
            &task,
            Some(agent.id.clone()),
            Some(claimed),
            Vec::new(),
            kinds,
            documents,
        ) {
            return error;
        }
        state.tell_holder(&start.from, &agent.id, &task, rule::start_prompt(&task));
        Response::Agent { agent }
    }
}
