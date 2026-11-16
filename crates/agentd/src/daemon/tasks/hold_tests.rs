//! A card's hold on a real daemon: each refusal says the claim it found,
//! its lease word where true, and the step past it; lease evidence sits
//! beside the claim and never replaces it.
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

/// A request by its wire form, so a test names only what it sets.
fn request(value: serde_json::Value) -> Request {
    serde_json::from_value(value).unwrap()
}

struct Board {
    daemon: Arc<Daemon>,
    work: std::path::PathBuf,
    _dir: TempDir,
}

impl Board {
    async fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        Board {
            daemon,
            work,
            _dir: dir,
        }
    }

    async fn agent(&self, name: &str) -> AgentRecord {
        register(&self.daemon, name, &self.work).await
    }

    async fn card(&self, title: &str, column: Column, blocker: &str) -> Task {
        self.card_on(None, title, column, blocker).await
    }

    /// A card on a board of the project's: the main one, or the agents
    /// template's, which has a Blocked lane.
    async fn card_on(
        &self,
        board: Option<&str>,
        title: &str,
        column: Column,
        blocker: &str,
    ) -> Task {
        if board == Some("agents") {
            let made = self
                .ask(serde_json::json!({
                    "op": "board_create",
                    "from": HUMAN,
                    "project": self.work.display().to_string(),
                    "name": "Agents",
                    "template": "agents",
                }))
                .await;
            assert!(
                matches!(made, Response::Board { .. })
                    || matches!(&made, Response::Error { message, .. } if message.contains("already")),
                "{made:?}"
            );
        }
        match self
            .daemon
            .handle(request(serde_json::json!({
                "op": "task_create",
                "from": HUMAN,
                "project": self.work.display().to_string(),
                "board": board,
                "title": title,
                "column": column,
                "external_blocker": blocker,
            })))
            .await
        {
            Response::Task { task } => task,
            other => panic!("{other:?}"),
        }
    }

    async fn ask(&self, value: serde_json::Value) -> Response {
        self.daemon.handle(request(value)).await
    }

    async fn pull(&self, who: &str, task: &Task, from: Option<&str>) -> Response {
        self.ask(serde_json::json!({
            "op": "task_pull",
            "agent": who,
            "task": task.id.to_string(),
            "take_over_from": from,
        }))
        .await
    }

    /// The person moves a card.
    async fn put(&self, who: &str, task: &Task, column: Column) -> Response {
        self.ask(serde_json::json!({
            "op": "task_move",
            "agent": who,
            "task": task.id.to_string(),
            "column": column,
        }))
        .await
    }

    /// The person hands a card to somebody.
    async fn hand(&self, task: &Task, to: &AgentRecord) {
        let handed = self
            .ask(serde_json::json!({
                "op": "task_update",
                "agent": HUMAN,
                "task": task.id.to_string(),
                "assignee": to.id.to_string(),
            }))
            .await;
        assert!(matches!(handed, Response::Task { .. }), "{handed:?}");
    }

    async fn claim(&self, who: &str, task: &Task, mode: &str, actor: Option<&str>) -> Response {
        self.ask(serde_json::json!({
            "op": "claim",
            "agent": who,
            "resource": format!("task:{}", task.id),
            "mode": mode,
            "ttl_secs": 600,
            "actor": actor,
        }))
        .await
    }

    async fn release_all(&self, who: &str) {
        let released = self
            .ask(serde_json::json!({
                "op": "release_all",
                "agent": who,
                "only_automatic": false,
            }))
            .await;
        assert!(matches!(released, Response::Leases { .. }), "{released:?}");
    }

    fn stored(&self, task: &Task) -> Task {
        lock(&self.daemon.state)
            .resolve_task(task.id.as_str())
            .unwrap()
    }

    fn holders(&self, task: &Task) -> Vec<AgentId> {
        let state = lock(&self.daemon.state);
        let mut holders: Vec<AgentId> = state
            .leases
            .holders_of(&task_resource(task))
            .into_iter()
            .map(|lease| lease.holder.clone())
            .collect();
        holders.sort();
        holders
    }
}

/// The refusal's code, message and details, or a panic naming what came.
fn refusal(response: Response) -> (ErrorCode, String, serde_json::Value) {
    match response {
        Response::Error {
            code,
            message,
            details,
        } => (code, message, details.unwrap_or_default()),
        other => panic!("not refused: {other:?}"),
    }
}

/// Every refusal of a held card says what it found and the step past it,
/// keeping `assignee` and `column`, and says `hold` only where it is
/// true: `live` for a live lease of the named holder, `lapsed` for a
/// worked card without one, and nothing for a card that only names
/// somebody, has ended or is archived.
#[tokio::test]
async fn a_refusal_says_the_hold_it_found_and_the_step_past_it() {
    let board = Board::new().await;
    let alice = board.agent("alice").await;
    board.agent("bob").await;
    let carol = board.agent("carol").await;
    let task = board.card("Port the parser", Column::Ready, "").await;
    assert!(matches!(
        board.pull("alice", &task, None).await,
        Response::TaskDetail { .. }
    ));

    // Held: live, wait until the lease runs out.
    let (code, message, details) = refusal(board.pull("bob", &task, None).await);
    assert_eq!(code, ErrorCode::Conflict);
    assert_eq!(details["hold"], "live");
    assert_eq!(details["state"], "held");
    assert_eq!(details["holder"], alice.id.as_str());
    assert!(details["until"].is_string(), "{details}");
    assert_eq!(details["step"], "wait");
    assert_eq!(details["assignee"], alice.id.as_str());
    assert_eq!(details["column"], "in_progress");
    assert!(
        message.contains("is held by") && message.contains("wait"),
        "{message}"
    );

    // Lapsed: a plain pull is told to take it over by name.
    board.release_all("alice").await;
    let (code, message, details) = refusal(board.pull("bob", &task, None).await);
    assert_eq!(code, ErrorCode::Conflict);
    assert_eq!(details["hold"], "lapsed");
    assert_eq!(details["state"], "lapsed");
    assert_eq!(details["step"], "take_over_from");
    assert!(
        message.contains(&format!("take_over_from naming {}", alice.id)),
        "{message}"
    );
    // A take-over naming the wrong holder: the hold it found, and the step
    // that names the right one.
    let (code, message, details) = refusal(board.pull("bob", &task, Some("carol")).await);
    assert_eq!(code, ErrorCode::Conflict);
    assert_eq!(details["state"], "lapsed");
    assert_eq!(details["holder"], alice.id.as_str());
    assert_eq!(details["step"], "take_over_from");
    assert!(message.contains(alice.id.as_str()), "{message}");
    // Its holder moving it after the lapse: forbidden, as before, with
    // the claim and the step.
    let (code, _, details) = refusal(board.put("alice", &task, Column::Review).await);
    assert_eq!(code, ErrorCode::Forbidden);
    assert_eq!(details["hold"], "lapsed");
    assert_eq!(details["state"], "lapsed");
    assert_eq!(details["step"], "take_over_from");

    // Named, waiting, no lease: no longer said to be live.
    let named = board.card("Write the docs", Column::Backlog, "").await;
    board.hand(&named, &carol).await;
    let (code, _, details) = refusal(board.pull("bob", &named, None).await);
    assert_eq!(code, ErrorCode::Conflict);
    assert!(details.get("hold").is_none(), "{details}");
    assert_eq!(details["state"], "named");
    assert_eq!(details["holder"], carol.id.as_str());
    assert_eq!(details["step"], "release");
    // The caller's own card outside the source: move it there first.
    let (_, _, details) = refusal(board.pull("carol", &named, None).await);
    assert_eq!(details["step"], "move_to_source");

    // Ended: reopen it first; no lease word.
    let ended = board.card("Ship it", Column::Ready, "").await;
    assert!(matches!(
        board.pull("carol", &ended, None).await,
        Response::TaskDetail { .. }
    ));
    assert!(matches!(
        board.put(HUMAN, &ended, Column::Done).await,
        Response::Task { .. }
    ));
    let (code, _, details) = refusal(board.pull("bob", &ended, None).await);
    assert_eq!(code, ErrorCode::Conflict);
    assert!(details.get("hold").is_none(), "{details}");
    assert_eq!(details["state"], "ended");
    assert_eq!(details["step"], "reopen");

    // Archived: invalid, as before, saying so.
    let archived = board.card("Old idea", Column::Backlog, "").await;
    let gone = board
        .ask(serde_json::json!({"op": "task_archive", "agent": HUMAN, "task": archived.id.to_string()}))
        .await;
    assert!(matches!(gone, Response::Ok), "{gone:?}");
    let (code, _, details) = refusal(board.pull("bob", &archived, None).await);
    assert_eq!(code, ErrorCode::Invalid);
    assert_eq!(details["state"], "archived");
    assert_eq!(details["step"], "unarchive");
}

/// A waiting card naming an agent that has ended is taken by name, its
/// wait said over in the same pull, and says whom it was taken from; a
/// Blocked one is not freed so. A lease taken by hand still refuses the
/// pull after the card was freed, and the card stays as it was.
#[tokio::test]
async fn a_card_left_by_an_ended_holder_is_taken_by_name_with_its_wait_said_over() {
    let board = Board::new().await;
    let gone = board.agent("gone").await;
    let bob = board.agent("bob").await;
    board.agent("carol").await;
    let left = board
        .card("Left behind", Column::Backlog, "the vendor")
        .await;
    let blocked = board
        .card_on(Some("agents"), "Stuck", Column::Blocked, "")
        .await;
    assert_eq!(blocked.column, Column::Blocked);
    let leased = board.card("Claimed by hand", Column::Ready, "").await;
    for card in [&left, &blocked, &leased] {
        board.hand(card, &gone).await;
    }
    let ended = board
        .ask(serde_json::json!({"op": "deregister", "agent": "gone"}))
        .await;
    assert!(!matches!(ended, Response::Error { .. }), "{ended:?}");

    let taken = board
        .ask(serde_json::json!({
            "op": "task_pull",
            "agent": "bob",
            "task": left.id.to_string(),
            "unblock": "the vendor shipped",
        }))
        .await;
    let Response::TaskDetail { task, .. } = taken else {
        panic!("{taken:?}")
    };
    assert!(task.external_blocker.is_empty());
    assert_eq!(task.assignee.as_ref(), Some(&bob.id));
    let pulled = board
        .daemon
        .recent_events(50)
        .into_iter()
        .find_map(|event| match event.kind {
            EventKind::TaskPulled { task: id, from, .. } if id == left.id => Some(from),
            _ => None,
        })
        .expect("pulled");
    assert_eq!(
        pulled.as_ref(),
        Some(&gone.id),
        "taken from the ended holder"
    );

    // Blocked keeps its holder: a member's move releases it.
    let (code, _, details) = refusal(board.pull("bob", &blocked, None).await);
    assert_eq!(code, ErrorCode::Conflict);
    assert_eq!(details["state"], "named");
    assert_eq!(details["step"], "release");
    assert_eq!(board.stored(&blocked).assignee.as_ref(), Some(&gone.id));

    // Carol's lease by hand refuses bob's pull, and the card, freed only
    // on the pull's own copy, is stored as it was.
    assert!(matches!(
        board.claim("carol", &leased, "exclusive", None).await,
        Response::Lease { .. }
    ));
    let before = board.stored(&leased);
    let (code, message, _) = refusal(board.pull("bob", &leased, None).await);
    assert_eq!(code, ErrorCode::Conflict);
    assert!(
        message.contains(&format!("task:{}", before.id)),
        "{message}"
    );
    assert_eq!(board.stored(&leased), before);
}

/// Lease evidence sits beside the claim: another agent's shared lease
/// leaves a lapsed holder lapsed; the caller's own lease never stands in
/// its way; two shared owners are each other's conflict and both a third
/// caller's; a lease of another actor in the caller's session is decided
/// by the lease table, and a refused claim changes nothing.
#[tokio::test]
async fn the_leases_sit_beside_the_claim_and_the_lease_table_decides() {
    let board = Board::new().await;
    let alice = board.agent("alice").await;
    let bob = board.agent("bob").await;
    board.agent("carol").await;

    // Alice's hold lapsed; bob holds a shared lease: alice still may not
    // move it.
    let lapsed = board.card("Lapsed", Column::Ready, "").await;
    assert!(matches!(
        board.pull("alice", &lapsed, None).await,
        Response::TaskDetail { .. }
    ));
    board.release_all("alice").await;
    assert!(matches!(
        board.claim("bob", &lapsed, "shared", None).await,
        Response::Lease { .. }
    ));
    let (code, _, details) = refusal(board.put("alice", &lapsed, Column::Review).await);
    assert_eq!(code, ErrorCode::Forbidden);
    assert_eq!(details["state"], "lapsed");
    assert_eq!(details["hold"], "lapsed");

    // An unassigned card under the caller's own lease: pullable.
    let mine = board.card("Mine by hand", Column::Ready, "").await;
    assert!(matches!(
        board.claim("bob", &mine, "exclusive", None).await,
        Response::Lease { .. }
    ));
    assert!(matches!(
        board.pull("bob", &mine, None).await,
        Response::TaskDetail { .. }
    ));
    assert_eq!(board.holders(&mine), std::slice::from_ref(&bob.id));

    // Two shared owners: a third sees both; each sees the other.
    let shared = board.card("Shared", Column::Ready, "").await;
    for who in ["alice", "bob"] {
        assert!(matches!(
            board.claim(who, &shared, "shared", None).await,
            Response::Lease { .. }
        ));
    }
    // The lease table's conflict names each lease in the caller's way.
    let (code, message, _) = refusal(board.pull("carol", &shared, None).await);
    assert_eq!(code, ErrorCode::Conflict);
    assert!(
        message.contains(alice.id.short()) && message.contains(bob.id.short()),
        "{message}"
    );
    let (_, message, _) = refusal(board.pull("alice", &shared, None).await);
    assert!(
        message.contains(bob.id.short()) && !message.contains(alice.id.short()),
        "{message}"
    );
    let before = board.stored(&shared);
    assert_eq!(board.holders(&shared).len(), 2, "refusals change no lease");
    assert_eq!(board.stored(&shared), before);

    // An actor's lease in alice's own session is no lease by hand in her
    // way: the pull's check passes it by, as it always has, and the lease
    // table's own rule lets her claim stand beside it.
    let actor = board.card("Actor's", Column::Ready, "").await;
    assert!(matches!(
        board
            .claim("alice", &actor, "exclusive", Some("helper"))
            .await,
        Response::Lease { .. }
    ));
    assert!(matches!(
        board.pull("alice", &actor, None).await,
        Response::TaskDetail { .. }
    ));
    assert!(
        board
            .holders(&actor)
            .iter()
            .all(|holder| holder == &alice.id)
    );
    // Bob is refused by it, as by any lease, and nothing changes.
    let before = board.stored(&actor);
    let (code, message, _) = refusal(board.pull("bob", &actor, None).await);
    assert_eq!(code, ErrorCode::Conflict);
    assert!(message.contains(alice.id.short()), "{message}");
    assert_eq!(board.stored(&actor), before);
}

/// A rule start on a card that is not free says the claim it found and the
/// step past it, always `conflict`, as a start has always answered.
#[tokio::test]
async fn a_rule_start_says_the_claim_it_found() {
    let board = Board::new().await;
    let alice = board.agent("alice").await;
    let named = board.card("Named", Column::Ready, "").await;
    board.hand(&named, &alice).await;
    let held = board.card("Held", Column::Ready, "").await;
    assert!(matches!(
        board.pull("alice", &held, None).await,
        Response::TaskDetail { .. }
    ));
    let ended = board.card("Ended", Column::Done, "").await;
    let archived = board.card("Archived", Column::Backlog, "").await;
    assert!(matches!(
        board
            .ask(serde_json::json!({"op": "task_archive", "agent": HUMAN, "task": archived.id.to_string()}))
            .await,
        Response::Ok
    ));
    let refused = |task: &Task| {
        let task = board.stored(task);
        lock(&board.daemon.state)
            .start_refusal(&task)
            .expect("refused")
    };
    for (task, state, step) in [
        (&named, "named", "release"),
        (&held, "held", "wait"),
        (&ended, "ended", "reopen"),
        (&archived, "archived", "unarchive"),
    ] {
        let (code, message, details) = refusal(refused(task));
        assert_eq!(code, ErrorCode::Conflict, "{state}");
        assert_eq!(details["state"], state, "{message}");
        assert_eq!(details["step"], step, "{message}");
        assert!(
            message.contains("an agent starts on a card nobody holds"),
            "{message}"
        );
    }
    board.release_all("alice").await;
    let (_, message, details) = refusal(refused(&held));
    assert_eq!(details["state"], "lapsed");
    assert_eq!(details["hold"], "lapsed");
    assert!(message.contains("whose hold has lapsed"), "{message}");
    let free = board.card("Free", Column::Ready, "").await;
    let free = board.stored(&free);
    assert!(lock(&board.daemon.state).start_refusal(&free).is_none());
}

/// A pull with no card named picks as it always has: the first card in
/// the source that names nobody, even one a lease taken by hand will then
/// refuse.
#[tokio::test]
async fn a_pull_with_no_card_named_picks_as_before() {
    let board = Board::new().await;
    board.agent("bob").await;
    board.agent("carol").await;
    let first = board.card("First", Column::Ready, "").await;
    board.card("Second", Column::Ready, "").await;
    assert!(matches!(
        board.claim("carol", &first, "exclusive", None).await,
        Response::Lease { .. }
    ));
    let refused = board
        .ask(serde_json::json!({"op": "task_pull", "agent": "bob"}))
        .await;
    let (code, message, _) = refusal(refused);
    assert_eq!(code, ErrorCode::Conflict);
    assert!(message.contains(&format!("task:{}", first.id)), "{message}");
}
