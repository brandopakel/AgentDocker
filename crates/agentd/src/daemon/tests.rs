#[cfg(unix)]
#[test]
fn a_shell_is_its_original_only_by_its_own_birth() {
    let me = std::process::id();
    let born = procinfo::start_time(me).expect("this process has a birth");
    assert!(born_as(me, born));
    assert!(
        !born_as(me, born + chrono::Duration::seconds(1)),
        "another birth"
    );
    assert!(!born_as(u32::MAX - 7, born), "a process that is gone");
}
use super::*;
use agentdocker_core::Activity;
use agentdocker_core::contest::{Contest, Measure, Metric, Standing};
use tempfile::TempDir;

fn open(dir: &TempDir) -> Arc<Daemon> {
    let home = dir.path().to_path_buf();
    Arc::new(Daemon::open(home.clone(), home.join("sock")).unwrap())
}

fn spec(name: &str) -> AgentSpec {
    AgentSpec {
        name: name.to_owned(),
        ..AgentSpec::default()
    }
}

/// `daemon status` says how each experimental switch stands, from
/// the daemon's own file, so a bridge or hook follows the daemon's
/// word; the reload gate reads the same switch.
#[tokio::test]
async fn ping_reports_the_experimental_switches_the_daemon_applies() {
    let dir = TempDir::new().unwrap();
    std::fs::write(
        dir.path().join(agentdocker_core::config::FILE_NAME),
        "[experimental]\nreload = true\nnative_codex = false\n",
    )
    .unwrap();
    let daemon = open(&dir);
    let Response::Pong {
        experimental: Some(switches),
        ..
    } = daemon.handle(Request::Ping).await
    else {
        panic!("ping reports the switches");
    };
    assert!(switches.reload && !switches.secret_input && !switches.native_codex);
    assert!(reload::enabled(dir.path()));
    std::fs::write(
        dir.path().join(agentdocker_core::config::FILE_NAME),
        "[experimental]\nsecret_input = true\n",
    )
    .unwrap();
    let Response::Pong {
        experimental: Some(switches),
        ..
    } = daemon.handle(Request::Ping).await
    else {
        panic!("ping reports the switches");
    };
    assert!(
        switches.secret_input && switches.native_codex,
        "the file is read as it stands"
    );
}

/// A spec that says where it is working.
///
/// Identity compares the physical checkout and will not match two
/// agents that cannot say where they are, so a fixture standing in
/// for a real session has to have one — a session always does.
fn spec_here(name: &str) -> AgentSpec {
    AgentSpec {
        workdir: Some(std::env::temp_dir()),
        ..spec(name)
    }
}

/// One process is one agent, however many of its halves register.
///
/// Claude Code registers twice for a single session — its hooks
/// adapter under the session id, and the MCP server it launches
/// under the pid — and both give the same pid. Before this, one
/// session showed up as two agents: the MCP-side row never left
/// idle, because activity arrives through hooks, and a message sent
/// to one half never reached the other.
#[tokio::test]
async fn two_halves_of_one_session_register_as_one_agent() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let me = std::process::id();

    let hooks = register_here(&daemon, "claude-63bc3dca", Some(me)).await;
    let mcp = register_here(&daemon, "claude-code-4242", Some(me)).await;
    assert_eq!(mcp.id, hooks.id, "the second half is the first half");
    assert_eq!(
        mcp.spec.name, "claude-63bc3dca",
        "and keeps the name the first half registered"
    );
    assert_eq!(
        lock(&daemon.state).registry.live().count(),
        1,
        "one process, one row"
    );

    // A different process is a different agent, and an agent with no
    // pid at all — the human — is never folded into one.
    let other = register_here(&daemon, "codex-4343", Some(1)).await;
    assert_ne!(other.id, hooks.id);
    let human = register(&daemon, "user", None).await;
    let also_human = register(&daemon, "someone-else", None).await;
    assert_ne!(
        human.id, also_human.id,
        "no pid is not the same pid; two people are two agents"
    );
}

/// Two spellings of one directory are one checkout.
///
/// Comparing raw paths let `/var/...` and `/private/var/...` — one
/// directory, two spellings — look like two places to work. The
/// alias is made here rather than borrowed from the platform,
/// because the system temp directory is reached through a symlink on
/// macOS and directly on Linux, and a test that relied on that would
/// assert something true on one machine and false on another.
#[tokio::test]
async fn two_spellings_of_one_checkout_are_one_agent() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let me = std::process::id();
    let real = TempDir::new().unwrap();
    let alias = dir.path().join("alias-to-the-checkout");
    std::os::unix::fs::symlink(real.path(), &alias).unwrap();

    let register = async |name: &str, workdir: &std::path::Path| {
        let mut spec = spec(name);
        spec.workdir = Some(workdir.to_path_buf());
        match daemon
            .handle(Request::Register {
                spec,
                pid: Some(me),
                session: None,
            })
            .await
        {
            Response::Agent { agent } => agent,
            other => panic!("unexpected {other:?}"),
        }
    };
    let first = register("claude-direct", real.path()).await;
    let through_alias = register("claude-aliased", &alias).await;
    assert_eq!(
        through_alias.id, first.id,
        "the same directory under another name is the same checkout"
    );
    assert_eq!(lock(&daemon.state).registry.live().count(), 1);
}

#[tokio::test]
async fn registration_refuses_unresolved_or_non_directory_workdirs_without_state_changes() {
    let directory = TempDir::new().unwrap();
    let daemon = open(&directory);
    let fixture = TempDir::new().unwrap();
    let file = fixture.path().join("file");
    std::fs::write(&file, "fixture").unwrap();
    let cycle = fixture.path().join("cycle");
    std::os::unix::fs::symlink(&cycle, &cycle).unwrap();
    let before = daemon.recent_events(100);
    for workdir in [fixture.path().join("missing"), file, cycle] {
        let response = daemon
            .handle(Request::Register {
                spec: AgentSpec {
                    workdir: Some(workdir),
                    ..spec("refused")
                },
                pid: Some(std::process::id()),
                session: None,
            })
            .await;
        assert!(
            matches!(
                response,
                Response::Error {
                    code: ErrorCode::Invalid,
                    ..
                }
            ),
            "{response:?}"
        );
        let state = lock(&daemon.state);
        assert_eq!(state.registry.live().count(), 0);
        assert!(state.store.load_agents().unwrap().is_empty());
        drop(state);
        assert_eq!(daemon.recent_events(100), before);
    }
}

/// Two checkouts of one project are two agents.
///
/// Asserted against the predicate directly, with records that agree
/// on everything else including the project id. Two unrelated
/// temporary directories would have been rejected by the project
/// comparison before the checkout clause was ever reached, so a test
/// built that way passes whether or not the clause exists.
#[tokio::test]
async fn one_project_in_two_checkouts_is_two_agents() {
    let me = std::process::id();
    let born = procinfo::start_time(me);
    let one_project = ProjectRef::directory("/somewhere/that/is/one/project");
    let in_tree = |tree: &str| {
        let mut record = AgentRecord::new(spec("half"), false, Utc::now());
        record.spec.workdir = Some(PathBuf::from(tree));
        record.project = Some(one_project.clone());
        record.pid = Some(me);
        record.process_started_at = born;
        record.status = AgentStatus::Running;
        record
    };
    let main_checkout = in_tree("/repo");
    let worktree = in_tree("/repo-worktree");
    assert_eq!(
        main_checkout.project.as_ref().map(ProjectRef::id),
        worktree.project.as_ref().map(ProjectRef::id),
        "one project, which is exactly why the project alone is not enough"
    );
    assert!(
        !same_agent(&main_checkout, &worktree),
        "a linked worktree is another place to work"
    );
    // And the same tree really is the same agent, so the clause is
    // not simply refusing everything.
    assert!(same_agent(&main_checkout, &in_tree("/repo")));
}

/// Two sessions in one process do not collapse into one agent.
///
/// "Absent on one side is not a mismatch" is what lets an MCP-first
/// record be joined by its hooks half, and on its own it is not
/// transitive: the record still named no session afterwards, so the
/// *next* session in the same process matched it too and adopted
/// the same identity. Learning the first verified session id is
/// what closes it.
#[tokio::test]
async fn a_record_learns_its_session_so_the_next_one_cannot_take_it() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let me = std::process::id();
    let with_session = |name: &str, session: Option<&str>| {
        let mut spec = spec_here(name);
        if let Some(session) = session {
            spec.labels
                .insert("session_id".to_owned(), session.to_owned());
        }
        spec
    };
    let register = async |spec: AgentSpec| match daemon
        .handle(Request::Register {
            spec,
            pid: Some(me),
            session: None,
        })
        .await
    {
        Response::Agent { agent } => agent,
        other => panic!("unexpected {other:?}"),
    };

    // MCP first, naming no session. Then session A joins it.
    let mcp = register(with_session("claude-code-4242", None)).await;
    let first = register(with_session("claude-aaaa", Some("session-a"))).await;
    assert_eq!(first.id, mcp.id, "the hooks half joins the MCP record");
    assert_eq!(
        first.spec.labels.get("session_id").map(String::as_str),
        Some("session-a"),
        "and the record learns whose session it is"
    );

    // A second session in the same process is a second agent.
    let second = register(with_session("claude-bbbb", Some("session-b"))).await;
    assert_ne!(
        second.id, mcp.id,
        "a different session must not inherit the first one's identity"
    );
    assert_eq!(lock(&daemon.state).registry.live().count(), 2);

    // And session A rejoining still finds its own.
    let again = register(with_session("claude-aaaa", Some("session-a"))).await;
    assert_eq!(again.id, first.id);
}

/// One identity survives both halves registering, and keeps what it
/// was given.
///
/// Named for what it proves. It drives the daemon through the
/// registration order and asserts the record keeps its lease and its
/// inbox — it does **not** run `McpServer::shutdown`, which lives in
/// another crate and talks over a socket. That end-to-end trial
/// against the real adapter is still owed and is not claimed here;
/// what `shutdown` decides is covered separately, by unit test, in
/// `mcp.rs`.
#[tokio::test]
async fn one_identity_survives_both_halves_and_keeps_what_it_was_given() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let me = std::process::id();
    let register = async |name: &str, session: Option<&str>| {
        let mut spec = spec_here(name);
        spec.labels.insert("via".to_owned(), "mcp".to_owned());
        if let Some(session) = session {
            spec.labels
                .insert("session_id".to_owned(), session.to_owned());
        }
        match daemon
            .handle(Request::Register {
                spec,
                pid: Some(me),
                session: None,
            })
            .await
        {
            Response::Agent { agent } => agent,
            other => panic!("unexpected {other:?}"),
        }
    };

    // MCP registers, hooks joins the same record, and the session
    // takes a lease and is sent something.
    let mcp = register("claude-code-4242", None).await;
    let hooks = register("claude-63bc3dca", Some("session-a")).await;
    assert_eq!(hooks.id, mcp.id);
    // The name that survives is the first registrar's, so the hooks
    // half is answered as `claude-code-4242` however it asked. That
    // is a race and it is deliberately not settled here; what
    // matters is that there is one identity, and the id is it.
    assert_eq!(hooks.spec.name, "claude-code-4242");
    assert!(matches!(
        claim(&daemon, &hooks.id.to_string(), "task:in-progress").await,
        Response::Lease { .. }
    ));

    // Something is sent to it, so there is state that a wrongly
    // timed deregistration would strand.
    let delivered = daemon
        .handle(Request::Send {
            from: hooks.id.to_string(),
            to: mcp.id.to_string(),
            kind: "chat".into(),
            payload: json!({"text": "still here?"}),
            reply_to: None,
            links: Vec::new(),
        })
        .await;
    assert!(matches!(delivered, Response::Sent { .. }), "{delivered:?}");

    assert!(daemon.is_live(&mcp.id), "one identity, still running");
    assert_eq!(
        lock(&daemon.state).leases.by_holder(&mcp.id).len(),
        1,
        "still holding what it took"
    );
    assert_eq!(
        lock(&daemon.state)
            .inboxes
            .get(&mcp.id)
            .map_or(0, VecDeque::len),
        1,
        "and still holding what it was sent"
    );

    // Only the process ending ends the agent.
    let ended = lock(&daemon.state).deregister(&hooks.id.to_string());
    let _ = &ended;
    assert!(matches!(ended, Response::Agent { .. }));
    assert!(!daemon.is_live(&mcp.id));
}

/// A registration that names no session cannot pick between two.
///
/// Absent-on-one-side is what lets an MCP server join its hooks
/// half. Once a process holds two sessions there are two candidates
/// and nothing to tell them apart, and taking the first the registry
/// yields would hand the same transport a different identity from
/// one run to the next.
#[tokio::test]
async fn an_unnamed_registration_refuses_to_guess_between_two_sessions() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let me = std::process::id();
    let register = async |name: &str, session: Option<&str>| {
        let mut spec = spec_here(name);
        if let Some(session) = session {
            spec.labels
                .insert("session_id".to_owned(), session.to_owned());
        }
        daemon
            .handle(Request::Register {
                spec,
                pid: Some(me),
                session: None,
            })
            .await
    };
    let named = async |r: Response| match r {
        Response::Agent { agent } => agent,
        other => panic!("unexpected {other:?}"),
    };
    let a = named(register("claude-aaaa", Some("session-a")).await).await;
    let b = named(register("claude-bbbb", Some("session-b")).await).await;
    assert_ne!(a.id, b.id);

    // Refused outright. Giving it a record of its own would be
    // giving this process a third sessionless wildcard — one that
    // matches every future session here and breeds more duplicates.
    let refused = register("claude-code-4242", None).await;
    let Response::Error { code, message, .. } = refused else {
        panic!("a registration that cannot be placed must not be placed anyway")
    };
    assert_eq!(code, ErrorCode::Invalid);
    assert!(message.contains("session_id"), "{message}");
    assert_eq!(
        lock(&daemon.state).registry.live().count(),
        2,
        "and no third record was created"
    );

    // The named sessions are untouched by the refusal and still
    // resolve to themselves.
    let Response::Agent { agent: a_again } = register("claude-aaaa", Some("session-a")).await
    else {
        panic!("a named session still knows which it is")
    };
    assert_eq!(a_again.id, a.id);
    let Response::Agent { agent: b_again } = register("claude-bbbb", Some("session-b")).await
    else {
        panic!("and so does the other")
    };
    assert_eq!(b_again.id, b.id);
    assert_eq!(lock(&daemon.state).registry.live().count(), 2);
}

/// Learning a session is written down before it is believed.
#[tokio::test]
async fn managed_registration_binds_the_session_and_preserves_the_owner() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let checkout = dir.path().join("checkout");
    std::fs::create_dir(&checkout).unwrap();
    let alias = dir.path().join("checkout-alias");
    std::os::unix::fs::symlink(&checkout, &alias).unwrap();
    let mut command = spec("managed-claude-binding");
    command.runtime = "claude-code".into();
    // A direct protocol caller need not have normalized its spelling.
    command.workdir = Some(alias.clone());
    command.command = vec!["sh".into(), "-c".into(), "sleep 30".into()];
    let Response::Agent { agent: original } = daemon.handle(Request::Run { spec: command }).await
    else {
        panic!("managed launch failed");
    };
    assert!(original.managed && original.owner.is_some());
    if original.spec.workdir.as_ref() != Some(&checkout.canonicalize().unwrap()) {
        daemon.stop_all().await;
        panic!("managed launch did not record its physical checkout");
    }
    let mut spec = original.spec.clone();
    spec.labels
        .insert("session_id".into(), "managed-session".into());
    for workdir in [Some(alias), original.spec.workdir.clone()] {
        let mut registration = spec.clone();
        registration.workdir = workdir;
        let response = daemon
            .handle(Request::Register {
                spec: registration,
                pid: original.pid,
                session: None,
            })
            .await;
        let Response::Agent { agent } = response else {
            daemon.stop_all().await;
            panic!("managed re-registration failed: {response:?}");
        };
        assert_eq!(agent.id, original.id);
        assert_eq!(agent.spec, spec);
        assert_eq!(agent.pid, original.pid);
        assert_eq!(agent.process_started_at, original.process_started_at);
        assert_eq!(agent.owner, original.owner);
        assert!(agent.managed && agent.status.is_live());
    }
    assert_eq!(lock(&daemon.state).registry.live().count(), 1);
    let events = lock(&daemon.state).store.recent_events(50).unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(&event.kind,
        EventKind::AgentSessionBound { agent, session }
        if *agent == original.id && session == "managed-session"))
            .count(),
        1
    );
    daemon.stop_all().await;
    drop(daemon);
    let reopened = open(&dir);
    let state = lock(&reopened.state);
    let saved = state.registry.get(&original.id).unwrap();
    assert_eq!(
        saved.spec.labels.get("session_id").map(String::as_str),
        Some("managed-session")
    );
    assert!(saved.managed);
}

#[tokio::test]
async fn launch_refuses_unresolved_workdirs_before_creating_agents_or_processes() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let file = dir.path().join("file");
    std::fs::write(&file, "fixture").unwrap();
    let cycle = dir.path().join("cycle");
    std::os::unix::fs::symlink(&cycle, &cycle).unwrap();
    let marker = dir.path().join("unexpected-child");
    let before = daemon.recent_events(100);
    for workdir in [dir.path().join("missing"), file, cycle] {
        let response = daemon
            .handle(Request::Run {
                spec: AgentSpec {
                    workdir: Some(workdir),
                    command: vec![
                        "sh".into(),
                        "-c".into(),
                        "touch \"$1\"".into(),
                        "fixture".into(),
                        marker.to_string_lossy().into_owned(),
                    ],
                    ..spec("refused-launch")
                },
            })
            .await;
        assert!(
            matches!(
                response,
                Response::Error {
                    code: ErrorCode::Invalid,
                    ..
                }
            ),
            "{response:?}"
        );
        let state = lock(&daemon.state);
        assert_eq!(state.registry.live().count(), 0);
        assert!(state.store.load_agents().unwrap().is_empty());
        drop(state);
        assert_eq!(daemon.recent_events(100), before);
        assert!(!marker.exists());
    }
}

/// Learning a session is written down before it is believed.
#[tokio::test]
async fn a_bound_session_is_stored_and_announced_before_it_is_answered() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let me = std::process::id();
    let register = async |name: &str, session: Option<&str>| {
        let mut spec = spec_here(name);
        if let Some(session) = session {
            spec.labels
                .insert("session_id".to_owned(), session.to_owned());
        }
        daemon
            .handle(Request::Register {
                spec,
                pid: Some(me),
                session: None,
            })
            .await
    };
    register("claude-code-4242", None).await;
    let joined = register("claude-aaaa", Some("session-a")).await;
    let Response::Agent { agent } = joined else {
        panic!("{joined:?}")
    };

    // On disk, not only in memory: a restart must not forget whose
    // session this was and let the next one adopt it.
    drop(daemon);
    let daemon = open(&dir);
    let reopened = lock(&daemon.state)
        .registry
        .get(&agent.id)
        .cloned()
        .unwrap();
    assert_eq!(
        reopened.spec.labels.get("session_id").map(String::as_str),
        Some("session-a")
    );
    // And it was announced, because every state change is.
    assert!(
        lock(&daemon.state)
            .store
            .recent_events(50)
            .unwrap()
            .iter()
            .any(
                |e| matches!(&e.kind, EventKind::AgentSessionBound { session, .. }
                if session == "session-a")
            ),
        "binding a session is a state change and says so"
    );
}

/// A session that comes back as a new process is the record it ended
/// with: same id, the new pid on it, every queue the session left —
/// the record that ended last stays, an earlier life's and the
/// newcomer's fold into it, in order, a broadcast that reached two
/// lives once — a question an earlier life asked now its own to
/// cancel, an alias that pointed at an earlier life pointed at it,
/// its cursor where it was, all of it durable across a reopen. A
/// record whose process is still there is not resumed.
#[tokio::test]
async fn a_session_that_comes_back_takes_up_its_ended_record() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let sleeper = || {
        std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap()
    };
    let (mut p0, mut p1, mut p2, mut p3) = (sleeper(), sleeper(), sleeper(), sleeper());
    let register = async |daemon: &Arc<Daemon>, name: &str, session: Option<&str>, pid: u32| {
        let mut spec = spec_here(name);
        spec.runtime = "claude-code".to_owned();
        if let Some(session) = session {
            spec.labels
                .insert("session_id".to_owned(), session.to_owned());
        }
        match daemon
            .handle(Request::Register {
                spec,
                pid: Some(pid),
                session: None,
            })
            .await
        {
            Response::Agent { agent } => agent,
            other => panic!("{other:?}"),
        }
    };
    let queue = async |daemon: &Arc<Daemon>, to: &str| match send(daemon, "peer", to).await {
        Response::Sent { message, .. } => message,
        other => panic!("{other:?}"),
    };
    let ended = |daemon: &Arc<Daemon>, id: &AgentId, minutes_ago: i64| {
        let mut state = lock(&daemon.state);
        state.mark_exited(id, AgentStatus::Exited { code: Some(0) });
        let record = state.registry.get_mut(id).unwrap();
        record.finished_at = Some(Utc::now() - chrono::Duration::minutes(minutes_ago));
    };
    let human = me(&daemon).await;
    let peer = register_here(&daemon, "peer", None).await;

    // The oldest life ends for good; the next life takes up its
    // record, so the newcomer's own id becomes an alias of it.
    let oldest = register(&daemon, "claude-aaaa", Some("session-a"), p0.id()).await;
    p0.kill().unwrap();
    p0.wait().unwrap();
    ended(&daemon, &oldest.id, 120);
    let earlier = register(&daemon, "claude-aaaa", Some("session-a"), p1.id()).await;
    assert_eq!(earlier.id, oldest.id, "the oldest life's record, taken up");
    let alias_of_earlier = lock(&daemon.state)
        .registry
        .aliases()
        .iter()
        .find(|(_, target)| **target == earlier.id)
        .map(|(retired, _)| retired.clone())
        .expect("the newcomer's id is an alias");
    // While that life runs, another process names the session (under
    // a name of its own — a live name is taken): it is not the return
    // of anything and gets a record of its own. A broadcast then
    // reaches both lives, each life gets words of its own, and the
    // earlier one asks the person something.
    let first = register(&daemon, "claude-bbbb", Some("session-a"), p2.id()).await;
    assert_ne!(first.id, earlier.id, "the other process is still there");
    assert_eq!(
        register(&daemon, "claude-code-1", None, p2.id()).await.id,
        first.id,
        "the MCP half joins its hooks half as ever"
    );
    let broadcast = queue(&daemon, "all").await;
    let m0 = queue(&daemon, earlier.id.as_str()).await;
    let m1 = queue(&daemon, first.id.as_str()).await;
    let Response::Sent {
        message: question, ..
    } = daemon
        .handle(Request::PostQuestion {
            from: earlier.id.to_string(),
            to: human.id.to_string(),
            question: "still there?".to_owned(),
            presentation: None,
            timeout_secs: 600,
        })
        .await
    else {
        panic!("the earlier life asks")
    };
    {
        let state = lock(&daemon.state);
        for id in [&earlier.id, &first.id] {
            assert!(
                state.inboxes[id].iter().any(|m| m.id == broadcast),
                "the broadcast reached {id}"
            );
        }
    }
    // Real retained observations in both ended lives must follow the
    // session too, without making its original queue ineligible.
    let earlier_read = agentdocker_core::ReadMark {
        path: "/fixture/earlier.rs".into(),
        version: "earlier-version".into(),
        head: None,
        at: Utc::now(),
    };
    let later_read = agentdocker_core::ReadMark {
        path: "/fixture/later.rs".into(),
        version: "later-version".into(),
        head: None,
        at: Utc::now(),
    };
    {
        let state = lock(&daemon.state);
        state
            .store
            .put_document("reads", earlier.id.as_str(), &vec![earlier_read.clone()])
            .unwrap();
        state
            .store
            .put_document("reads", first.id.as_str(), &vec![later_read.clone()])
            .unwrap();
    }
    let room = agentdocker_core::Channel {
        id: "resume-room".into(),
        project: earlier.project.as_ref().unwrap().id(),
        name: Some("resume-room".into()),
        subject: agentdocker_core::channel::ChannelSubject::Task {
            task: "retained room".into(),
        },
        members: vec![earlier.id.clone(), first.id.clone(), peer.id.clone()],
        opened_by: Some(earlier.id.clone()),
        opened_at: Utc::now(),
        reviews: vec![],
        closed_at: None,
        resolution: None,
        role: None,
    };
    {
        let mut state = lock(&daemon.state);
        state
            .store
            .put_document("channel", room.id.as_str(), &room)
            .unwrap();
        state.channels.insert(room.id.clone(), room.clone());
    }
    // Both lives end, the earlier one first, and their processes go.
    ended(&daemon, &earlier.id, 60);
    p1.kill().unwrap();
    p1.wait().unwrap();
    ended(&daemon, &first.id, 1);
    p2.kill().unwrap();
    p2.wait().unwrap();

    // The session's next life takes up the record that ended last;
    // the earlier life and the newcomer fold into it.
    let resumed = register(&daemon, "claude-aaaa", Some("session-a"), p3.id()).await;
    assert_eq!(resumed.id, first.id, "the record it ended with");
    assert_eq!(resumed.pid, Some(p3.id()));
    assert!(resumed.status.is_live());
    assert_eq!(resumed.finished_at, None);
    let m2 = queue(&daemon, first.id.as_str()).await;
    let fresh_id = {
        let state = lock(&daemon.state);
        let retired: Vec<AgentId> = state
            .registry
            .aliases()
            .iter()
            .filter(|(_, target)| **target == first.id)
            .map(|(retired, _)| retired.clone())
            .collect();
        assert_eq!(
            retired.len(),
            3,
            "the earlier life, its own alias, the newcomer: {retired:?}"
        );
        assert!(retired.contains(&earlier.id));
        assert!(
            retired.contains(&alias_of_earlier),
            "flat: the old alias points here now"
        );
        let fresh_id = retired
            .iter()
            .find(|id| **id != earlier.id && **id != alias_of_earlier)
            .unwrap()
            .clone();
        assert!(
            !state
                .registry
                .all()
                .any(|r| r.id == earlier.id || r.id == fresh_id),
            "the folded records are gone"
        );
        assert_eq!(
            state.registry.get(&earlier.id).map(|r| &r.id),
            Some(&first.id)
        );
        let queued: Vec<_> = state.inboxes[&first.id]
            .iter()
            .map(|m| m.id.clone())
            .collect();
        assert_eq!(
            queued,
            vec![broadcast.clone(), m0.clone(), m1.clone(), m2.clone()],
            "every queue, in the order the store holds, the broadcast once"
        );
        assert!(!state.inboxes.contains_key(&earlier.id));
        assert_eq!(state.questions[&question].from, first.id.to_string());
        fresh_id
    };
    assert!(
        daemon.recent_events(20).iter().any(|e| matches!(&e.kind,
            EventKind::SessionResumed { agent, retired, session, pid }
            if *agent == first.id && retired.len() == 2 && retired.contains(&fresh_id)
                && retired.contains(&earlier.id) && session == "session-a" && *pid == p3.id())),
        "announced, naming every retired record"
    );
    // Durable: reopened, the record has the new process, every alias
    // resolves, the queue is whole and the question is the record's
    // own to cancel.
    drop(daemon);
    let daemon = open(&dir);
    {
        let state = lock(&daemon.state);
        let record = state.registry.get(&first.id).unwrap();
        assert_eq!(record.pid, Some(p3.id()));
        for id in [&earlier.id, &alias_of_earlier, &fresh_id] {
            assert_eq!(
                state.registry.get(id).map(|r| &r.id),
                Some(&first.id),
                "{id}"
            );
        }
        let queued: Vec<_> = state.inboxes[&first.id]
            .iter()
            .map(|m| m.id.clone())
            .collect();
        assert_eq!(
            queued,
            vec![broadcast.clone(), m0.clone(), m1.clone(), m2.clone()],
            "the same queue in the same order after a reopen"
        );
        assert_eq!(
            state
                .store
                .document::<Vec<agentdocker_core::ReadMark>>("reads", first.id.as_str())
                .unwrap(),
            Some(vec![earlier_read, later_read])
        );
        assert!(
            state
                .store
                .document::<Vec<agentdocker_core::ReadMark>>("reads", earlier.id.as_str())
                .unwrap()
                .is_none()
        );
    }
    assert!(matches!(
        daemon
            .handle(Request::CancelQuestion {
                agent: first.id.to_string(),
                message: question.clone(),
            })
            .await,
        Response::Ok
    ));
    assert!(!lock(&daemon.state).questions.contains_key(&question));
    {
        let state = lock(&daemon.state);
        let channel = &state.channels[&room.id];
        assert!(channel.is_open());
        assert_eq!(channel.members, vec![first.id.clone(), peer.id.clone()]);
        assert_eq!(channel.opened_by, Some(first.id.clone()));
    }
    let channel_message = queue(&daemon, "channel:resume-room").await;
    {
        let state = lock(&daemon.state);
        assert_eq!(
            state.inboxes[&first.id]
                .iter()
                .filter(|m| m.id == channel_message)
                .count(),
            1
        );
        assert!(!state.inboxes.contains_key(&earlier.id));
        assert!(!state.inboxes.contains_key(&fresh_id));
        assert_eq!(
            state.store.load_inboxes().unwrap()[&first.id]
                .iter()
                .filter(|m| m.id == channel_message)
                .count(),
            1
        );
    }

    // The MCP half of the live session joins it as before.
    assert_eq!(
        register(&daemon, "claude-code-2", None, p3.id()).await.id,
        first.id
    );
    p3.kill().unwrap();
    p3.wait().unwrap();
}

#[test]
fn returning_session_read_errors_disable_coordination() {
    for malformed_canonical in [false, true] {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let now = Utc::now();
        let mut old = AgentRecord::new(spec_here("old"), false, now);
        old.spec.runtime = "claude-code".into();
        old.spec.labels.insert("session_id".into(), "same".into());
        old.project = Some(ProjectRef::directory(std::env::temp_dir()));
        old.status = AgentStatus::Exited { code: Some(0) };
        old.finished_at = Some(now);
        let mut fresh = old.clone();
        fresh.id = AgentId::generate();
        fresh.pid = Some(std::process::id());
        fresh.status = AgentStatus::Running;
        fresh.finished_at = None;
        let mut state = lock(&daemon.state);
        for record in [&old, &fresh] {
            state.registry.insert(record.clone()).unwrap();
            state.store.upsert_agent(record).unwrap();
        }
        state
            .store
            .put_document(
                "reads",
                if malformed_canonical {
                    old.id.as_str()
                } else {
                    fresh.id.as_str()
                },
                &json!("malformed"),
            )
            .unwrap();
        let seq = state.next_seq;
        assert_eq!(state.resume_session(fresh.clone()).id, fresh.id);
        assert!(
            state.storage_failure().is_some(),
            "the read error disables further coordination"
        );
        assert_eq!(state.registry.all().count(), 2);
        assert!(state.registry.aliases().is_empty());
        assert!(state.store.identity_aliases().unwrap().is_empty());
        assert_eq!(state.next_seq, seq);
    }
}

/// While coordination is being handed over, a session's return is not
/// folded: the write is skipped, memory stays as it was, no event is
/// recorded, and the fresh record stands. Once authority is back, the
/// same return folds.
#[tokio::test]
async fn a_fenced_daemon_leaves_a_returning_session_unfolded() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let mut old_life = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let mut new_life = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let register = async |name: &str, session: Option<&str>, pid: u32| {
        let mut spec = spec_here(name);
        spec.runtime = "claude-code".to_owned();
        if let Some(session) = session {
            spec.labels
                .insert("session_id".to_owned(), session.to_owned());
        }
        match daemon
            .handle(Request::Register {
                spec,
                pid: Some(pid),
                session: None,
            })
            .await
        {
            Response::Agent { agent } => agent,
            other => panic!("{other:?}"),
        }
    };
    let old = register("claude-aaaa", Some("session-a"), old_life.id()).await;
    old_life.kill().unwrap();
    old_life.wait().unwrap();
    lock(&daemon.state).mark_exited(&old.id, AgentStatus::Exited { code: Some(0) });
    // The new life's MCP half registers before the handover starts.
    let fresh = register("claude-code-9", None, new_life.id()).await;
    assert_ne!(fresh.id, old.id);
    let retained = agentdocker_core::ReadMark {
        path: "/fixture/retained.rs".into(),
        version: "retained".into(),
        head: None,
        at: Utc::now(),
    };
    lock(&daemon.state)
        .store
        .put_document("reads", fresh.id.as_str(), &vec![retained.clone()])
        .unwrap();
    daemon.offer_transfer(1).unwrap();
    let seq = lock(&daemon.state).next_seq;
    let mut named = fresh.clone();
    named
        .spec
        .labels
        .insert("session_id".to_owned(), "session-a".to_owned());
    {
        let mut state = lock(&daemon.state);
        let answered = state.resume_session(named.clone());
        assert_eq!(
            answered.id, fresh.id,
            "the fresh record stands while fenced"
        );
        assert!(
            state.registry.all().any(|r| r.id == old.id),
            "nothing folded"
        );
        assert!(state.registry.aliases().is_empty());
        assert_eq!(state.next_seq, seq, "no event while fenced");
        assert_eq!(
            state
                .store
                .document::<Vec<agentdocker_core::ReadMark>>("reads", fresh.id.as_str())
                .unwrap(),
            Some(vec![retained.clone()])
        );
        assert!(
            state
                .store
                .document::<Vec<agentdocker_core::ReadMark>>("reads", old.id.as_str())
                .unwrap()
                .is_none()
        );
        assert!(state.registry.get(&old.id).unwrap().finished_at.is_some());
    }
    assert!(daemon.abort_transfer("cleanup"));
    {
        let mut state = lock(&daemon.state);
        let seq = state.next_seq;
        let answered = state.resume_session(named);
        assert_eq!(answered.id, old.id, "folded once authority is back");
        assert_eq!(answered.pid, Some(new_life.id()));
        assert_eq!(state.registry.get(&fresh.id).map(|r| &r.id), Some(&old.id));
        assert_eq!(state.next_seq, seq + 1, "the fold's one event");
        assert_eq!(
            state
                .store
                .document::<Vec<agentdocker_core::ReadMark>>("reads", old.id.as_str())
                .unwrap(),
            Some(vec![retained])
        );
        assert!(
            state
                .store
                .document::<Vec<agentdocker_core::ReadMark>>("reads", fresh.id.as_str())
                .unwrap()
                .is_none()
        );
    }
    new_life.kill().unwrap();
    new_life.wait().unwrap();
}

/// A channel can initialize before SessionStart gives its registration a
/// session ID. Its already-offered head must not jump ahead of old backlog
/// when that later hook arrives. Keep both queues and their IDs intact.
#[tokio::test]
async fn an_initialized_receiver_is_not_folded_ahead_of_old_backlog() {
    use agentdocker_core::InputReport;
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let mut old_life = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let mut new_life = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let register = async |name: &str, session: Option<&str>, pid: u32| {
        let mut spec = spec_here(name);
        spec.runtime = "claude-code".into();
        if let Some(session) = session {
            spec.labels.insert("session_id".into(), session.into());
        }
        match daemon
            .handle(Request::Register {
                spec,
                pid: Some(pid),
                session: None,
            })
            .await
        {
            Response::Agent { agent } => agent,
            other => panic!("{other:?}"),
        }
    };
    let old = register("old-session", Some("session-a"), old_life.id()).await;
    let Response::Sent {
        message: old_message,
        ..
    } = send(&daemon, "peer", old.id.as_str()).await
    else {
        panic!("old queue")
    };
    old_life.kill().unwrap();
    old_life.wait().unwrap();
    lock(&daemon.state).mark_exited(&old.id, AgentStatus::Exited { code: Some(0) });
    let fresh = register("fresh-channel", None, new_life.id()).await;
    let Response::Sent {
        message: fresh_message,
        ..
    } = send(&daemon, "peer", fresh.id.as_str()).await
    else {
        panic!("fresh queue")
    };
    assert!(matches!(
        daemon
            .handle(Request::ReportInput {
                agent: fresh.id.to_string(),
                process_started_at: fresh.process_started_at.unwrap(),
                observed_at: Utc::now(),
                report: InputReport::Ready,
                token: None,
            })
            .await,
        Response::Ok
    ));
    let returned = register("session-hook", Some("session-a"), new_life.id()).await;
    // Retire fixtures before the assertions, including on a regression.
    new_life.kill().unwrap();
    new_life.wait().unwrap();
    assert_eq!(
        returned.id, fresh.id,
        "the initialized receiver keeps its identity"
    );
    assert!(returned.input_delivery.is_some());
    let state = lock(&daemon.state);
    assert!(state.registry.aliases().is_empty());
    assert_eq!(
        state.inboxes[&old.id]
            .iter()
            .map(|m| &m.id)
            .collect::<Vec<_>>(),
        [&old_message]
    );
    assert_eq!(
        state.inboxes[&fresh.id]
            .iter()
            .map(|m| &m.id)
            .collect::<Vec<_>>(),
        [&fresh_message]
    );
    assert!(
        !state
            .store
            .recent_events(10)
            .unwrap()
            .iter()
            .any(|event| matches!(event.kind, EventKind::SessionResumed { .. }))
    );
}

/// A binding that cannot be stored is not answered as if it were.
///
/// The row and the event announcing it are one transaction, so a
/// storage failure takes both or neither. What must never happen is
/// the third outcome: memory saying the record has a session, the
/// store saying it does not, and a restart picking whichever it
/// reads first — at which point the next session in this process
/// adopts an identity that is already spoken for.
///
/// This test is here because the claim that it was one transaction
/// was made before it was true, and nothing failed.
#[tokio::test]
async fn a_binding_that_cannot_be_stored_changes_nothing() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let me = std::process::id();
    let register = async |name: &str, session: Option<&str>| {
        let mut spec = spec_here(name);
        if let Some(session) = session {
            spec.labels
                .insert("session_id".to_owned(), session.to_owned());
        }
        daemon
            .handle(Request::Register {
                spec,
                pid: Some(me),
                session: None,
            })
            .await
    };
    let Response::Agent { agent: mcp } = register("claude-code-4242", None).await else {
        panic!("the first half registers")
    };
    let events_before = daemon.recent_events(200).len();

    // The event insert alone, not every write. That is the case
    // the old sequence got wrong: it wrote the row, failed on the
    // event, and moved memory on anyway. Rejecting everything
    // cannot tell that apart from taking neither.
    lock(&daemon.state)
        .store
        .reject_session_binding_event_for_test();
    let refused = register("claude-aaaa", Some("session-a")).await;
    assert!(
        matches!(
            refused,
            Response::Error {
                code: ErrorCode::StorageUnavailable,
                ..
            }
        ),
        "{refused:?}"
    );
    // The row must have gone back with the event. If the write had
    // been two commits, this is where the agent row would already
    // carry the session while the log knew nothing about it.
    assert!(
        lock(&daemon.state)
            .store
            .load_agents()
            .unwrap()
            .iter()
            .find(|a| a.id == mcp.id)
            .is_some_and(|a| !a.spec.labels.contains_key("session_id")),
        "the row was rolled back with the event it could not write"
    );

    // Memory did not move ahead of the store.
    assert!(
        lock(&daemon.state)
            .registry
            .get(&mcp.id)
            .is_some_and(|a| !a.spec.labels.contains_key("session_id")),
        "the binding was not published to memory"
    );
    assert_eq!(
        daemon.recent_events(200).len(),
        events_before,
        "and nothing was announced"
    );

    // Nor to disk: reopening finds the record exactly as it was, so
    // the next session in this process is still free to claim it.
    drop(daemon);
    let daemon = open(&dir);
    assert!(
        lock(&daemon.state)
            .registry
            .get(&mcp.id)
            .is_some_and(|a| !a.spec.labels.contains_key("session_id")),
        "and the store never took it either"
    );
}

/// An empty session label is an absent one.
///
/// A record carrying `session_id: ""` would otherwise count as
/// already knowing whose it is and never learn, leaving the wildcard
/// open for the next session in the process.
#[tokio::test]
async fn an_empty_session_label_is_treated_as_no_session_at_all() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let me = std::process::id();
    let register = async |name: &str, session: &str| {
        let mut spec = spec_here(name);
        spec.labels
            .insert("session_id".to_owned(), session.to_owned());
        match daemon
            .handle(Request::Register {
                spec,
                pid: Some(me),
                session: None,
            })
            .await
        {
            Response::Agent { agent } => agent,
            other => panic!("unexpected {other:?}"),
        }
    };
    let blank = register("claude-code-4242", "").await;
    let joined = register("claude-aaaa", "session-a").await;
    assert_eq!(joined.id, blank.id, "the empty label did not block joining");
    assert_eq!(
        joined.spec.labels.get("session_id").map(String::as_str),
        Some("session-a"),
        "and the empty label was replaced rather than kept"
    );
}

/// A registration for a session in a checkout with a marker, as a real
/// session's hooks and MCP server send it.
async fn register_session(
    daemon: &Arc<Daemon>,
    name: &str,
    workdir: &Path,
    session: Option<&str>,
) -> Response {
    let mut spec = spec(name);
    spec.runtime = "claude-code".to_owned();
    spec.workdir = Some(workdir.to_path_buf());
    if let Some(session) = session {
        spec.labels
            .insert("session_id".to_owned(), session.to_owned());
    }
    daemon
        .handle(Request::Register {
            spec,
            pid: Some(std::process::id()),
            session: None,
        })
        .await
}

/// One Claude Code process, one session. Its MCP server registered from
/// where the process was launched and its first hook joined it there;
/// then the session's Bash tool `cd`'d into a subdirectory, and Claude
/// Code reports that as every later hook's `cwd`. Identity required the
/// two halves to agree on the directory, so the next hook registered a
/// second agent for the same process and session — the `claude-c960824c`
/// beside `claude-code-4321`, and `claude-1606cc8b` beside
/// `claude-code-5678`, that made the desktop count three sessions
/// where there were two, and five where there was one.
#[tokio::test]
async fn a_hook_from_a_moved_shell_joins_its_sessions_record() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let checkout = TempDir::new().unwrap();
    std::fs::write(checkout.path().join("Agentfile.toml"), "").unwrap();
    let moved = checkout.path().join("artifacts").join("out");
    std::fs::create_dir_all(&moved).unwrap();
    let agent = |response: Response| match response {
        Response::Agent { agent } => agent,
        other => panic!("{other:?}"),
    };
    let mcp = agent(register_session(&daemon, "claude-code-4321", checkout.path(), None).await);
    let started = agent(
        register_session(
            &daemon,
            "claude-c960824c",
            checkout.path(),
            Some("c960824c"),
        )
        .await,
    );
    assert_eq!(started.id, mcp.id, "the hooks half joins at the launch");
    let moved_hook =
        agent(register_session(&daemon, "claude-c960824c", &moved, Some("c960824c")).await);
    assert_eq!(
        moved_hook.id, mcp.id,
        "a hook from a subdirectory is still the same session"
    );
    assert_eq!(
        moved_hook.spec.workdir, mcp.spec.workdir,
        "and the record keeps the session's checkout"
    );
    assert_eq!(lock(&daemon.state).registry.live().count(), 1);
    // A transport that names no session from another directory may
    // belong to any session here, so it is not this one; another
    // session in the same process is another agent, wherever it
    // registers from (here it joins that sessionless transport, the one
    // record in its directory that names no session yet).
    let sessionless = agent(register_session(&daemon, "claude-code-other", &moved, None).await);
    assert_ne!(sessionless.id, mcp.id);
    let other =
        agent(register_session(&daemon, "claude-0ther000", &moved, Some("another-session")).await);
    assert_ne!(other.id, mcp.id);
    assert_eq!(other.id, sessionless.id);
}

/// What a hook from a moved shell could leave before this rule: the MCP
/// server's record still names no session in the launch checkout, and
/// an old hook's record names the session in a subdirectory. A hook
/// from the launch checkout matches both; the record that names the
/// session is the session's and answers, however many transports beside
/// it name none, and they are neither joined nor taught the session,
/// which would make a second record of one session. With no record
/// naming its session, transports in its checkout that cannot be told
/// apart refuse the registration.
#[tokio::test]
async fn a_session_left_beside_a_sessionless_transport_is_answered_by_its_own_record() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let checkout = TempDir::new().unwrap();
    std::fs::write(checkout.path().join("Agentfile.toml"), "").unwrap();
    let moved = checkout.path().join("artifacts");
    std::fs::create_dir_all(&moved).unwrap();
    let agent = |response: Response| match response {
        Response::Agent { agent } => agent,
        other => panic!("{other:?}"),
    };
    let drifted =
        agent(register_session(&daemon, "claude-c960824c", &moved, Some("c960824c")).await);
    let mcp = agent(register_session(&daemon, "claude-code-4321", checkout.path(), None).await);
    assert_ne!(mcp.id, drifted.id);
    // A second transport in the same checkout naming no session, as only
    // an older daemon could have left it.
    let spare = {
        let mut record = mcp.clone();
        record.id = AgentId::generate();
        record.spec.name = "claude-code-spare".into();
        let mut state = lock(&daemon.state);
        state.store.upsert_agent(&record).unwrap();
        state.registry.insert(record.clone()).unwrap();
        record
    };
    let hook = agent(
        register_session(
            &daemon,
            "claude-c960824c",
            checkout.path(),
            Some("c960824c"),
        )
        .await,
    );
    assert_eq!(hook.id, drifted.id, "the record that names the session");
    {
        let state = lock(&daemon.state);
        for transport in [&mcp, &spare] {
            let record = state.registry.get(&transport.id).unwrap();
            assert_eq!(record.id, transport.id, "not folded");
            assert!(
                !record.spec.labels.contains_key("session_id"),
                "and taught nothing"
            );
        }
        assert_eq!(state.registry.live().count(), 3);
    }
    let response = register_session(
        &daemon,
        "claude-0ther000",
        checkout.path(),
        Some("0ther000"),
    )
    .await;
    assert!(
        matches!(&response, Response::Error { code: ErrorCode::Invalid, message, .. }
            if message.contains("cannot be told")),
        "{response:?}"
    );
}

/// One session counted twice, as the old rule could leave it: two live
/// records of one process and session, the second registered from a
/// moved shell. A registration of the session is answered by the record
/// that stays (`identity::keeper`: the first registered, here), and
/// nothing folds the other while both live; the offline identity repair
/// reconciles what was left.
#[tokio::test]
async fn one_sessions_existing_duplicate_is_answered_by_the_record_that_stays() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let checkout = TempDir::new().unwrap();
    std::fs::write(checkout.path().join("Agentfile.toml"), "").unwrap();
    let moved = checkout.path().join("artifacts").join("out");
    std::fs::create_dir_all(&moved).unwrap();
    let Response::Agent { agent: kept } = register_session(
        &daemon,
        "claude-code-4321",
        checkout.path(),
        Some("c960824c"),
    )
    .await
    else {
        panic!("the session registers")
    };
    // What a hook from the moved shell made before: another live record
    // for the same process and session, in the subdirectory. Built by
    // hand and stored, because registration no longer makes one.
    let drifted = {
        let mut record = AgentRecord::new(
            AgentSpec {
                name: "claude-c960824c".into(),
                runtime: "claude-code".into(),
                workdir: Some(moved.canonicalize().unwrap()),
                labels: BTreeMap::from([
                    ("session_id".into(), "c960824c".into()),
                    ("via".into(), "hook".into()),
                ]),
                ..AgentSpec::default()
            },
            false,
            kept.created_at + chrono::Duration::minutes(35),
        );
        record.project = kept.project.clone();
        record.pid = kept.pid;
        record.process_started_at = kept.process_started_at;
        record.status = AgentStatus::Running;
        let mut state = lock(&daemon.state);
        state.store.upsert_agent(&record).unwrap();
        state.registry.insert(record.clone()).unwrap();
        record
    };
    let Response::Agent { agent: answered } =
        register_session(&daemon, "claude-c960824c", &moved, Some("c960824c")).await
    else {
        panic!("the session's hook is answered")
    };
    assert_eq!(answered.id, kept.id);
    daemon.check_liveness();
    assert!(daemon.is_live(&kept.id) && daemon.is_live(&drifted.id));
    assert_eq!(
        lock(&daemon.state).registry.aliases().get(&drifted.id),
        None,
        "a live duplicate is not folded"
    );
}

/// Codex 0.160 runs a terminal session's MCP servers under its detached
/// app-server, and an MCP server from before that was understood
/// registered the app-server as a Codex session of its own beside the
/// real one. Registration refuses it by what the process is, on the
/// evidence of the process's own command line.
#[cfg(unix)]
#[tokio::test]
async fn codexs_detached_app_server_is_never_a_session() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    // A process whose command line is the app-server's: bash's `exec -a`
    // names it `codex`, and it only waits on its stdin. `read` is a
    // builtin, so no child outlives it holding the test's handles, and
    // dropping the pipe ends it even if an assertion fails first.
    let mut host = std::process::Command::new("bash")
        .arg("-c")
        .arg("exec -a codex bash -c 'read -r _' app-server --listen unix:// --managed-daemon")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let pid = host.id();
    // `exec -a` takes effect once the shell has replaced itself.
    let table_shows = || {
        procinfo::inspect(pid)
            .is_some_and(|process| procinfo::detached_codex_app_server(&process.argv))
    };
    for _ in 0..100 {
        if table_shows() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(table_shows(), "{:?}", procinfo::inspect(pid));
    let mut spec = spec_here("codex-7002");
    spec.runtime = "codex".into();
    let response = daemon
        .handle(Request::Register {
            spec,
            pid: Some(pid),
            session: None,
        })
        .await;
    assert!(
        matches!(&response, Response::Error { code: ErrorCode::Invalid, message, .. }
            if message.contains("detached app-server")),
        "{response:?}"
    );
    host.kill().unwrap();
    host.wait().unwrap();
}

/// Sharing a process is not the same as being the same agent.
///
/// Every one of these was raised in review by the Codex session
/// working alongside, and every one of them coalesced before it was.
#[tokio::test]
async fn one_process_is_not_one_agent_on_the_pid_alone() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let me = std::process::id();
    let mine = register_here(&daemon, "claude-63bc3dca", Some(me)).await;

    // A different runtime in the same process is a different agent:
    // a host that runs several kinds of session in one process would
    // otherwise have them all collapse into whichever arrived first.
    let elsewhere = {
        let mut spec = spec_here("codex-in-the-same-process");
        spec.runtime = "codex".to_owned();
        match daemon
            .handle(Request::Register {
                spec,
                pid: Some(me),
                session: None,
            })
            .await
        {
            Response::Agent { agent } => agent,
            other => panic!("unexpected {other:?}"),
        }
    };
    assert_ne!(elsewhere.id, mine.id, "a different runtime is not us");

    // And a birth time nobody could read is not evidence that two
    // records are the same process. `None == None` is not proof.
    let unreadable = |name: &str| {
        let mut record = AgentRecord::new(spec(name), false, Utc::now());
        record.pid = Some(424_242);
        record.process_started_at = None;
        record.status = AgentStatus::Running;
        record
    };
    let first = match lock(&daemon.state).insert_record(unreadable("ghost-one")) {
        Response::Agent { agent } => agent,
        other => panic!("unexpected {other:?}"),
    };
    let second = match lock(&daemon.state).insert_record(unreadable("ghost-two")) {
        Response::Agent { agent } => agent,
        other => panic!("unexpected {other:?}"),
    };
    assert_ne!(
        first.id, second.id,
        "two unreadable start times are not one process"
    );
}

/// A pid no other agent in this test is using.
///
/// One process is one agent now, so a fixture that handed several
/// agents the test process's own pid was describing something that
/// cannot happen — and got one agent back where it wanted three.
/// These tests want distinct agents, not live processes; the ones
/// that sweep for liveness ask for a real pid themselves.
fn distinct_pid() -> Option<u32> {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
    Some(NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
}

async fn register(daemon: &Arc<Daemon>, name: &str, pid: Option<u32>) -> AgentRecord {
    match daemon
        .handle(Request::Register {
            spec: spec(name),
            pid,
            session: None,
        })
        .await
    {
        Response::Agent { agent } => agent,
        other => panic!("unexpected {other:?}"),
    }
}

/// Reading its own queue is a session's contact, bound to its process:
/// one with no adapter that takes its messages itself is reachable.
#[tokio::test]
async fn reading_its_own_queue_is_a_sessions_contact() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let agent = register(&daemon, "poller", Some(child.id())).await;
    let born = agent.process_started_at.expect("a live process's birth");
    assert!(agent.queue_read.is_none());
    assert!(matches!(
        daemon
            .handle(Request::Inbox {
                agent: agent.id.to_string(),
                drain: false,
            })
            .await,
        Response::Messages { .. }
    ));
    let read = lock(&daemon.state)
        .registry
        .get(&agent.id)
        .unwrap()
        .queue_read
        .clone()
        .expect("the read is recorded");
    assert_eq!(read.process_started_at, born);
    let later = Utc::now() + agentdocker_core::input::FIRST_CONTACT_GRACE * 2;
    let record = lock(&daemon.state).registry.get(&agent.id).unwrap().clone();
    let _ = child.kill();
    let _ = child.wait();
    assert!(!agentdocker_core::input::unreachable(&record, later));
}

/// Register something that could be matched with another half.
///
/// Identity compares the physical checkout, so only a registration
/// that says where it is working can be. Kept separate from
/// `register` because giving every fixture a directory would put
/// them all in one project and change what the grouping and event
/// tests are looking at.
async fn register_here(daemon: &Arc<Daemon>, name: &str, pid: Option<u32>) -> AgentRecord {
    match daemon
        .handle(Request::Register {
            spec: spec_here(name),
            pid,
            session: None,
        })
        .await
    {
        Response::Agent { agent } => agent,
        other => panic!("unexpected {other:?}"),
    }
}

/// A Codex conversation nothing has bound attaches by its key: the
/// first attach makes its one record (no process, kept by contact), and
/// every later or racing one finds it. Another profile for the same
/// thread is refused; a record of the conversation with a process is
/// answered as it is; only a Codex conversation attaches this way.
#[tokio::test]
async fn a_conversation_attaches_by_its_key_to_one_record() {
    use agentdocker_core::AdapterKind;
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let checkout = TempDir::new().unwrap();
    let thread = "de4e3b54-48e1-7b9b-82cc-773c517fc9c5";
    let attach = |profile: &str, session: &str, runtime: &str| Request::SessionAttach {
        runtime: runtime.into(),
        profile: profile.into(),
        session: session.into(),
        workdir: checkout.path().to_path_buf(),
        adapter: AdapterKind::Mcp,
    };
    let Response::Agent { agent: first } =
        daemon.handle(attach("/p/.codex", thread, "codex")).await
    else {
        panic!("the first attach makes the record")
    };
    assert!(first.keyed_only() && first.pid.is_none() && first.status.is_live());
    assert_eq!(first.session_key().unwrap().session, thread);
    assert_eq!(
        first
            .spec
            .labels
            .get(agentdocker_core::agent::PROFILE_LABEL)
            .map(String::as_str),
        Some("/p/.codex")
    );
    let (a, b) = tokio::join!(
        daemon.handle(attach("/p/.codex", thread, "codex")),
        daemon.handle(attach("/p/.codex", thread, "codex"))
    );
    for response in [a, b] {
        let Response::Agent { agent } = response else {
            panic!("found, not made again")
        };
        assert_eq!(agent.id, first.id);
    }
    assert_eq!(
        lock(&daemon.state)
            .registry
            .live()
            .filter(|r| r.spec.labels.get("session_id").map(String::as_str) == Some(thread))
            .count(),
        1,
        "one conversation, one record"
    );
    assert!(matches!(
        daemon.handle(attach("/q/.codex", thread, "codex")).await,
        Response::Error {
            code: ErrorCode::Conflict,
            ..
        }
    ));
    assert!(matches!(
        daemon
            .handle(attach("/p/.codex", thread, "claude-code"))
            .await,
        Response::Error {
            code: ErrorCode::Invalid,
            ..
        }
    ));
    assert!(matches!(
        daemon
            .handle(attach("/p/.codex", "not plain; rm", "codex"))
            .await,
        Response::Error {
            code: ErrorCode::Invalid,
            ..
        }
    ));
    // A record of the thread that does not say its profile is no
    // wildcard: refused. One that says this profile, with a process, is
    // the conversation's record, answered as it is.
    let other = "a1b2c3d4-0000-7000-8000-000000000001";
    let register = |profile: Option<&str>| {
        let mut spec = spec("codex-terminal");
        spec.runtime = "codex".into();
        spec.labels.insert("session_id".into(), other.into());
        if let Some(profile) = profile {
            spec.labels.insert(
                agentdocker_core::agent::PROFILE_LABEL.into(),
                profile.into(),
            );
        }
        Request::Register {
            spec,
            pid: Some(std::process::id()),
            session: None,
        }
    };
    let Response::Agent { agent: unsaid } = daemon.handle(register(None)).await else {
        panic!("registered")
    };
    assert!(matches!(
        daemon.handle(attach("/p/.codex", other, "codex")).await,
        Response::Error {
            code: ErrorCode::Conflict,
            ..
        }
    ));
    assert!(matches!(
        daemon
            .handle(Request::Deregister {
                agent: unsaid.id.to_string()
            })
            .await,
        Response::Agent { .. }
    ));
    lock(&daemon.state).registry.remove(&unsaid.id);
    let Response::Agent { agent: terminal } = daemon.handle(register(Some("/p/.codex"))).await
    else {
        panic!("registered")
    };
    let Response::Agent { agent: answered } =
        daemon.handle(attach("/p/.codex", other, "codex")).await
    else {
        panic!("answered")
    };
    assert_eq!(answered.id, terminal.id);
    assert!(!answered.keyed_only());

    // Two threads that share a prefix are two records, each named.
    let twin = "de4e3b54-0000-7000-8000-000000000002";
    let Response::Agent { agent: second } = daemon.handle(attach("/p/.codex", twin, "codex")).await
    else {
        panic!("a second conversation attaches")
    };
    assert_ne!(second.id, first.id);
    assert_ne!(second.spec.name, first.spec.name);
}

/// The conversation's process, once proven, joins its keyed record:
/// the same id, its queue with it, never a second record. A retired
/// keyed record is the conversation's still when it is heard from
/// again. A binding that names the thread refuses the attach.
#[tokio::test]
async fn a_keyed_conversation_keeps_its_one_record_through_proof_and_retirement() {
    use agentdocker_core::AdapterKind;
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let checkout = TempDir::new().unwrap();
    let workdir = checkout.path().canonicalize().unwrap();
    let thread = "f00dcafe-0000-7000-8000-000000000003";
    let attach = || Request::SessionAttach {
        runtime: "codex".into(),
        profile: "/p/.codex".into(),
        session: thread.into(),
        workdir: workdir.clone(),
        adapter: AdapterKind::Hooks,
    };
    let Response::Agent { agent: keyed } = daemon.handle(attach()).await else {
        panic!("attached")
    };
    // Retired, then heard from again: the same record, live again.
    assert!(matches!(
        daemon
            .handle(Request::Deregister {
                agent: keyed.id.to_string()
            })
            .await,
        Response::Agent { .. }
    ));
    let Response::Agent { agent: again } = daemon.handle(attach()).await else {
        panic!("reattached")
    };
    assert_eq!(again.id, keyed.id);
    assert!(again.status.is_live() && again.keyed_only());
    // A message waits for it.
    let peer = register(&daemon, "peer", None).await;
    assert!(matches!(
        send(&daemon, peer.id.as_str(), keyed.id.as_str()).await,
        Response::Sent { .. }
    ));
    // Its terminal is proven (`codex resume <thread>` in its checkout):
    // it joins the record, the message with it.
    let mut spec = spec("codex-terminal");
    spec.runtime = "codex".into();
    spec.workdir = Some(workdir.clone());
    spec.labels.insert("session_id".into(), thread.into());
    spec.labels.insert(
        agentdocker_core::agent::PROFILE_LABEL.into(),
        "/p/.codex".into(),
    );
    let Response::Agent { agent: proven } = daemon
        .handle(Request::Register {
            spec,
            pid: Some(std::process::id()),
            session: None,
        })
        .await
    else {
        panic!("proven")
    };
    assert_eq!(proven.id, keyed.id, "the same record");
    assert_eq!(proven.pid, Some(std::process::id()));
    assert!(!proven.keyed_only() && proven.session_contact.is_none());
    assert_eq!(lock(&daemon.state).inboxes[&keyed.id].len(), 1);

    // A binding that names a conversation owns it: no attach.
    let bound_thread = "f00dcafe-0000-7000-8000-000000000004";
    {
        let now = Utc::now();
        let mut owner = AgentRecord::new(spec_for_binding(), true, now);
        owner.status = AgentStatus::Exited { code: None };
        owner.input_binding = Some(agentdocker_core::InputBinding {
            provider: agentdocker_core::ProviderGeneration {
                process: agentdocker_core::ProcessIdentity {
                    pid: 1,
                    started_at: now,
                },
                session: bound_thread.into(),
                profile: "/p/.codex".into(),
            },
            controller: agentdocker_core::ProcessIdentity {
                pid: 2,
                started_at: now,
            },
            controller_since: now,
            token_sha256: "fixture".into(),
            bound_at: now,
            controller_generations: 1,
            uncertain: Vec::new(),
            launch: None,
            restart: Default::default(),
        });
        let mut state = lock(&daemon.state);
        state.store.upsert_agent(&owner).unwrap();
        state.registry.insert(owner).unwrap();
    }
    assert!(matches!(
        daemon
            .handle(Request::SessionAttach {
                runtime: "codex".into(),
                profile: "/p/.codex".into(),
                session: bound_thread.into(),
                workdir,
                adapter: AdapterKind::Mcp,
            })
            .await,
        Response::Error {
            code: ErrorCode::Conflict,
            ..
        }
    ));
}

/// The transitions the second review found: a terminal that already
/// has its own (adopted) record refuses to be a second process record;
/// a record that learns its session learns the verified profile with
/// it; an ended record of the thread whose process exited is the
/// conversation's owner still; and a name another agent took meanwhile
/// is replaced, the id kept.
#[tokio::test]
async fn a_keyed_conversation_survives_adoption_process_exit_and_a_taken_name() {
    use agentdocker_core::AdapterKind;
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let checkout = TempDir::new().unwrap();
    let workdir = checkout.path().canonicalize().unwrap();
    let profile = "/p/.codex";
    let attach = |thread: &str| Request::SessionAttach {
        runtime: "codex".into(),
        profile: profile.into(),
        session: thread.into(),
        workdir: workdir.clone(),
        adapter: AdapterKind::Mcp,
    };
    let keyed_spec = |thread: Option<&str>, with_profile: bool| {
        let mut spec = spec("codex-terminal");
        spec.runtime = "codex".into();
        spec.workdir = Some(workdir.clone());
        if let Some(thread) = thread {
            spec.labels.insert("session_id".into(), thread.into());
        }
        if with_profile {
            spec.labels.insert(
                agentdocker_core::agent::PROFILE_LABEL.into(),
                profile.into(),
            );
        }
        spec
    };

    // Adopted before the proof: the terminal keeps its one record.
    let adopted_thread = "aa000000-0000-7000-8000-000000000001";
    let Response::Agent { agent: keyed } = daemon.handle(attach(adopted_thread)).await else {
        panic!("attached")
    };
    let mut adopted_spec = keyed_spec(None, false);
    adopted_spec.name = "codex-adopted".into();
    adopted_spec.labels.insert("adopted".into(), "true".into());
    let Response::Agent { agent: adopted } = daemon
        .handle(Request::Register {
            spec: adopted_spec,
            pid: Some(std::process::id()),
            session: None,
        })
        .await
    else {
        panic!("adopted")
    };
    let mut joining = keyed_spec(Some(adopted_thread), true);
    joining.name = "codex-adopted".into();
    assert!(matches!(
        daemon
            .handle(Request::Register {
                spec: joining,
                pid: Some(std::process::id()),
                session: None,
            })
            .await,
        Response::Error {
            code: ErrorCode::Conflict,
            ..
        }
    ));
    {
        let state = lock(&daemon.state);
        assert!(state.registry.get(&keyed.id).unwrap().keyed_only());
        assert!(state.registry.get(&adopted.id).unwrap().pid.is_some());
    }
    // A process record (no keyed record yet) learns session and profile
    // together, so a later attach under that profile finds it.
    let learned_thread = "aa000000-0000-7000-8000-000000000002";
    let Response::Agent { agent: learned } = daemon
        .handle(Request::Register {
            spec: keyed_spec(Some(learned_thread), true),
            pid: Some(std::process::id()),
            session: None,
        })
        .await
    else {
        panic!("joined")
    };
    assert_eq!(learned.id, adopted.id, "the adopted record learns it");
    assert_eq!(
        learned
            .spec
            .labels
            .get(agentdocker_core::agent::PROFILE_LABEL)
            .map(String::as_str),
        Some(profile)
    );
    let Response::Agent { agent: found } = daemon.handle(attach(learned_thread)).await else {
        panic!("found by its key")
    };
    assert_eq!(found.id, adopted.id);

    // Proven, then its process exits: the ended record is the owner.
    let exited_thread = "aa000000-0000-7000-8000-000000000003";
    let Response::Agent { agent: owner } = daemon.handle(attach(exited_thread)).await else {
        panic!("attached")
    };
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let Response::Agent { agent: proven } = daemon
        .handle(Request::Register {
            spec: keyed_spec(Some(exited_thread), true),
            pid: Some(child.id()),
            session: None,
        })
        .await
    else {
        panic!("proven")
    };
    assert_eq!(proven.id, owner.id);
    let _ = child.kill();
    let _ = child.wait();
    daemon.check_liveness();
    let first_end = {
        let state = lock(&daemon.state);
        let ended = state.registry.get(&owner.id).unwrap();
        assert!(!ended.status.is_live());
        ended.finished_at.expect("its exit recorded")
    };
    // Its name was taken meanwhile.
    let taken = owner.spec.name.clone();
    let mut other = spec(&taken);
    other.runtime = "custom".into();
    assert!(matches!(
        daemon
            .handle(Request::Register {
                spec: other,
                pid: None,
                session: None,
            })
            .await,
        Response::Agent { .. }
    ));
    let Response::Agent { agent: back } = daemon.handle(attach(exited_thread)).await else {
        panic!("the same conversation, back")
    };
    assert_eq!(back.id, owner.id, "its owner, not a second record");
    assert!(back.keyed_only() && back.status.is_live() && back.pid.is_none());
    assert_eq!(back.finished_at, None, "live again, so not finished");
    assert_ne!(back.spec.name, taken, "a name free among live agents");

    // Proven again and exited again: the second exit has its own time.
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let Response::Agent { agent: again } = daemon
        .handle(Request::Register {
            spec: keyed_spec(Some(exited_thread), true),
            pid: Some(child.id()),
            session: None,
        })
        .await
    else {
        panic!("proven again")
    };
    assert_eq!(again.id, owner.id);
    assert_eq!(again.finished_at, None);
    let _ = child.kill();
    let _ = child.wait();
    daemon.check_liveness();
    let second_end = lock(&daemon.state)
        .registry
        .get(&owner.id)
        .unwrap()
        .finished_at
        .expect("its second exit recorded");
    assert!(second_end > first_end, "the second exit, not the first");

    // A conversation whose only record ended under AgentDocker's
    // supervision, with no binding left, still has that owner: attach
    // refuses rather than making a second record.
    let supervised_thread = "aa000000-0000-7000-8000-000000000004";
    let mut supervised = register(&daemon, "codex-supervised", None).await;
    supervised.managed = true;
    supervised.spec.runtime = "codex".into();
    supervised.spec.workdir = Some(workdir.clone());
    supervised
        .spec
        .labels
        .insert("session_id".into(), supervised_thread.into());
    supervised.spec.labels.insert(
        agentdocker_core::agent::PROFILE_LABEL.into(),
        profile.into(),
    );
    supervised.status = AgentStatus::Exited { code: Some(0) };
    supervised.finished_at = Some(Utc::now());
    {
        let mut state = lock(&daemon.state);
        state.store.upsert_agent(&supervised).unwrap();
        *state.registry.get_mut(&supervised.id).unwrap() = supervised.clone();
    }
    let before = lock(&daemon.state).registry.all().count();
    assert!(matches!(
        daemon.handle(attach(supervised_thread)).await,
        Response::Error {
            code: ErrorCode::Conflict,
            ..
        }
    ));
    assert_eq!(
        lock(&daemon.state).registry.all().count(),
        before,
        "no second record"
    );
}

fn spec_for_binding() -> AgentSpec {
    AgentSpec {
        name: "codex-bound".into(),
        runtime: "codex".into(),
        ..AgentSpec::default()
    }
}

#[tokio::test]
async fn retention_forgets_finished_agents_that_hold_nothing() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let gone = register(&daemon, "gone", None).await;
    let holder = register(&daemon, "holder", None).await;
    let owed = register(&daemon, "owed", None).await;
    let live = register(&daemon, "live", None).await;
    for agent in [&gone, &holder, &owed] {
        assert!(matches!(
            daemon
                .handle(Request::Deregister {
                    agent: agent.id.to_string()
                })
                .await,
            Response::Agent { .. }
        ));
    }
    let inspect = |daemon: &Arc<Daemon>, id: &AgentId| lock(&daemon.state).inspect(id.as_str());
    // Fresh records stay, however they ended.
    daemon.apply_agent_retention();
    assert!(matches!(inspect(&daemon, &gone.id), Response::Agent { .. }));
    // Forty days on, the default window has passed for both finished
    // records; the one that is to come back under its id after a
    // restart stays (its leases went when it ended; what it holds is
    // its restore), and the live one stays.
    {
        let mut state = lock(&daemon.state);
        for id in [&gone.id, &holder.id, &owed.id] {
            let record = state.registry.get_mut(id).unwrap();
            record.finished_at = Some(Utc::now() - Duration::days(40));
            record.last_seen = Utc::now() - Duration::days(40);
        }
        state.registry.get_mut(&holder.id).unwrap().spec.restore = true;
        // A message nobody has delivered is owed to the record: a
        // session resumed under this id would still get it.
        state
            .inboxes
            .entry(owed.id.clone())
            .or_default()
            .push_back(Envelope::new(
                "user",
                Destination::Agent(owed.id.clone()),
                "chat",
                serde_json::json!({"text": "still waiting"}),
                None,
                Utc::now() - Duration::days(41),
            ));
    }
    daemon.apply_agent_retention();
    assert!(matches!(
        inspect(&daemon, &gone.id),
        Response::Error {
            code: ErrorCode::NotFound,
            ..
        }
    ));
    assert!(matches!(
        inspect(&daemon, &holder.id),
        Response::Agent { .. }
    ));
    assert!(
        matches!(inspect(&daemon, &owed.id), Response::Agent { .. }),
        "a record with a queued message is owed that message and stays"
    );
    assert!(matches!(inspect(&daemon, &live.id), Response::Agent { .. }));
    assert_eq!(
        daemon
            .recent_events(50)
            .iter()
            .filter(|e| matches!(e.kind, EventKind::AgentsPruned { removed: 1 }))
            .count(),
        1
    );
    // Its restore given up, `off` still keeps the record; a shorter
    // window forgets it.
    lock(&daemon.state)
        .registry
        .get_mut(&holder.id)
        .unwrap()
        .spec
        .restore = false;
    let config = daemon.home.join("agentd.toml");
    std::fs::write(&config, "[agents]\nretention = \"off\"\n").unwrap();
    daemon.apply_agent_retention();
    assert!(matches!(
        inspect(&daemon, &holder.id),
        Response::Agent { .. }
    ));
    std::fs::write(&config, "[agents]\nretention = \"7d\"\n").unwrap();
    daemon.apply_agent_retention();
    assert!(matches!(
        inspect(&daemon, &holder.id),
        Response::Error {
            code: ErrorCode::NotFound,
            ..
        }
    ));
    assert!(matches!(inspect(&daemon, &live.id), Response::Agent { .. }));
}

// ----- the human as an agent -----------------------------------------

async fn me(daemon: &Arc<Daemon>) -> AgentRecord {
    match daemon.handle(Request::Me { workdir: None }).await {
        Response::Agent { agent } => agent,
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test]
async fn me_is_idempotent_and_outlives_the_liveness_check() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);

    let first = me(&daemon).await;
    assert_eq!(first.spec.name, agentdocker_core::HUMAN);
    assert_eq!(first.spec.runtime, agentdocker_core::HUMAN_RUNTIME);
    assert_eq!(first.pid, None, "a person is not a process");

    let again = me(&daemon).await;
    assert_eq!(again.id, first.id, "the same person, not a second one");

    // A record with no pid has nothing for liveness to check, so the
    // human is still here after a sweep that reaps dead processes.
    daemon.check_liveness();
    assert!(daemon.is_live(&first.id));
}

/// A window started from a Dock or a launcher inherits `/` as its
/// working directory and reports it. Taking that at face value moved
/// the person out of the project they were in, and everything that
/// needs their checkout — `commit`, the journal digest — then had a
/// filesystem root to work with instead of a repository.
#[tokio::test]
async fn a_client_with_no_working_directory_does_not_move_the_human_to_nowhere() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let workdir = dir.path().to_path_buf();
    // What the daemon stores is the resolved path, because identity
    // compares physical checkouts and `/var/folders` and
    // `/private/var/folders` are one directory spelled two ways.
    let resolved = project::canonical(&workdir);

    let Response::Agent { agent } = daemon
        .handle(Request::Me {
            workdir: Some(workdir.clone()),
        })
        .await
    else {
        panic!("me failed")
    };
    assert_eq!(agent.spec.workdir, Some(resolved.clone()));

    // The same person, reported from nowhere.
    let Response::Agent { agent } = daemon
        .handle(Request::Me {
            workdir: Some(PathBuf::from("/")),
        })
        .await
    else {
        panic!("me failed")
    };
    assert_eq!(
        agent.spec.workdir,
        Some(resolved.clone()),
        "the record they had is kept, not replaced with the root"
    );

    // And an actual directory still moves them, which is the whole
    // point of `me` being idempotent rather than write-once.
    let elsewhere = dir.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let Response::Agent { agent } = daemon
        .handle(Request::Me {
            workdir: Some(elsewhere.clone()),
        })
        .await
    else {
        panic!("me failed")
    };
    assert_eq!(agent.spec.workdir, Some(elsewhere));
}

#[tokio::test]
async fn ask_returns_the_answer_that_names_it() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let human = me(&daemon).await;
    let asker = register(&daemon, "worker", distinct_pid()).await;

    let answering = {
        let daemon = daemon.clone();
        tokio::spawn(async move {
            // Wait for the question to be recorded, then answer it.
            let id = loop {
                let Response::Questions { questions } = daemon
                    .handle(Request::Questions {
                        agent: Some("user".to_owned()),
                    })
                    .await
                else {
                    panic!("questions did not answer with questions")
                };
                if let Some(question) = questions.first() {
                    assert_eq!(question.text, "ship it?");
                    break question.id.clone();
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            };
            daemon
                .handle(Request::Answer {
                    from: None,
                    message: id,
                    text: "yes".to_owned(),
                })
                .await
        })
    };

    let response = daemon
        .handle(Request::Ask {
            from: asker.id.to_string(),
            to: "user".to_owned(),
            question: "ship it?".to_owned(),
            timeout_secs: 10,
        })
        .await;
    let Response::Answer { from, text, .. } = response else {
        panic!("unexpected {response:?}")
    };
    assert_eq!(text, "yes");
    assert_eq!(from, human.id.to_string());
    answering.await.unwrap();

    // The question is no longer waiting for anyone.
    let Response::Questions { questions } = daemon.handle(Request::Questions { agent: None }).await
    else {
        panic!("questions did not answer with questions")
    };
    assert!(questions.is_empty(), "answered, so no longer open");

    // And the asker can still read the answer, because an answer is an
    // ordinary message as well as the end of a wait.
    let waiting = inbox(&daemon, asker.id.as_str(), true).await;
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].kind, "answer");
}

#[tokio::test]
async fn ask_times_out_rather_than_waiting_forever() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    me(&daemon).await;
    let asker = register(&daemon, "worker", distinct_pid()).await;

    let response = daemon
        .handle(Request::Ask {
            from: asker.id.to_string(),
            to: "user".to_owned(),
            question: "anyone there?".to_owned(),
            timeout_secs: 1,
        })
        .await;
    assert!(
        matches!(
            &response,
            Response::Error {
                code: ErrorCode::Timeout,
                ..
            }
        ),
        "unexpected {response:?}"
    );
    // Nobody is blocked on it any more, so it is not still open.
    let Response::Questions { questions } = daemon.handle(Request::Questions { agent: None }).await
    else {
        panic!("questions did not answer with questions")
    };
    assert!(questions.is_empty());
    // The question itself still reached the human's inbox: a timeout
    // is the asker giving up, not the message being withdrawn.
    let waiting = inbox(&daemon, "user", true).await;
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].kind, "question");
}

#[tokio::test]
async fn an_empty_question_is_refused_and_an_unknown_one_is_not_found() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    me(&daemon).await;

    let response = daemon
        .handle(Request::Ask {
            from: "user".to_owned(),
            to: "user".to_owned(),
            question: "   ".to_owned(),
            timeout_secs: 1,
        })
        .await;
    assert!(
        matches!(
            &response,
            Response::Error {
                code: ErrorCode::Invalid,
                ..
            }
        ),
        "unexpected {response:?}"
    );

    let response = daemon
        .handle(Request::Answer {
            from: None,
            message: MessageId::from("nosuchquestion".to_owned()),
            text: "hello?".to_owned(),
        })
        .await;
    assert!(
        matches!(
            &response,
            Response::Error {
                code: ErrorCode::NotFound,
                ..
            }
        ),
        "unexpected {response:?}"
    );
}

#[tokio::test]
async fn questions_are_listed_only_for_whoever_was_asked() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    me(&daemon).await;
    let alpha = register(&daemon, "alpha", distinct_pid()).await;
    let beta = register(&daemon, "beta", distinct_pid()).await;

    // Two questions in flight, to different agents. Neither is
    // answered, so both asks are still waiting when we look.
    let to_human = {
        let daemon = daemon.clone();
        let from = alpha.id.to_string();
        tokio::spawn(async move {
            daemon
                .handle(Request::Ask {
                    from,
                    to: "user".to_owned(),
                    question: "for the human".to_owned(),
                    timeout_secs: 30,
                })
                .await
        })
    };
    let to_beta = {
        let daemon = daemon.clone();
        let from = alpha.id.to_string();
        let beta = beta.id.to_string();
        tokio::spawn(async move {
            daemon
                .handle(Request::Ask {
                    from,
                    to: beta,
                    question: "for beta".to_owned(),
                    timeout_secs: 30,
                })
                .await
        })
    };

    let both = loop {
        let Response::Questions { questions } =
            daemon.handle(Request::Questions { agent: None }).await
        else {
            panic!("questions did not answer with questions")
        };
        if questions.len() == 2 {
            break questions;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    };
    assert!(both[0].asked_at >= both[1].asked_at, "newest first");

    let Response::Questions { questions } = daemon
        .handle(Request::Questions {
            agent: Some("user".to_owned()),
        })
        .await
    else {
        panic!("questions did not answer with questions")
    };
    assert_eq!(questions.len(), 1);
    assert_eq!(questions[0].text, "for the human");

    to_human.abort();
    to_beta.abort();
}

#[tokio::test]
async fn a_message_to_a_person_asks_for_a_notification_and_one_to_a_program_does_not() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let human = me(&daemon).await;
    let worker = register(&daemon, "worker", distinct_pid()).await;

    let (tx, mut notices) = mpsc::channel(8);
    lock(&daemon.state).notifier = Some(tx);

    daemon
        .handle(Request::Send {
            from: worker.id.to_string(),
            to: human.id.to_string(),
            kind: "chat".to_owned(),
            payload: json!({ "text": "look at this" }),
            reply_to: None,
            links: Vec::new(),
        })
        .await;
    let notice = notices.try_recv().expect("a person is worth interrupting");
    assert_eq!(notice.from, "worker", "by name, not by id");
    assert_eq!(notice.kind, "chat");
    assert_eq!(notice.text, "look at this");
    assert_eq!(notice.target.agent, worker.id);
    let Response::Messages { messages } = daemon
        .handle(Request::Inbox {
            agent: human.id.to_string(),
            drain: false,
        })
        .await
    else {
        panic!("inbox")
    };
    assert_eq!(notice.target.message, messages[0].id);
    assert!(notice.target.channel.is_none());

    daemon
        .handle(Request::Send {
            from: "user".to_owned(),
            to: worker.id.to_string(),
            kind: "chat".to_owned(),
            payload: json!({ "text": "carry on" }),
            reply_to: None,
            links: Vec::new(),
        })
        .await;
    assert!(
        notices.try_recv().is_err(),
        "a program has no desktop to interrupt"
    );
}

// ----- snapshot restore ----------------------------------------------

/// A daemon that stops and comes back on the same home, as a restart
/// looks from the store's point of view.
async fn restart(dir: &TempDir, daemon: Arc<Daemon>) -> Arc<Daemon> {
    daemon.stop_all().await;
    drop(daemon);
    let daemon = open(dir);
    daemon.restore_agents().await;
    daemon
}

/// An ended terminal session comes back as this daemon's own process
/// under its own record, on a real terminal: the same id, its queued
/// messages, the question it asked and the card it holds untouched,
/// now supervised. It is refused while its process lives — a pid
/// that is alive and cannot be dated counts as living — while
/// something is subscribed on it, for another checkout, and while it
/// is already back; two presses at once bring back one process; a
/// launch that fails leaves the record ended with its queue and a
/// later relaunch still works; a fenced store commits nothing.
#[tokio::test]
async fn an_ended_session_is_relaunched_under_its_own_record_with_its_queue() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let checkout = dir.path().join("checkout");
    std::fs::create_dir(&checkout).unwrap();
    let session = "5e55a0d1-7c3b-4f2e-9a61-0b4d2c8e7f13";
    // A session registered the way a terminal session is: by its
    // hooks, with its conversation id, in its checkout.
    let mut ended_spec = spec("claude-code-4242");
    ended_spec.runtime = "claude-code".into();
    ended_spec.workdir = Some(checkout.clone());
    ended_spec
        .labels
        .insert("session_id".into(), session.into());
    let Response::Agent { agent: ended } = daemon
        .handle(Request::Register {
            spec: ended_spec,
            pid: None,
            session: None,
        })
        .await
    else {
        panic!("registration failed")
    };
    let peer = register(&daemon, "peer", None).await;
    // What it left behind while live: a question it asked, a card it
    // pulled. Both are the record's, by its id, and stay so.
    let Response::Sent {
        message: question, ..
    } = daemon
        .handle(Request::PostQuestion {
            from: ended.id.to_string(),
            to: peer.id.to_string(),
            question: "still there?".to_owned(),
            presentation: None,
            timeout_secs: 600,
        })
        .await
    else {
        panic!("the session asks")
    };
    let Response::Task { task: card } = daemon
        .handle(Request::TaskCreate {
            from: peer.id.to_string(),
            project: Some(checkout.display().to_string()),
            title: "Reconnect".to_owned(),
            acceptance: "it is back".to_owned(),
            column: Some(agentdocker_core::Column::Ready),
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
        panic!("a card is filed")
    };
    let Response::TaskDetail { task: pulled, .. } = daemon
        .handle(Request::TaskPull {
            agent: ended.id.to_string(),
            task: Some(card.id.to_string()),
            take_over_from: None,
            project: None,
            board: None,
        })
        .await
    else {
        panic!("the session pulls the card")
    };
    assert_eq!(pulled.assignee.as_ref(), Some(&ended.id));
    // Something queued for it while it was away.
    let Response::Sent { message, .. } = daemon
        .handle(Request::Send {
            from: peer.id.to_string(),
            to: ended.id.to_string(),
            kind: "chat".into(),
            payload: json!({"text": "still for you"}),
            reply_to: None,
            links: Vec::new(),
        })
        .await
    else {
        panic!("send failed")
    };
    let relaunch = |command: Vec<&str>, workdir: PathBuf| AgentSpec {
        runtime: "claude-code".into(),
        command: command.into_iter().map(str::to_owned).collect(),
        workdir: Some(workdir),
        ..Default::default()
    };
    let good = || AgentSpec {
        tty: true,
        ..relaunch(
            vec!["sh", "-c", "sleep 30", "--resume", session],
            checkout.clone(),
        )
    };
    // Live: not relaunched.
    assert!(matches!(
        daemon
            .handle(Request::ResumeSession {
                agent: ended.id.to_string(),
                spec: good(),
            })
            .await,
        Response::Error {
            code: ErrorCode::Invalid,
            ..
        }
    ));
    daemon.mark_exited(&ended.id, AgentStatus::Exited { code: Some(0) });
    // The record says ended, but its pid is alive and cannot be dated:
    // that is its process until proven otherwise.
    {
        let mut state = lock(&daemon.state);
        let record = state.registry.get_mut(&ended.id).unwrap();
        record.pid = Some(std::process::id());
        record.process_started_at = None;
    }
    assert!(matches!(
        daemon
            .handle(Request::ResumeSession {
                agent: ended.id.to_string(),
                spec: good(),
            })
            .await,
        Response::Error {
            code: ErrorCode::Conflict,
            ..
        }
    ));
    lock(&daemon.state).registry.get_mut(&ended.id).unwrap().pid = None;
    // Another checkout: not its own.
    let elsewhere = dir.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    assert!(matches!(
        daemon
            .handle(Request::ResumeSession {
                agent: ended.id.to_string(),
                spec: relaunch(vec!["sh", "-c", "sleep 30", "--resume", session], elsewhere),
            })
            .await,
        Response::Error {
            code: ErrorCode::Invalid,
            ..
        }
    ));
    // A subscriber still on the record: refused until it goes.
    {
        let (subscription, _receiver) = daemon
            .subscribe(Some(ended.id.as_str()), Vec::new())
            .unwrap();
        assert!(matches!(
            daemon
                .handle(Request::ResumeSession {
                    agent: ended.id.to_string(),
                    spec: good(),
                })
                .await,
            Response::Error {
                code: ErrorCode::Conflict,
                ..
            }
        ));
        drop(subscription);
    }
    // A launch that dies at once: whether it is refused or ends right
    // after starting, the record ends again with its queue kept and
    // nothing living under it.
    let _ = daemon
        .handle(Request::ResumeSession {
            agent: ended.id.to_string(),
            spec: relaunch(
                vec!["sh", "-c", "exit 3", "--resume", session],
                checkout.clone(),
            ),
        })
        .await;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while lock(&daemon.state)
        .registry
        .get(&ended.id)
        .is_some_and(|r| r.status.is_live())
        && std::time::Instant::now() < deadline
    {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let failed = lock(&daemon.state)
        .registry
        .get(&ended.id)
        .cloned()
        .unwrap();
    assert!(!failed.status.is_live(), "{:?}", failed.status);
    let queued = inbox(&daemon, ended.id.as_str(), false).await;
    assert!(queued.iter().any(|m| m.id == message), "the queue is kept");
    // Back, under its own record, on a terminal. The failed launch's
    // owner may still be letting go of the id for a moment; a
    // relaunch that meets it is refused, not taken over, and is asked
    // again. Two presses at once: one process comes back, the other
    // press is refused, never a second process under the id.
    let mut events = daemon.subscribe_events();
    let mut answer = Response::Ok;
    for _ in 0..4 {
        let (first, second) = tokio::join!(
            daemon.handle(Request::ResumeSession {
                agent: ended.id.to_string(),
                spec: good(),
            }),
            daemon.handle(Request::ResumeSession {
                agent: ended.id.to_string(),
                spec: good(),
            })
        );
        let (won, lost) = match (&first, &second) {
            (Response::Agent { .. }, other) => (first.clone(), other.clone()),
            (other, Response::Agent { .. }) => (second.clone(), other.clone()),
            _ => {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                continue;
            }
        };
        assert!(
            matches!(
                lost,
                Response::Error {
                    code: ErrorCode::Conflict | ErrorCode::Invalid,
                    ..
                }
            ),
            "the other press is refused: {lost:?}"
        );
        answer = won;
        break;
    }
    let Response::Agent { agent: back } = answer else {
        panic!("relaunch failed: {answer:?}")
    };
    assert!(back.spec.tty, "a terminal, as asked");
    assert_eq!(back.id, ended.id, "the same identity");
    assert_eq!(back.status, AgentStatus::Running);
    assert!(back.managed && back.pid.is_some());
    assert_eq!(back.spec.name, "claude-code-4242");
    assert_eq!(
        back.spec.labels.get("session_id").map(String::as_str),
        Some(session)
    );
    assert!(
        back.spec
            .command
            .windows(2)
            .any(|w| w[0] == "--resume" && w[1] == session)
    );
    let mut kinds = Vec::new();
    while let Ok(Ok(event)) =
        tokio::time::timeout(std::time::Duration::from_millis(200), events.recv()).await
    {
        kinds.push(event.kind);
    }
    assert!(kinds.iter().any(|k| matches!(k, EventKind::SessionRelaunched { agent, session: s } if *agent == ended.id && s == session)));
    assert!(
        kinds
            .iter()
            .any(|k| matches!(k, EventKind::AgentStarted { agent, .. } if *agent == ended.id))
    );
    let queued = inbox(&daemon, ended.id.as_str(), false).await;
    assert!(
        queued.iter().any(|m| m.id == message),
        "the queue rode along"
    );
    let Response::Questions { questions } = daemon.handle(Request::Questions { agent: None }).await
    else {
        panic!("questions did not answer")
    };
    assert!(
        questions
            .iter()
            .any(|q| q.id == question && q.from == ended.id.as_str()),
        "its question is still its own: {questions:?}"
    );
    let Response::Tasks { tasks, .. } = daemon
        .handle(Request::Tasks {
            project: Some(checkout.display().to_string()),
            column: None,
            archived: false,
            offset: 0,
            limit: 10,
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
        panic!("the board did not answer")
    };
    assert!(
        tasks
            .iter()
            .any(|t| t.id == card.id && t.assignee.as_ref() == Some(&ended.id)),
        "its card is still its own: {tasks:?}"
    );
    // Already back: not twice.
    assert!(matches!(
        daemon
            .handle(Request::ResumeSession {
                agent: ended.id.to_string(),
                spec: good(),
            })
            .await,
        Response::Error {
            code: ErrorCode::Invalid,
            ..
        }
    ));
    daemon.stop_all().await;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while lock(&daemon.state)
        .registry
        .get(&ended.id)
        .is_some_and(|r| r.status.is_live())
        && std::time::Instant::now() < deadline
    {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    // Something lands on the record between the checks and the
    // commit — a label here, as a role or an availability report
    // would: the relaunch is refused rather than written over it,
    // and the label is still there.
    let hold = Arc::new(tokio::sync::Barrier::new(2));
    *lock(&daemon.relaunch_hold) = Some(hold.clone());
    let held = {
        let daemon = daemon.clone();
        let id = ended.id.to_string();
        let spec = good();
        tokio::spawn(async move {
            daemon
                .handle(Request::ResumeSession { agent: id, spec })
                .await
        })
    };
    hold.wait().await;
    lock(&daemon.state)
        .registry
        .get_mut(&ended.id)
        .unwrap()
        .spec
        .labels
        .insert("role".into(), "reviewer".into());
    hold.wait().await;
    assert!(matches!(
        held.await.unwrap(),
        Response::Error {
            code: ErrorCode::Conflict,
            ..
        }
    ));
    let kept = lock(&daemon.state)
        .registry
        .get(&ended.id)
        .cloned()
        .unwrap();
    assert!(!kept.status.is_live(), "{:?}", kept.status);
    assert_eq!(
        kept.spec.labels.get("role").map(String::as_str),
        Some("reviewer")
    );
    // Gone again; a store that cannot commit the return commits
    // nothing: the record stays ended, the answer says storage.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while lock(&daemon.state)
        .registry
        .get(&ended.id)
        .is_some_and(|r| r.status.is_live())
        && std::time::Instant::now() < deadline
    {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let before = lock(&daemon.state)
        .registry
        .get(&ended.id)
        .cloned()
        .unwrap();
    assert!(!before.status.is_live());
    lock(&daemon.state)
        .store
        .reject_event_for_test("session_relaunched");
    assert!(matches!(
        daemon
            .handle(Request::ResumeSession {
                agent: ended.id.to_string(),
                spec: good(),
            })
            .await,
        Response::Error {
            code: ErrorCode::StorageUnavailable,
            ..
        }
    ));
    let stale = lock(&daemon.state)
        .registry
        .get(&ended.id)
        .cloned()
        .unwrap();
    assert_eq!(stale, before, "a fenced return changes nothing in memory");
}

#[tokio::test]
async fn shutdown_waits_for_an_acknowledged_owner_to_release_its_lock() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let sessions = agentdocker_core::session::sessions_dir(dir.path());
    std::fs::create_dir_all(&sessions).unwrap();
    let path = sessions.join("retiring.lock");
    let held = agentdocker_host::lock::try_exclusive(&path)
        .unwrap()
        .unwrap();
    // Exit persistence has already removed the supervision entry, but
    // its owner still holds the lock while processing acknowledgement.
    let mut stopping = Box::pin(daemon.stop_all());
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut stopping)
            .await
            .is_err(),
        "shutdown must wait for the owner after its registry exit"
    );
    assert!(path.exists(), "a held owner lock must not be removed");
    drop(held);
    tokio::time::timeout(std::time::Duration::from_secs(2), stopping)
        .await
        .expect("shutdown completes after owner retirement");
    assert!(
        !sessions.exists(),
        "the final sweep removes the released lock"
    );
}

/// A daemon that stops leaves nothing of its sessions behind that
/// nobody holds: a stopped owner's lock and socket go, the session
/// directory goes when it is empty, and so does the short `/tmp`
/// directory a long home's sockets fell back to. An exit report that
/// no daemon has recorded yet stays, with the directory around it.
#[tokio::test]
async fn a_stopped_daemon_leaves_no_session_directory_behind() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let mut command = spec("brief");
    command.workdir = Some(dir.path().to_path_buf());
    command.command = vec!["sh".into(), "-c".into(), "sleep 30".into()];
    let Response::Agent { agent } = daemon.handle(Request::Run { spec: command }).await else {
        panic!("managed launch failed");
    };
    let sessions = agentdocker_core::session::sessions_dir(dir.path());
    assert!(
        sessions.join(format!("{}.lock", agent.id)).exists(),
        "the owner holds its lock while it runs"
    );
    daemon.stop_all().await;
    assert!(!sessions.exists(), "{}", sessions.display());
    let short = agentdocker_core::paths::short_socket_dir(dir.path());
    if short != dir.path() {
        assert!(!short.exists(), "{}", short.display());
    }
    // An exit report nobody recorded keeps its place.
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(sessions.join("deadbeef.exit"), b"{}").unwrap();
    std::fs::write(sessions.join("deadbeef.lock"), b"").unwrap();
    sweep_sessions(dir.path());
    assert!(sessions.join("deadbeef.exit").exists());
    assert!(sessions.join("deadbeef.lock").exists());
    std::fs::remove_file(sessions.join("deadbeef.exit")).unwrap();
    sweep_sessions(dir.path());
    assert!(!sessions.exists(), "an unheld lock with no report goes");
}

/// A relaunch whose program cannot be started at all is answered with
/// the reason, leaves the record ended with its queue, and does not
/// stand in the way of the next relaunch.
#[tokio::test]
async fn a_relaunch_that_cannot_start_answers_and_leaves_the_record_ended() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let checkout = dir.path().join("checkout");
    std::fs::create_dir(&checkout).unwrap();
    let session = "0f0f0f0f-1111-2222-3333-444444444444";
    let mut ended_spec = spec("claude-code-7");
    ended_spec.runtime = "claude-code".into();
    ended_spec.workdir = Some(checkout.clone());
    ended_spec
        .labels
        .insert("session_id".into(), session.into());
    let Response::Agent { agent: ended } = daemon
        .handle(Request::Register {
            spec: ended_spec,
            pid: None,
            session: None,
        })
        .await
    else {
        panic!("registration failed")
    };
    let peer = register(&daemon, "peer", None).await;
    let Response::Sent { message, .. } = daemon
        .handle(Request::Send {
            from: peer.id.to_string(),
            to: ended.id.to_string(),
            kind: "chat".into(),
            payload: json!({"text": "waiting"}),
            reply_to: None,
            links: Vec::new(),
        })
        .await
    else {
        panic!("send failed")
    };
    daemon.mark_exited(&ended.id, AgentStatus::Exited { code: Some(0) });
    let relaunch = |program: &str| AgentSpec {
        runtime: "claude-code".into(),
        command: vec![program.to_owned(), "--resume".into(), session.into()],
        workdir: Some(checkout.clone()),
        ..Default::default()
    };
    let missing = dir.path().join("no-such-claude");
    let answer = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        daemon.handle(Request::ResumeSession {
            agent: ended.id.to_string(),
            spec: relaunch(&missing.display().to_string()),
        }),
    )
    .await
    .expect("a launch that cannot start is answered");
    assert!(
        matches!(answer, Response::Error { .. }),
        "the reason comes back: {answer:?}"
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while lock(&daemon.state)
        .registry
        .get(&ended.id)
        .is_some_and(|r| r.status.is_live())
        && std::time::Instant::now() < deadline
    {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let after = lock(&daemon.state)
        .registry
        .get(&ended.id)
        .cloned()
        .unwrap();
    assert!(!after.status.is_live(), "{:?}", after.status);
    let queued = inbox(&daemon, ended.id.as_str(), false).await;
    assert!(queued.iter().any(|m| m.id == message), "the queue is kept");
    // The next relaunch is not held up by the one that never started.
    let mut answer = Response::Ok;
    for _ in 0..4 {
        answer = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            daemon.handle(Request::ResumeSession {
                agent: ended.id.to_string(),
                spec: AgentSpec {
                    command: vec![
                        "sh".into(),
                        "-c".into(),
                        "sleep 30".into(),
                        "--resume".into(),
                        session.into(),
                    ],
                    ..relaunch("sh")
                },
            }),
        )
        .await
        .expect("the next relaunch is answered");
        if matches!(answer, Response::Agent { .. }) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    let Response::Agent { agent: back } = answer else {
        panic!("relaunch after a failed start: {answer:?}")
    };
    assert_eq!(back.id, ended.id);
    assert_eq!(back.status, AgentStatus::Running);
    daemon.stop_all().await;
}

#[tokio::test]
async fn a_restarted_daemon_brings_back_a_restorable_agent_under_its_own_identity() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let mut command = spec("keeper");
    command.workdir = Some(dir.path().to_path_buf());
    command.restore = true;
    command.command = vec!["sh".into(), "-c".into(), "sleep 30".into()];
    let Response::Agent { agent } = daemon.handle(Request::Run { spec: command }).await else {
        panic!("managed launch failed");
    };
    // Something worth keeping: a lease, and a journal cursor, both
    // keyed by this agent's id.
    assert!(matches!(
        claim(&daemon, "keeper", "task:the-work").await,
        Response::Lease { .. }
    ));
    let before = agent.pid;

    let daemon = restart(&dir, daemon).await;

    let Response::Agent { agent: after } = daemon
        .handle(Request::Inspect {
            agent: agent.id.to_string(),
        })
        .await
    else {
        panic!("the agent is gone")
    };
    assert_eq!(after.id, agent.id, "the same identity, not a new agent");
    assert_eq!(after.status, AgentStatus::Running);
    assert_ne!(after.pid, before, "a new process under the old identity");
    // The lease is still held, because its holder never stopped being
    // live and never stopped being this id.
    let leases = list_leases(&daemon).await;
    assert_eq!(leases.len(), 1);
    assert_eq!(leases[0].holder, agent.id);

    daemon.stop_all().await;
}

#[tokio::test]
async fn an_agent_that_did_not_ask_to_be_restored_is_not() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let mut command = spec("ephemeral");
    command.workdir = Some(dir.path().to_path_buf());
    command.command = vec!["sh".into(), "-c".into(), "sleep 30".into()];
    let Response::Agent { agent } = daemon.handle(Request::Run { spec: command }).await else {
        panic!("managed launch failed");
    };

    let daemon = restart(&dir, daemon).await;

    // Nothing was relaunched, and the liveness sweep retires it.
    daemon.check_liveness();
    assert!(!daemon.is_live(&agent.id));
    daemon.stop_all().await;
}

#[tokio::test]
async fn a_restored_agent_is_told_what_changed_while_it_was_down() {
    let dir = TempDir::new().unwrap();
    let work = dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let watched = work.join("parser.rs");
    std::fs::write(&watched, "fn parse() {}\n").unwrap();

    let daemon = open(&dir);
    let mut command = spec("reader");
    command.workdir = Some(work.clone());
    command.restore = true;
    command.command = vec!["sh".into(), "-c".into(), "sleep 30".into()];
    let Response::Agent { agent } = daemon.handle(Request::Run { spec: command }).await else {
        panic!("managed launch failed");
    };
    let observed = daemon
        .handle(Request::Observe {
            agent: agent.id.to_string(),
            paths: vec![watched.display().to_string()],
        })
        .await;
    assert!(matches!(observed, Response::Reads { .. }), "{observed:?}");
    let Response::Checkpoint { checkpoint } = daemon
        .handle(Request::Checkpoint {
            agent: agent.id.to_string(),
            key: "k1".to_owned(),
            task: "rewrite the tokenizer".to_owned(),
            assumptions: Vec::new(),
            next_steps: vec!["handle raw strings".to_owned()],
            release_leases: false,
            links: Vec::new(),
        })
        .await
    else {
        panic!("checkpoint failed")
    };

    daemon.stop_all().await;
    drop(daemon);
    // Somebody edits the file while nothing is running.
    std::fs::write(&watched, "fn parse() { todo!() }\n").unwrap();
    let daemon = open(&dir);
    daemon.restore_agents().await;

    let waiting = inbox(&daemon, agent.id.as_str(), true).await;
    let brief = waiting
        .iter()
        .find(|m| m.kind == "restored")
        .expect("a restored agent is told it was restored");
    assert_eq!(brief.payload["restored"], json!(true));
    assert_eq!(brief.payload["reads"], json!(1));
    let stale = brief.payload["stale"].as_array().unwrap();
    assert_eq!(stale.len(), 1, "the edited path: {stale:?}");
    assert_eq!(brief.payload["checkpoint"], json!(checkpoint.id));
    assert_eq!(brief.payload["task"], json!("rewrite the tokenizer"));
    assert_eq!(brief.payload["next_steps"], json!(["handle raw strings"]));
    let text = brief.payload["text"].as_str().unwrap();
    assert!(text.contains("reread"), "and told to reread it: {text}");
    assert!(text.contains("reader"), "under its own name: {text}");
    assert!(
        text.contains("rewrite the tokenizer") && text.contains("handle raw strings"),
        "and what it was doing: {text}"
    );

    daemon.stop_all().await;
}

#[tokio::test]
async fn a_restore_that_cannot_run_records_the_failure_and_does_not_stop_the_others() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);

    let mut broken = spec("broken");
    broken.workdir = Some(dir.path().to_path_buf());
    broken.restore = true;
    broken.command = vec!["sh".into(), "-c".into(), "sleep 30".into()];
    let Response::Agent { agent: broken } = daemon.handle(Request::Run { spec: broken }).await
    else {
        panic!("managed launch failed");
    };
    let mut fine = spec("fine");
    fine.workdir = Some(dir.path().to_path_buf());
    fine.restore = true;
    fine.command = vec!["sh".into(), "-c".into(), "sleep 30".into()];
    let Response::Agent { agent: fine } = daemon.handle(Request::Run { spec: fine }).await else {
        panic!("managed launch failed");
    };

    daemon.stop_all().await;
    drop(daemon);
    // The command is gone by the time the daemon comes back.
    let daemon = open(&dir);
    {
        let mut state = lock(&daemon.state);
        let record = state.registry.get_mut(&broken.id).unwrap();
        record.spec.command = vec!["/nonexistent/agentdocker-no-such-binary".into()];
        let record = record.clone();
        let _ = state.persist("agent", |store| store.upsert_agent(&record));
    }
    daemon.restore_agents().await;

    let Response::Agent { agent: broken } = daemon
        .handle(Request::Inspect {
            agent: broken.id.to_string(),
        })
        .await
    else {
        panic!("record gone")
    };
    assert!(
        matches!(&broken.status, AgentStatus::Failed { reason } if reason.contains("restored")),
        "unexpected {:?}",
        broken.status
    );
    assert!(daemon.is_live(&fine.id), "the other one still came back");
    daemon.stop_all().await;
}

#[tokio::test]
async fn an_agent_stopped_on_purpose_is_not_brought_back() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let mut command = spec("finished");
    command.workdir = Some(dir.path().to_path_buf());
    command.restore = true;
    command.command = vec!["sh".into(), "-c".into(), "sleep 30".into()];
    let Response::Agent { agent } = daemon.handle(Request::Run { spec: command }).await else {
        panic!("managed launch failed");
    };
    daemon
        .handle(Request::Stop {
            agent: agent.id.to_string(),
            force: true,
        })
        .await;
    // The flag is cleared in the record itself, so the reason it will
    // not come back is visible rather than hidden.
    let Response::Agent { agent: stopped } = daemon
        .handle(Request::Inspect {
            agent: agent.id.to_string(),
        })
        .await
    else {
        panic!("record gone")
    };
    assert!(!stopped.spec.restore);

    let daemon = restart(&dir, daemon).await;
    daemon.check_liveness();
    assert!(!daemon.is_live(&agent.id));
    daemon.stop_all().await;
}

/// A refused claim says by whom: the holder's name, which is who the
/// person can go and talk to, with its id and what it holds.
#[tokio::test]
async fn a_refused_claim_names_the_holder() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let writer = register(&daemon, "writer", None).await;
    let reviewer = register(&daemon, "reviewer", None).await;
    let claim = |agent: &AgentRecord, resource: &str| Request::Claim {
        agent: agent.id.to_string(),
        resource: resource.to_owned(),
        mode: LeaseMode::Exclusive,
        amount: None,
        ttl_secs: 300,
        note: Some("refactoring the parser".to_owned()),
        wait_secs: 0,
        automatic: false,
    };
    assert!(matches!(
        daemon.handle(claim(&writer, "path:/work/src")).await,
        Response::Lease { .. }
    ));
    let Response::Error { code, message, .. } = daemon
        .handle(claim(&reviewer, "path:/work/src/parser.rs"))
        .await
    else {
        panic!("the second claim is refused")
    };
    assert_eq!(code, ErrorCode::Conflict);
    assert_eq!(
        message,
        format!(
            "path:/work/src/parser.rs is held by writer ({}; exclusive on path:/work/src)",
            writer.id.short()
        )
    );
}

#[tokio::test]
async fn a_clean_shutdown_hands_the_leases_back_and_says_it_did() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let mut command = spec("holder");
    command.workdir = Some(dir.path().to_path_buf());
    command.restore = true;
    command.command = vec!["sh".into(), "-c".into(), "sleep 30".into()];
    let Response::Agent { agent } = daemon.handle(Request::Run { spec: command }).await else {
        panic!("managed launch failed");
    };
    let Response::Lease { lease } = daemon
        .handle(Request::Claim {
            agent: agent.id.to_string(),
            resource: "task:the-refactor".to_owned(),
            mode: LeaseMode::Shared,
            amount: None,
            ttl_secs: 300,
            note: Some("halfway through".to_owned()),
            wait_secs: 0,
            automatic: false,
        })
        .await
    else {
        panic!("claim failed")
    };

    // A clean shutdown stops the agent, and stopping it releases the
    // lease — correctly, because nothing is working on it any more.
    daemon.stop_all().await;
    assert!(list_leases(&daemon).await.is_empty());
    drop(daemon);

    let daemon = open(&dir);
    daemon.restore_agents().await;

    let leases = list_leases(&daemon).await;
    assert_eq!(leases.len(), 1, "put back for the restored agent");
    assert_eq!(leases[0].holder, agent.id);
    assert_eq!(leases[0].resource, lease.resource);
    assert_eq!(leases[0].mode, LeaseMode::Shared, "as it was held");
    assert_eq!(leases[0].note.as_deref(), Some("halfway through"));
    assert_ne!(leases[0].id, lease.id, "a new lease, not a resurrected one");

    let waiting = inbox(&daemon, agent.id.as_str(), true).await;
    let brief = waiting.iter().find(|m| m.kind == "restored").unwrap();
    assert_eq!(
        brief.payload["reclaimed_leases"],
        json!([lease.resource.to_string()])
    );
    assert_eq!(
        brief.payload["leases"],
        json!([lease.resource.to_string()]),
        "listed once, not once per way it came back"
    );
    let text = brief.payload["text"].as_str().unwrap();
    assert_eq!(
        text.matches("task:the-refactor").count(),
        1,
        "and read once: {text}"
    );
    assert!(text.contains("You still hold"), "{text}");
    daemon.stop_all().await;
}

#[tokio::test]
async fn a_question_has_to_reach_somebody_who_can_answer_it() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    me(&daemon).await;
    // A topic delivers only to live subscribers and queues for
    // nobody, so a question put to one could wait out its whole
    // timeout without ever appearing in `questions`.
    let response = daemon
        .handle(Request::Ask {
            from: "user".into(),
            to: "topic:reviews".into(),
            question: "anyone?".into(),
            timeout_secs: 1,
        })
        .await;
    assert!(
        matches!(
            &response,
            Response::Error {
                code: ErrorCode::Invalid,
                ..
            }
        ),
        "unexpected {response:?}"
    );
    let Response::Questions { questions } = daemon.handle(Request::Questions { agent: None }).await
    else {
        panic!("questions did not answer with questions")
    };
    assert!(questions.is_empty(), "and nothing was left waiting");
}

/// The daemon has to know about a question before anyone can see it.
/// A recipient that answers the instant the message arrives must not
/// be told there is no such question.
#[tokio::test]
async fn an_answer_that_races_the_question_still_lands() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let human = me(&daemon).await;
    let asker = register(&daemon, "worker", distinct_pid()).await;

    let mut messages = lock(&daemon.state).bus.subscribe();
    let racing = {
        let daemon = daemon.clone();
        tokio::spawn(async move {
            loop {
                let Ok(envelope) = messages.recv().await else {
                    return None;
                };
                if envelope.kind == "question" {
                    // No pause: whatever the daemon knows at the
                    // moment the message goes out is all it knows.
                    return Some(
                        daemon
                            .handle(Request::Answer {
                                from: Some(human.id.to_string()),
                                message: envelope.id,
                                text: "at once".to_owned(),
                            })
                            .await,
                    );
                }
            }
        })
    };

    let response = daemon
        .handle(Request::Ask {
            from: asker.id.to_string(),
            to: "user".into(),
            question: "now?".into(),
            timeout_secs: 10,
        })
        .await;
    let answered = racing.await.unwrap().expect("the question was published");
    assert!(
        matches!(answered, Response::Sent { .. }),
        "the answer found its question: {answered:?}"
    );
    let Response::Answer { text, .. } = response else {
        panic!("unexpected {response:?}")
    };
    assert_eq!(text, "at once");
}

// ----- the wait queue, deadlock, and derived activity ----------------

async fn activity_of(daemon: &Arc<Daemon>, agent: &str) -> Activity {
    let Response::Activity { activity } = daemon
        .handle(Request::Activity {
            agent: Some(agent.to_owned()),
            project: None,
            all: true,
        })
        .await
    else {
        panic!("activity did not answer with activity")
    };
    activity.into_iter().next().expect("one agent").activity
}

async fn claim_waiting(
    daemon: &Arc<Daemon>,
    agent: &str,
    resource: &str,
    wait_secs: u64,
) -> Response {
    daemon
        .handle(Request::Claim {
            agent: agent.to_owned(),
            resource: resource.to_owned(),
            mode: LeaseMode::Exclusive,
            amount: None,
            ttl_secs: 60,
            note: None,
            wait_secs,
            automatic: false,
        })
        .await
}

/// Wait until the queue is this deep, so a test never races the tasks
/// it started.
async fn queued(daemon: &Arc<Daemon>, want: usize) -> Vec<agentdocker_core::Waiter> {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let Response::Waiting { waiting } = daemon.handle(Request::Waiting).await else {
                panic!("waiting did not answer with waiters")
            };
            if waiting.len() == want {
                return waiting;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the queue reached the expected depth")
}

async fn release(daemon: &Arc<Daemon>, agent: &str, lease: LeaseId) {
    daemon
        .handle(Request::Release {
            agent: agent.to_owned(),
            lease,
            summary: None,
            summary_source: agentdocker_core::SummarySource::Explicit,
        })
        .await;
}

#[tokio::test]
async fn the_agent_that_waited_longest_gets_the_lease() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let holder = register(&daemon, "holder", distinct_pid()).await;
    let first = register(&daemon, "first", distinct_pid()).await;
    let second = register(&daemon, "second", distinct_pid()).await;

    let Response::Lease { lease } = claim(&daemon, "holder", "task:contested").await else {
        panic!("the first claim should succeed")
    };

    let first_claim = {
        let daemon = daemon.clone();
        tokio::spawn(async move { claim_waiting(&daemon, "first", "task:contested", 10).await })
    };
    // Queued before the second one arrives, so the arrival order
    // under test is the one intended rather than whichever task the
    // runtime happened to poll first.
    let queue = queued(&daemon, 1).await;
    assert_eq!(queue[0].agent, first.id);

    let second_claim = {
        let daemon = daemon.clone();
        tokio::spawn(async move { claim_waiting(&daemon, "second", "task:contested", 10).await })
    };
    let queue = queued(&daemon, 2).await;
    assert_eq!(queue[1].agent, second.id, "and behind the first");

    release(&daemon, holder.id.as_str(), lease.id).await;

    let won = tokio::time::timeout(std::time::Duration::from_secs(5), first_claim)
        .await
        .expect("the first waiter finished")
        .unwrap();
    let Response::Lease { lease: won } = won else {
        panic!("the older waiter should have won: {won:?}")
    };
    assert_eq!(won.holder, first.id, "not whoever raced fastest");
    second_claim.abort();
}

#[tokio::test]
async fn a_waiter_that_goes_away_does_not_hold_the_queue() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let holder = register(&daemon, "holder", distinct_pid()).await;
    register(&daemon, "leaver", distinct_pid()).await;
    let stayer = register(&daemon, "stayer", distinct_pid()).await;

    let Response::Lease { lease } = claim(&daemon, "holder", "task:contested").await else {
        panic!("the first claim should succeed")
    };
    let leaving = {
        let daemon = daemon.clone();
        tokio::spawn(async move { claim_waiting(&daemon, "leaver", "task:contested", 30).await })
    };
    queued(&daemon, 1).await;
    let staying = {
        let daemon = daemon.clone();
        tokio::spawn(async move { claim_waiting(&daemon, "stayer", "task:contested", 30).await })
    };
    queued(&daemon, 2).await;

    // The client at the head of the queue disconnects: its future is
    // dropped, which must give up its place rather than starve the
    // one behind it.
    leaving.abort();
    queued(&daemon, 1).await;
    release(&daemon, holder.id.as_str(), lease.id).await;

    let won = tokio::time::timeout(std::time::Duration::from_secs(5), staying)
        .await
        .expect("the remaining waiter finished")
        .unwrap();
    let Response::Lease { lease: won } = won else {
        panic!("the waiter behind it should have won: {won:?}")
    };
    assert_eq!(won.holder, stayer.id);
}

#[tokio::test]
async fn a_claim_that_would_close_a_ring_is_refused_with_the_ring() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let alpha = register(&daemon, "alpha", distinct_pid()).await;
    let beta = register(&daemon, "beta", distinct_pid()).await;

    // alpha holds x, beta holds y.
    assert!(matches!(
        claim(&daemon, "alpha", "task:x").await,
        Response::Lease { .. }
    ));
    assert!(matches!(
        claim(&daemon, "beta", "task:y").await,
        Response::Lease { .. }
    ));
    // alpha waits for y.
    let alpha_waits = {
        let daemon = daemon.clone();
        tokio::spawn(async move { claim_waiting(&daemon, "alpha", "task:y", 30).await })
    };
    queued(&daemon, 1).await;

    // beta asking for x closes the ring, so it is refused at once
    // rather than after both TTLs.
    let refused = claim_waiting(&daemon, "beta", "task:x", 30).await;
    let Response::Error {
        code,
        message,
        details,
    } = refused
    else {
        panic!("beta should have been refused")
    };
    assert_eq!(code, ErrorCode::Deadlock);
    assert!(message.contains("deadlock"), "{message}");
    let cycle = details.expect("a cycle").get("cycle").cloned().unwrap();
    let cycle: Vec<agentdocker_core::Blocked> = serde_json::from_value(cycle).unwrap();
    assert_eq!(cycle.len(), 2, "beta → alpha → beta: {cycle:?}");
    assert_eq!(cycle[0].agent, beta.id);
    assert_eq!(cycle[0].held_by, alpha.id);
    assert_eq!(cycle[1].agent, alpha.id);
    assert_eq!(cycle[1].held_by, beta.id);
    // And the refusal changed nothing: alpha is still waiting.
    assert_eq!(queued(&daemon, 1).await.len(), 1);
    alpha_waits.abort();
}

#[tokio::test]
async fn plain_contention_still_waits_rather_than_being_called_a_deadlock() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    register(&daemon, "holder", distinct_pid()).await;
    register(&daemon, "waiter", distinct_pid()).await;
    assert!(matches!(
        claim(&daemon, "holder", "task:x").await,
        Response::Lease { .. }
    ));
    // The holder wants nothing, so there is no cycle: the waiter
    // waits and then reports an ordinary conflict.
    let response = claim_waiting(&daemon, "waiter", "task:x", 1).await;
    assert!(
        matches!(
            &response,
            Response::Error {
                code: ErrorCode::Conflict,
                ..
            }
        ),
        "unexpected {response:?}"
    );
}

#[tokio::test]
async fn activity_says_what_an_agent_is_blocked_on_and_who_has_it() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let holder = register(&daemon, "holder", distinct_pid()).await;
    let waiter = register(&daemon, "waiter", distinct_pid()).await;

    // Registration proves presence, not work.
    assert!(matches!(
        activity_of(&daemon, "holder").await,
        Activity::Unknown
    ));

    assert!(matches!(
        claim(&daemon, "holder", "task:the-work").await,
        Response::Lease { .. }
    ));
    let blocked = {
        let daemon = daemon.clone();
        tokio::spawn(async move { claim_waiting(&daemon, "waiter", "task:the-work", 30).await })
    };
    queued(&daemon, 1).await;

    let activity = activity_of(&daemon, waiter.id.as_str()).await;
    let Activity::Blocked {
        resource, held_by, ..
    } = activity
    else {
        panic!("the waiter should be blocked: {activity:?}")
    };
    assert_eq!(resource.as_str(), "task:the-work", "on a named resource");
    assert_eq!(held_by, vec![holder.id.clone()], "held by a named agent");

    // Blocked sorts first, because it is the one somebody has to do
    // something about.
    let Response::Activity { activity } = daemon
        .handle(Request::Activity {
            agent: None,
            project: None,
            all: false,
        })
        .await
    else {
        panic!("activity did not answer with activity")
    };
    assert_eq!(activity.len(), 2);
    assert_eq!(activity[0].agent, waiter.id);
    assert_eq!(activity[0].activity.label(), "blocked");

    blocked.abort();
}

#[tokio::test]
async fn a_quiet_agent_is_unknown_and_a_finished_one_is_finished() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let quiet = register(&daemon, "quiet", distinct_pid()).await;
    // Backdate its last contact past the activity window.
    {
        let mut state = lock(&daemon.state);
        let record = state.registry.get_mut(&quiet.id).unwrap();
        record.last_seen = Utc::now() - Duration::hours(1);
    }
    assert!(matches!(
        activity_of(&daemon, quiet.id.as_str()).await,
        Activity::Unknown
    ));

    daemon
        .handle(Request::Deregister {
            agent: quiet.id.to_string(),
        })
        .await;
    assert_eq!(
        activity_of(&daemon, quiet.id.as_str()).await,
        Activity::Finished
    );
}

/// What a wait is for travels with the report, into the store and out
/// of the agent's activity, so the app can say which errand it is.
#[tokio::test]
async fn a_wait_keeps_what_it_is_for_through_the_store() {
    use agentdocker_core::{ActivityObservation, ReportedActivity, WaitReason};
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let agent = register(&daemon, "waiting-fixture", None).await;
    let now = Utc::now();
    let waiting = ActivityObservation {
        activity: ReportedActivity::Waiting,
        observed_at: now,
        reason: Some(WaitReason::Permission),
    };
    let mut state = lock(&daemon.state);
    assert!(matches!(
        state.report_activity(agent.id.as_str(), waiting.clone(), now),
        Response::Ok
    ));
    assert_eq!(
        state.store.load_agents().unwrap()[0].reported_activity,
        Some(waiting)
    );
    let record = state.registry.get(&agent.id).unwrap();
    assert_eq!(
        state.activity_of(record, now + Duration::minutes(30)),
        Activity::Waiting {
            since: now,
            reason: Some(WaitReason::Permission),
            judged: None,
        }
    );
}

#[tokio::test]
async fn provider_activity_is_ordered_expires_and_commits_with_its_event() {
    use agentdocker_core::{ActivityObservation, ReportedActivity};
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let agent = register(&daemon, "activity-fixture", None).await;
    let now = Utc::now();
    let working = ActivityObservation {
        activity: ReportedActivity::Working,
        observed_at: now - Duration::seconds(2),
        reason: None,
    };
    let idle = ActivityObservation {
        activity: ReportedActivity::Idle,
        observed_at: now - Duration::seconds(1),
        reason: None,
    };
    let mut events = daemon.subscribe_events();
    {
        let mut state = lock(&daemon.state);
        for observation in [&working, &idle] {
            assert!(matches!(
                state.report_activity(agent.id.as_str(), observation.clone(), now),
                Response::Ok
            ));
            assert!(matches!(
                events.try_recv().unwrap().kind,
                EventKind::AgentActivityReported { .. }
            ));
        }
        assert!(matches!(
            state.report_activity(agent.id.as_str(), working.clone(), now),
            Response::Ok
        ));
        assert!(
            events.try_recv().is_err(),
            "late reports do not revive a stopped turn"
        );
        let record = state.registry.get(&agent.id).unwrap();
        assert_eq!(record.reported_activity, Some(idle.clone()));
        assert_eq!(
            state.store.load_agents().unwrap()[0].reported_activity,
            Some(idle)
        );
        assert!(matches!(
            state.activity_of(record, now),
            Activity::Idle { .. }
        ));
        assert_eq!(
            state.activity_of(record, now + Duration::minutes(5)),
            Activity::Unknown
        );
        assert!(matches!(
            state.report_activity(agent.id.as_str(), working, now + Duration::minutes(6)),
            Response::Error {
                code: ErrorCode::Invalid,
                ..
            }
        ));
    }
    assert!(events.try_recv().is_err());
}

#[tokio::test]
async fn adapter_contact_is_generation_bound_ordered_and_atomic() {
    use agentdocker_core::{AdapterContact, AdapterKind};
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let mut agent = register(&daemon, "adapter-fixture", None).await;
    let birth = Utc::now() - Duration::seconds(10);
    let now = birth + Duration::seconds(10);
    agent.spec.runtime = "codex".into();
    agent.process_started_at = Some(birth);
    let mut events = daemon.subscribe_events();
    let mut state = lock(&daemon.state);
    state.store.upsert_agent(&agent).unwrap();
    *state.registry.get_mut(&agent.id).unwrap() = agent.clone();
    let contact = AdapterContact {
        process_started_at: birth,
        observed_at: now - Duration::seconds(1),
    };
    for kind in [AdapterKind::Mcp, AdapterKind::Hooks] {
        assert!(matches!(
            state.report_adapter(agent.id.as_str(), kind, contact.clone(), now),
            Response::Ok
        ));
        assert!(
            matches!(events.try_recv().unwrap().kind, EventKind::AdapterContactReported { adapter, .. } if adapter == kind)
        );
    }
    let saved = state.registry.get(&agent.id).unwrap().clone();
    assert_eq!(saved.adapter_contacts.len(), 2);
    assert_eq!(state.store.load_agents().unwrap()[0], saved);
    assert!(
        saved.reported_activity.is_none(),
        "contact is not a working/idle observation"
    );
    assert!(
        saved.input_delivery.is_none(),
        "MCP and hooks do not establish idle input delivery"
    );
    assert!(matches!(
        state.report_adapter(agent.id.as_str(), AdapterKind::Mcp, contact.clone(), now),
        Response::Ok
    ));
    assert!(events.try_recv().is_err());
    assert!(matches!(
        state.report_adapter(
            agent.id.as_str(),
            AdapterKind::Mcp,
            AdapterContact {
                observed_at: now + Duration::seconds(28),
                ..contact.clone()
            },
            now + Duration::seconds(28)
        ),
        Response::Ok
    ));
    assert_eq!(state.registry.get(&agent.id).unwrap(), &saved);
    assert!(events.try_recv().is_err(), "nearby callbacks are coalesced");
    for invalid in [
        AdapterContact {
            process_started_at: now,
            ..contact.clone()
        },
        AdapterContact {
            observed_at: now + Duration::seconds(1),
            ..contact.clone()
        },
        AdapterContact {
            observed_at: now - Duration::minutes(5),
            ..contact.clone()
        },
    ] {
        assert!(matches!(
            state.report_adapter(agent.id.as_str(), AdapterKind::Mcp, invalid, now),
            Response::Error {
                code: ErrorCode::Invalid,
                ..
            }
        ));
    }
    state.store.reject_agent_writes_for_test();
    assert!(matches!(
        state.report_adapter(
            agent.id.as_str(),
            AdapterKind::Mcp,
            AdapterContact {
                observed_at: now + Duration::seconds(31),
                ..contact
            },
            now + Duration::seconds(31)
        ),
        Response::Error {
            code: ErrorCode::StorageUnavailable,
            ..
        }
    ));
    assert_eq!(state.registry.get(&agent.id).unwrap(), &saved);
    assert_eq!(state.store.load_agents().unwrap()[0], saved);
    assert!(events.try_recv().is_err());
}

#[tokio::test]
async fn input_delivery_persists_receipts_and_pause_without_inventing_delivery() {
    use agentdocker_core::{InputReceipt, InputReport, ReceivedInput};
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let mut receiver = register(&daemon, "receipt-status", None).await;
    let generation = Utc::now();
    receiver.spec.runtime = "claude-code".into();
    receiver.process_started_at = Some(generation);
    {
        let mut state = lock(&daemon.state);
        state.store.upsert_agent(&receiver).unwrap();
        *state.registry.get_mut(&receiver.id).unwrap() = receiver.clone();
    }
    let Response::Sent { message, .. } = send(&daemon, "user", "receipt-status").await else {
        panic!("not queued")
    };
    let input = ReceivedInput {
        messages: vec![message.clone()],
        receipt: InputReceipt::ClaudeChannel,
    };
    let report = InputReport::Received {
        input: input.clone(),
    };
    let request = |report| Request::ReportInput {
        agent: receiver.id.to_string(),
        process_started_at: generation,
        observed_at: Utc::now(),
        report,
        token: None,
    };
    let mut events = daemon.subscribe_events();
    assert!(matches!(
        daemon.handle(request(report.clone())).await,
        Response::Ok
    ));
    assert!(matches!(
        events.try_recv().unwrap().kind,
        EventKind::InputDeliveryReported { .. }
    ));
    assert_eq!(
        inbox(&daemon, "receipt-status", false).await.len(),
        1,
        "a receipt report does not silently remove input"
    );
    // Still queued, but the receipt covers it: nothing awaits a receipt.
    // A later message the receipt does not name does.
    let awaiting = |daemon: &Arc<Daemon>| {
        let daemon = daemon.clone();
        let id = receiver.id.to_string();
        async move {
            let Response::Activity { activity } = daemon
                .handle(Request::Activity {
                    agent: Some(id),
                    project: None,
                    all: true,
                })
                .await
            else {
                panic!("activity")
            };
            (activity[0].queued_inputs, activity[0].awaiting_receipt)
        }
    };
    assert_eq!(awaiting(&daemon).await, (Some(1), Some(0)));
    let Response::Sent { message: later, .. } = send(&daemon, "user", "receipt-status").await
    else {
        panic!("not queued")
    };
    assert_eq!(awaiting(&daemon).await, (Some(2), Some(1)));
    assert!(matches!(
        daemon
            .handle(Request::AckInbox {
                agent: receiver.id.to_string(),
                messages: vec![message, later]
            })
            .await,
        Response::Ok
    ));
    let before = lock(&daemon.state).next_seq;
    assert!(
        matches!(daemon.handle(request(report)).await, Response::Ok),
        "a lost ACK reply can repeat the exact durable receipt"
    );
    assert_eq!(lock(&daemon.state).next_seq, before);
    assert!(matches!(
        daemon
            .handle(request(InputReport::Received {
                input: ReceivedInput {
                    messages: vec!["never-queued".to_owned().into()],
                    receipt: InputReceipt::ClaudeChannel
                }
            }))
            .await,
        Response::Ok
    ));
    assert!(matches!(
        daemon
            .handle(request(InputReport::Paused {
                reason: "Retained input requires review".into()
            }))
            .await,
        Response::Ok
    ));
    // A retry of an already-ACKed old receipt is not evidence that the
    // later pause was recovered. Keep that attention state and its reason.
    let before_retry = lock(&daemon.state).next_seq;
    assert!(matches!(
        daemon
            .handle(request(InputReport::Received {
                input: input.clone(),
            }))
            .await,
        Response::Ok
    ));
    assert_eq!(lock(&daemon.state).next_seq, before_retry);
    for observed_at in [
        generation - Duration::seconds(1),
        Utc::now() + Duration::minutes(1),
    ] {
        assert!(matches!(
            daemon
                .handle(Request::ReportInput {
                    agent: receiver.id.to_string(),
                    process_started_at: generation,
                    observed_at,
                    report: InputReport::Ready,
                    token: None,
                })
                .await,
            Response::Error {
                code: ErrorCode::Invalid,
                ..
            }
        ));
    }
    let saved = lock(&daemon.state)
        .store
        .load_agents()
        .unwrap()
        .into_iter()
        .find(|a| a.id == receiver.id)
        .unwrap();
    let delivery = saved.input_delivery.unwrap();
    assert_eq!(delivery.received, Some(input));
    assert!(delivery.paused_for(Some(generation)));
    assert_eq!(
        delivery.pause_reason.as_deref(),
        Some("Retained input requires review")
    );
    let Response::Activity { activity } = daemon
        .handle(Request::Activity {
            agent: Some(receiver.id.to_string()),
            project: None,
            all: true,
        })
        .await
    else {
        panic!("activity")
    };
    assert_eq!(activity[0].queued_inputs, Some(0));
    // A supervised successor has another process birth. Delayed reports
    // from the old process must neither clear its pause nor invent receipt.
    let successor = generation + Duration::seconds(1);
    lock(&daemon.state)
        .registry
        .get_mut(&receiver.id)
        .unwrap()
        .process_started_at = Some(successor);
    assert!(matches!(
        daemon.handle(request(InputReport::Ready)).await,
        Response::Error {
            code: ErrorCode::Invalid,
            ..
        }
    ));
    assert!(!delivery.paused_for(Some(successor)));
    assert!(
        lock(&daemon.state)
            .registry
            .get(&receiver.id)
            .unwrap()
            .input_delivery
            .as_ref()
            .unwrap()
            .paused
    );
}

#[tokio::test]
async fn equal_input_observations_allow_exact_retries_but_reject_conflicting_state() {
    use agentdocker_core::{InputReceipt, InputReport, ReceivedInput};
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let mut receiver = register(&daemon, "equal-receipts", None).await;
    let generation = Utc::now();
    receiver.spec.runtime = "claude-code".into();
    receiver.process_started_at = Some(generation);
    let mut ids = Vec::new();
    for _ in 0..2 {
        let Response::Sent { message, .. } = send(&daemon, "user", "equal-receipts").await else {
            panic!("not queued")
        };
        ids.push(message);
    }
    let reports = vec![
        InputReport::Ready,
        InputReport::Paused {
            reason: "First pause".into(),
        },
        InputReport::Paused {
            reason: "Different pause".into(),
        },
        InputReport::Received {
            input: ReceivedInput {
                messages: vec![ids[0].clone()],
                receipt: InputReceipt::ClaudeChannel,
            },
        },
        InputReport::Received {
            input: ReceivedInput {
                messages: vec![ids[1].clone()],
                receipt: InputReceipt::ClaudeChannel,
            },
        },
    ];
    let mut state = lock(&daemon.state);
    for initial in &reports {
        state.store.upsert_agent(&receiver).unwrap();
        *state.registry.get_mut(&receiver.id).unwrap() = receiver.clone();
        assert!(matches!(
            state.report_input(
                receiver.id.as_str(),
                generation,
                generation,
                initial.clone(),
                None,
                generation + Duration::seconds(1)
            ),
            Response::Ok
        ));
        let saved = state
            .registry
            .get(&receiver.id)
            .unwrap()
            .input_delivery
            .clone();
        let next_seq = state.next_seq;
        // A later retry of the same observation must preserve received_at
        // and emit nothing, even while its exact IDs are still queued.
        assert!(matches!(
            state.report_input(
                receiver.id.as_str(),
                generation,
                generation,
                initial.clone(),
                None,
                generation + Duration::seconds(2)
            ),
            Response::Ok
        ));
        assert_eq!(state.next_seq, next_seq);
        for conflicting in reports.iter().filter(|report| *report != initial) {
            let response = state.report_input(
                receiver.id.as_str(),
                generation,
                generation,
                conflicting.clone(),
                None,
                generation + Duration::seconds(2),
            );
            // Ready after an unpaused receipt preserves every saved field.
            // Equal timestamps constrain resulting state, not report tags.
            let unchanged = matches!(initial, InputReport::Received { .. })
                && matches!(conflicting, InputReport::Ready);
            assert!(
                if unchanged {
                    matches!(response, Response::Ok)
                } else {
                    matches!(
                        response,
                        Response::Error {
                            code: ErrorCode::Invalid,
                            ..
                        }
                    )
                },
                "unexpected equal observation: {initial:?} to {conflicting:?}: {response:?}"
            );
            assert_eq!(state.next_seq, next_seq);
            assert_eq!(
                state.registry.get(&receiver.id).unwrap().input_delivery,
                saved
            );
            let persisted = state
                .store
                .load_agents()
                .unwrap()
                .into_iter()
                .find(|record| record.id == receiver.id)
                .unwrap();
            assert_eq!(persisted.input_delivery, saved);
            assert_eq!(state.inboxes[&receiver.id].len(), 2);
        }
        assert!(matches!(
            state.report_input(
                receiver.id.as_str(),
                generation,
                generation + Duration::seconds(1),
                InputReport::Ready,
                None,
                generation + Duration::seconds(2)
            ),
            Response::Ok
        ));
    }
}

#[tokio::test]
async fn failed_input_receipt_write_keeps_queue_and_publishes_nothing() {
    use agentdocker_core::{InputReceipt, InputReport, ReceivedInput};
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let mut receiver = register(&daemon, "receipt-write-failure", None).await;
    let generation = Utc::now();
    receiver.spec.runtime = "claude-code".into();
    receiver.process_started_at = Some(generation);
    {
        let mut state = lock(&daemon.state);
        state.store.upsert_agent(&receiver).unwrap();
        *state.registry.get_mut(&receiver.id).unwrap() = receiver.clone();
    }
    let Response::Sent { message, .. } = send(&daemon, "user", "receipt-write-failure").await
    else {
        panic!("not queued")
    };
    let mut events = daemon.subscribe_events();
    lock(&daemon.state).store.reject_agent_writes_for_test();
    assert!(matches!(
        daemon
            .handle(Request::ReportInput {
                agent: receiver.id.to_string(),
                process_started_at: generation,
                observed_at: generation,
                report: InputReport::Received {
                    input: ReceivedInput {
                        messages: vec![message.clone()],
                        receipt: InputReceipt::ClaudeChannel
                    }
                },
                token: None,
            })
            .await,
        Response::Error {
            code: ErrorCode::StorageUnavailable,
            ..
        }
    ));
    let state = lock(&daemon.state);
    assert!(
        state
            .registry
            .get(&receiver.id)
            .unwrap()
            .input_delivery
            .is_none()
    );
    assert_eq!(state.inboxes.get(&receiver.id).unwrap()[0].id, message);
    assert!(events.try_recv().is_err());
}

#[tokio::test]
async fn failed_activity_write_publishes_no_state_or_event() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let agent = register(&daemon, "activity-write-failure", None).await;
    let before = daemon.recent_events(100);
    let mut events = daemon.subscribe_events();
    lock(&daemon.state).store.reject_agent_writes_for_test();
    let response = daemon
        .handle(Request::ReportActivity {
            agent: agent.id.to_string(),
            observation: agentdocker_core::ActivityObservation {
                activity: agentdocker_core::ReportedActivity::Working,
                observed_at: Utc::now(),
                reason: None,
            },
        })
        .await;
    assert!(matches!(
        response,
        Response::Error {
            code: ErrorCode::StorageUnavailable,
            ..
        }
    ));
    let state = lock(&daemon.state);
    assert_eq!(state.registry.get(&agent.id), Some(&agent));
    assert_eq!(state.store.load_agents().unwrap(), [agent]);
    drop(state);
    assert_eq!(daemon.recent_events(100), before);
    assert!(events.try_recv().is_err());
}

#[tokio::test]
async fn delayed_idle_report_cannot_become_work_when_it_expires() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let agent = register(&daemon, "delayed-report", None).await;
    let now = Utc::now();
    let mut state = lock(&daemon.state);
    let record = state.registry.get_mut(&agent.id).unwrap();
    record.created_at = now - Duration::hours(1);
    record.last_seen = record.created_at;
    state.report_activity(
        agent.id.as_str(),
        agentdocker_core::ActivityObservation {
            activity: agentdocker_core::ReportedActivity::Idle,
            observed_at: now - Duration::seconds(299),
            reason: None,
        },
        now,
    );
    let record = state.registry.get(&agent.id).unwrap();
    assert!(matches!(
        state.activity_of(record, now),
        Activity::Idle { .. }
    ));
    assert_eq!(
        state.activity_of(record, now + Duration::seconds(1)),
        Activity::Unknown
    );
    state.registry.get_mut(&agent.id).unwrap().last_seen = now;
    assert!(
        matches!(
            state.activity_of(state.registry.get(&agent.id).unwrap(), now),
            Activity::Working { .. }
        ),
        "newer coordination supersedes the old idle observation"
    );
}

#[tokio::test]
async fn polling_does_not_turn_a_hooked_agents_idle_report_into_work() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let agent = register(&daemon, "hooked", None).await;
    let now = Utc::now();
    let born = now - Duration::hours(1);
    let mut state = lock(&daemon.state);
    let record = state.registry.get_mut(&agent.id).unwrap();
    record.created_at = born;
    record.process_started_at = Some(born);
    record.adapter_contacts.insert(
        agentdocker_core::AdapterKind::Hooks,
        agentdocker_core::AdapterContact {
            process_started_at: born,
            observed_at: now - Duration::seconds(30),
        },
    );
    state.report_activity(
        agent.id.as_str(),
        agentdocker_core::ActivityObservation {
            activity: agentdocker_core::ReportedActivity::Idle,
            observed_at: now - Duration::seconds(20),
            reason: None,
        },
        now,
    );
    // Its MCP server polls for messages a few seconds later.
    state.registry.get_mut(&agent.id).unwrap().last_seen = now;
    let record = state.registry.get(&agent.id).unwrap();
    assert!(matches!(
        state.activity_of(record, now),
        Activity::Idle { .. }
    ));
    // Past the report's expiry the polling still says nothing of work.
    state.registry.get_mut(&agent.id).unwrap().last_seen = now + Duration::minutes(6);
    let record = state.registry.get(&agent.id).unwrap();
    assert_eq!(
        state.activity_of(record, now + Duration::minutes(6)),
        Activity::Unknown
    );
    // Hooks from an earlier process of the same record prove nothing
    // about this one: contact counts again, as for a hookless agent.
    state
        .registry
        .get_mut(&agent.id)
        .unwrap()
        .process_started_at = Some(now);
    let record = state.registry.get(&agent.id).unwrap();
    assert!(matches!(
        state.activity_of(record, now + Duration::minutes(6)),
        Activity::Working { .. }
    ));
}

#[tokio::test]
async fn a_deadlock_refusal_says_the_wait_ended() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    register(&daemon, "alpha", distinct_pid()).await;
    register(&daemon, "beta", distinct_pid()).await;
    assert!(matches!(
        claim(&daemon, "alpha", "task:x").await,
        Response::Lease { .. }
    ));
    assert!(matches!(
        claim(&daemon, "beta", "task:y").await,
        Response::Lease { .. }
    ));
    let alpha_waits = {
        let daemon = daemon.clone();
        tokio::spawn(async move { claim_waiting(&daemon, "alpha", "task:y", 30).await })
    };
    queued(&daemon, 1).await;

    let mut events = daemon.subscribe_events();
    let refused = claim_waiting(&daemon, "beta", "task:x", 30).await;
    assert!(matches!(
        &refused,
        Response::Error {
            code: ErrorCode::Deadlock,
            ..
        }
    ));

    // A wait that never began still ended, and `lease_wait_ended` is
    // the event subscribers are told to expect for every outcome —
    // including the one that was refused before it could queue.
    let mut saw_deadlock = false;
    let mut saw_ended = false;
    while let Ok(event) = events.try_recv() {
        match event.kind {
            EventKind::LeaseDeadlock { .. } => saw_deadlock = true,
            EventKind::LeaseWaitEnded {
                outcome: agentdocker_core::WaitOutcome::Deadlock,
                ..
            } => saw_ended = true,
            _ => {}
        }
    }
    assert!(saw_deadlock, "the cycle itself is announced");
    assert!(saw_ended, "and so is the wait ending on it");
    alpha_waits.abort();
}

// ----- contests -------------------------------------------------------

/// A registered agent in its own checkout, which is what a contest
/// entrant is.
async fn entrant(daemon: &Arc<Daemon>, root: &Path, name: &str) -> AgentRecord {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("work.txt"), name).unwrap();
    let mut spec = spec(name);
    spec.workdir = Some(dir);
    register_spec(daemon, spec).await
}

/// Run a validation for this agent and return its id.
async fn validate(daemon: &Arc<Daemon>, agent: &str, command: &str) -> Response {
    daemon
        .handle(Request::Validate {
            agent: agent.to_owned(),
            command: vec!["sh".into(), "-c".into(), command.to_owned()],
            timeout_secs: 30,
        })
        .await
}

async fn validation_id(daemon: &Arc<Daemon>, agent: &str, command: &str) -> String {
    match validate(daemon, agent, command).await {
        Response::Validation { validation, .. } => validation.id,
        other => panic!("validate failed: {other:?}"),
    }
}

async fn open_contest(
    daemon: &Arc<Daemon>,
    agent: &str,
    measure: Measure,
    noise: f64,
    entrants: Vec<String>,
) -> Contest {
    let response = daemon
        .handle(Request::ContestOpen {
            agent: agent.to_owned(),
            project: None,
            task: "make it faster".to_owned(),
            metric: Metric {
                measure,
                direction: agentdocker_core::contest::Direction::Lower,
                noise,
            },
            entrants,
            channel: true,
        })
        .await;
    match response {
        Response::Contest { contest, .. } => contest,
        other => panic!("contest_open failed: {other:?}"),
    }
}

#[tokio::test]
async fn an_entry_needs_a_passing_validation_of_its_own() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let root = dir.path().join("work");
    let alpha = entrant(&daemon, &root, "alpha").await;
    let beta = entrant(&daemon, &root, "beta").await;
    let contest = open_contest(
        &daemon,
        alpha.id.as_str(),
        Measure::Reported {
            name: "allocations".to_owned(),
        },
        0.0,
        vec![beta.id.to_string()],
    )
    .await;

    // A failing run is not evidence of anything.
    let failed = validation_id(&daemon, alpha.id.as_str(), "exit 1").await;
    let response = daemon
        .handle(Request::ContestSubmit {
            agent: alpha.id.to_string(),
            contest: contest.id.clone(),
            validation: failed,
            score: Some(1.0),
        })
        .await;
    assert!(
        matches!(
            &response,
            Response::Error {
                code: ErrorCode::Invalid,
                message,
                ..
            } if message.contains("did not pass")
        ),
        "unexpected {response:?}"
    );

    // Nor is somebody else's passing run.
    let borrowed = validation_id(&daemon, beta.id.as_str(), "true").await;
    let response = daemon
        .handle(Request::ContestSubmit {
            agent: alpha.id.to_string(),
            contest: contest.id.clone(),
            validation: borrowed,
            score: Some(1.0),
        })
        .await;
    assert!(
        matches!(
            &response,
            Response::Error {
                code: ErrorCode::Forbidden,
                ..
            }
        ),
        "unexpected {response:?}"
    );

    // Its own passing run is.
    let mine = validation_id(&daemon, alpha.id.as_str(), "true").await;
    let Response::Contest { contest, .. } = daemon
        .handle(Request::ContestSubmit {
            agent: alpha.id.to_string(),
            contest: contest.id.clone(),
            validation: mine.clone(),
            score: Some(7.0),
        })
        .await
    else {
        panic!("a passing validation of its own should be accepted")
    };
    assert_eq!(contest.entries.len(), 1);
    assert_eq!(contest.entries[0].validation, mine);
    assert_eq!(contest.entries[0].score, 7.0);
}

#[tokio::test]
async fn a_measured_contest_ignores_what_the_entrant_says_the_score_was() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let root = dir.path().join("work");
    let alpha = entrant(&daemon, &root, "alpha").await;
    let contest = open_contest(
        &daemon,
        alpha.id.as_str(),
        Measure::ValidationSeconds,
        0.0,
        vec![],
    )
    .await;

    let mine = validation_id(&daemon, alpha.id.as_str(), "true").await;
    let Response::Contest { contest, .. } = daemon
        .handle(Request::ContestSubmit {
            agent: alpha.id.to_string(),
            contest: contest.id.clone(),
            validation: mine,
            // A flattering claim, which must be ignored: the daemon
            // timed the run itself.
            score: Some(-999.0),
        })
        .await
    else {
        panic!("submit failed")
    };
    let score = contest.entries[0].score;
    assert!(
        (0.0..30.0).contains(&score),
        "the daemon's own timing, not the claim: {score}"
    );
}

#[tokio::test]
async fn a_reported_contest_needs_a_number() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let root = dir.path().join("work");
    let alpha = entrant(&daemon, &root, "alpha").await;
    let contest = open_contest(
        &daemon,
        alpha.id.as_str(),
        Measure::Reported {
            name: "allocations".to_owned(),
        },
        0.0,
        vec![],
    )
    .await;
    let mine = validation_id(&daemon, alpha.id.as_str(), "true").await;
    let response = daemon
        .handle(Request::ContestSubmit {
            agent: alpha.id.to_string(),
            contest: contest.id,
            validation: mine,
            score: None,
        })
        .await;
    assert!(
        matches!(
            &response,
            Response::Error {
                code: ErrorCode::Invalid,
                message,
                ..
            } if message.contains("allocations")
        ),
        "unexpected {response:?}"
    );
}

#[tokio::test]
async fn a_tie_cannot_be_closed_by_the_metric_and_a_clear_win_can() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let root = dir.path().join("work");
    let alpha = entrant(&daemon, &root, "alpha").await;
    let beta = entrant(&daemon, &root, "beta").await;
    let contest = open_contest(
        &daemon,
        alpha.id.as_str(),
        Measure::Reported {
            name: "allocations".to_owned(),
        },
        // A generous noise floor, so two close numbers tie.
        5.0,
        vec![beta.id.to_string()],
    )
    .await;

    for (agent, score) in [(&alpha, 100.0), (&beta, 102.0)] {
        let evidence = validation_id(&daemon, agent.id.as_str(), "true").await;
        daemon
            .handle(Request::ContestSubmit {
                agent: agent.id.to_string(),
                contest: contest.id.clone(),
                validation: evidence,
                score: Some(score),
            })
            .await;
    }

    // Two apart with a floor of five: the metric has said all it can.
    let refused = daemon
        .handle(Request::ContestClose {
            agent: alpha.id.to_string(),
            contest: contest.id.clone(),
            winner: None,
            resolution: None,
        })
        .await;
    assert!(
        matches!(
            &refused,
            Response::Error {
                code: ErrorCode::Conflict,
                message,
                ..
            } if message.contains("noise floor")
        ),
        "unexpected {refused:?}"
    );

    // Review is the tie-break, so an explicit winner settles it.
    let Response::Contest { contest, standing } = daemon
        .handle(Request::ContestClose {
            agent: alpha.id.to_string(),
            contest: contest.id.clone(),
            winner: Some(beta.id.to_string()),
            resolution: Some("clearer, and the numbers were a tie".to_owned()),
        })
        .await
    else {
        panic!("an explicit winner should settle it")
    };
    assert_eq!(contest.winner, Some(beta.id.clone()));
    assert!(!contest.is_open());
    assert!(matches!(standing, Standing::Settled { .. }));

    // And a closed contest takes nothing more.
    let evidence = validation_id(&daemon, alpha.id.as_str(), "true").await;
    let response = daemon
        .handle(Request::ContestSubmit {
            agent: alpha.id.to_string(),
            contest: contest.id,
            validation: evidence,
            score: Some(1.0),
        })
        .await;
    assert!(
        matches!(&response, Response::Error { .. }),
        "unexpected {response:?}"
    );
}

#[tokio::test]
async fn a_clear_winner_needs_no_arbiter_and_a_stranger_cannot_enter_by_submitting() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let root = dir.path().join("work");
    let alpha = entrant(&daemon, &root, "alpha").await;
    let beta = entrant(&daemon, &root, "beta").await;
    let stranger = entrant(&daemon, &root, "stranger").await;
    let contest = open_contest(
        &daemon,
        alpha.id.as_str(),
        Measure::Reported {
            name: "allocations".to_owned(),
        },
        1.0,
        vec![beta.id.to_string()],
    )
    .await;
    // A channel came with it, because a tie has to be argued
    // somewhere and asking for a room afterwards looks like a
    // rematch.
    assert!(contest.channel.is_some());

    for (agent, score) in [(&alpha, 100.0), (&beta, 40.0)] {
        let evidence = validation_id(&daemon, agent.id.as_str(), "true").await;
        daemon
            .handle(Request::ContestSubmit {
                agent: agent.id.to_string(),
                contest: contest.id.clone(),
                validation: evidence,
                score: Some(score),
            })
            .await;
    }

    // Somebody who never entered cannot submit.
    let evidence = validation_id(&daemon, stranger.id.as_str(), "true").await;
    let response = daemon
        .handle(Request::ContestSubmit {
            agent: stranger.id.to_string(),
            contest: contest.id.clone(),
            validation: evidence,
            score: Some(1.0),
        })
        .await;
    assert!(
        matches!(
            &response,
            Response::Error {
                code: ErrorCode::Forbidden,
                ..
            }
        ),
        "unexpected {response:?}"
    );

    // Sixty apart with a floor of one: no arbiter needed.
    let Response::Contest { contest, standing } = daemon
        .handle(Request::ContestClose {
            agent: alpha.id.to_string(),
            contest: contest.id.clone(),
            winner: None,
            resolution: None,
        })
        .await
    else {
        panic!("a clear win should close on the ranking")
    };
    assert_eq!(contest.winner, Some(beta.id.clone()));
    assert!(matches!(standing, Standing::Settled { winner, .. } if winner == beta.id));
}

#[tokio::test]
async fn entering_admits_a_latecomer_to_the_contests_channel() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let root = dir.path().join("work");
    let alpha = entrant(&daemon, &root, "alpha").await;
    let latecomer = entrant(&daemon, &root, "latecomer").await;
    let contest = open_contest(
        &daemon,
        alpha.id.as_str(),
        Measure::ValidationSeconds,
        0.0,
        vec![],
    )
    .await;
    let channel = contest.channel.clone().expect("a room");

    let Response::Contest { contest, .. } = daemon
        .handle(Request::ContestEnter {
            agent: latecomer.id.to_string(),
            contest: contest.id,
        })
        .await
    else {
        panic!("entering failed")
    };
    assert!(contest.has(&latecomer.id));
    let room = lock(&daemon.state)
        .channels
        .get(&channel)
        .cloned()
        .expect("the channel is still there");
    assert!(
        room.has(&latecomer.id),
        "and the latecomer can argue its own case"
    );
}

#[tokio::test]
async fn only_the_people_in_a_contest_can_close_it() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let root = dir.path().join("work");
    let alpha = entrant(&daemon, &root, "alpha").await;
    let beta = entrant(&daemon, &root, "beta").await;
    let outsider = entrant(&daemon, &root, "outsider").await;
    let contest = open_contest(
        &daemon,
        alpha.id.as_str(),
        Measure::ValidationSeconds,
        0.0,
        vec![beta.id.to_string()],
    )
    .await;
    let evidence = validation_id(&daemon, beta.id.as_str(), "true").await;
    daemon
        .handle(Request::ContestSubmit {
            agent: beta.id.to_string(),
            contest: contest.id.clone(),
            validation: evidence,
            score: None,
        })
        .await;

    // Closing settles somebody's work, so an agent that is neither
    // the opener nor an entrant may not do it.
    let refused = daemon
        .handle(Request::ContestClose {
            agent: outsider.id.to_string(),
            contest: contest.id.clone(),
            winner: Some(beta.id.to_string()),
            resolution: None,
        })
        .await;
    assert!(
        matches!(
            &refused,
            Response::Error {
                code: ErrorCode::Forbidden,
                ..
            }
        ),
        "unexpected {refused:?}"
    );

    // An entrant can, and so could the opener.
    let Response::Contest { contest, .. } = daemon
        .handle(Request::ContestClose {
            agent: beta.id.to_string(),
            contest: contest.id.clone(),
            winner: None,
            resolution: None,
        })
        .await
    else {
        panic!("an entrant should be able to close it")
    };
    assert_eq!(contest.winner, Some(beta.id));
}

#[tokio::test]
async fn one_contest_is_looked_up_rather_than_scanned_for() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let root = dir.path().join("work");
    let alpha = entrant(&daemon, &root, "alpha").await;
    let wanted = open_contest(
        &daemon,
        alpha.id.as_str(),
        Measure::ValidationSeconds,
        0.0,
        vec![],
    )
    .await;
    open_contest(
        &daemon,
        alpha.id.as_str(),
        Measure::ValidationSeconds,
        0.0,
        vec![],
    )
    .await;

    let Response::Contests { contests } = daemon
        .handle(Request::Contests {
            contest: Some(wanted.id.clone()),
            project: None,
            agent: None,
            all: true,
        })
        .await
    else {
        panic!("contests did not answer with contests")
    };
    assert_eq!(contests.len(), 1, "the one asked for, not both");
    assert_eq!(contests[0].id, wanted.id);

    let missing = daemon
        .handle(Request::Contests {
            contest: Some(agentdocker_core::ContestId::from(
                "nosuchcontest".to_owned(),
            )),
            project: None,
            agent: None,
            all: true,
        })
        .await;
    assert!(
        matches!(
            &missing,
            Response::Error {
                code: ErrorCode::NotFound,
                ..
            }
        ),
        "a lookup that finds nothing says so: {missing:?}"
    );
}

#[tokio::test]
async fn an_agent_says_which_multiplexer_it_lives_in_and_the_daemon_keeps_it() {
    use agentdocker_core::multiplexer::{Evidence, Session};
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);

    // A client registering itself is inside the session it reports,
    // and on macOS that is the only exact answer: a process's
    // environment is not readable from outside.
    let reported = Session {
        kind: "tmux".to_owned(),
        session: Some("work".to_owned()),
        pane: Some("%3".to_owned()),
        socket: Some("/private/tmp/tmux-501/default".to_owned()),
        evidence: Evidence::Environment,
    };
    let Response::Agent { agent } = daemon
        .handle(Request::Register {
            spec: spec("in-tmux"),
            pid: Some(std::process::id()),
            session: Some(reported.clone()),
        })
        .await
    else {
        panic!("register failed")
    };
    assert_eq!(
        agent.session.as_ref().map(Session::describe).as_deref(),
        Some("tmux:%3")
    );
    assert_eq!(agent.session, Some(reported));

    // And it survives, because it is part of the record rather than
    // something re-derived on every listing.
    let Response::Agent { agent } = daemon
        .handle(Request::Inspect {
            agent: agent.id.to_string(),
        })
        .await
    else {
        panic!("inspect failed")
    };
    assert_eq!(agent.session.map(|s| s.kind), Some("tmux".to_owned()));
}

#[tokio::test]
async fn an_agent_with_no_process_is_in_no_session() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    // No pid: nothing to look up, and nothing reported.
    let agent = register(&daemon, "bodiless", None).await;
    assert_eq!(agent.session, None);
}

// ----- run --in-pane --------------------------------------------------

/// Kill a tmux session however a test ends, so a failure does not
/// leave a stray server behind for the next one.
struct TmuxSession(String);

impl Drop for TmuxSession {
    fn drop(&mut self) {
        let _ = std::process::Command::new("tmux")
            .args(["kill-session", "-t", &self.0])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

fn tmux_says(pane: &str, format: &str) -> String {
    let out = std::process::Command::new("tmux")
        .args(["display-message", "-p", "-t", pane, format])
        .output()
        .expect("tmux answers");
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

#[tokio::test]
async fn an_agent_in_a_pane_is_registered_rather_than_supervised() {
    if !agentdocker_host::multiplexer::tmux::available() {
        return;
    }
    let dir = TempDir::new().unwrap();
    let work = dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let daemon = open(&dir);
    // A distinct name per run, so concurrent tests cannot collide on
    // one tmux session.
    let name = format!("paned-{}", std::process::id());
    let _cleanup = TmuxSession(name.clone());

    let mut spec = spec(&name);
    spec.workdir = Some(work.clone());
    spec.in_pane = true;
    spec.command = vec!["sh".into(), "-c".into(), "sleep 30".into()];
    let Response::Agent { agent } = daemon.handle(Request::Run { spec }).await else {
        panic!("run --in-pane failed")
    };

    // tmux owns the process, so the daemon registered it rather than
    // supervising a child of its own.
    assert!(!agent.managed, "tmux started it, not us");
    assert_eq!(agent.status, AgentStatus::Running);
    let session = agent.session.as_ref().expect("the pane is on the record");
    assert_eq!(session.kind, "tmux");
    assert_eq!(session.session.as_deref(), Some(name.as_str()));
    let pane = session.pane.as_deref().expect("a pane id");

    // And tmux agrees about which process that is, which is what
    // makes `stop`, liveness and attribution work at all.
    assert_eq!(
        tmux_says(pane, "#{pane_pid}"),
        agent.pid.expect("a pid").to_string(),
        "the record's pid is the pane's pid"
    );
    assert_eq!(tmux_says(pane, "#{session_name}"), name);

    // It can coordinate like any other agent: the pane got its id.
    assert!(matches!(
        claim(&daemon, agent.id.as_str(), "task:in-a-pane").await,
        Response::Lease { .. }
    ));
}

#[tokio::test]
async fn a_pane_agent_cannot_also_ask_for_a_terminal_or_a_restore() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let work = dir.path().to_path_buf();

    // The CLI refuses these combinations too, but the daemon has to:
    // MCP, an Agentfile and the protocol itself all bypass clap.
    let mut both = spec("two-terminals");
    both.workdir = Some(work.clone());
    both.in_pane = true;
    both.tty = true;
    both.command = vec!["sh".into()];
    let response = daemon.handle(Request::Run { spec: both }).await;
    assert!(
        matches!(&response, Response::Error { code: ErrorCode::Invalid, message, .. }
                 if message.contains("two terminals")),
        "unexpected {response:?}"
    );

    let mut restoring = spec("not-ours-to-restore");
    restoring.workdir = Some(work);
    restoring.in_pane = true;
    restoring.restore = true;
    restoring.command = vec!["sh".into()];
    let response = daemon.handle(Request::Run { spec: restoring }).await;
    assert!(
        matches!(&response, Response::Error { code: ErrorCode::Invalid, message, .. }
                 if message.contains("nothing to bring back")),
        "unexpected {response:?}"
    );

    // And a container is the engine's to start, so there is nothing
    // for tmux to own. `run_container` never reached `run_in_pane`,
    // so the flag would otherwise have been silently ignored.
    let mut containerised = spec("engine-started");
    containerised.workdir = Some(dir.path().to_path_buf());
    containerised.in_pane = true;
    containerised.command = vec!["sh".into()];
    let response = daemon
        .handle(Request::RunContainer {
            spec: containerised,
            build: "nosuchbuild".to_owned(),
            options: agentdocker_core::container::ContainerRunOptions::default(),
        })
        .await;
    assert!(
        matches!(&response, Response::Error { code: ErrorCode::Invalid, message, .. }
                 if message.contains("nothing for tmux to own")),
        "unexpected {response:?}"
    );
}

#[tokio::test]
/// Deliberately not skipped where tmux is absent: whether a request
/// is well formed does not depend on the machine, and CI proved the
/// point by failing here when the tmux probe ran first.
async fn a_pane_agent_needs_a_workdir_for_tmux_to_start_in() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let mut spec = spec("nowhere");
    spec.in_pane = true;
    spec.workdir = None;
    spec.command = vec!["sh".into()];
    let response = daemon.handle(Request::Run { spec }).await;
    assert!(
        matches!(&response, Response::Error { code: ErrorCode::Invalid, message, .. }
                 if message.contains("workdir")),
        "unexpected {response:?}"
    );
}

/// Two agents each holding what the other wants, asking at once:
/// exactly one is refused and the other simply waits.
///
/// This is the user-visible invariant — a ring is always broken, and
/// broken once — and it holds whichever order the two arrive in,
/// because the state lock serialises their first attempts.
///
/// It is **not** a test of the atomicity fix, and it is worth saying
/// so where somebody will read it. I checked: reintroducing the old
/// shape — releasing the lock between the deadlock check and the
/// join — leaves this test passing, because the window is nanoseconds
/// wide in the same task and nothing makes the other task land in it.
/// A test that did catch it would need a hook inside that window,
/// and the fix's whole point is that the window no longer exists, so
/// the hook would have to be restored by the same regression it was
/// meant to catch. That fix rests on reading the code: there is no
/// unlock between `state.deadlock(..)` and `waiting.join_locked(..)`.
// Two workers, not four: the test needs the two claims to run at
// once and nothing more, and this is the only multi-threaded runtime
// in the suite — on a machine already running one test per core,
// extra worker threads are contention every other test pays for.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_agents_closing_one_ring_leave_exactly_one_refused() {
    for round in 0..3 {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let alpha = register(&daemon, "alpha", distinct_pid()).await;
        let beta = register(&daemon, "beta", distinct_pid()).await;
        assert!(matches!(
            claim(&daemon, "alpha", "task:x").await,
            Response::Lease { .. }
        ));
        assert!(matches!(
            claim(&daemon, "beta", "task:y").await,
            Response::Lease { .. }
        ));

        let a = {
            let daemon = daemon.clone();
            let id = alpha.id.to_string();
            tokio::spawn(async move { claim_waiting(&daemon, &id, "task:y", 1).await })
        };
        let b = {
            let daemon = daemon.clone();
            let id = beta.id.to_string();
            tokio::spawn(async move { claim_waiting(&daemon, &id, "task:x", 1).await })
        };
        let outcomes = [a.await.unwrap(), b.await.unwrap()];

        let deadlocked = outcomes
            .iter()
            .filter(|r| {
                matches!(
                    r,
                    Response::Error {
                        code: ErrorCode::Deadlock,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(
            deadlocked, 1,
            "round {round}: the ring is broken exactly once, got {outcomes:?}"
        );
        // The other waited its second and reported an ordinary
        // conflict, which is the honest answer: it is queued behind a
        // lease nobody released.
        assert!(
            outcomes.iter().any(|r| matches!(
                r,
                Response::Error {
                    code: ErrorCode::Conflict,
                    ..
                }
            )),
            "round {round}: the other one simply waited: {outcomes:?}"
        );
    }
}

#[tokio::test]
async fn a_pane_agent_is_announced_as_started_once_and_only_after_tmux_has_it() {
    if !agentdocker_host::multiplexer::tmux::available() {
        return;
    }
    let dir = TempDir::new().unwrap();
    let work = dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let daemon = open(&dir);
    let name = format!("announce-{}", std::process::id());
    let _cleanup = TmuxSession(name.clone());

    let mut events = daemon.subscribe_events();
    let mut spec = spec(&name);
    spec.workdir = Some(work);
    spec.in_pane = true;
    spec.command = vec!["sh".into(), "-c".into(), "sleep 30".into()];
    let Response::Agent { agent } = daemon.handle(Request::Run { spec }).await else {
        panic!("run --in-pane failed")
    };

    // The record exists before tmux is asked for the process, so the
    // ordinary unmanaged-registration announcement is suppressed:
    // otherwise subscribers would be told an agent started that might
    // never start, and told again when it did.
    let mut starts = Vec::new();
    while let Ok(event) = events.try_recv() {
        if let EventKind::AgentStarted { agent: id, pid } = event.kind
            && id == agent.id
        {
            starts.push(pid);
        }
    }
    assert_eq!(starts.len(), 1, "announced once, not twice: {starts:?}");
    assert_eq!(
        starts[0], agent.pid,
        "and with the pid tmux actually started, never None"
    );
}

// ----- restart policies -----------------------------------------------

/// Wait until an agent's process group is really gone.
///
/// Signalling is not reaping. A test that returns while its children
/// are still dying leaves them for whatever runs next, which under a
/// parallel runner shows up as somebody else's leak.
async fn gone(pid: Option<u32>) {
    let Some(pid) = pid else { return };
    let _ = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while supervisor::group_exists(pid) {
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(-(pid as i32)),
                nix::sys::signal::Signal::SIGKILL,
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
}

/// Wait for a condition on an agent, so a test never races the
/// supervisor's own tasks.
async fn until(
    daemon: &Arc<Daemon>,
    id: &AgentId,
    what: &str,
    ready: impl Fn(&AgentRecord) -> bool,
) -> AgentRecord {
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            let found = lock(&daemon.state).registry.get(id).cloned();
            if let Some(record) = found
                && ready(&record)
            {
                return record;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

#[tokio::test]
async fn a_failing_agent_comes_back_until_its_limit_and_then_stays_down() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let mut spec = spec("flaky");
    spec.workdir = Some(dir.path().to_path_buf());
    spec.command = vec!["sh".into(), "-c".into(), "exit 7".into()];
    spec.restart = agentdocker_core::RestartPolicy::OnFailure { max: 2 };
    let Response::Agent { agent } = daemon.handle(Request::Run { spec }).await else {
        panic!("run failed")
    };

    // Two restarts, and then it is left alone: the command is broken
    // rather than flaky, and retrying forever would say nothing new.
    let settled = until(&daemon, &agent.id, "the limit to be reached", |r| {
        r.restarts >= 2 && !r.status.is_live()
    })
    .await;
    assert_eq!(settled.restarts, 2, "exactly the limit");
    // It stays that way.
    tokio::time::sleep(std::time::Duration::from_millis(600)).await;
    let after = lock(&daemon.state)
        .registry
        .get(&agent.id)
        .cloned()
        .unwrap();
    assert_eq!(after.restarts, 2, "and no more");
    assert!(!after.status.is_live());
    gone(after.pid).await;
}

#[tokio::test]
async fn a_clean_exit_is_the_end_of_an_on_failure_agent() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let mut spec = spec("tidy");
    spec.workdir = Some(dir.path().to_path_buf());
    spec.command = vec!["sh".into(), "-c".into(), "exit 0".into()];
    spec.restart = agentdocker_core::RestartPolicy::OnFailure { max: 5 };
    let Response::Agent { agent } = daemon.handle(Request::Run { spec }).await else {
        panic!("run failed")
    };
    until(&daemon, &agent.id, "it to finish", |r| !r.status.is_live()).await;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let after = lock(&daemon.state)
        .registry
        .get(&agent.id)
        .cloned()
        .unwrap();
    assert_eq!(after.restarts, 0, "nothing failed, so nothing to retry");
    assert_eq!(after.status, AgentStatus::Exited { code: Some(0) });
    gone(after.pid).await;
}

#[tokio::test]
async fn a_restart_keeps_the_agents_identity_and_says_so() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let mut spec = spec("service");
    spec.workdir = Some(dir.path().to_path_buf());
    spec.command = vec!["sh".into(), "-c".into(), "exit 1".into()];
    spec.restart = agentdocker_core::RestartPolicy::OnFailure { max: 1 };
    // Subscribed before the agent exists. `exit 1` is over almost at
    // once, and a subscription taken afterwards can miss the very
    // event this is about — the stream carries what happens after
    // you join it, not what already happened.
    let mut events = daemon.subscribe_events();
    let Response::Agent { agent } = daemon.handle(Request::Run { spec }).await else {
        panic!("run failed")
    };

    let restarted = until(&daemon, &agent.id, "one restart", |r| r.restarts >= 1).await;
    // Same identity: everything already recorded about it — its read
    // set, journal cursor, leases, ledger rows — still describes it.
    assert_eq!(restarted.id, agent.id);
    assert_eq!(restarted.spec.name, "service");

    let mut announced = None;
    while let Ok(event) = events.try_recv() {
        if let EventKind::AgentRestarted {
            agent: id, attempt, ..
        } = event.kind
            && id == agent.id
        {
            announced = Some(attempt);
        }
    }
    assert_eq!(
        announced,
        Some(1),
        "the restart is announced, with its number"
    );
    gone(restarted.pid).await;
}

#[tokio::test]
async fn an_agent_stopped_on_purpose_is_not_restarted() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let mut spec = spec("stoppable");
    spec.workdir = Some(dir.path().to_path_buf());
    spec.command = vec![
        "sh".into(),
        "-c".into(),
        "while true; do sleep 1; done".into(),
    ];
    spec.restart = agentdocker_core::RestartPolicy::Always;
    let Response::Agent { agent } = daemon.handle(Request::Run { spec }).await else {
        panic!("run failed")
    };
    until(&daemon, &agent.id, "it to be running", |r| {
        r.status == AgentStatus::Running
    })
    .await;

    daemon
        .handle(Request::Stop {
            agent: agent.id.to_string(),
            force: true,
        })
        .await;
    let stopped = until(&daemon, &agent.id, "it to stop", |r| !r.status.is_live()).await;
    // The policy is cleared on the record, so the reason it will not
    // come back is visible in `inspect` rather than hidden here.
    assert!(stopped.spec.restart.is_no(), "stopping clears the policy");

    tokio::time::sleep(std::time::Duration::from_millis(700)).await;
    let after = lock(&daemon.state)
        .registry
        .get(&agent.id)
        .cloned()
        .unwrap();
    assert_eq!(after.restarts, 0, "and it stayed stopped");
    assert!(!after.status.is_live());
    gone(after.pid).await;
}

#[tokio::test]
async fn an_unmanaged_agent_is_never_restarted_whatever_it_asks_for() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let mut spec = spec("not-ours");
    spec.restart = agentdocker_core::RestartPolicy::Always;
    let agent = register_spec(&daemon, spec).await;
    // The daemon did not start it, so it cannot start it again.
    daemon.mark_exited(&agent.id, AgentStatus::Exited { code: Some(1) });
    daemon.consider_restart(&agent.id, &AgentStatus::Exited { code: Some(1) });
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    let after = lock(&daemon.state)
        .registry
        .get(&agent.id)
        .cloned()
        .unwrap();
    assert_eq!(after.restarts, 0);
    assert!(!after.status.is_live());
}

// ----- admission policy and quotas ------------------------------------

/// Write a host policy and wait for the daemon to pick it up, which
/// it does on its own tick by modification time.
fn write_policy(daemon: &Arc<Daemon>, toml: &str) {
    std::fs::write(daemon.home.join("policy.toml"), toml).unwrap();
    daemon.reload_policies();
}

#[tokio::test]
async fn a_denied_claim_is_refused_and_says_which_rule() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let agent = register(&daemon, "writer", distinct_pid()).await;
    write_policy(
        &daemon,
        r#"
[[rule]]
name = "migrations are mine"
deny = ["claim:task:migrations"]
"#,
    );

    let mut events = daemon.subscribe_events();
    let refused = claim(&daemon, agent.id.as_str(), "task:migrations").await;
    let Response::Error {
        code,
        message,
        details,
    } = refused
    else {
        panic!("the policy should have refused it")
    };
    assert_eq!(code, ErrorCode::Forbidden);
    assert!(message.contains("migrations are mine"), "{message}");
    let details = details.expect("the rule is in the details");
    assert_eq!(details["rule"], json!("host: migrations are mine"));
    assert_eq!(details["action"], json!("claim:task:migrations"));

    // A refusal is explainable from the event stream alone.
    let mut announced = false;
    while let Ok(event) = events.try_recv() {
        if let EventKind::PolicyDenied { action, .. } = &event.kind
            && action == "claim:task:migrations"
        {
            announced = true;
        }
    }
    assert!(announced, "policy_denied is emitted");

    // Everything else is untouched.
    assert!(matches!(
        claim(&daemon, agent.id.as_str(), "task:anything-else").await,
        Response::Lease { .. }
    ));
}

#[tokio::test]
async fn a_policy_takes_effect_without_a_restart_and_a_broken_one_does_not_disarm_it() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let agent = register(&daemon, "writer", distinct_pid()).await;
    // Nothing written: everything allowed.
    assert!(matches!(
        claim(&daemon, agent.id.as_str(), "task:one").await,
        Response::Lease { .. }
    ));

    write_policy(&daemon, "[[rule]]\ndeny = [\"claim:task:**\"]\n");
    assert!(matches!(
        claim(&daemon, agent.id.as_str(), "task:two").await,
        Response::Error {
            code: ErrorCode::Forbidden,
            ..
        }
    ));

    // A file that will not parse must not read as "no rules": an
    // empty policy allows everything, so a typo would switch off
    // every rule it was written to enforce.
    std::fs::write(daemon.home.join("policy.toml"), "[[rule]\ndeny = oops").unwrap();
    daemon.reload_policies();
    assert!(
        matches!(
            claim(&daemon, agent.id.as_str(), "task:three").await,
            Response::Error {
                code: ErrorCode::Forbidden,
                ..
            }
        ),
        "the last good policy stays in force"
    );

    // Removing the file is a decision, and does take effect.
    std::fs::remove_file(daemon.home.join("policy.toml")).unwrap();
    daemon.reload_policies();
    assert!(matches!(
        claim(&daemon, agent.id.as_str(), "task:four").await,
        Response::Lease { .. }
    ));
}

#[tokio::test]
async fn a_denied_message_never_reaches_anybody() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let sender = register(&daemon, "loud", distinct_pid()).await;
    let listener = register(&daemon, "quiet", distinct_pid()).await;
    write_policy(
        &daemon,
        r#"
[[rule]]
agent = "loud"
deny = ["send:all"]
"#,
    );
    let refused = daemon
        .handle(Request::Send {
            from: sender.id.to_string(),
            to: "all".into(),
            kind: "chat".into(),
            payload: json!({ "text": "everyone!" }),
            reply_to: None,
            links: Vec::new(),
        })
        .await;
    assert!(matches!(
        &refused,
        Response::Error {
            code: ErrorCode::Forbidden,
            ..
        }
    ));
    assert!(
        inbox(&daemon, listener.id.as_str(), true).await.is_empty(),
        "a refused message is not delivered to anyone"
    );
    // Addressing one agent is still fine.
    assert!(matches!(
        daemon
            .handle(Request::Send {
                from: sender.id.to_string(),
                to: listener.id.to_string(),
                kind: "chat".into(),
                payload: json!({ "text": "just you" }),
                reply_to: None,
                links: Vec::new(),
            })
            .await,
        Response::Sent { .. }
    ));
}

#[tokio::test]
async fn a_quota_is_spent_by_several_agents_until_it_runs_out() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let alpha = register(&daemon, "alpha", distinct_pid()).await;
    let beta = register(&daemon, "beta", distinct_pid()).await;
    write_policy(&daemon, "[quota]\ntokens = 100\n");

    let take = |agent: String, amount: u64| {
        let daemon = daemon.clone();
        async move {
            daemon
                .handle(Request::Claim {
                    agent,
                    resource: "quota:tokens".into(),
                    mode: LeaseMode::Shared,
                    amount: Some(amount),
                    ttl_secs: 300,
                    note: None,
                    wait_secs: 0,
                    automatic: false,
                })
                .await
        }
    };

    // A quota is shared: several agents hold it at once.
    assert!(matches!(
        take(alpha.id.to_string(), 60).await,
        Response::Lease { .. }
    ));
    assert!(matches!(
        take(beta.id.to_string(), 30).await,
        Response::Lease { .. }
    ));
    // 90 of 100 spent; 20 does not fit.
    let refused = take(alpha.id.to_string(), 20).await;
    let Response::Error {
        code,
        message,
        details,
    } = refused
    else {
        panic!("the quota should have refused it: {refused:?}")
    };
    assert_eq!(code, ErrorCode::Conflict);
    assert!(message.contains("10 of 100 left"), "{message}");
    let details = details.expect("the arithmetic is in the details");
    assert_eq!(details["committed"], json!(90));
    assert_eq!(details["capacity"], json!(100));
    // What does fit still does.
    assert!(matches!(
        take(beta.id.to_string(), 10).await,
        Response::Lease { .. }
    ));
}

#[tokio::test]
async fn a_quota_nobody_set_a_capacity_for_is_unlimited() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let agent = register(&daemon, "spender", distinct_pid()).await;
    write_policy(&daemon, "[quota]\ntokens = 10\n");
    // A typo in a quota name must loosen nothing that was not
    // already loose, so an unmentioned quota has no ceiling.
    let response = daemon
        .handle(Request::Claim {
            agent: agent.id.to_string(),
            resource: "quota:tokns".into(),
            mode: LeaseMode::Shared,
            amount: Some(1_000_000),
            ttl_secs: 300,
            note: None,
            wait_secs: 0,
            automatic: false,
        })
        .await;
    assert!(matches!(response, Response::Lease { .. }), "{response:?}");
}

// ----- every checkout of a project --------------------------------------

/// The bug this exists for, found by auditing a real fleet: a
/// project had eight checkouts, the daemon watched the one an agent
/// was registered in, and twenty-seven commits produced four
/// journal entries. Worse, `overlap` answered "no path was changed
/// in more than one checkout" — a confident wrong answer to the
/// question the feature exists for — because it could only see one.
#[tokio::test]
async fn a_commit_in_a_worktree_nobody_registered_still_reaches_the_journal() {
    if !have_git() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("kept.txt"), "one\n").unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.name", "t"],
        vec!["config", "user.email", "t@example.com"],
        vec!["config", "commit.gpgsign", "false"],
        vec!["add", "."],
        vec!["commit", "-q", "-m", "root"],
    ] {
        assert!(git(dir.path(), &repo, &args), "{args:?}");
    }
    // A second checkout of the same repository, with no agent in it.
    let elsewhere = dir.path().join("elsewhere");
    assert!(git(
        dir.path(),
        &repo,
        &[
            "worktree",
            "add",
            "-b",
            "side",
            "--",
            elsewhere.to_str().unwrap(),
            "HEAD",
        ],
    ));

    let daemon = open(&dir);
    daemon.expect_watcher();
    tokio::spawn(crate::watcher::run(
        daemon.clone(),
        std::time::Duration::from_millis(100),
        std::time::Duration::from_millis(50),
    ));
    register_in(&daemon, "home", &repo).await;
    // The enumeration is what tells the watcher the second checkout
    // exists; on the daemon it runs on the five-second tick.
    daemon.refresh_project_checkouts().await;
    let watched: Vec<_> = daemon.watch_targets().into_iter().map(|c| c.dir).collect();
    assert!(
        watched.iter().any(|d| d.ends_with("elsewhere")),
        "the other checkout is watched: {watched:?}"
    );

    // Work happens over there, and nothing is registered there.
    std::fs::write(elsewhere.join("kept.txt"), "changed elsewhere\n").unwrap();
    assert!(git(dir.path(), &elsewhere, &["add", "."]));
    assert!(git(
        dir.path(),
        &elsewhere,
        &["commit", "-q", "-m", "work nobody registered"],
    ));

    let entries = eventually(async || {
        let Response::Journal { entries, .. } = daemon
            .handle(Request::Journal {
                project: repo.display().to_string(),
                agent: None,
                since_seq: None,
                until_seq: None,
                branch: None,
                kind: Some("commit".into()),
                path: None,
                grep: None,
                limit: 10,
                digest: None,
            })
            .await
        else {
            panic!("journal failed")
        };
        (!entries.is_empty()).then_some(entries)
    })
    .await;
    let entry = &entries[0];
    assert!(
        entry.summary.contains("work nobody registered"),
        "the commit is named: {}",
        entry.summary
    );
    assert_eq!(
        entry.checkout,
        Some(elsewhere.canonicalize().unwrap()),
        "and attributed to the checkout it happened in"
    );
}

/// A commit made with git alone in a private checkout is still
/// somebody's: the agent the daemon created the worktree for, or the
/// agent holding the checkout under an exclusive `path:` lease. Only a
/// checkout nobody made or holds is `external`.
#[tokio::test]
async fn a_commit_in_a_held_or_daemon_made_worktree_is_that_agents() {
    if !have_git() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (daemon, repo) = repo_with_agent(&dir, "writer").await;
    daemon.expect_watcher();
    tokio::spawn(crate::watcher::run(
        daemon.clone(),
        std::time::Duration::from_millis(100),
        std::time::Duration::from_millis(50),
    ));
    // One worktree through the daemon, from the root commit rather
    // than HEAD; one by hand, held under a path lease; one by hand
    // that nobody holds.
    let root_sha = {
        let out = std::process::Command::new("git")
            .args(["-C", repo.to_str().unwrap(), "rev-parse", "HEAD"])
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    };
    let made = dir.path().join("made");
    let Response::Worktree {
        path: made_path,
        branch,
        ..
    } = daemon
        .handle(Request::WorktreeCreate {
            agent: "writer".into(),
            path: made.to_string_lossy().into_owned(),
            branch: "made-branch".into(),
            from: Some(root_sha.clone()),
            seed_build: Vec::new(),
        })
        .await
    else {
        panic!("worktree_create failed");
    };
    assert_eq!(branch, "made-branch");
    assert!(matches!(
        daemon
            .handle(Request::WorktreeCreate {
                agent: "writer".into(),
                path: dir.path().join("bad").to_string_lossy().into_owned(),
                branch: "bad-branch".into(),
                from: Some("--output=/tmp/x".into()),
                seed_build: Vec::new(),
            })
            .await,
        Response::Error { .. }
    ));
    let held = dir.path().join("held");
    let loose = dir.path().join("loose");
    for (path, branch) in [(&held, "held-branch"), (&loose, "loose-branch")] {
        assert!(git(
            dir.path(),
            &repo,
            &[
                "worktree",
                "add",
                "-b",
                branch,
                "--",
                path.to_str().unwrap(),
                &root_sha
            ],
        ));
    }
    let Response::Lease { .. } = daemon
        .handle(Request::Claim {
            agent: "writer".into(),
            resource: format!("path:{}", held.canonicalize().unwrap().display()),
            amount: None,
            mode: LeaseMode::Exclusive,
            ttl_secs: 600,
            note: Some("private checkout".into()),
            wait_secs: 0,
            automatic: false,
        })
        .await
    else {
        panic!("claim failed");
    };
    daemon.refresh_project_checkouts().await;
    // The watcher needs one look at each checkout before a commit
    // there is a move.
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    for (path, text) in [
        (&made_path, "made by the daemon"),
        (&held, "held under a lease"),
        (&loose, "nobody's"),
    ] {
        std::fs::write(path.join("kept.txt"), format!("{text}\n")).unwrap();
        assert!(git(dir.path(), path, &["add", "."]));
        assert!(git(dir.path(), path, &["commit", "-q", "-m", text]));
    }
    let entries = eventually(async || {
        let Response::Journal { entries, .. } = daemon
            .handle(Request::Journal {
                project: repo.display().to_string(),
                agent: None,
                since_seq: None,
                until_seq: None,
                branch: None,
                kind: Some("commit".into()),
                path: None,
                grep: None,
                limit: 10,
                digest: None,
            })
            .await
        else {
            panic!("journal failed")
        };
        (entries.len() >= 3).then_some(entries)
    })
    .await;
    let who = |text: &str| {
        entries
            .iter()
            .find(|e| e.summary.contains(text))
            .map(|e| e.agent_name.clone())
            .unwrap_or_else(|| panic!("no entry for {text}: {entries:?}"))
    };
    assert_eq!(who("made by the daemon"), "writer");
    assert_eq!(who("held under a lease"), "writer");
    assert_eq!(who("nobody's"), "external");
}

// ----- commit ----------------------------------------------------------

/// Set up a repository with one commit and an agent registered in
/// it, with a git identity local to the repository so the test does
/// not depend on the machine having one.
async fn repo_with_agent(dir: &TempDir, name: &str) -> (Arc<Daemon>, PathBuf) {
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("kept.txt"), "one\n").unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.name", "t"],
        vec!["config", "user.email", "t@example.com"],
        vec!["config", "commit.gpgsign", "false"],
        vec!["add", "."],
        vec!["commit", "-q", "-m", "root"],
    ] {
        assert!(git(dir.path(), &repo, &args), "{args:?}");
    }
    let daemon = open(dir);
    register_in(&daemon, name, &repo).await;
    (daemon, repo)
}

async fn commit(daemon: &Arc<Daemon>, agent: &str, message: &str, all: bool) -> Response {
    daemon
        .handle(Request::Commit {
            agent: agent.into(),
            message: message.into(),
            all,
            push: false,
        })
        .await
}

/// The point of the whole request. The watcher already writes a
/// `commit` entry when it sees HEAD move, but it has to guess whose
/// it was and can only synthesise a summary from the sha. Going
/// through the daemon means the agent that asked is the agent
/// recorded, with the message it actually wrote.
#[tokio::test]
async fn a_commit_through_the_daemon_is_attributed_and_carries_its_message() {
    if !have_git() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (daemon, repo) = repo_with_agent(&dir, "writer").await;
    std::fs::write(repo.join("new.txt"), "two\n").unwrap();
    assert!(git(dir.path(), &repo, &["add", "."]));

    let Response::Committed {
        head,
        branch,
        files,
        pushed,
    } = commit(&daemon, "writer", "teach the parser about dashes", false).await
    else {
        panic!("commit failed");
    };
    assert_eq!(files, 1);
    assert!(!pushed, "nothing asked for a push");
    assert_eq!(branch.as_deref(), Some("main"));
    assert_eq!(head.len(), 40, "a full sha: {head}");

    let Response::Journal { entries, .. } = daemon
        .handle(Request::Journal {
            project: repo.display().to_string(),
            agent: Some("writer".into()),
            since_seq: None,
            until_seq: None,
            branch: None,
            kind: Some("commit".into()),
            path: None,
            grep: None,
            limit: 10,
            digest: None,
        })
        .await
    else {
        panic!("journal failed")
    };
    assert_eq!(entries.len(), 1, "exactly one entry: {entries:?}");
    let entry = &entries[0];
    assert_eq!(entry.agent_name, "writer", "attributed, not guessed");
    assert!(
        entry.summary.contains("teach the parser about dashes"),
        "the agent's own message, not a summary of the sha: {}",
        entry.summary
    );
    assert_eq!(entry.head_after.as_ref(), Some(&head));
    assert_eq!(entry.branch.as_deref(), Some("main"));
    assert!(
        daemon.recent_events(200).iter().any(|e| matches!(
            &e.kind,
            EventKind::Committed { agent, files: 1, .. } if agent.as_str() == entry.agent.as_ref().unwrap().as_str()
        )),
        "the state change is an event"
    );
}

/// A watcher running alongside must not add a second `commit` entry
/// for the same move a moment later, attributed by guesswork.
#[tokio::test]
async fn the_watcher_does_not_double_up_on_a_commit_the_daemon_made() {
    if !have_git() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (daemon, repo) = repo_with_agent(&dir, "writer").await;
    daemon.expect_watcher();
    tokio::spawn(crate::watcher::run(
        daemon.clone(),
        std::time::Duration::from_secs(60),
        std::time::Duration::from_millis(50),
    ));
    std::fs::write(repo.join("new.txt"), "two\n").unwrap();
    assert!(git(dir.path(), &repo, &["add", "."]));
    let Response::Committed { .. } = commit(&daemon, "writer", "one change", false).await else {
        panic!("commit failed");
    };
    // Long enough for the watcher to have looked several times.
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    let Response::Journal { entries, .. } = daemon
        .handle(Request::Journal {
            project: repo.display().to_string(),
            agent: None,
            since_seq: None,
            until_seq: None,
            branch: None,
            kind: Some("commit".into()),
            path: None,
            grep: None,
            limit: 10,
            digest: None,
        })
        .await
    else {
        panic!("journal failed")
    };
    assert_eq!(entries.len(), 1, "one commit, one entry: {entries:?}");
}

#[tokio::test]
async fn a_commit_with_nothing_in_it_is_refused() {
    if !have_git() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (daemon, repo) = repo_with_agent(&dir, "writer").await;

    // Nothing at all.
    let refused = commit(&daemon, "writer", "empty", false).await;
    assert!(
        matches!(&refused, Response::Error { code, .. } if *code == ErrorCode::Conflict),
        "{refused:?}"
    );

    // Changed but unstaged is still nothing, without --all: an agent
    // that meant to commit everything must say so.
    std::fs::write(repo.join("kept.txt"), "changed\n").unwrap();
    let refused = commit(&daemon, "writer", "unstaged", false).await;
    assert!(
        matches!(&refused, Response::Error { code, message, .. }
            if *code == ErrorCode::Conflict && message.contains("--all")),
        "{refused:?}"
    );

    // And with --all it goes in.
    let Response::Committed { files, .. } = commit(&daemon, "writer", "with all", true).await
    else {
        panic!("commit failed");
    };
    assert_eq!(files, 1);
}

/// A commit that fails must not leave the checkout marked. The mark
/// tells the watcher to keep out, so one left behind does not fail
/// loudly — it silently stops that checkout being journaled for as
/// long as the daemon lives.
#[tokio::test]
async fn a_failed_commit_does_not_leave_the_checkout_marked() {
    if !have_git() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (daemon, repo) = repo_with_agent(&dir, "writer").await;
    std::fs::write(repo.join("new.txt"), "two\n").unwrap();
    assert!(git(dir.path(), &repo, &["add", "."]));

    // A commit git will refuse. A pre-commit hook that says no is
    // the portable way to arrange that; the point is the failure,
    // not which failure.
    let hooks = dir.path().join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    let hook = hooks.join("pre-commit");
    let write_hook = |body: &str| {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(&hook, body).unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    write_hook("#!/bin/sh\nexit 1\n");
    assert!(git(
        dir.path(),
        &repo,
        &["config", "core.hooksPath", hooks.to_str().unwrap()],
    ));
    let refused = commit(&daemon, "writer", "cannot be made", false).await;
    assert!(
        matches!(refused, Response::Error { .. }),
        "the commit failed: {refused:?}"
    );
    assert!(
        lock(&daemon.state).committing.is_empty(),
        "and the checkout is not still marked"
    );

    // Which means the next one works, and is journaled.
    write_hook("#!/bin/sh\nexit 0\n");
    let Response::Committed { .. } = commit(&daemon, "writer", "and now it can", false).await
    else {
        panic!("commit failed")
    };
    assert!(lock(&daemon.state).committing.is_empty());
}

/// A message beginning with a dash is a message. Passing it as
/// `--message=<text>` or positionally would make git read it as a
/// flag and fail, or worse, succeed at something else.
#[tokio::test]
async fn a_message_that_looks_like_a_flag_is_still_a_message() {
    if !have_git() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (daemon, repo) = repo_with_agent(&dir, "writer").await;
    std::fs::write(repo.join("new.txt"), "two\n").unwrap();
    assert!(git(dir.path(), &repo, &["add", "."]));
    let Response::Committed { head, .. } =
        commit(&daemon, "writer", "--amend is not what I meant", false).await
    else {
        panic!("commit failed");
    };
    let subject = std::process::Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["log", "-1", "--format=%s", &head])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&subject.stdout).trim(),
        "--amend is not what I meant"
    );
    // And it is one commit on top of the root, not an amended root.
    let count = std::process::Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["rev-list", "--count", "HEAD"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&count.stdout).trim(), "2");
}

/// Committing is an action a policy can refuse, like any other.
#[tokio::test]
async fn a_policy_can_refuse_a_commit() {
    if !have_git() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (daemon, repo) = repo_with_agent(&dir, "writer").await;
    std::fs::write(
        dir.path().join("policy.toml"),
        "[[rule]]\nname = \"no commits\"\ndeny = [\"commit:**\"]\n",
    )
    .unwrap();
    daemon.reload_policies();
    std::fs::write(repo.join("new.txt"), "two\n").unwrap();
    assert!(git(dir.path(), &repo, &["add", "."]));
    let refused = commit(&daemon, "writer", "blocked", false).await;
    assert!(
        matches!(&refused, Response::Error { code, message, .. }
            if *code == ErrorCode::Forbidden && message.contains("no commits")),
        "{refused:?}"
    );
}

// ----- daemon reload --------------------------------------------------

#[tokio::test]
async fn a_linked_worktree_commit_uses_project_policy_and_writes_only_its_checkout() {
    let dir = TempDir::new().unwrap();
    let (daemon, repo) = repo_with_agent(&dir, "main-writer").await;
    let worktree = dir.path().join("linked");
    assert!(git(
        dir.path(),
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "linked",
            worktree.to_str().unwrap()
        ]
    ));
    let nested = worktree.join("nested");
    std::fs::create_dir(&nested).unwrap();
    let writer = register_in(&daemon, "linked-writer", &nested).await;
    let policy_root = writer.project.as_ref().unwrap().dir();
    assert_eq!(policy_root, worktree.canonicalize().unwrap());
    std::fs::write(worktree.join("kept.txt"), "linked change\n").unwrap();
    let policy = dir.path().join("policy.toml");
    for action in ["commit", "push"] {
        std::fs::write(
            &policy,
            format!(
                "[[rule]]\nname = \"project policy\"\ndeny = [\"{action}:{}\"]\n",
                policy_root.display()
            ),
        )
        .unwrap();
        daemon.reload_policies();
        let reply = daemon
            .handle(Request::Commit {
                agent: "linked-writer".into(),
                message: "linked change".into(),
                all: true,
                push: action == "push",
            })
            .await;
        assert!(
            matches!(
                reply,
                Response::Error {
                    code: ErrorCode::Forbidden,
                    ..
                }
            ),
            "{reply:?}"
        );
    }
    std::fs::write(policy, "").unwrap();
    daemon.reload_policies();
    let reply = commit(&daemon, "linked-writer", "linked change", true).await;
    assert!(matches!(reply, Response::Committed { .. }), "{reply:?}");
    assert_eq!(
        std::fs::read_to_string(repo.join("kept.txt")).unwrap(),
        "one\n"
    );
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(&worktree)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    assert!(output.status.success() && output.stdout.is_empty());
}

#[tokio::test]
async fn an_ordinary_shutdown_still_stops_what_it_started() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let mut spec = spec("ordinary");
    spec.workdir = Some(dir.path().to_path_buf());
    spec.command = vec![
        "sh".into(),
        "-c".into(),
        "while true; do sleep 1; done".into(),
    ];
    let Response::Agent { agent } = daemon.handle(Request::Run { spec }).await else {
        panic!("run failed")
    };
    let pid = agent.pid.expect("a pid");
    daemon.stop_all().await;
    assert!(
        !supervisor::group_exists(pid),
        "the usual shutdown still takes its children with it"
    );
    gone(Some(pid)).await;
}

async fn claim(daemon: &Arc<Daemon>, agent: &str, resource: &str) -> Response {
    daemon
        .handle(Request::Claim {
            agent: agent.to_owned(),
            resource: resource.to_owned(),
            mode: LeaseMode::Exclusive,
            amount: None,
            ttl_secs: 60,
            note: None,
            wait_secs: 0,
            automatic: false,
        })
        .await
}

async fn list_leases(daemon: &Arc<Daemon>) -> Vec<Lease> {
    match daemon
        .handle(Request::Leases {
            agent: None,
            resource: None,
        })
        .await
    {
        Response::Leases { leases } => leases,
        other => panic!("unexpected {other:?}"),
    }
}

async fn inbox(daemon: &Arc<Daemon>, agent: &str, drain: bool) -> Vec<Envelope> {
    match daemon
        .handle(Request::Inbox {
            agent: agent.to_owned(),
            drain,
        })
        .await
    {
        Response::Messages { messages } => messages,
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test]
async fn failed_removal_retains_memory_and_never_reports_success() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let receiver = register(&daemon, "receiver", None).await;
    assert!(matches!(
        daemon
            .handle(Request::Send {
                from: "user".into(),
                to: "receiver".into(),
                kind: "chat".into(),
                payload: json!({"text":"retain if removal fails"}),
                reply_to: None,
                links: Vec::new(),
            })
            .await,
        Response::Sent { .. }
    ));
    assert!(matches!(
        daemon
            .handle(Request::Deregister {
                agent: receiver.id.to_string(),
            })
            .await,
        Response::Agent { .. }
    ));
    let mut events = daemon.subscribe_events();
    let next_seq = {
        let state = lock(&daemon.state);
        state.store.reject_event_for_test("agent_removed");
        state.next_seq
    };
    let response = daemon
        .handle(Request::Remove {
            agent: receiver.id.to_string(),
        })
        .await;
    assert!(matches!(
        response,
        Response::Error {
            code: ErrorCode::StorageUnavailable,
            ..
        }
    ));
    let state = lock(&daemon.state);
    assert!(state.registry.get(&receiver.id).is_some());
    assert_eq!(state.inboxes[&receiver.id].len(), 1);
    assert_eq!(state.store.load_inboxes().unwrap()[&receiver.id].len(), 1);
    assert!(
        state
            .store
            .load_agents()
            .unwrap()
            .iter()
            .any(|a| a.id == receiver.id)
    );
    assert_eq!(state.next_seq, next_seq);
    assert!(events.try_recv().is_err());
}

#[tokio::test]
async fn destructive_inbox_failure_retains_queue_events_and_subscription_routing() {
    for event_failure in [false, true] {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let receiver = register(&daemon, "receiver", None).await;
        assert!(matches!(
            daemon
                .handle(Request::Send {
                    from: "user".into(),
                    to: "receiver".into(),
                    kind: "chat".into(),
                    payload: json!({"text":"retain after failed drain"}),
                    reply_to: None,
                    links: Vec::new(),
                })
                .await,
            Response::Sent { .. }
        ));
        let queued = inbox(&daemon, "receiver", false).await;
        let mut events = daemon.subscribe_events();
        let next_seq = {
            let state = lock(&daemon.state);
            if event_failure {
                state.store.reject_event_for_test("inbox_acknowledged");
            } else {
                state.store.reject_writes_for_test();
            }
            state.next_seq
        };
        let response = daemon
            .handle(Request::Inbox {
                agent: "receiver".into(),
                drain: true,
            })
            .await;
        assert!(
            matches!(
                response,
                Response::Error {
                    code: ErrorCode::StorageUnavailable,
                    ..
                }
            ),
            "{response:?}"
        );
        let state = lock(&daemon.state);
        assert_eq!(
            state.inboxes[&receiver.id]
                .iter()
                .map(|message| &message.id)
                .collect::<Vec<_>>(),
            queued.iter().map(|message| &message.id).collect::<Vec<_>>()
        );
        assert_eq!(
            state.store.load_inboxes().unwrap()[&receiver.id].len(),
            queued.len()
        );
        assert_eq!(state.next_seq, next_seq);
        assert!(!state.live_subscribers.contains_key(&receiver.id));
        assert!(events.try_recv().is_err());
    }
}

#[tokio::test]
async fn failed_inbox_touch_does_not_acknowledge_messages() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let receiver = register(&daemon, "receiver", None).await;
    assert!(matches!(
        daemon
            .handle(Request::Send {
                from: "user".into(),
                to: "receiver".into(),
                kind: "chat".into(),
                payload: json!({"text":"retain after failed touch"}),
                reply_to: None,
                links: Vec::new(),
            })
            .await,
        Response::Sent { .. }
    ));
    let mut events = daemon.subscribe_events();
    let next_seq = {
        let state = lock(&daemon.state);
        state.store.reject_agent_writes_for_test();
        state.next_seq
    };
    let response = daemon
        .handle(Request::Inbox {
            agent: "receiver".into(),
            drain: true,
        })
        .await;
    assert!(matches!(
        response,
        Response::Error {
            code: ErrorCode::StorageUnavailable,
            ..
        }
    ));
    let state = lock(&daemon.state);
    assert_eq!(state.inboxes[&receiver.id].len(), 1);
    assert_eq!(state.store.load_inboxes().unwrap()[&receiver.id].len(), 1);
    assert_eq!(state.next_seq, next_seq);
    assert!(events.try_recv().is_err());
}

#[tokio::test]
async fn destructive_inbox_success_commits_one_acknowledgement_before_delivery() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let receiver = register(&daemon, "receiver", None).await;
    for text in ["first", "second"] {
        assert!(matches!(
            daemon
                .handle(Request::Send {
                    from: "user".into(),
                    to: "receiver".into(),
                    kind: "chat".into(),
                    payload: json!({"text":text}),
                    reply_to: None,
                    links: Vec::new(),
                })
                .await,
            Response::Sent { .. }
        ));
    }
    let mut events = daemon.subscribe_events();
    let delivered = inbox(&daemon, "receiver", true).await;
    assert_eq!(delivered.len(), 2);
    let event = events.try_recv().unwrap();
    assert!(
        matches!(&event.kind, EventKind::InboxAcknowledged { agent, messages }
            if agent == &receiver.id && *messages == delivered.iter().map(|message| message.id.clone()).collect::<Vec<_>>())
    );
    let state = lock(&daemon.state);
    assert!(
        state
            .inboxes
            .get(&receiver.id)
            .is_none_or(|queue| queue.is_empty())
    );
    assert!(
        !state
            .store
            .load_inboxes()
            .unwrap()
            .contains_key(&receiver.id)
    );
    assert_eq!(state.store.recent_events(1).unwrap()[0].seq, event.seq);
    assert!(events.try_recv().is_err());
}

#[tokio::test]
async fn stream_reconnect_and_restart_replay_unacknowledged_human_and_peer_input() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let receiver = register(&daemon, "receiver", None).await;
    register(&daemon, "peer", None).await;
    assert!(matches!(
        send(&daemon, "user", "receiver").await,
        Response::Sent { .. }
    ));
    let (mut stream, mut live) = daemon.subscribe(Some("receiver"), vec![]).unwrap();
    let mut accepted = stream.take_backlog();
    assert_eq!(accepted.len(), 1);
    for from in ["peer", "user", "peer"] {
        assert!(matches!(
            send(&daemon, from, "receiver").await,
            Response::Sent { .. }
        ));
        accepted.push(live.try_recv().unwrap());
    }
    assert_eq!(inbox(&daemon, "receiver", false).await, accepted);
    drop(stream);
    drop(live);
    let (mut reconnected, _) = daemon.subscribe(Some("receiver"), vec![]).unwrap();
    assert_eq!(reconnected.take_backlog(), accepted);
    drop(reconnected);
    drop(daemon);
    let daemon = open(&dir);
    let (mut reconnected, _) = daemon.subscribe(Some("receiver"), vec![]).unwrap();
    assert_eq!(reconnected.take_backlog(), accepted);
    assert_eq!(
        lock(&daemon.state).inbox_bytes[&receiver.id],
        accepted.iter().map(message_bytes).sum::<usize>()
    );
    assert!(matches!(
        daemon
            .handle(Request::AckInbox {
                agent: "receiver".into(),
                messages: accepted
                    .iter()
                    .take(2)
                    .map(|message| message.id.clone())
                    .collect(),
            })
            .await,
        Response::Ok
    ));
    assert_eq!(inbox(&daemon, "receiver", false).await, accepted[2..]);
    assert_eq!(
        lock(&daemon.state).inbox_bytes[&receiver.id],
        accepted[2..].iter().map(message_bytes).sum::<usize>()
    );
    drop(reconnected);
    drop(daemon);
    assert_eq!(inbox(&open(&dir), "receiver", false).await, accepted[2..]);
}

#[tokio::test]
async fn subscription_does_not_write_an_acknowledgement() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    register(&daemon, "receiver", None).await;
    assert!(matches!(
        send(&daemon, "user", "receiver").await,
        Response::Sent { .. }
    ));
    let accepted = inbox(&daemon, "receiver", false).await;
    let mut events = daemon.subscribe_events();
    lock(&daemon.state).store.reject_writes_for_test();
    let (mut stream, _) = daemon.subscribe(Some("receiver"), vec![]).unwrap();
    assert_eq!(stream.take_backlog(), accepted);
    assert!(events.try_recv().is_err());
    assert!(lock(&daemon.state).storage_error.is_none());
}

#[tokio::test]
async fn full_inbox_rejects_entire_broadcast_without_evicting_accepted_work() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let full = register(&daemon, "full", None).await;
    let other = register(&daemon, "other", None).await;
    let (stream, mut live) = daemon.subscribe(Some("other"), vec![]).unwrap();
    for _ in 0..INBOX_CAPACITY {
        assert!(matches!(
            send(&daemon, "user", "full").await,
            Response::Sent { .. }
        ));
    }
    // Discard this raw bus receiver's unrelated traffic before checking
    // that the rejected broadcast never reaches even a healthy recipient.
    while live.try_recv().is_ok() {}
    let accepted = inbox(&daemon, "full", false).await;
    let mut events = daemon.subscribe_events();
    let next_seq = lock(&daemon.state).next_seq;
    let response = send(&daemon, "user", "all").await;
    assert!(
        matches!(
            response,
            Response::Error {
                code: ErrorCode::Backpressure,
                ..
            }
        ),
        "{response:?}"
    );
    assert!(live.try_recv().is_err());
    assert!(events.try_recv().is_err());
    {
        let state = lock(&daemon.state);
        assert_eq!(state.next_seq, next_seq);
        assert!(state.storage_error.is_none());
        let stored = state.store.load_inboxes().unwrap();
        assert_eq!(
            stored[&full.id].iter().cloned().collect::<Vec<_>>(),
            accepted
        );
        assert!(!stored.contains_key(&other.id));
    }
    assert_eq!(inbox(&daemon, "full", true).await, accepted);
    assert!(matches!(
        send(&daemon, "user", "all").await,
        Response::Sent { .. }
    ));
    assert_eq!(inbox(&daemon, "full", false).await.len(), 1);
    assert_eq!(inbox(&daemon, "other", false).await.len(), 1);
    drop(stream);
}

#[tokio::test]
async fn inbox_byte_pressure_survives_restart_and_acknowledgement_releases_capacity() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    register(&daemon, "receiver", None).await;
    let payload = json!({"text": "x".repeat(512 * 1024)});
    let request = Request::Send {
        from: "user".into(),
        to: "receiver".into(),
        kind: "chat".into(),
        payload,
        reply_to: None,
        links: Vec::new(),
    };
    for _ in 0..7 {
        assert!(matches!(
            daemon.handle(request.clone()).await,
            Response::Sent { .. }
        ));
    }
    assert!(matches!(
        daemon.handle(request.clone()).await,
        Response::Error {
            code: ErrorCode::Backpressure,
            ..
        }
    ));
    let accepted = inbox(&daemon, "receiver", false).await;
    drop(daemon);
    let daemon = open(&dir);
    assert!(matches!(
        daemon.handle(request.clone()).await,
        Response::Error {
            code: ErrorCode::Backpressure,
            ..
        }
    ));
    assert_eq!(inbox(&daemon, "receiver", false).await, accepted);
    assert!(matches!(
        daemon
            .handle(Request::AckInbox {
                agent: "receiver".into(),
                messages: vec![accepted[0].id.clone()]
            })
            .await,
        Response::Ok
    ));
    assert!(matches!(
        daemon.handle(request).await,
        Response::Sent { .. }
    ));
    let remaining = inbox(&daemon, "receiver", false).await;
    assert_eq!(remaining.len(), 7);
    assert_eq!(remaining[..6], accepted[1..]);
}

#[tokio::test]
async fn legacy_duplicate_channel_members_receive_one_durable_message() {
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("project");
    std::fs::create_dir(&project).unwrap();
    std::fs::write(project.join("Agentfile.toml"), "").unwrap();
    let daemon = open(&dir);
    register_in(&daemon, "sender", &project).await;
    let receiver = register_in(&daemon, "receiver", &project).await;
    let Response::Channel { mut channel } = daemon
        .handle(Request::ChannelOpen {
            agent: "sender".into(),
            task: "legacy membership".into(),
            members: vec!["receiver".into()],
            name: None,
            project: None,
        })
        .await
    else {
        panic!("channel failed")
    };
    inbox(&daemon, "receiver", true).await;
    channel.members.push(receiver.id.clone());
    lock(&daemon.state)
        .store
        .put_document("channel", channel.id.as_str(), &channel)
        .unwrap();
    drop(daemon);
    let daemon = open(&dir);
    assert!(matches!(
        send(&daemon, "sender", &format!("channel:{}", channel.id)).await,
        Response::Sent { .. }
    ));
    let queued = inbox(&daemon, "receiver", false).await;
    assert_eq!(queued.len(), 1);
    let state = lock(&daemon.state);
    assert_eq!(state.store.load_inboxes().unwrap()[&receiver.id].len(), 1);
    assert_eq!(state.inbox_bytes[&receiver.id], message_bytes(&queued[0]));
}

#[tokio::test]
async fn inbox_receipts_name_only_real_unacknowledged_ids_once() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let receiver = register(&daemon, "receiver", None).await;
    assert!(matches!(
        send(&daemon, "user", "receiver").await,
        Response::Sent { .. }
    ));
    let accepted = inbox(&daemon, "receiver", false).await;
    let bogus = MessageId::from("never-sent".to_owned());
    let mut events = daemon.subscribe_events();
    let next_seq = lock(&daemon.state).next_seq;
    assert!(matches!(
        daemon
            .handle(Request::AckInbox {
                agent: "receiver".into(),
                messages: vec![bogus.clone()],
            })
            .await,
        Response::Ok
    ));
    assert_eq!(lock(&daemon.state).next_seq, next_seq);
    assert!(events.try_recv().is_err());
    let ids = vec![bogus, accepted[0].id.clone(), accepted[0].id.clone()];
    for _ in 0..2 {
        assert!(matches!(
            daemon
                .handle(Request::AckInbox {
                    agent: "receiver".into(),
                    messages: ids.clone(),
                })
                .await,
            Response::Ok
        ));
    }
    let event = events.try_recv().unwrap();
    assert!(
        matches!(event.kind, EventKind::InboxAcknowledged { agent, messages }
        if agent == receiver.id && messages == vec![accepted[0].id.clone()])
    );
    assert!(events.try_recv().is_err());
    assert_eq!(lock(&daemon.state).next_seq, next_seq + 1);
    assert!(inbox(&daemon, "receiver", false).await.is_empty());
}

#[tokio::test]
async fn codex_input_reserves_the_same_durable_human_and_peer_queue() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let mut receiver = register(&daemon, "receiver", None).await;
    receiver.managed = true;
    receiver.spec.runtime = "codex".into();
    receiver.spec.env.insert(
        agentdocker_host::provider_input::CODEX_INPUT_ENV.into(),
        "1".into(),
    );
    {
        let mut state = lock(&daemon.state);
        state.store.upsert_agent(&receiver).unwrap();
        *state.registry.get_mut(&receiver.id).unwrap() = receiver.clone();
    }
    let mut ids = Vec::new();
    for sender in ["user", "peer", "user"] {
        let Response::Sent { message, .. } = send(&daemon, sender, "receiver").await else {
            panic!("not queued");
        };
        ids.push(message);
    }
    // Legacy consumers are told the queue is the bridge's, not
    // refused: nothing for them to deliver. A look without draining
    // stays open to a person.
    for request in [
        Request::Inbox {
            agent: "receiver".into(),
            drain: true,
        },
        Request::AckInbox {
            agent: "receiver".into(),
            messages: ids.clone(),
        },
        Request::DeliveryQueue {
            agent: "receiver".into(),
        },
    ] {
        assert!(matches!(
            daemon.handle(request).await,
            Response::InputOwned { ref owner, controller: None, .. } if owner == "bridge"
        ));
    }
    assert!(matches!(
        daemon
            .handle(Request::Inbox {
                agent: "receiver".into(),
                drain: false,
            })
            .await,
        Response::Messages { ref messages } if messages.len() == 3
    ));
    assert!(daemon.subscribe(Some("receiver"), Vec::new()).is_err());
    let read = || Request::ProviderInbox {
        agent: "receiver".into(),
        acknowledge: Vec::new(),
        token: None,
    };
    let Response::Messages { messages } = daemon.handle(read()).await else {
        panic!("missing queue");
    };
    assert_eq!(
        messages.iter().map(|m| &m.id).collect::<Vec<_>>(),
        ids.iter().collect::<Vec<_>>()
    );
    let ack = || Request::ProviderInbox {
        agent: "receiver".into(),
        acknowledge: vec![ids[0].clone()],
        token: None,
    };
    for _ in 0..2 {
        let Response::Messages { messages } = daemon.handle(ack()).await else {
            panic!("ack refused");
        };
        assert_eq!(
            messages.iter().map(|m| &m.id).collect::<Vec<_>>(),
            ids[1..].iter().collect::<Vec<_>>()
        );
    }
    drop(daemon);
    let daemon = open(&dir);
    let Response::Messages { messages } = daemon.handle(read()).await else {
        panic!("queue lost on reopen");
    };
    assert_eq!(
        messages.iter().map(|m| &m.id).collect::<Vec<_>>(),
        ids[1..].iter().collect::<Vec<_>>()
    );
    register(&daemon, "ordinary", None).await;
    assert!(matches!(
        daemon
            .handle(Request::ProviderInbox {
                agent: "ordinary".into(),
                acknowledge: Vec::new(),
                token: None,
            })
            .await,
        Response::Error {
            code: ErrorCode::Invalid,
            ..
        }
    ));
}

#[tokio::test]
async fn inbox_ack_is_idempotent_and_preserves_new_arrivals() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    register(&daemon, "receiver", None).await;
    for text in ["first", "second"] {
        daemon
            .handle(Request::Send {
                from: "user".into(),
                to: "receiver".into(),
                kind: "chat".into(),
                payload: json!({"text": text}),
                reply_to: None,
                links: Vec::new(),
            })
            .await;
    }
    let first = inbox(&daemon, "receiver", false).await[0].id.clone();
    for _ in 0..2 {
        assert!(matches!(
            daemon
                .handle(Request::AckInbox {
                    agent: "receiver".into(),
                    messages: vec![first.clone()],
                })
                .await,
            Response::Ok
        ));
    }
    let remaining = inbox(&daemon, "receiver", false).await;
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].payload["text"], "second");
    assert_eq!(
        lock(&daemon.state)
            .store
            .load_inboxes()
            .unwrap()
            .values()
            .next()
            .unwrap()
            .len(),
        1
    );
}

/// A pid that certainly no longer exists: a child we already reaped.
fn dead_pid() -> u32 {
    let child = std::process::Command::new("true").spawn().unwrap();
    let pid = child.id();
    let mut child = child;
    child.wait().unwrap();
    pid
}

#[tokio::test]
async fn state_survives_restart() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    // A real pid, because this one does sweep for liveness at the
    // end and expects to find alpha still there.
    let alpha = register(&daemon, "alpha", Some(std::process::id())).await;
    assert!(matches!(
        claim(&daemon, "alpha", "task:1").await,
        Response::Lease { .. }
    ));
    daemon
        .handle(Request::Send {
            from: "user".into(),
            to: "alpha".into(),
            kind: "chat".into(),
            payload: json!({ "text": "hello" }),
            reply_to: None,
            links: Vec::new(),
        })
        .await;
    drop(daemon);

    let daemon = open(&dir);
    let agents = lock(&daemon.state).registry.list(true);
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].id, alpha.id);
    assert!(agents[0].status.is_live());

    let leases = list_leases(&daemon).await;
    assert_eq!(leases.len(), 1);
    assert_eq!(leases[0].holder, alpha.id);
    // The restored lease still blocks others.
    assert!(matches!(
        register(&daemon, "beta", None).await.status,
        AgentStatus::Running
    ));
    assert!(matches!(
        claim(&daemon, "beta", "task:1").await,
        Response::Error {
            code: ErrorCode::Conflict,
            ..
        }
    ));

    let queued = inbox(&daemon, "alpha", true).await;
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].payload["text"], "hello");
    drop(daemon);

    // Draining was persisted too.
    let daemon = open(&dir);
    assert!(inbox(&daemon, "alpha", false).await.is_empty());
    // Our own pid is alive, so the liveness check leaves alpha alone.
    daemon.check_liveness();
    assert!(daemon.is_live(&alpha.id));
}

#[tokio::test]
async fn vanished_process_is_recorded_as_exited_and_freed() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let ghost = register(&daemon, "ghost", Some(dead_pid())).await;
    assert!(matches!(
        claim(&daemon, "ghost", "path:/tmp/x").await,
        Response::Lease { .. }
    ));
    // Without a pid there is nothing to poll, so this one stays live.
    let quiet = register(&daemon, "quiet", None).await;

    daemon.check_liveness();

    assert!(!daemon.is_live(&ghost.id));
    assert!(daemon.is_live(&quiet.id));
    assert!(list_leases(&daemon).await.is_empty());
    drop(daemon);

    let daemon = open(&dir);
    assert!(!daemon.is_live(&ghost.id));
    assert!(lock(&daemon.state).leases.is_empty());
}

#[tokio::test]
async fn remove_forgets_agent_and_inbox() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let done = register(&daemon, "done", None).await;
    daemon
        .handle(Request::Send {
            from: "user".into(),
            to: "done".into(),
            kind: "chat".into(),
            payload: json!({ "text": "late" }),
            reply_to: None,
            links: Vec::new(),
        })
        .await;
    daemon
        .handle(Request::Deregister {
            agent: "done".into(),
        })
        .await;
    assert!(matches!(
        daemon
            .handle(Request::Remove {
                agent: "done".into()
            })
            .await,
        Response::Ok
    ));
    drop(daemon);

    let daemon = open(&dir);
    assert!(lock(&daemon.state).registry.get(&done.id).is_none());
    assert!(lock(&daemon.state).inboxes.is_empty());
}

#[tokio::test]
async fn remove_takes_the_agents_captured_output_with_it() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let done = register(&daemon, "done", None).await;
    let log = daemon.log_path(&done.id);
    std::fs::create_dir_all(log.parent().unwrap()).unwrap();
    std::fs::write(&log, "later\n").unwrap();
    std::fs::write(paths::rotated_log(&log), "earlier\n").unwrap();
    let controller = controller_log(&daemon.home, &done.id);
    std::fs::write(&controller, "receiver\n").unwrap();
    std::fs::write(paths::rotated_log(&controller), "receiver, earlier\n").unwrap();
    daemon
        .handle(Request::Deregister {
            agent: "done".into(),
        })
        .await;
    assert!(matches!(
        daemon
            .handle(Request::Remove {
                agent: "done".into()
            })
            .await,
        Response::Ok
    ));
    assert!(!log.exists());
    assert!(!paths::rotated_log(&log).exists());
    assert!(!controller.exists());
    assert!(!paths::rotated_log(&controller).exists());
}

#[tokio::test]
async fn retention_prunes_only_old_logs_of_agents_nobody_knows() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let known = register(&daemon, "known", None).await;
    let logs = daemon.home.join("logs");
    std::fs::create_dir_all(&logs).unwrap();
    let hours_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 60 * 60);
    let mut kept = vec![
        daemon.log_path(&known.id),
        controller_log(&daemon.home, &known.id),
        logs.join("validation-abc.log"),
        logs.join("notes.txt"),
        logs.join("fresh-orphan.log"),
    ];
    let stale = [
        logs.join("orphan.log"),
        logs.join("orphan.log.1"),
        logs.join("orphan.controller.log"),
    ];
    for path in kept.iter().chain(&stale) {
        std::fs::write(path, "x").unwrap();
    }
    for path in kept.iter().take(4).chain(&stale) {
        std::fs::File::open(path)
            .unwrap()
            .set_modified(hours_ago)
            .unwrap();
    }
    daemon.prune_logs();
    kept.sort();
    let mut left: Vec<_> = std::fs::read_dir(&logs)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    left.sort();
    assert_eq!(left, kept);
}

#[test]
fn diagnostic_logs_are_trimmed_in_place_past_their_cap() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let log = paths::daemon_log(&daemon.home);
    let rotated = paths::rotated_log(&log);
    std::fs::write(&log, "short\n").unwrap();
    daemon.trim_diagnostic_logs();
    assert_eq!(std::fs::read_to_string(&log).unwrap(), "short\n");
    assert!(!rotated.exists());

    let long = "line\n".repeat((DAEMON_LOG_CAP as usize / 5) + 1);
    std::fs::write(&log, &long).unwrap();
    let controller = controller_log(&daemon.home, &AgentId::from("receiver"));
    std::fs::create_dir_all(controller.parent().unwrap()).unwrap();
    std::fs::write(&controller, &long).unwrap();
    let session = daemon.log_path(&AgentId::from("session"));
    std::fs::write(&session, &long).unwrap();
    daemon.trim_diagnostic_logs();
    assert_eq!(std::fs::metadata(&log).unwrap().len(), 0);
    assert_eq!(std::fs::read_to_string(&rotated).unwrap(), long);
    assert_eq!(std::fs::metadata(&controller).unwrap().len(), 0);
    assert_eq!(
        std::fs::read_to_string(paths::rotated_log(&controller)).unwrap(),
        long
    );
    assert_eq!(
        std::fs::metadata(&session).unwrap().len(),
        long.len() as u64,
        "a session's captured output is the owner's to rotate"
    );
}

#[test]
fn a_log_names_its_owner_whatever_its_suffix() {
    assert_eq!(log_owner("abc.log"), Some("abc"));
    assert_eq!(log_owner("abc.log.1"), Some("abc"));
    assert_eq!(log_owner("abc.controller.log"), Some("abc"));
    assert_eq!(log_owner("abc.controller.log.1"), Some("abc"));
    assert_eq!(log_owner("validation-x.log"), Some("validation-x"));
    assert_eq!(log_owner("notes.txt"), None);
}

#[tokio::test]
async fn events_are_stored_for_replay() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    register(&daemon, "a", None).await;
    let recent = daemon.recent_events(10);
    assert!(matches!(
        recent.as_slice(),
        [
            Event {
                kind: EventKind::AgentCreated { .. },
                ..
            },
            Event {
                kind: EventKind::AgentStarted { .. },
                ..
            }
        ]
    ));
    assert!(daemon.recent_events(0).is_empty());
}

#[tokio::test]
async fn event_seqs_increase_and_survive_restart() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let mut receiver = daemon.subscribe_events();
    register(&daemon, "a", None).await;
    let replayed = daemon.recent_events(10);
    assert_eq!(replayed.len(), 2);
    assert!(replayed[0].seq < replayed[1].seq);
    // The same event reaches a live subscriber with the same seq, which
    // is what lets the server drop it after a replay.
    let live = receiver.recv().await.unwrap();
    assert_eq!(live.seq, replayed[0].seq);
    drop(daemon);

    let daemon = open(&dir);
    register(&daemon, "b", None).await;
    let all = daemon.recent_events(10);
    assert_eq!(all.len(), 4);
    assert!(all.windows(2).all(|pair| pair[0].seq < pair[1].seq));
}

#[tokio::test]
async fn spawning_agents_are_not_reaped() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    // What `run` looks like between registry insert and spawn completing.
    let record = AgentRecord::new(spec("spawning"), true, Utc::now());
    let id = record.id.clone();
    lock(&daemon.state).registry.insert(record).unwrap();

    daemon.check_liveness();
    assert!(daemon.is_live(&id));
}

#[tokio::test]
async fn restore_retires_half_spawned_agents_and_orphaned_leases() {
    let dir = TempDir::new().unwrap();
    let now = Utc::now();
    let first_id;
    {
        let store = Store::open(&dir.path().join("state.db")).unwrap();
        let mut first = AgentRecord::new(spec("twin"), false, now);
        first.status = AgentStatus::Running;
        first_id = first.id.clone();
        let mut second = AgentRecord::new(spec("twin"), false, now + Duration::seconds(1));
        second.status = AgentStatus::Exited { code: Some(0) };
        let half_spawned = AgentRecord::new(spec("half"), true, now);
        for record in [&first, &second, &half_spawned] {
            store.upsert_agent(record).unwrap();
        }
        let lease = |id: &str, holder: AgentId, resource: &str| Lease {
            id: LeaseId::from(id),
            resource: ResourceKey::new(resource),
            holder,
            mode: LeaseMode::Exclusive,
            acquired_at: now,
            change_seq: None,
            expires_at: now + Duration::hours(1),
            note: None,
            amount: 0,
            automatic: false,
        };
        store
            .upsert_lease(&lease("kept", first.id.clone(), "task:kept"))
            .unwrap();
        store
            .upsert_lease(&lease("orphan", AgentId::from("ghost"), "task:orphan"))
            .unwrap();
        store
            .upsert_lease(&lease("twin2", second.id.clone(), "task:twin2"))
            .unwrap();
    }

    let daemon = open(&dir);
    let agents = lock(&daemon.state).registry.list(true);
    assert_eq!(agents.len(), 3, "every stored record is still listed");
    let live: Vec<&AgentRecord> = agents.iter().filter(|a| a.status.is_live()).collect();
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].id, first_id);
    assert!(
        agents
            .iter()
            .any(|a| a.spec.name == "half" && matches!(a.status, AgentStatus::Failed { .. }))
    );

    let leases = list_leases(&daemon).await;
    assert_eq!(leases.len(), 1);
    assert_eq!(leases[0].id, LeaseId::from("kept"));
    let events = daemon.recent_events(100);
    let half = agents
        .iter()
        .find(|record| record.spec.name == "half")
        .unwrap();
    assert!(events.iter().any(|event| matches!(&event.kind,
        EventKind::AgentExited { agent, status } if agent == &half.id && status == &half.status)));
    let mut removed: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::LeaseReleased { lease } => Some(lease.id.to_string()),
            _ => None,
        })
        .collect();
    removed.sort();
    assert_eq!(removed, ["orphan", "twin2"]);
    drop(daemon);
    let daemon = open(&dir);
    assert_eq!(
        daemon.recent_events(100),
        events,
        "restart does not duplicate cleanup events"
    );
    drop(daemon);

    // The retired records and dropped leases were written back.
    let store = Store::open(&dir.path().join("state.db")).unwrap();
    assert_eq!(store.load_leases().unwrap().len(), 1);
    let live_in_store = store
        .load_agents()
        .unwrap()
        .into_iter()
        .filter(|a| a.status.is_live())
        .count();
    assert_eq!(live_in_store, 1);
}

#[test]
fn ambiguous_live_names_refuse_before_recovery_changes_any_record_or_lease() {
    let dir = TempDir::new().unwrap();
    let database = dir.path().join("state.db");
    let store = Store::open(&database).unwrap();
    let now = Utc::now();
    // This record would normally be marked failed during startup. Its
    // earlier position must not permit a partial recovery before refusal.
    let half_spawned = AgentRecord::new(spec("half-spawned"), true, now);
    let mut first = AgentRecord::new(spec("ambiguous"), false, now + Duration::seconds(1));
    first.status = AgentStatus::Running;
    let mut second = AgentRecord::new(spec("ambiguous"), false, now + Duration::seconds(2));
    second.status = AgentStatus::Running;
    for record in [&half_spawned, &first, &second] {
        store.upsert_agent(record).unwrap();
    }
    let protection = Lease {
        id: LeaseId::from("still-owned"),
        resource: ResourceKey::new("task:protected"),
        holder: second.id.clone(),
        mode: LeaseMode::Exclusive,
        acquired_at: now,
        change_seq: None,
        expires_at: now + Duration::hours(1),
        note: None,
        amount: 0,
        automatic: false,
    };
    store.upsert_lease(&protection).unwrap();
    let before = store.load_agents().unwrap();
    let result = Daemon::with_store(dir.path().into(), dir.path().join("sock"), store);
    let error = result
        .err()
        .expect("ambiguous live names must refuse")
        .to_string();
    assert!(
        error.contains("duplicate live agent name ambiguous"),
        "{error}"
    );
    assert!(error.contains(first.id.as_str()) && error.contains(second.id.as_str()));
    let reopened = Store::open(&database).unwrap();
    assert_eq!(reopened.load_agents().unwrap(), before);
    assert_eq!(reopened.load_leases().unwrap(), [protection]);
    assert!(reopened.recent_events(10).unwrap().is_empty());
}

#[test]
fn startup_failure_and_its_event_commit_together() {
    let dir = TempDir::new().unwrap();
    let database = dir.path().join("state.db");
    let store = Store::open(&database).unwrap();
    let record = AgentRecord::new(spec("never-started"), true, Utc::now());
    store.upsert_agent(&record).unwrap();
    store.reject_event_for_test("agent_exited");
    assert!(Daemon::with_store(dir.path().into(), dir.path().join("sock"), store).is_err());
    let reopened = Store::open(&database).unwrap();
    assert_eq!(
        reopened.load_agents().unwrap()[0].status,
        AgentStatus::Created
    );
    assert!(reopened.recent_events(10).unwrap().is_empty());
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[tokio::test]
async fn recycled_pid_is_not_mistaken_for_the_agent() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    // Both on this process's own pid, on purpose: a real one, so
    // liveness has something to check, and the same one, so the
    // second registration is the recycled-pid case. It is not folded
    // into the first because their start times disagree — which is
    // the whole reason "one process, one agent" compares both.
    let stale = register(&daemon, "stale", Some(std::process::id())).await;
    // Pretend the process that registered started long before the one
    // holding the pid now (as after a reboot).
    lock(&daemon.state)
        .registry
        .get_mut(&stale.id)
        .unwrap()
        .process_started_at = Some(Utc::now() - Duration::hours(24 * 30));
    let fresh = register(&daemon, "fresh", Some(std::process::id())).await;
    assert_ne!(fresh.id, stale.id, "a recycled pid is a different agent");
    assert!(fresh.process_started_at.is_some());

    daemon.check_liveness();
    assert!(!daemon.is_live(&stale.id));
    assert!(daemon.is_live(&fresh.id));
}

#[test]
fn invalid_pids_are_never_alive() {
    assert!(!process_exists(0));
    assert!(!process_exists(u32::MAX));
    assert!(process_exists(std::process::id()));
}

#[tokio::test]
async fn claim_wait_acquires_when_the_holder_releases() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    register(&daemon, "a", None).await;
    let b = register(&daemon, "b", None).await;
    let Response::Lease { lease } = claim(&daemon, "a", "task:w").await else {
        panic!("a should hold task:w")
    };
    let waiter = {
        let daemon = daemon.clone();
        tokio::spawn(async move {
            daemon
                .handle(Request::Claim {
                    agent: "b".into(),
                    resource: "task:w".into(),
                    mode: LeaseMode::Exclusive,
                    amount: None,
                    ttl_secs: 60,
                    note: None,
                    wait_secs: 5,
                    automatic: false,
                })
                .await
        })
    };
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    daemon
        .handle(Request::Release {
            agent: "a".into(),
            lease: lease.id,
            summary: None,
            summary_source: SummarySource::Explicit,
        })
        .await;
    let response = waiter.await.unwrap();
    assert!(matches!(response, Response::Lease { lease } if lease.holder == b.id));
}

#[tokio::test]
async fn claim_wait_gives_up_at_the_deadline() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    register(&daemon, "a", None).await;
    register(&daemon, "b", None).await;
    claim(&daemon, "a", "task:w").await;
    let started = std::time::Instant::now();
    let response = daemon
        .handle(Request::Claim {
            agent: "b".into(),
            resource: "task:w".into(),
            mode: LeaseMode::Exclusive,
            amount: None,
            ttl_secs: 60,
            note: None,
            wait_secs: 1,
            automatic: false,
        })
        .await;
    assert!(started.elapsed() >= std::time::Duration::from_secs(1));
    assert!(matches!(
        response,
        Response::Error {
            code: ErrorCode::Conflict,
            ..
        }
    ));
    // One conflict event per request, however long it waited.
    let conflicts = daemon
        .recent_events(50)
        .iter()
        .filter(|e| matches!(e.kind, EventKind::LeaseConflict { .. }))
        .count();
    assert_eq!(conflicts, 1);
}

fn spec_in(name: &str, workdir: &Path) -> AgentSpec {
    AgentSpec {
        name: name.to_owned(),
        workdir: Some(workdir.to_path_buf()),
        ..AgentSpec::default()
    }
}

async fn register_spec(daemon: &Arc<Daemon>, spec: AgentSpec) -> AgentRecord {
    match daemon
        .handle(Request::Register {
            spec,
            pid: None,
            session: None,
        })
        .await
    {
        Response::Agent { agent } => agent,
        other => panic!("unexpected {other:?}"),
    }
}

async fn register_in(daemon: &Arc<Daemon>, name: &str, workdir: &Path) -> AgentRecord {
    register_spec(daemon, spec_in(name, workdir)).await
}

async fn list(daemon: &Arc<Daemon>, project: Option<&str>, labels: &[(&str, &str)]) -> Response {
    daemon
        .handle(Request::List {
            all: false,
            project: project.map(str::to_owned),
            labels: labels
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        })
        .await
}

fn names(response: Response) -> Vec<String> {
    match response {
        Response::Agents { agents, .. } => agents.into_iter().map(|a| a.spec.name).collect(),
        other => panic!("unexpected {other:?}"),
    }
}

fn drain_discovered(events: &mut broadcast::Receiver<Event>) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    while let Ok(event) = events.try_recv() {
        if let EventKind::ProjectDiscovered { project } = event.kind {
            roots.push(project.root);
        }
    }
    roots
}

fn git(home: &Path, dir: &Path, args: &[&str]) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn have_git() -> bool {
    std::process::Command::new("git")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[tokio::test]
async fn agents_group_by_project_and_list_filters_by_it() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let alpha = dir.path().join("alpha");
    let beta = dir.path().join("beta");
    std::fs::create_dir_all(alpha.join("src")).unwrap();
    std::fs::create_dir_all(&beta).unwrap();
    std::fs::write(alpha.join("Agentfile.toml"), "").unwrap();

    let one = register_in(&daemon, "one", &alpha).await;
    let two = register_in(&daemon, "two", &alpha.join("src")).await;
    let three = register_in(&daemon, "three", &beta).await;
    let loner = register(&daemon, "loner", None).await;
    let mut tagged = spec_in("tagged", &alpha);
    tagged.labels.insert("team".to_owned(), "x".to_owned());
    let tagged = register_spec(&daemon, tagged).await;

    let project = one.project.clone().expect("derived from workdir");
    assert_eq!(project.source, ProjectSource::Agentfile);
    assert_eq!(project.root, alpha.canonicalize().unwrap());
    assert_eq!(project.worktree, None);
    assert_eq!(
        two.project.as_ref(),
        Some(&project),
        "nested dirs share the root"
    );
    assert_eq!(tagged.project.as_ref(), Some(&project));
    assert_eq!(
        three.project.as_ref().unwrap().source,
        ProjectSource::Directory
    );
    assert_ne!(three.project.as_ref().unwrap().id(), project.id());
    assert_eq!(loner.project, None);

    // Grouped by project, agents outside any project last.
    assert_eq!(
        names(list(&daemon, None, &[]).await),
        ["one", "two", "tagged", "three", "loner"]
    );
    // A path inside the project, or any unique prefix of its id.
    let inside = alpha.join("src").to_string_lossy().into_owned();
    assert_eq!(
        names(list(&daemon, Some(&inside), &[]).await),
        ["one", "two", "tagged"]
    );
    let id = project.id();
    let prefix = &id.as_str()[..8];
    assert_eq!(
        names(list(&daemon, Some(prefix), &[]).await),
        ["one", "two", "tagged"]
    );
    assert_eq!(
        names(list(&daemon, Some(prefix), &[("team", "x")]).await),
        ["tagged"]
    );
    assert_eq!(
        names(list(&daemon, None, &[("team", "x")]).await),
        ["tagged"]
    );
    assert!(matches!(
        list(&daemon, Some("zzzz"), &[]).await,
        Response::Error {
            code: ErrorCode::NotFound,
            ..
        }
    ));
}

#[tokio::test]
async fn repositories_are_fingerprinted_once_and_announced() {
    if !have_git() {
        return;
    }
    let dir = TempDir::new().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(repo.join("sub")).unwrap();
    assert!(git(dir.path(), &repo, &["init", "-q"]));
    assert!(git(
        dir.path(),
        &repo,
        &["commit", "-q", "--allow-empty", "-m", "root"]
    ));

    let daemon = open(&dir);
    let mut events = daemon.subscribe_events();
    let a = register_in(&daemon, "a", &repo).await;
    let b = register_in(&daemon, "b", &repo.join("sub")).await;
    let project = a.project.clone().unwrap();
    assert_eq!(project.source, ProjectSource::Git);
    let fingerprint = project.fingerprint.clone().expect("root commit");
    assert_eq!(project.id().as_str(), fingerprint);
    assert_eq!(b.project.unwrap().id(), project.id());
    assert_eq!(drain_discovered(&mut events), vec![project.root.clone()]);

    // The fingerprint is persisted: a restarted daemon neither walks the
    // history again nor announces the repository twice.
    drop(daemon);
    let daemon = open(&dir);
    let mut events = daemon.subscribe_events();
    let c = register_in(&daemon, "c", &repo).await;
    assert_eq!(
        c.project.unwrap().fingerprint.as_deref(),
        Some(fingerprint.as_str())
    );
    assert!(drain_discovered(&mut events).is_empty());
}

async fn send(daemon: &Arc<Daemon>, from: &str, to: &str) -> Response {
    daemon
        .handle(Request::Send {
            from: from.to_owned(),
            to: to.to_owned(),
            kind: "chat".to_owned(),
            payload: json!({ "text": "hi" }),
            reply_to: None,
            links: Vec::new(),
        })
        .await
}

#[tokio::test]
async fn send_readiness_reports_only_queued_recipients_and_never_receipts() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let alpha = dir.path().join("alpha");
    let beta = dir.path().join("beta");
    std::fs::create_dir_all(&alpha).unwrap();
    std::fs::create_dir_all(&beta).unwrap();
    let sender = register_in(&daemon, "sender", &alpha).await;
    let ready = register_in(&daemon, "ready", &alpha).await;
    let unbound = register_in(&daemon, "unbound", &alpha).await;
    let unrelated = register_in(&daemon, "unrelated", &beta).await;
    {
        let mut state = lock(&daemon.state);
        let now = Utc::now();
        let agent = state.registry.get_mut(&ready.id).unwrap();
        agent.process_started_at = Some(now);
        agent.spec.runtime = "codex".into();
        agent.input_delivery = Some(agentdocker_core::InputDelivery {
            process_started_at: now,
            reported_at: now,
            paused: false,
            pause_reason: None,
            received: None,
            received_at: None,
        });
        let agent = state.registry.get_mut(&unbound.id).unwrap();
        agent.spec.runtime = "claude-code".into();
        agent
            .spec
            .labels
            .insert("session_id".into(), "resume-session".into());
    }
    let Response::Channel { channel } = daemon
        .handle(Request::ChannelOpen {
            agent: sender.id.to_string(),
            task: "readiness test".into(),
            members: vec![ready.id.to_string(), unbound.id.to_string()],
            name: None,
            project: None,
        })
        .await
    else {
        panic!("channel failed")
    };
    for (destination, count) in [
        (unbound.id.to_string(), 1),
        (
            format!("project:{}", sender.project.as_ref().unwrap().id()),
            2,
        ),
        (format!("channel:{}", channel.id), 2),
    ] {
        let Response::Sent {
            message,
            recipient_readiness: Some(report),
            ..
        } = send(&daemon, sender.id.as_str(), &destination).await
        else {
            panic!("send failed")
        };
        assert_eq!(report.recipients, count);
        assert_eq!(report.needs_attention, 1);
        assert_eq!(report.details[0].agent, unbound.id);
        assert_eq!(
            report.details[0].issue,
            agentdocker_core::SendIssue::NoReceiver
        );
        assert!(
            report.details[0]
                .guidance()
                .contains("claude --resume resume-session")
        );
        // The fix itself rides with it, the command whole, for a client
        // to show as it is.
        assert!(matches!(
            &report.details[0].fix,
            Some(agentdocker_core::readiness::Fix::Reconnect { command })
                if command.contains("claude --resume resume-session")
        ));
        assert!(
            inbox(&daemon, unbound.id.as_str(), false)
                .await
                .iter()
                .any(|m| m.id == message)
        );
        assert!(
            !inbox(&daemon, unrelated.id.as_str(), false)
                .await
                .iter()
                .any(|m| m.id == message)
        );
        assert!(
            !inbox(&daemon, sender.id.as_str(), false)
                .await
                .iter()
                .any(|m| m.id == message)
        );
    }
    // A quota held by a different project still blocks a recipient
    // whose receiver is current; it must not add that source to fanout.
    {
        let mut state = lock(&daemon.state);
        for id in [&ready.id, &unrelated.id] {
            let agent = state.registry.get_mut(id).unwrap();
            agent.spec.provider = Some("fixture-provider".into());
            agent
                .spec
                .labels
                .insert("provider-quota".into(), "shared".into());
        }
        let agent = state.registry.get_mut(&unrelated.id).unwrap();
        // Finished sources still carry shared-quota blocks.
        agent.status = AgentStatus::Exited { code: Some(1) };
        agent.provider_availability = Some(agentdocker_core::ProviderAvailability {
            process_started_at: Utc::now(),
            observed_at: Utc::now(),
            cleared_observation: None,
            issue: Some(agentdocker_core::ProviderIssue {
                quota_group: Some("shared".into()),
                ..agentdocker_core::ProviderIssue::local(agentdocker_core::ProviderIssueKind::Usage)
            }),
            retries: 0,
        });
    }
    let Response::Sent {
        recipient_readiness: Some(report),
        ..
    } = send(&daemon, sender.id.as_str(), ready.id.as_str()).await
    else {
        panic!("send failed")
    };
    assert_eq!(report.recipients, 1);
    assert_eq!(report.details[0].agent, ready.id);
    assert_eq!(
        report.details[0].issue,
        agentdocker_core::SendIssue::ProviderBlocked(agentdocker_core::ProviderIssueKind::Usage)
    );
    let queued = inbox(&daemon, unbound.id.as_str(), false).await;
    lock(&daemon.state).store.reject_writes_for_test();
    assert!(matches!(
        send(&daemon, sender.id.as_str(), unbound.id.as_str()).await,
        Response::Error {
            code: ErrorCode::StorageUnavailable,
            ..
        }
    ));
    assert_eq!(
        lock(&daemon.state).inboxes[&unbound.id]
            .iter()
            .cloned()
            .collect::<Vec<_>>(),
        queued
    );
}

/// The person can see where their message stands with each agent it
/// was queued for: one that took it and answered says so, one nothing
/// woke waits for its next prompt, and a session caught up after the
/// message is followed too. An agent's own message is not followed.
#[tokio::test]
async fn the_person_sees_where_their_message_stands_with_each_agent() {
    use agentdocker_core::delivery::State as Delivery;
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let alpha = dir.path().join("alpha");
    std::fs::create_dir_all(&alpha).unwrap();
    std::fs::write(alpha.join("Agentfile.toml"), "").unwrap();
    register_spec(
        &daemon,
        AgentSpec {
            name: "person".to_owned(),
            runtime: agentdocker_core::HUMAN_RUNTIME.to_owned(),
            workdir: Some(alpha.clone()),
            ..AgentSpec::default()
        },
    )
    .await;
    let quick = register_in(&daemon, "quick", &alpha).await;
    register_in(&daemon, "slow", &alpha).await;
    let everyone = format!("project:{}", alpha.display());
    let Response::Sent { message, .. } = send(&daemon, "person", &everyone).await else {
        panic!("the person speaks")
    };
    let stands = |response: Response| match response {
        Response::Delivery { recipients, .. } => recipients
            .into_iter()
            .map(|r| (r.name.unwrap_or_default(), r.state))
            .collect::<BTreeMap<_, _>>(),
        other => panic!("unexpected {other:?}"),
    };
    let delivery = || {
        daemon.handle(Request::Delivery {
            message: message.clone(),
        })
    };
    assert_eq!(
        stands(delivery().await),
        BTreeMap::from([
            ("quick".to_owned(), Delivery::WaitingForPrompt),
            ("slow".to_owned(), Delivery::WaitingForPrompt),
        ])
    );

    assert!(matches!(
        daemon
            .handle(Request::AckInbox {
                agent: "quick".into(),
                messages: vec![message.clone()],
            })
            .await,
        Response::Ok
    ));
    assert_eq!(stands(delivery().await)["quick"], Delivery::Delivered);
    assert!(matches!(
        daemon
            .handle(Request::Send {
                from: quick.id.to_string(),
                to: "person".into(),
                kind: "chat".into(),
                payload: json!({"text": "on it"}),
                reply_to: Some(message.clone()),
                links: Vec::new(),
            })
            .await,
        Response::Sent { .. }
    ));
    let after = stands(delivery().await);
    assert_eq!(after["quick"], Delivery::Answered);
    assert_eq!(after["slow"], Delivery::WaitingForPrompt);

    // A session that opens now is caught up and followed as well.
    register_in(&daemon, "late", &alpha).await;
    assert_eq!(stands(delivery().await)["late"], Delivery::WaitingForPrompt);

    let Response::Sent {
        message: theirs, ..
    } = send(&daemon, "slow", &everyone).await
    else {
        panic!("an agent speaks")
    };
    assert!(matches!(
        daemon.handle(Request::Delivery { message: theirs }).await,
        Response::Delivery { recipients, .. } if recipients.is_empty()
    ));
}

/// A session that opens in a project within an hour of the person's
/// word to everyone there gets it as its first input, once, with the
/// event that says so. An agent's word, an older one, another project's
/// and a second record of the same session are not caught up.
#[tokio::test]
async fn a_session_that_opens_after_the_persons_word_to_everyone_gets_it() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let alpha = dir.path().join("alpha");
    let beta = dir.path().join("beta");
    for root in [&alpha, &beta] {
        std::fs::create_dir_all(root).unwrap();
        std::fs::write(root.join("Agentfile.toml"), "").unwrap();
    }
    let person = register_spec(
        &daemon,
        AgentSpec {
            name: "person".to_owned(),
            runtime: agentdocker_core::HUMAN_RUNTIME.to_owned(),
            workdir: Some(alpha.clone()),
            ..AgentSpec::default()
        },
    )
    .await;
    let early = register_in(&daemon, "early", &alpha).await;
    let project = early.project.clone().unwrap().id();
    let everyone = |root: &Path| format!("project:{}", root.display());
    let Response::Sent { message: word, .. } = send(&daemon, "person", &everyone(&alpha)).await
    else {
        panic!("the person speaks")
    };
    assert!(matches!(
        send(&daemon, "early", &everyone(&alpha)).await,
        Response::Sent { .. }
    ));
    assert!(matches!(
        send(&daemon, "person", &everyone(&beta)).await,
        Response::Sent { .. }
    ));
    {
        let mut state = lock(&daemon.state);
        let old = Envelope::new(
            person.id.as_str(),
            Destination::Project(project.clone()),
            "chat",
            json!({"text": "two hours ago"}),
            None,
            Utc::now() - Duration::hours(2),
        );
        assert!(matches!(state.publish(old), Response::Sent { .. }));
    }
    let mut events = daemon.subscribe_events();

    let late = register_spec(
        &daemon,
        AgentSpec {
            labels: [("session_id".to_owned(), "late-session".to_owned())].into(),
            ..spec_in("late", &alpha)
        },
    )
    .await;
    let queued = inbox(&daemon, "late", false).await;
    assert_eq!(
        queued.iter().map(|m| &m.id).collect::<Vec<_>>(),
        [&word],
        "only the person's recent word to this project"
    );
    let mut caught_up = false;
    while let Ok(event) = events.try_recv() {
        caught_up |= matches!(&event.kind, EventKind::MessagesCaughtUp { agent, messages }
            if *agent == late.id && *messages == [word.clone()]);
    }
    assert!(caught_up, "the catch-up is announced");

    // Another record of the same session already had its chance.
    register_spec(
        &daemon,
        AgentSpec {
            labels: [("session_id".to_owned(), "late-session".to_owned())].into(),
            ..spec_in("late-again", &alpha)
        },
    )
    .await;
    assert!(inbox(&daemon, "late-again", false).await.is_empty());
}

#[tokio::test]
async fn project_messages_reach_everyone_in_the_project_but_the_sender() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let alpha = dir.path().join("alpha");
    let beta = dir.path().join("beta");
    std::fs::create_dir_all(alpha.join("src")).unwrap();
    std::fs::create_dir_all(&beta).unwrap();
    std::fs::write(alpha.join("Agentfile.toml"), "").unwrap();
    let one = register_in(&daemon, "one", &alpha).await;
    let two = register_in(&daemon, "two", &alpha.join("src")).await;
    let three = register_in(&daemon, "three", &beta).await;
    let project = one.project.clone().unwrap().id();

    // Addressed by a path inside the project.
    let inside = alpha.join("src").to_string_lossy().into_owned();
    let Response::Sent { subscribers, .. } =
        send(&daemon, "one", &format!("project:{inside}")).await
    else {
        panic!("send failed");
    };
    assert_eq!(subscribers, 0, "nobody is live-subscribed");
    assert_eq!(inbox(&daemon, "two", false).await.len(), 1);
    assert!(
        inbox(&daemon, "one", false).await.is_empty(),
        "not to the sender"
    );
    assert!(
        inbox(&daemon, "three", false).await.is_empty(),
        "other project"
    );
    let queued = &inbox(&daemon, "two", true).await[0];
    assert_eq!(queued.to, Destination::Project(project.clone()));
    assert_eq!(queued.from, one.id.as_str());

    // Addressed by an id prefix; the user is not in any project and
    // still reaches everyone in it.
    let prefix = &project.as_str()[..8];
    assert!(matches!(
        send(&daemon, "user", &format!("project:{prefix}")).await,
        Response::Sent { .. }
    ));
    assert_eq!(inbox(&daemon, "one", false).await.len(), 1);
    assert_eq!(inbox(&daemon, "two", false).await.len(), 1);
    assert!(inbox(&daemon, "three", false).await.is_empty());
    assert!(matches!(
        send(&daemon, "user", "project:zzzz").await,
        Response::Error {
            code: ErrorCode::NotFound,
            ..
        }
    ));

    // A live subscription filters by the subscriber's own project.
    let (in_alpha, _rx) = daemon.subscribe(Some("two"), Vec::new()).unwrap();
    let (in_beta, _rx) = daemon.subscribe(Some("three"), Vec::new()).unwrap();
    let (sender, _rx) = daemon.subscribe(Some("one"), Vec::new()).unwrap();
    let envelope = Envelope::new(
        one.id.as_str(),
        Destination::Project(project.clone()),
        "chat",
        json!({}),
        None,
        Utc::now(),
    );
    assert!(in_alpha.wants(&envelope));
    assert!(!in_beta.wants(&envelope));
    assert!(!sender.wants(&envelope));
    drop(two);
    drop(three);
}

/// An agent reaches only its own project and the person: another
/// repository's agent, that project and a question to it are refused
/// with nothing queued, and the agent's `all` is its own project. The
/// person, and the daemon's own word, still reach anyone.
#[tokio::test]
async fn an_agent_reaches_only_its_own_project_and_the_person() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let alpha = dir.path().join("alpha");
    let beta = dir.path().join("beta");
    for root in [&alpha, &beta] {
        std::fs::create_dir_all(root).unwrap();
        std::fs::write(root.join("Agentfile.toml"), "").unwrap();
    }
    let one = register_in(&daemon, "one", &alpha).await;
    register_in(&daemon, "two", &alpha).await;
    register_in(&daemon, "far", &beta).await;
    register_spec(
        &daemon,
        AgentSpec {
            name: "person".to_owned(),
            runtime: agentdocker_core::HUMAN_RUNTIME.to_owned(),
            workdir: Some(beta.clone()),
            ..AgentSpec::default()
        },
    )
    .await;
    let forbidden = |response: Response| {
        assert!(
            matches!(
                response,
                Response::Error {
                    code: ErrorCode::Forbidden,
                    ..
                }
            ),
            "{response:?}"
        );
    };

    forbidden(send(&daemon, "one", "far").await);
    forbidden(send(&daemon, "one", &format!("project:{}", beta.display())).await);
    forbidden(
        daemon
            .handle(Request::Ask {
                from: "one".into(),
                to: "far".into(),
                question: "?".into(),
                timeout_secs: 1,
            })
            .await,
    );
    assert!(inbox(&daemon, "far", false).await.is_empty());

    assert!(matches!(
        send(&daemon, "one", "all").await,
        Response::Sent { .. }
    ));
    let heard = inbox(&daemon, "two", true).await;
    assert_eq!(heard.len(), 1);
    assert_eq!(
        heard[0].to,
        Destination::Project(one.project.clone().unwrap().id())
    );
    assert!(inbox(&daemon, "far", false).await.is_empty());

    // The person is reached from any project, and reaches any.
    for (from, to) in [("one", "person"), ("person", "far"), ("agentd", "far")] {
        assert!(
            matches!(send(&daemon, from, to).await, Response::Sent { .. }),
            "{from} -> {to}"
        );
    }
    assert!(matches!(
        send(&daemon, "person", "all").await,
        Response::Sent { .. }
    ));
    assert_eq!(inbox(&daemon, "far", false).await.len(), 3);
}

#[tokio::test]
async fn path_claims_protect_the_physical_checkout() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let alpha = dir.path().join("alpha");
    std::fs::create_dir_all(alpha.join("src")).unwrap();
    std::fs::write(alpha.join("Agentfile.toml"), "").unwrap();
    let one = register_in(&daemon, "one", &alpha).await;
    register_in(&daemon, "two", &alpha.join("src")).await;
    register(&daemon, "outsider", None).await;
    let _project = one.project.clone().unwrap().id();

    // The claimed path is under the non-canonical temp dir and the file
    // does not exist yet; the key still has its eventual physical identity.
    let lib = alpha.join("src/lib.rs");
    let Response::Lease { lease } = claim(&daemon, "one", &format!("path:{}", lib.display())).await
    else {
        panic!("claim failed");
    };
    assert_eq!(
        lease.resource.as_str(),
        format!("path:{}", project::canonical(&lib).display())
    );

    // Everyone naming that file collides: a project mate, and an agent
    // with no project at all (the Agentfile root is found from the path).
    for who in ["two", "outsider"] {
        assert!(
            matches!(
                claim(&daemon, who, &format!("path:{}", lib.display())).await,
                Response::Error {
                    code: ErrorCode::Conflict,
                    ..
                }
            ),
            "{who} should conflict"
        );
    }
    // So does the whole project.
    assert!(matches!(
        claim(&daemon, "two", &format!("path:{}", alpha.display())).await,
        Response::Error {
            code: ErrorCode::Conflict,
            ..
        }
    ));

    // Queries by path find the translated lease.
    let Response::Leases { leases } = daemon
        .handle(Request::Leases {
            agent: None,
            resource: Some(format!("path:{}", alpha.join("src").display())),
        })
        .await
    else {
        panic!("leases failed");
    };
    assert_eq!(leases.len(), 1);

    // Paths outside any project keep their kind.
    let Response::Lease { lease } = claim(&daemon, "outsider", "path:/definitely/not/here/x").await
    else {
        panic!("claim failed");
    };
    assert_eq!(lease.resource.as_str(), "path:/definitely/not/here/x");
    let Response::Lease { lease } = claim(&daemon, "outsider", "task:ISSUE-1").await else {
        panic!("claim failed");
    };
    assert_eq!(lease.resource.as_str(), "task:ISSUE-1");
}

fn discovery_row(pid: u32, started_at: chrono::DateTime<Utc>) -> DiscoveredProcess {
    DiscoveredProcess {
        pid,
        ppid: 1,
        runtime: "codex".into(),
        command: "codex".into(),
        cwd: None,
        project: None,
        started_at: Some(started_at),
        session: None,
    }
}

#[tokio::test]
async fn concurrent_discovery_joins_the_inflight_scan_and_preserves_its_failure() {
    use std::sync::atomic::Ordering;
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let row = discovery_row(123456, Utc::now());
    for result in [Ok(vec![row]), Err("scan failed".to_owned())] {
        assert!(!daemon.scanning.swap(true, Ordering::AcqRel));
        let flight = ScanGuard {
            scanning: &daemon.scanning,
            finished: &daemon.scan_finished,
        };
        let joined = daemon.scan_agents();
        tokio::pin!(joined);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut joined)
                .await
                .is_err()
        );
        let committed = daemon.apply_scan(result);
        drop(flight);
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(1), joined)
                .await
                .unwrap(),
            committed
        );
        assert!(!daemon.scanning.load(Ordering::Acquire));
    }
}

#[test]
fn a_fenced_scan_preserves_discovery_until_transfer_aborts() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let first = discovery_row(123456, Utc::now());
    let replacement = discovery_row(first.pid, first.started_at.unwrap() + Duration::seconds(1));
    daemon.apply_scan(Ok(vec![first.clone()])).unwrap();
    daemon.offer_transfer(4242).unwrap();
    let mut events = daemon.subscribe_events();
    let (at, next_seq) = {
        let state = lock(&daemon.state);
        (state.discovered.at, state.next_seq)
    };
    // Both a completed background scan and its error path leave the last
    // committed projection intact while the successor takes ownership.
    for result in [Ok(vec![replacement.clone()]), Err("ps failed".into())] {
        assert!(daemon.apply_scan(result).is_err());
        let state = lock(&daemon.state);
        assert_eq!(state.discovered.processes, vec![first.clone()]);
        assert_eq!(state.discovered.at, at);
        assert!(state.discovered.error.is_none());
        assert_eq!(state.next_seq, next_seq);
        assert!(events.try_recv().is_err());
    }
    assert!(daemon.abort_transfer("scan fixture"));
    while events.try_recv().is_ok() {}
    daemon.apply_scan(Ok(vec![replacement.clone()])).unwrap();
    assert!(matches!(events.try_recv().unwrap().kind,
        EventKind::AgentVanished { started_at, .. } if started_at == first.started_at));
    assert!(matches!(events.try_recv().unwrap().kind,
        EventKind::AgentDiscovered { started_at, .. } if started_at == replacement.started_at));
    assert!(events.try_recv().is_err());
}

#[tokio::test]
async fn the_background_scan_waits_for_a_listener_and_discover_never_does() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    assert!(
        !daemon.discovery_watched(),
        "nobody listens to a fresh daemon"
    );
    assert!(
        matches!(
            daemon.handle(Request::Discover).await,
            Response::Processes { .. }
        ),
        "asked, it scans whether or not anyone listens"
    );
    let events = daemon.subscribe_events();
    assert!(daemon.discovery_watched());
    drop(events);
    assert!(
        !daemon.discovery_watched(),
        "the last listener gone, it rests"
    );
}

#[tokio::test]
async fn discovery_requests_are_mutations_and_refused_during_transfer() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    daemon.offer_transfer(4242).unwrap();
    for request in [Request::Discover, Request::Runtimes] {
        assert!(mutates(&request));
        assert!(matches!(
            daemon.handle(request).await,
            Response::Error {
                code: ErrorCode::Transferring,
                ..
            }
        ));
    }
}

#[test]
fn scan_failure_retains_snapshot_and_recovery_distinguishes_pid_generations() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let first = discovery_row(123456, Utc::now());
    let mut events = daemon.subscribe_events();
    daemon.apply_scan(Ok(vec![first.clone()])).unwrap();
    while events.try_recv().is_ok() {}
    let at = lock(&daemon.state).discovered.at;
    assert!(daemon.apply_scan(Err("ps unavailable".into())).is_err());
    assert_eq!(
        lock(&daemon.state).discovered.processes,
        vec![first.clone()]
    );
    assert_eq!(lock(&daemon.state).discovered.at, at);
    assert!(matches!(
        events.try_recv().unwrap().kind,
        EventKind::DiscoveryUnavailable { .. }
    ));
    assert!(
        events.try_recv().is_err(),
        "failure never invents vanished agents"
    );
    assert!(daemon.apply_scan(Err("ps unavailable".into())).is_err());
    assert!(
        events.try_recv().is_err(),
        "same failure is not repeated every tick"
    );
    let replacement = discovery_row(first.pid, first.started_at.unwrap() + Duration::seconds(1));
    daemon.apply_scan(Ok(vec![replacement.clone()])).unwrap();
    assert!(
        matches!(events.try_recv().unwrap().kind, EventKind::AgentVanished { started_at, adopted: false, .. } if started_at == first.started_at)
    );
    assert!(
        matches!(events.try_recv().unwrap().kind, EventKind::AgentDiscovered { started_at, .. } if started_at == replacement.started_at)
    );
    assert!(matches!(
        events.try_recv().unwrap().kind,
        EventKind::DiscoveryAvailable
    ));
    daemon.apply_scan(Ok(vec![replacement.clone()])).unwrap();
    assert!(events.try_recv().is_err());
    let mut moved = replacement;
    moved.cwd = Some(PathBuf::from("/tmp/new-project"));
    daemon.apply_scan(Ok(vec![moved])).unwrap();
    assert!(matches!(
        events.try_recv().unwrap().kind,
        EventKind::AgentDiscovered { cwd: Some(_), .. }
    ));
    assert!(events.try_recv().is_err());
}

#[tokio::test]
async fn a_scan_started_before_registration_cannot_restore_an_adopted_process() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let row = discovery_row(child.id(), procinfo::start_time(child.id()).unwrap());
    daemon.apply_scan(Ok(vec![row.clone()])).unwrap();
    let response = daemon
        .register(
            AgentSpec {
                name: "joined-during-scan".into(),
                ..AgentSpec::default()
            },
            Some(child.id()),
            None,
        )
        .await;
    assert!(matches!(response, Response::Agent { .. }), "{response:?}");
    let mut events = daemon.subscribe_events();
    assert!(daemon.apply_scan(Ok(vec![row.clone()])).unwrap().is_empty());
    assert!(matches!(
        events.try_recv().unwrap().kind,
        EventKind::AgentVanished { adopted: true, .. }
    ));
    assert!(daemon.apply_scan(Ok(vec![row])).unwrap().is_empty());
    assert!(events.try_recv().is_err());
    child.kill().unwrap();
    child.wait().unwrap();
}

#[tokio::test]
async fn scans_announce_agents_appearing_going_and_adopted() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let project = dir.path().join("proj");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("Agentfile.toml"), "").unwrap();
    let fake = dir.path().join("codex");
    std::os::unix::fs::symlink("/bin/sleep", &fake).unwrap();
    let mut events = daemon.subscribe_events();
    daemon.scan_agents().await.unwrap();
    let mut child = std::process::Command::new(&fake)
        .arg("60")
        .current_dir(&project)
        .spawn()
        .unwrap();
    let pid = child.id();

    daemon.scan_agents().await.unwrap();
    let mut seen = Vec::new();
    while let Ok(event) = events.try_recv() {
        seen.push(event.kind);
    }
    assert!(
        seen.iter().any(|k| matches!(k, EventKind::AgentDiscovered { pid: p, runtime, project: Some(_), .. } if *p == pid && runtime == "codex")),
        "{seen:?}"
    );
    // Fresh scans answer from the cache and announce nothing twice.
    daemon.scan_agents().await.unwrap();
    assert!(
        !std::iter::from_fn(|| events.try_recv().ok())
            .any(|e| matches!(e.kind, EventKind::AgentDiscovered { pid: p, .. } if p == pid))
    );
    let Response::Processes { processes } = daemon.handle(Request::Discover).await else {
        panic!("discover failed");
    };
    assert!(processes.iter().any(|p| p.pid == pid));
    let Response::Runtimes { runtimes } = daemon.handle(Request::Runtimes).await else {
        panic!("runtimes failed");
    };
    let codex = runtimes.iter().find(|r| r.name == "codex").unwrap();
    assert!(codex.running >= 1, "{codex:?}");

    // Adopted: it leaves the discovered set at once, as adopted, not
    // as gone — and a fresh discover no longer lists it.
    assert!(matches!(
        daemon
            .handle(Request::Adopt {
                pid,
                name: None,
                runtime: None,
            })
            .await,
        Response::Agent { .. }
    ));
    let kinds: Vec<EventKind> = std::iter::from_fn(|| events.try_recv().ok())
        .map(|e| e.kind)
        .collect();
    assert!(
        kinds.iter().any(
            |k| matches!(k, EventKind::AgentVanished { pid: p, adopted: true, .. } if *p == pid)
        ),
        "{kinds:?}"
    );
    let Response::Processes { processes } = daemon.handle(Request::Discover).await else {
        panic!("discover failed");
    };
    assert!(processes.iter().all(|p| p.pid != pid));
    daemon.scan_agents().await.unwrap();
    assert!(
        !std::iter::from_fn(|| events.try_recv().ok())
            .any(|e| matches!(e.kind, EventKind::AgentVanished { pid: p, .. } if p == pid)),
        "announced once"
    );

    // Gone: a second fake that exits is announced as vanished.
    let mut short = std::process::Command::new(&fake)
        .arg("30")
        .current_dir(&project)
        .spawn()
        .unwrap();
    let short_pid = short.id();
    daemon.scan_agents().await.unwrap();
    assert!(
        std::iter::from_fn(|| events.try_recv().ok())
            .any(|e| matches!(e.kind, EventKind::AgentDiscovered { pid: p, .. } if p == short_pid))
    );
    short.kill().unwrap();
    short.wait().unwrap();
    daemon.scan_agents().await.unwrap();
    assert!(
        std::iter::from_fn(|| events.try_recv().ok())
            .any(|e| matches!(e.kind, EventKind::AgentVanished { pid: p, adopted: false, .. } if p == short_pid))
    );
    child.kill().unwrap();
    child.wait().unwrap();
}

#[tokio::test]
async fn discover_finds_known_runtimes_and_adopt_registers_them() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    // A process whose executable is called `claude`, working in an
    // Agentfile project, is what a hook-less Claude Code session looks
    // like from the process table.
    let project = dir.path().join("proj");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("Agentfile.toml"), "").unwrap();
    let fake = dir.path().join("claude");
    std::os::unix::fs::symlink("/bin/sleep", &fake).unwrap();
    let mut child = std::process::Command::new(&fake)
        .arg("60")
        .current_dir(&project)
        .spawn()
        .unwrap();
    let pid = child.id();

    let found = match daemon.handle(Request::Discover).await {
        Response::Processes { processes } => processes,
        other => panic!("unexpected {other:?}"),
    };
    let mine = found
        .iter()
        .find(|p| p.pid == pid)
        .expect("the fake claude is discovered");
    assert_eq!(mine.runtime, "claude-code");
    assert_eq!(mine.cwd, Some(project.canonicalize().unwrap()));
    assert_eq!(
        mine.project.as_ref().map(|p| p.source),
        Some(ProjectSource::Agentfile)
    );
    assert_eq!(mine.default_name(), format!("claude-code-{pid}"));

    let adopted = match daemon
        .handle(Request::Adopt {
            pid,
            name: None,
            runtime: None,
        })
        .await
    {
        Response::Agent { agent } => agent,
        other => panic!("unexpected {other:?}"),
    };
    assert_eq!(adopted.spec.name, format!("claude-code-{pid}"));
    assert_eq!(adopted.spec.runtime, "claude-code");
    assert_eq!(adopted.pid, Some(pid));
    assert_eq!(
        adopted.spec.labels.get("adopted").map(String::as_str),
        Some("true")
    );
    assert_eq!(
        adopted.project.as_ref().map(|p| p.root.clone()),
        Some(project.canonicalize().unwrap())
    );
    assert!(process_exists(pid));

    // Registered pids disappear from discovery, and cannot be adopted twice.
    let found = match daemon.handle(Request::Discover).await {
        Response::Processes { processes } => processes,
        other => panic!("unexpected {other:?}"),
    };
    assert!(found.iter().all(|p| p.pid != pid));
    assert!(matches!(
        daemon
            .handle(Request::Adopt {
                pid,
                name: None,
                runtime: None
            })
            .await,
        Response::Error {
            code: ErrorCode::Invalid,
            ..
        }
    ));

    child.kill().unwrap();
    child.wait().unwrap();
    assert!(matches!(
        daemon
            .handle(Request::Adopt {
                pid: dead_pid(),
                name: None,
                runtime: None
            })
            .await,
        Response::Error {
            code: ErrorCode::NotFound,
            ..
        }
    ));
}

/// The bridge a browser launches for Claude Code is that CLI's
/// helper: discovery never lists it, and adopting it by pid is refused
/// with what it is, whichever runtime the caller names. Registered, it
/// would be an agent nobody can reach in a project named after the
/// helper's directory, which is what a person reads as "my browser
/// agent is connected". The process here is `node` running a script
/// under the package path, which is how an npm-installed Claude Code
/// appears in the process table; without `node` there is no way to
/// stage that command line, and the classifier's own tests cover it.
#[tokio::test]
async fn a_runtime_helper_is_not_discovered_and_cannot_be_adopted() {
    let Some(node) = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join("node"))
            .find(|candidate| candidate.is_file())
    }) else {
        return;
    };
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let package = dir.path().join("node_modules/@anthropic-ai/claude-code");
    std::fs::create_dir_all(&package).unwrap();
    let script = package.join("cli.js");
    std::fs::write(&script, "setTimeout(() => {}, 60000);\n").unwrap();
    /// The fake ends with the test, passed or failed: an assertion
    /// that fails must not leave a sixty-second child behind.
    struct Reaped(std::process::Child);
    impl Drop for Reaped {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let child = Reaped(
        std::process::Command::new(node)
            .arg(&script)
            .arg("--chrome-native-host")
            .current_dir(dir.path())
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    let pid = child.0.id();
    // Give the process table time to show the interpreter's argv.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let found = match daemon.handle(Request::Discover).await {
        Response::Processes { processes } => processes,
        other => panic!("unexpected {other:?}"),
    };
    assert!(
        found.iter().all(|p| p.pid != pid),
        "a helper is not a session"
    );
    for runtime in [
        None,
        Some("claude-code".to_owned()),
        Some("custom".to_owned()),
    ] {
        match daemon
            .handle(Request::Adopt {
                pid,
                name: None,
                runtime,
            })
            .await
        {
            Response::Error { code, message, .. } => {
                assert_eq!(code, ErrorCode::Invalid);
                assert!(
                    message.contains("bridge for the browser extension")
                        && message.contains("not a session"),
                    "{message}"
                );
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    let Response::Agents { agents, .. } = daemon
        .handle(Request::List {
            all: true,
            project: None,
            labels: Default::default(),
        })
        .await
    else {
        panic!("list failed");
    };
    assert!(agents.iter().all(|a| a.pid != Some(pid)));
    drop(child);
    assert!(!process_exists(pid), "the guard reaps the fake");
}

fn drain_vcs(events: &mut broadcast::Receiver<Event>) -> Vec<Option<String>> {
    let mut branches = Vec::new();
    while let Ok(event) = events.try_recv() {
        if let EventKind::AgentVcsChanged { vcs, .. } = event.kind {
            branches.push(vcs.branch);
        }
    }
    branches
}

#[tokio::test]
async fn checkout_is_observed_at_creation_refreshed_and_reported() {
    if !have_git() {
        return;
    }
    let dir = TempDir::new().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    assert!(git(dir.path(), &repo, &["init", "-q"]));
    assert!(git(
        dir.path(),
        &repo,
        &["commit", "-q", "--allow-empty", "-m", "root"]
    ));
    let daemon = open(&dir);
    let mut events = daemon.subscribe_events();

    // Known from the moment of registration, with no event yet.
    let a = register_in(&daemon, "a", &repo).await;
    let initial = a.vcs.clone().expect("checkout read at creation");
    assert_eq!(initial.branch.as_deref(), Some("main"));
    assert!(initial.head.is_some());
    assert!(drain_vcs(&mut events).is_empty());

    // The timer notices a branch switch made outside AgentDocker.
    assert!(git(dir.path(), &repo, &["checkout", "-q", "-b", "feature"]));
    daemon.refresh_vcs(None).await;
    assert_eq!(drain_vcs(&mut events), vec![Some("feature".to_owned())]);
    daemon.refresh_vcs(None).await;
    assert!(drain_vcs(&mut events).is_empty(), "no change, no event");

    // A hook can report ahead of the timer; the same state is silent,
    // a different one is announced and visible in the record.
    let reported = VcsState {
        branch: Some("feature".to_owned()),
        head: initial.head.clone(),
        dirty: None,
        updated_at: Utc::now(),
    };
    assert!(matches!(
        daemon
            .handle(Request::Report {
                agent: "a".to_owned(),
                vcs: Some(reported.clone()),
            })
            .await,
        Response::Ok
    ));
    assert!(drain_vcs(&mut events).is_empty());
    daemon
        .handle(Request::Report {
            agent: "a".to_owned(),
            vcs: Some(VcsState {
                branch: Some("hotfix".to_owned()),
                ..reported
            }),
        })
        .await;
    assert_eq!(drain_vcs(&mut events), vec![Some("hotfix".to_owned())]);
    let Response::Agent { agent } = daemon
        .handle(Request::Inspect {
            agent: "a".to_owned(),
        })
        .await
    else {
        panic!("inspect failed");
    };
    assert_eq!(agent.vcs.unwrap().branch.as_deref(), Some("hotfix"));
}

#[tokio::test]
async fn aliases_and_root_claims_conflict_for_outsiders_without_project_markers() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let checkout = dir.path().join("checkout");
    std::fs::create_dir_all(&checkout).unwrap();
    let owner = register_in(&daemon, "owner", &checkout).await;
    register(&daemon, "outsider", None).await;
    let path = checkout.join("file");
    assert!(matches!(
        claim(&daemon, "owner", &format!("path:{}", path.display())).await,
        Response::Lease { .. }
    ));
    for alias in [&path, &checkout, &checkout.join("missing/../file")] {
        assert!(matches!(
            claim(&daemon, "outsider", &format!("path:{}", alias.display())).await,
            Response::Error {
                code: ErrorCode::Conflict,
                ..
            }
        ));
    }
    let id = owner.project.unwrap().id();
    assert!(matches!(
        claim(&daemon, "owner", &format!("file:{id}/file")).await,
        Response::Lease { .. }
    ));
    let Response::Leases { leases } = daemon
        .handle(Request::Leases {
            agent: None,
            resource: Some(format!("path:{}", checkout.display())),
        })
        .await
    else {
        panic!()
    };
    assert_eq!(leases.len(), 1);
}

#[tokio::test]
async fn exited_agents_cannot_claim_or_win_a_pending_wait() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    register(&daemon, "holder", None).await;
    register(&daemon, "waiter", None).await;
    assert!(matches!(
        claim(&daemon, "holder", "task:wait").await,
        Response::Lease { .. }
    ));
    let mut events = daemon.subscribe_events();
    let pending = {
        let daemon = daemon.clone();
        tokio::spawn(async move {
            daemon
                .handle(Request::Claim {
                    agent: "waiter".into(),
                    resource: "task:wait".into(),
                    mode: LeaseMode::Exclusive,
                    amount: None,
                    ttl_secs: 60,
                    note: None,
                    wait_secs: 5,
                    automatic: false,
                })
                .await
        })
    };
    while !matches!(
        events.recv().await.unwrap().kind,
        EventKind::LeaseConflict { .. }
    ) {}
    daemon
        .handle(Request::Deregister {
            agent: "waiter".into(),
        })
        .await;
    daemon
        .handle(Request::ReleaseAll {
            agent: "holder".into(),
            summary: None,
            summary_source: SummarySource::Explicit,
            only_automatic: false,
        })
        .await;
    assert!(matches!(
        pending.await.unwrap(),
        Response::Error {
            code: ErrorCode::Invalid,
            ..
        }
    ));
    assert!(matches!(
        claim(&daemon, "waiter", "task:other").await,
        Response::Error {
            code: ErrorCode::Invalid,
            ..
        }
    ));
    assert!(list_leases(&daemon).await.is_empty());
}

#[tokio::test]
async fn invalid_pid_registration_never_reaches_a_signal_target() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    for pid in [0, i32::MAX as u32 + 1, u32::MAX] {
        assert!(signal_pid(pid).is_none());
        assert!(matches!(
            daemon
                .handle(Request::Register {
                    spec: spec("invalid"),
                    pid: Some(pid),
                    session: None,
                })
                .await,
            Response::Error {
                code: ErrorCode::Invalid,
                ..
            }
        ));
    }
}

#[tokio::test]
async fn stopping_process_keeps_leases_until_observed_exit() {
    use std::io::{BufRead, BufReader};
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    // The only process signalled is this test's child. trap is installed before READY.
    let mut child = std::process::Command::new("sh")
        .args(["-c", "trap '' TERM; echo READY; while :; do :; done"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut ready = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut ready)
        .unwrap();
    assert_eq!(ready.trim(), "READY");
    let record = register(&daemon, "resistant", Some(child.id())).await;
    claim(&daemon, "resistant", "task:live").await;
    let response = daemon
        .handle(Request::Stop {
            agent: record.id.to_string(),
            force: false,
        })
        .await;
    let alive = child.try_wait().unwrap().is_none();
    let held = list_leases(&daemon).await.len();
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(matches!(response,Response::Agent {agent} if agent.status == AgentStatus::Stopping));
    assert!(alive);
    assert_eq!(held, 1);
    daemon.check_liveness();
    assert!(list_leases(&daemon).await.is_empty());
}

#[tokio::test]
async fn managed_group_keeps_protection_until_descendants_stop() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let mut command = spec("managed-group");
    command.workdir = Some(dir.path().to_path_buf());
    // A file gate makes the lease acquisition deterministic. The child
    // ignores TERM, so the supervisor must escalate and observe its exit.
    command.command = vec!["sh".into(), "-c".into(),
        "trap '' TERM; sleep 30 & echo $! > child.pid; while [ ! -f exit-now ]; do sleep 0.05; done".into()];
    let Response::Agent { agent } = daemon.handle(Request::Run { spec: command }).await else {
        panic!("managed launch failed");
    };
    assert_eq!(agent.process_group, agent.pid);
    assert!(matches!(
        claim(&daemon, "managed-group", "task:group").await,
        Response::Lease { .. }
    ));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !dir.path().join("child.pid").exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    std::fs::write(dir.path().join("exit-now"), "").unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(supervisor::group_exists(agent.pid.unwrap()));
    assert_eq!(list_leases(&daemon).await.len(), 1);
    tokio::time::timeout(std::time::Duration::from_secs(8), async {
        while !list_leases(&daemon).await.is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    assert!(!supervisor::group_exists(agent.pid.unwrap()));
}

#[tokio::test]
async fn failed_storage_never_acknowledges_or_serves_new_coordination() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    register(&daemon, "owner", None).await;
    register(&daemon, "other", None).await;
    let Response::Lease { lease } = claim(&daemon, "owner", "task:durable").await else {
        panic!()
    };
    let before = daemon.recent_events(100).len();
    lock(&daemon.state).store.reject_writes_for_test();
    let failed = daemon
        .handle(Request::Release {
            summary: None,
            summary_source: agentdocker_core::SummarySource::Explicit,
            agent: "owner".into(),
            lease: lease.id,
        })
        .await;
    assert!(matches!(
        failed,
        Response::Error {
            code: ErrorCode::StorageUnavailable,
            ..
        }
    ));
    assert!(matches!(
        claim(&daemon, "other", "task:durable").await,
        Response::Error {
            code: ErrorCode::StorageUnavailable,
            ..
        }
    ));
    assert_eq!(daemon.recent_events(100).len(), before);
    drop(daemon);
    let daemon = open(&dir);
    assert!(matches!(
        claim(&daemon, "other", "task:durable").await,
        Response::Error {
            code: ErrorCode::Conflict,
            ..
        }
    ));
}

#[tokio::test]
async fn failed_message_event_never_reaches_a_live_subscriber() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    register(&daemon, "sender", None).await;
    let recipient = register(&daemon, "recipient", None).await;
    let (_subscription, mut messages) = daemon.subscribe(Some("recipient"), vec![]).unwrap();
    let mut events = daemon.subscribe_events();
    lock(&daemon.state).next_seq -= 1;
    let response = lock(&daemon.state).send(
        "sender".into(),
        Destination::Agent(recipient.id),
        "chat".into(),
        json!({"text": "must not be delivered"}),
        None,
    );
    assert!(matches!(
        response,
        Response::Error {
            code: ErrorCode::StorageUnavailable,
            ..
        }
    ));
    assert!(messages.try_recv().is_err());
    assert!(events.try_recv().is_err());
}

#[tokio::test]
async fn managed_shutdown_uses_owned_child_when_start_time_is_unavailable() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let mut command = spec("owned");
    command.workdir = Some(dir.path().to_path_buf());
    command.command = vec![
        "sh".into(),
        "-c".into(),
        "trap '' TERM; echo ready > ready; while :; do sleep 0.05; done".into(),
    ];
    let Response::Agent { agent } = daemon.handle(Request::Run { spec: command }).await else {
        panic!("launch failed");
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !dir.path().join("ready").exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    claim(&daemon, "owned", "task:shutdown").await;
    lock(&daemon.state)
        .registry
        .get_mut(&agent.id)
        .unwrap()
        .process_started_at = None;
    let response = daemon.stop("owned", false);
    let held_while_stopping = list_leases(&daemon).await.len();
    daemon.stop_all().await;
    assert!(matches!(response, Response::Agent { agent } if agent.status == AgentStatus::Stopping));
    assert_eq!(held_while_stopping, 1);
    assert!(!supervisor::group_exists(agent.pid.unwrap()));
    assert!(list_leases(&daemon).await.is_empty());
    assert!(!daemon.is_live(&agent.id));
}

#[tokio::test]
async fn claim_expiration_removes_storage_and_emits_once() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let owner = register(&daemon, "owner", None).await;
    register(&daemon, "next", None).await;
    let now = Utc::now();
    let expired = Lease {
        id: LeaseId::from("expired"),
        resource: ResourceKey::new("task:expire"),
        holder: owner.id,
        mode: LeaseMode::Exclusive,
        acquired_at: now - Duration::seconds(5),
        change_seq: None,
        expires_at: now - Duration::seconds(1),
        note: None,
        amount: 0,
        automatic: false,
    };
    {
        let mut state = lock(&daemon.state);
        state.store.upsert_lease(&expired).unwrap();
        state.leases.restore(expired);
    }
    let mut events = daemon.subscribe_events();
    assert!(matches!(
        claim(&daemon, "next", "task:expire").await,
        Response::Lease { .. }
    ));
    daemon.expire_leases();
    let mut count = 0;
    while let Ok(e) = events.try_recv() {
        if matches!(e.kind, EventKind::LeaseExpired { .. }) {
            count += 1;
        }
    }
    assert_eq!(count, 1);
    assert!(
        lock(&daemon.state)
            .store
            .load_leases()
            .unwrap()
            .iter()
            .all(|l| l.id.as_str() != "expired")
    );
}

#[tokio::test]
async fn vcs_observations_cannot_rewind_and_survive_restart() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let owner = register(&daemon, "owner", None).await;
    let now = Utc::now();
    let newest = VcsState {
        branch: Some("new".into()),
        head: None,
        dirty: None,
        updated_at: now,
    };
    daemon.apply_vcs(&owner.id, newest.clone());
    daemon.apply_vcs(
        &owner.id,
        VcsState {
            branch: Some("old".into()),
            updated_at: now - Duration::seconds(1),
            ..newest.clone()
        },
    );
    drop(daemon);
    let daemon = open(&dir);
    assert_eq!(
        lock(&daemon.state).registry.get(&owner.id).unwrap().vcs,
        Some(newest)
    );
}

#[test]
fn concurrent_events_publish_in_persisted_sequence_order() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let mut events = daemon.subscribe_events();
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let daemon = &daemon;
            scope.spawn(move || {
                for _ in 0..25 {
                    daemon.emit(EventKind::DaemonStopping {
                        reason: "test".into(),
                    });
                }
            });
        }
    });
    let mut received = Vec::new();
    while let Ok(event) = events.try_recv() {
        received.push(event.seq);
    }
    assert_eq!(received, (1..=200).collect::<Vec<_>>());
    assert_eq!(
        daemon
            .recent_events(200)
            .iter()
            .map(|e| e.seq)
            .collect::<Vec<_>>(),
        received
    );
}

async fn ledger(daemon: &Arc<Daemon>, project: &Path, path: Option<&str>) -> Vec<Change> {
    match daemon
        .handle(Request::Changes {
            project: project.to_string_lossy().into_owned(),
            since_seq: None,
            path: path.map(str::to_owned),
            agent: None,
            limit: 50,
        })
        .await
    {
        Response::Changes { changes } => changes,
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test]
async fn watcher_attribution_uses_physical_aliases_and_root_queries_include_all_paths() {
    let dir = TempDir::new().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("Agentfile.toml"), "").unwrap();
    let target = repo.join("src/lib.rs");
    std::fs::write(&target, "before").unwrap();
    std::os::unix::fs::symlink("src/lib.rs", repo.join("alias.rs")).unwrap();
    std::os::unix::fs::symlink("src", repo.join("alias-dir")).unwrap();
    let daemon = open(&dir);
    let agent = register_in(&daemon, "writer", &repo).await;
    register_in(&daemon, "reader", &repo).await;
    assert!(matches!(
        daemon
            .handle(Request::Observe {
                agent: "reader".into(),
                paths: vec!["src/lib.rs".into()],
            })
            .await,
        Response::Reads { .. }
    ));
    let Response::Lease { lease } = claim(
        &daemon,
        "writer",
        &format!("path:{}", repo.join("alias.rs").display()),
    )
    .await
    else {
        panic!("claim failed");
    };
    let checkout = daemon.watch_targets().pop().unwrap();
    std::fs::write(&target, "after").unwrap();
    daemon
        .record_fs_changes(
            ["alias.rs", "alias-dir/lib.rs", "notes.md"]
                .into_iter()
                .map(|path| Observed {
                    checkout: checkout.clone(),
                    path: path.into(),
                    kind: ChangeKind::Modified,
                })
                .collect(),
            vec![],
        )
        .await;
    let attributed = Attribution::Agent {
        agent: agent.id,
        lease: lease.id,
        note: None,
    };
    let entries = ledger(&daemon, &repo, None).await;
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].by, attributed);
    assert_eq!(entries[1].by, attributed);
    assert_eq!(entries[2].by, Attribution::External);
    // Both alias changes warn the reader, as one notice on the tick
    // naming the one physical path they share.
    assert!(
        daemon
            .recent_events(50)
            .iter()
            .filter(|e| matches!(&e.kind, EventKind::AgentStale { .. }))
            .count()
            >= 2,
        "each change is a live stale event"
    );
    daemon.flush_notices();
    let Response::Messages { messages } = daemon
        .handle(Request::Inbox {
            agent: "reader".into(),
            drain: true,
        })
        .await
    else {
        panic!("inbox failed");
    };
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert_eq!(messages[0].kind, "stale");
    assert_eq!(
        messages[0].payload["paths"],
        json!([project::canonical(&target)])
    );
    let root = repo.to_string_lossy();
    for filter in ["", ".", "./", root.as_ref()] {
        assert_eq!(ledger(&daemon, &repo, Some(filter)).await, entries);
    }
    assert_eq!(ledger(&daemon, &repo, Some("alias-dir")).await.len(), 1);

    // A deleted regular file still has its normalized physical key. A
    // removed symlink no longer supplies evidence of its former target.
    std::fs::remove_file(&target).unwrap();
    std::fs::remove_file(repo.join("alias.rs")).unwrap();
    daemon
        .record_fs_changes(
            ["alias-dir/lib.rs", "alias.rs"]
                .into_iter()
                .map(|path| Observed {
                    checkout: checkout.clone(),
                    path: path.into(),
                    kind: ChangeKind::Removed,
                })
                .collect(),
            vec![],
        )
        .await;
    let entries = ledger(&daemon, &repo, None).await;
    assert_eq!(entries.len(), 5);
    assert_eq!(entries[3].by, attributed);
    assert_eq!(entries[4].by, Attribution::External);
}

/// Poll until `check` passes or five seconds elapse.
async fn eventually<T>(mut check: impl AsyncFnMut() -> Option<T>) -> T {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Some(value) = check().await {
            return value;
        }
        assert!(tokio::time::Instant::now() < deadline, "timed out");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

async fn status_of(daemon: &Arc<Daemon>, reference: &str) -> AgentStatus {
    match daemon
        .handle(Request::Inspect {
            agent: reference.to_owned(),
        })
        .await
    {
        Response::Agent { agent } => agent.status,
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test]
async fn removing_a_watched_worktree_keeps_real_deletions_without_false_conflicts() {
    if !have_git() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    let worktree = dir.path().join("temporary-checkout");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/a.rs"), "a\n").unwrap();
    assert!(git(dir.path(), &repo, &["init", "-q"]));
    assert!(git(dir.path(), &repo, &["add", "."]));
    assert!(git(dir.path(), &repo, &["commit", "-q", "-m", "root"]));
    assert!(git(
        dir.path(),
        &repo,
        &["worktree", "add", "--detach", worktree.to_str().unwrap()]
    ));
    let daemon = open(&dir);
    daemon.expect_watcher();
    let watcher = tokio::spawn(crate::watcher::run(
        daemon.clone(),
        std::time::Duration::from_millis(50),
        std::time::Duration::from_millis(50),
    ));
    let home = register_in(&daemon, "home", &repo).await;
    daemon.refresh_project_checkouts().await;
    daemon
        .ensure_watched(&home, WATCHER_ATTACH_TIMEOUT)
        .await
        .unwrap();
    // Reconcile also covers discovered checkouts without registered agents.
    assert!(
        daemon
            .watch_targets()
            .iter()
            .any(|target| target.dir == project::canonical(&worktree))
    );
    std::fs::remove_dir_all(&worktree).unwrap();
    std::fs::remove_file(repo.join("src/a.rs")).unwrap();
    // Reconcile before the OS necessarily delivers the surviving root's
    // deletion. Removing another watch must not discard that pending edit.
    daemon
        .ensure_watched(&home, WATCHER_ATTACH_TIMEOUT)
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let entries = ledger(&daemon, &repo, Some("src/a.rs")).await;
        let removed = entries
            .iter()
            .any(|entry| entry.kind == ChangeKind::Removed);
        let events = daemon.recent_events(200);
        let gap = events.iter().any(|event| {
            matches!(&event.kind, EventKind::WatcherGap { reason } if reason.contains("removed or became unavailable"))
        });
        if removed && gap {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "missing deletion={}; missing unavailable-checkout gap={}; ledger={entries:?}; events={events:?}",
            !removed,
            !gap,
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    // Drain callbacks already queued by the OS before checking absence.
    let flush = lock(&daemon.watcher_flush).clone().unwrap();
    let (ack, done) = oneshot::channel();
    flush.send(ack).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), done)
        .await
        .unwrap()
        .unwrap();
    let entries = ledger(&daemon, &repo, Some("src/a.rs")).await;
    assert!(
        entries.iter().all(|entry| entry.worktree.is_none()),
        "{entries:?}"
    );
    assert!(
        !daemon
            .recent_events(200)
            .iter()
            .any(|event| matches!(&event.kind, EventKind::ChannelOpened { .. }))
    );
    watcher.abort();
    let _ = watcher.await;
}

#[tokio::test]
async fn a_second_checkout_touching_a_path_opens_a_channel_by_itself() {
    if !have_git() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/a.rs"), "a\n").unwrap();
    std::fs::write(repo.join("src/b.rs"), "b\n").unwrap();
    assert!(git(dir.path(), &repo, &["init", "-q"]));
    assert!(git(dir.path(), &repo, &["add", "."]));
    assert!(git(dir.path(), &repo, &["commit", "-q", "-m", "root"]));
    let daemon = open(&dir);
    daemon.expect_watcher();
    tokio::spawn(crate::watcher::run(
        daemon.clone(),
        std::time::Duration::from_secs(60),
        std::time::Duration::from_millis(50),
    ));
    register_in(&daemon, "home", &repo).await;
    let mut command = spec("isolated");
    command.workdir = Some(repo.clone());
    command.isolate = true;
    command.command = vec!["sh".into(), "-c".into(), "sleep 10".into()];
    let Response::Agent { agent: isolated } = daemon.handle(Request::Run { spec: command }).await
    else {
        panic!("isolated run failed");
    };
    let worktree = isolated.spec.workdir.clone().unwrap();

    // One checkout changing a path is nobody's business.
    std::fs::write(repo.join("src/a.rs"), "a from main\n").unwrap();
    let channels = |daemon: Arc<Daemon>| async move {
        match daemon
            .handle(Request::Channels {
                project: String::new(),
                all: true,
                agent: Some("home".into()),
            })
            .await
        {
            Response::Channels { channels } => channels,
            other => panic!("{other:?}"),
        }
    };

    // The second checkout on the same path is: a channel opens itself
    // with both agents in it.
    std::fs::write(worktree.join("src/a.rs"), "a from the worktree\n").unwrap();
    let opened = eventually(async || {
        let found = channels(daemon.clone()).await;
        (!found.is_empty()).then_some(found)
    })
    .await;
    assert_eq!(opened.len(), 1, "{opened:?}");
    let channel = &opened[0];
    assert_eq!(channel.members.len(), 2, "both agents are in it");
    assert!(channel.opened_by.is_none(), "the daemon opened it");
    assert_eq!(channel.paths(), [PathBuf::from("src/a.rs")]);
    assert!(daemon.recent_events(200).iter().any(
        |e| matches!(&e.kind, EventKind::ChannelOpened { members, .. } if members.len() == 2)
    ));

    // A second contested path joins the same room rather than opening
    // another.
    std::fs::write(repo.join("src/b.rs"), "b from main\n").unwrap();
    std::fs::write(worktree.join("src/b.rs"), "b from the worktree\n").unwrap();
    let widened = eventually(async || {
        let found = channels(daemon.clone()).await;
        found.first().filter(|c| c.paths().len() == 2).cloned()
    })
    .await;
    assert_eq!(channels(daemon.clone()).await.len(), 1, "one room, not two");
    assert_eq!(
        widened.paths(),
        [PathBuf::from("src/a.rs"), PathBuf::from("src/b.rs")]
    );

    // Both were told when the room opened; the widening is told on
    // the tick, once for every path that joined since the last one.
    let told = |daemon: Arc<Daemon>| async move {
        match daemon
            .handle(Request::Inbox {
                agent: "home".into(),
                drain: true,
            })
            .await
        {
            Response::Messages { messages } => messages,
            other => panic!("{other:?}"),
        }
    };
    let opening = told(daemon.clone()).await;
    assert_eq!(opening.len(), 1, "{opening:?}");
    assert!(
        opening[0].payload["text"]
            .as_str()
            .unwrap()
            .starts_with("You are both changing"),
        "{opening:?}"
    );
    daemon.flush_notices();
    let widening = told(daemon.clone()).await;
    assert_eq!(widening.len(), 1, "{widening:?}");
    assert_eq!(
        widening[0].payload["text"],
        json!("src/b.rs is contested too; it is part of this channel now.")
    );
    daemon.flush_notices();
    assert!(told(daemon.clone()).await.is_empty(), "nothing more to say");
    // The journal says the room exists.
    let Response::Journal { entries, .. } = daemon
        .handle(Request::Journal {
            project: repo.to_string_lossy().into_owned(),
            since_seq: None,
            until_seq: None,
            agent: None,
            branch: None,
            kind: Some("review".into()),
            path: None,
            grep: None,
            limit: 50,
            digest: None,
        })
        .await
    else {
        panic!("journal failed");
    };
    assert!(!entries.is_empty(), "the channel is in the journal");

    // When the last member leaves, the room closes itself.
    daemon
        .handle(Request::Deregister {
            agent: "home".into(),
        })
        .await;
    daemon
        .handle(Request::Stop {
            agent: isolated.id.to_string(),
            force: true,
        })
        .await;
    let closed = eventually(async || {
        channels(daemon.clone())
            .await
            .into_iter()
            .find(|c| !c.is_open())
    })
    .await;
    assert_eq!(closed.resolution.as_deref(), Some("everyone left"));
}

#[tokio::test]
async fn a_managed_agent_with_a_tty_gets_a_terminal_and_a_session() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let mut command = spec("interactive");
    command.workdir = Some(dir.path().to_path_buf());
    command.tty = true;
    // Proves it is a terminal, then waits so the session is still there
    // to inspect.
    command.command = vec![
        "sh".into(),
        "-c".into(),
        "test -t 0 && echo I_HAVE_A_TTY; sleep 5".into(),
    ];
    let Response::Agent { agent } = daemon.handle(Request::Run { spec: command }).await else {
        panic!("managed launch failed");
    };
    assert!(agent.spec.tty);

    // The daemon holds its terminal, and what it prints reaches both a
    // watcher and the log.
    let session = eventually(async || daemon.session(&agent.id)).await;
    let (_, mut watching) = session.attach();
    let printed = eventually(async || {
        let log = std::fs::read_to_string(daemon.log_path(&agent.id)).unwrap_or_default();
        log.contains("I_HAVE_A_TTY").then_some(log)
    })
    .await;
    assert!(printed.contains("I_HAVE_A_TTY"), "{printed}");

    // Typing at it arrives: `cat` echoes on a terminal, so what we
    // send comes straight back to anyone attached.
    session.input.send(b"hello\n".to_vec()).await.unwrap();
    let echoed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut seen = String::new();
        while let Ok(bytes) = watching.recv().await {
            seen.push_str(&String::from_utf8_lossy(&bytes));
            if seen.contains("hello") {
                return seen;
            }
        }
        seen
    })
    .await
    .unwrap_or_default();
    assert!(
        echoed.contains("hello"),
        "input reached the terminal: {echoed:?}"
    );

    // Resizing is accepted while it lives.
    session.resize(100, 30).unwrap();

    // Attaching late still shows what it printed, and shows it once:
    // the scrollback and the live stream are taken together.
    let (seen, mut live) = session.attach();
    let seen = String::from_utf8_lossy(&seen).into_owned();
    assert!(seen.contains("I_HAVE_A_TTY"), "the screen so far: {seen:?}");
    session.input.send(b"after\n".to_vec()).await.unwrap();
    let next = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut got = String::new();
        while let Ok(bytes) = live.recv().await {
            got.push_str(&String::from_utf8_lossy(&bytes));
            if got.contains("after") {
                return got;
            }
        }
        got
    })
    .await
    .unwrap_or_default();
    assert!(next.contains("after"), "and what comes next: {next:?}");
    assert!(
        !next.contains("I_HAVE_A_TTY"),
        "without repeating the history: {next:?}"
    );

    // When it ends, so does the terminal: an attach afterwards has
    // nothing to connect to.
    daemon
        .handle(Request::Stop {
            agent: agent.id.to_string(),
            force: true,
        })
        .await;
    eventually(async || daemon.session(&agent.id).is_none().then_some(())).await;
    // Keep the client's Session alive: it must not hold a sender that
    // prevents an existing attachment from observing terminal EOF.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match live.recv().await {
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
            }
        }
    })
    .await
    .expect("an attached terminal closes when its child exits");
    let (final_history, mut ended) = session.attach();
    assert!(!final_history.is_empty());
    assert!(matches!(
        ended.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Closed)
    ));
}

#[tokio::test]
async fn managed_exit_publishes_only_after_terminal_and_pipe_logs_are_complete() {
    for tty in [false, true] {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let mut command = spec("output-before-exit");
        command.workdir = Some(dir.path().to_path_buf());
        command.tty = tty;
        command.command = vec![
            "sh".into(),
            "-c".into(),
            "i=0; while [ $i -lt 4096 ]; do printf 'OUT_%04d_END\\n' \"$i\"; printf 'ERR_%04d_END\\n' \"$i\" >&2; i=$((i+1)); done; exit 17".into(),
        ];
        let Response::Agent { agent } = daemon.handle(Request::Run { spec: command }).await else {
            panic!("managed launch failed");
        };
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        let finished = loop {
            let current = lock(&daemon.state)
                .registry
                .get(&agent.id)
                .cloned()
                .expect("managed agent remains registered");
            if !current.status.is_live() {
                break current;
            }
            if tokio::time::Instant::now() >= deadline {
                // Keep the original deadline and completion assertions.
                // A bare timeout cannot distinguish a slow writer, a live
                // child/group, or an exit report awaiting the controller.
                let log = std::fs::read_to_string(daemon.log_path(&agent.id));
                let progress = log.as_ref().map(|text| {
                    ["OUT", "ERR"].map(|stream| {
                        let prefix = format!("{stream}_");
                        let markers: Vec<_> = text
                            .split_whitespace()
                            .filter(|word| word.starts_with(&prefix))
                            .collect();
                        (stream, markers.len(), markers.last().copied())
                    })
                });
                let exit = std::fs::read_to_string(agentdocker_core::session::exit_path(
                    &daemon.home,
                    &agent.id,
                ));
                panic!(
                    "managed output completion timed out: tty={tty}, status={:?}, \
                     child_pid={:?}, recorded_birth={:?}, observed_birth={:?}, \
                     group_exists={:?}, owner={:?}, terminal_attached={}, \
                     log_bytes={:?}, progress={progress:?}, exit_report={exit:?}",
                    current.status,
                    current.pid,
                    current.process_started_at,
                    current.pid.and_then(procinfo::start_time),
                    current.process_group.map(supervisor::group_exists),
                    current.owner,
                    daemon.session(&agent.id).is_some(),
                    log.as_ref().map(|text| text.len()),
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        };
        assert_eq!(finished.status, AgentStatus::Exited { code: Some(17) });
        // No eventual log polling: reported completion is the boundary
        // at which shutdown, restart and a successor can rely on the log.
        let log = std::fs::read_to_string(daemon.log_path(&agent.id)).unwrap();
        for stream in ["OUT", "ERR"] {
            let prefix = format!("{stream}_");
            let markers: Vec<_> = log
                .split_whitespace()
                .filter(|word| word.starts_with(&prefix))
                .collect();
            assert_eq!(
                markers.len(),
                4096,
                "tty={tty}: incomplete {stream} log at exit"
            );
            for (index, marker) in markers.into_iter().enumerate() {
                assert_eq!(
                    marker,
                    format!("{stream}_{index:04}_END"),
                    "tty={tty}: missing, reordered or duplicated {stream} line"
                );
            }
        }
    }
}

#[tokio::test]
async fn an_agent_without_a_tty_has_no_session() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let mut command = spec("piped");
    command.workdir = Some(dir.path().to_path_buf());
    command.command = vec!["sh".into(), "-c".into(), "echo piped; sleep 5".into()];
    let Response::Agent { agent } = daemon.handle(Request::Run { spec: command }).await else {
        panic!("managed launch failed");
    };
    assert!(!agent.spec.tty);
    let printed = eventually(async || {
        let log = std::fs::read_to_string(daemon.log_path(&agent.id)).unwrap_or_default();
        log.contains("piped").then_some(log)
    })
    .await;
    assert!(printed.contains("piped"));
    assert!(daemon.session(&agent.id).is_none(), "pipes, not a terminal");
    daemon
        .handle(Request::Stop {
            agent: agent.id.to_string(),
            force: true,
        })
        .await;
}

#[tokio::test]
async fn run_isolate_gives_the_agent_its_own_worktree_and_branch() {
    if !have_git() {
        return;
    }
    let dir = TempDir::new().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    assert!(git(dir.path(), &repo, &["init", "-q"]));
    assert!(git(
        dir.path(),
        &repo,
        &["commit", "-q", "--allow-empty", "-m", "root"]
    ));
    let daemon = Arc::new(Daemon::open(dir.path().join("state"), dir.path().join("sock")).unwrap());
    let home = register_in(&daemon, "home", &repo).await;

    let mut command = spec("writer");
    command.workdir = Some(repo.clone());
    command.isolate = true;
    command.command = vec!["sh".into(), "-c".into(), "sleep 0.2".into()];
    let Response::Agent { agent } = daemon
        .handle(Request::Run {
            spec: command.clone(),
        })
        .await
    else {
        panic!("isolated run failed");
    };
    let worktree = agent
        .spec
        .workdir
        .clone()
        .expect("workdir moved to the worktree");
    let worktrees = project::canonical(&paths::worktree_dir(&daemon.home));
    assert!(worktree.starts_with(&worktrees), "{}", worktree.display());
    assert!(worktree.ends_with("writer"), "{}", worktree.display());
    assert!(worktree.join(".git").exists(), "a linked worktree");
    assert!(agent.spec.isolate);
    let project = agent.project.clone().expect("in a project");
    assert_eq!(
        project.id(),
        home.project.as_ref().unwrap().id(),
        "same repository"
    );
    assert_eq!(project.worktree.as_deref(), Some(worktree.as_path()));
    assert_eq!(
        agent.vcs.as_ref().and_then(|v| v.branch.as_deref()),
        Some("agent/writer")
    );
    assert!(
        daemon
            .recent_events(50)
            .iter()
            .any(|e| matches!(&e.kind, EventKind::WorktreeCreated { agent: who, path } if *who == agent.id && *path == worktree))
    );

    // The same name again, once the first has finished: the path is
    // taken, so the second gets its id appended.
    eventually(async || (!status_of(&daemon, "writer").await.is_live()).then_some(())).await;
    let Response::Agent { agent: again } = daemon
        .handle(Request::Run {
            spec: command.clone(),
        })
        .await
    else {
        panic!("second isolated run failed");
    };
    let second = again.spec.workdir.clone().unwrap();
    assert_ne!(second, worktree);
    assert!(
        second
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("writer-")
    );
    assert_eq!(
        again.vcs.as_ref().and_then(|v| v.branch.as_deref()),
        Some(format!("agent/writer-{}", again.id.short()).as_str())
    );

    // A name git could not take: refused before anything is created.
    let mut command = spec("bad name!");
    command.workdir = Some(repo.clone());
    command.isolate = true;
    command.command = vec!["sh".into(), "-c".into(), "true".into()];
    assert!(matches!(
        daemon.handle(Request::Run { spec: command }).await,
        Response::Error { code: ErrorCode::Invalid, message, .. } if message.contains("bad name!")
    ));
    assert!(!worktrees.join("bad name!").exists());
    for (name, ok) in [
        ("writer", true),
        ("w.1_2-3", true),
        ("-lead", false),
        (".hidden", false),
        ("a..b", false),
        ("x.lock", false),
        ("with space", false),
        ("", false),
    ] {
        assert_eq!(isolate_name_ok(name), ok, "{name:?}");
    }

    // Not a repository: refused, nothing spawned.
    let plain = dir.path().join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let mut command = spec("loose");
    command.workdir = Some(plain);
    command.isolate = true;
    command.command = vec!["sh".into(), "-c".into(), "true".into()];
    assert!(matches!(
        daemon.handle(Request::Run { spec: command }).await,
        Response::Error {
            code: ErrorCode::Invalid,
            ..
        }
    ));
}

#[tokio::test]
async fn launch_refuses_failed_or_unconfirmed_checkout_coverage() {
    if !have_git() {
        return;
    }
    for failure in ["startup", "attachment", "timeout"] {
        let dir = TempDir::new().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        assert!(git(dir.path(), &repo, &["init", "-q"]));
        assert!(git(
            dir.path(),
            &repo,
            &["commit", "-q", "--allow-empty", "-m", "root"]
        ));
        let daemon = open(&dir);
        daemon.expect_watcher();
        let (tx, mut rx) = mpsc::channel::<WatcherAttachment>(1);
        let responder = if failure == "startup" {
            daemon.watcher_off("startup failed".into());
            None
        } else if failure == "attachment" {
            daemon.set_watcher_attach(tx);
            let expected = project::canonical(&repo);
            Some(tokio::spawn(async move {
                let request = rx.recv().await.unwrap();
                assert_eq!(request.checkout, expected);
                request
                    .ack
                    .send(Err("watch installation failed".into()))
                    .unwrap();
            }))
        } else {
            None
        };
        let mut command = spec("blocked-writer");
        command.workdir = Some(repo.clone());
        command.command = vec!["sh".into(), "-c".into(), "touch should-not-exist".into()];
        let response = daemon.handle(Request::Run { spec: command }).await;
        assert!(
            matches!(
                response,
                Response::Error {
                    code: ErrorCode::Unavailable,
                    ..
                }
            ),
            "{failure}: {response:?}"
        );
        assert!(!repo.join("should-not-exist").exists());
        assert!(lock(&daemon.state).supervised.is_empty());
        assert_eq!(lock(&daemon.state).registry.live().count(), 0);
        if let Some(task) = responder {
            task.await.unwrap();
        }
    }
}

#[tokio::test]
async fn failed_isolated_launches_clean_up_only_unchanged_worktrees() {
    if !have_git() {
        return;
    }
    for failure in ["duplicate", "watcher", "edited"] {
        let dir = TempDir::new().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        assert!(git(dir.path(), &repo, &["init", "-q"]));
        assert!(git(
            dir.path(),
            &repo,
            &["commit", "-q", "--allow-empty", "-m", "root"]
        ));
        let daemon =
            Arc::new(Daemon::open(dir.path().join("state"), dir.path().join("sock")).unwrap());
        let mut responder = None;
        if failure == "duplicate" {
            register_in(&daemon, "blocked", &repo).await;
        } else if failure == "watcher" {
            daemon.watcher_off("cannot start".into());
        } else {
            let (tx, mut rx) = mpsc::channel::<WatcherAttachment>(1);
            daemon.set_watcher_attach(tx);
            responder = Some(tokio::spawn(async move {
                let request = rx.recv().await.unwrap();
                std::fs::write(request.checkout.join("keep-me"), "user edit").unwrap();
                request.ack.send(Err("attachment failed".into())).unwrap();
            }));
        }
        let mut command = spec("blocked");
        command.isolate = true;
        command.workdir = Some(repo.clone());
        command.command = vec!["sh".into(), "-c".into(), "touch should-not-run".into()];
        assert!(matches!(
            daemon.handle(Request::Run { spec: command }).await,
            Response::Error { .. }
        ));
        if let Some(task) = responder {
            task.await.unwrap();
        }
        let events = daemon.recent_events(30);
        let (path, removed) = events
            .iter()
            .find_map(|event| match &event.kind {
                EventKind::WorktreeCleanup {
                    path,
                    worktree_removed,
                    ..
                } => Some((path.clone(), *worktree_removed)),
                _ => None,
            })
            .expect("cleanup is observable");
        assert!(!path.join("should-not-run").exists());
        if failure == "edited" {
            assert!(!removed);
            assert_eq!(
                std::fs::read_to_string(path.join("keep-me")).unwrap(),
                "user edit"
            );
        } else {
            assert!(removed, "{failure}");
            assert!(!path.exists());
            assert!(
                git(dir.path(), &repo, &["branch", "agent/blocked"]),
                "failed branch was removed and its name is reusable"
            );
        }
    }
}

#[tokio::test]
async fn overlap_names_paths_changed_in_two_checkouts() {
    if !have_git() {
        return;
    }
    let dir = TempDir::new().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/a.rs"), "a\n").unwrap();
    std::fs::write(repo.join("src/b.rs"), "b\n").unwrap();
    assert!(git(dir.path(), &repo, &["init", "-q"]));
    assert!(git(dir.path(), &repo, &["add", "."]));
    assert!(git(dir.path(), &repo, &["commit", "-q", "-m", "root"]));
    let daemon = Arc::new(Daemon::open(dir.path().join("state"), dir.path().join("sock")).unwrap());
    daemon.expect_watcher();
    tokio::spawn(crate::watcher::run(
        daemon.clone(),
        std::time::Duration::from_secs(60),
        std::time::Duration::from_millis(50),
    ));
    let a = register_in(&daemon, "a", &repo).await;
    let mut command = spec("b");
    command.workdir = Some(repo.clone());
    command.isolate = true;
    command.command = vec!["sh".into(), "-c".into(), "sleep 5".into()];
    let Response::Agent { agent: b } = daemon.handle(Request::Run { spec: command }).await else {
        panic!("isolated run failed");
    };
    let worktree = b.spec.workdir.clone().unwrap();
    std::fs::write(repo.join("src/a.rs"), "a from main\n").unwrap();
    std::fs::write(worktree.join("src/a.rs"), "a from the worktree\n").unwrap();
    std::fs::write(worktree.join("src/b.rs"), "only here\n").unwrap();
    let overlaps = eventually(async || {
        match daemon
            .handle(Request::Overlap {
                project: repo.to_string_lossy().into_owned(),
                since_seq: None,
                agent: None,
            })
            .await
        {
            Response::Overlap { overlaps } if !overlaps.is_empty() => Some(overlaps),
            _ => None,
        }
    })
    .await;
    assert_eq!(overlaps.len(), 1, "{overlaps:?}");
    assert_eq!(overlaps[0].path, Path::new("src/a.rs"));
    assert_eq!(overlaps[0].parties.len(), 2);
    assert!(
        overlaps[0]
            .parties
            .iter()
            .any(|p| p.checkout == project::canonical(&repo))
    );
    assert!(
        overlaps[0]
            .parties
            .iter()
            .any(|p| p.checkout == worktree && p.worktree.is_some())
    );
    // Nobody held the files: external on both sides.
    assert!(overlaps[0].parties.iter().all(|p| p.agents.is_empty()));
    // Seen from an agent: its checkout must be a party, and an empty
    // project means its own.
    let Response::Overlap { overlaps: mine } = daemon
        .handle(Request::Overlap {
            project: String::new(),
            since_seq: None,
            agent: Some(a.id.to_string()),
        })
        .await
    else {
        panic!("overlap failed");
    };
    assert_eq!(mine.len(), 1);
    assert!(matches!(
        daemon
            .handle(Request::Overlap {
                project: String::new(),
                since_seq: None,
                agent: None,
            })
            .await,
        Response::Error {
            code: ErrorCode::Invalid,
            ..
        }
    ));
}

#[tokio::test]
async fn registration_attaches_the_watcher_before_replying() {
    if !have_git() {
        return;
    }
    let dir = TempDir::new().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    assert!(git(dir.path(), &repo, &["init", "-q"]));
    assert!(git(
        dir.path(),
        &repo,
        &["commit", "-q", "--allow-empty", "-m", "root"]
    ));
    let daemon = open(&dir);
    // A reconcile tick that never comes during the test: only the
    // registration's own nudge can attach the watch. The watcher is
    // spawned and the agent registered at once, with no time for the
    // spawned task to have run: startup is waited for, not skipped.
    daemon.expect_watcher();
    tokio::spawn(crate::watcher::run(
        daemon.clone(),
        std::time::Duration::from_secs(60),
        std::time::Duration::from_millis(50),
    ));
    register_in(&daemon, "a", &repo).await;
    let lib = repo.join("src/lib.rs");
    std::fs::write(&lib, "fn a() {}\n").unwrap();
    let seen = eventually(async || {
        ledger(&daemon, &repo, None)
            .await
            .into_iter()
            .find(|c| c.path == Path::new("src/lib.rs"))
    })
    .await;
    assert!(
        seen.seq > 0,
        "an edit right after registration is in the ledger"
    );
    let kinds: Vec<EventKind> = daemon
        .recent_events(50)
        .into_iter()
        .map(|e| e.kind)
        .collect();
    let starting = kinds
        .iter()
        .position(|k| matches!(k, EventKind::WatcherStarting))
        .expect("starting announced");
    let started = kinds
        .iter()
        .position(|k| matches!(k, EventKind::WatcherStarted))
        .expect("started announced");
    assert!(starting < started);
}

#[tokio::test]
async fn watcher_records_attributed_changes_and_refreshes_branches() {
    if !have_git() {
        return;
    }
    let dir = TempDir::new().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join(".gitignore"), "target/\n").unwrap();
    assert!(git(dir.path(), &repo, &["init", "-q"]));
    assert!(git(dir.path(), &repo, &["add", "."]));
    assert!(git(dir.path(), &repo, &["commit", "-q", "-m", "root"]));
    let daemon = open(&dir);
    let a = register_in(&daemon, "a", &repo).await;
    tokio::spawn(crate::watcher::run(
        daemon.clone(),
        std::time::Duration::from_millis(50),
        std::time::Duration::from_millis(50),
    ));
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;

    // `a` holds src/lib.rs; nobody holds notes.md; target/ is ignored.
    let lib = repo.join("src/lib.rs");
    let Response::Lease { lease } = claim(&daemon, "a", &format!("path:{}", lib.display())).await
    else {
        panic!("claim failed");
    };
    std::fs::write(&lib, "fn a() {}\n").unwrap();
    std::fs::write(repo.join("notes.md"), "hi\n").unwrap();
    std::fs::create_dir_all(repo.join("target")).unwrap();
    std::fs::write(repo.join("target/out.bin"), "x").unwrap();

    let (mine, theirs) = eventually(async || {
        let entries = ledger(&daemon, &repo, None).await;
        let mine = entries
            .iter()
            .find(|c| c.path == Path::new("src/lib.rs"))?
            .clone();
        let theirs = entries
            .iter()
            .find(|c| c.path == Path::new("notes.md"))?
            .clone();
        Some((mine, theirs))
    })
    .await;
    assert_eq!(
        mine.by,
        Attribution::Agent {
            agent: a.id.clone(),
            lease: lease.id.clone(),
            note: None,
        }
    );
    assert!(mine.seq > 0);
    assert!(mine.head.is_some(), "head recorded");
    assert_eq!(theirs.by, Attribution::External);
    // Queries narrow by path (absolute, made relative by the daemon).
    let by_path = ledger(&daemon, &repo, Some(&lib.to_string_lossy())).await;
    assert!(!by_path.is_empty() && by_path.iter().all(|c| c.path == Path::new("src/lib.rs")));
    std::fs::write(repo.join("watcher-marker"), "processed").unwrap();
    eventually(async || {
        ledger(&daemon, &repo, None)
            .await
            .iter()
            .any(|c| c.path == Path::new("watcher-marker"))
            .then_some(())
    })
    .await;
    assert!(
        ledger(&daemon, &repo, None)
            .await
            .iter()
            .all(|c| !c.path.starts_with("target")),
        "ignored paths never reach the ledger"
    );

    // A branch switch reaches the record through the watcher, not a poll.
    assert!(git(dir.path(), &repo, &["checkout", "-q", "-b", "feature"]));
    eventually(async || {
        match daemon
            .handle(Request::Inspect {
                agent: "a".to_owned(),
            })
            .await
        {
            Response::Agent { agent }
                if agent.vcs.as_ref().and_then(|v| v.branch.as_deref()) == Some("feature") =>
            {
                Some(())
            }
            _ => None,
        }
    })
    .await;
}

async fn journal_of(
    daemon: &Arc<Daemon>,
    project: &Path,
    kind: Option<&str>,
    path: Option<&str>,
    grep: Option<&str>,
) -> Vec<JournalEntry> {
    match daemon
        .handle(Request::Journal {
            project: project.to_string_lossy().into_owned(),
            since_seq: None,
            until_seq: None,
            agent: None,
            branch: None,
            kind: kind.map(str::to_owned),
            path: path.map(str::to_owned),
            grep: grep.map(str::to_owned),
            limit: 50,
            digest: None,
        })
        .await
    {
        Response::Journal { entries, .. } => entries,
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test]
async fn releases_notes_joins_and_leaves_are_journaled() {
    if !have_git() {
        return;
    }
    let dir = TempDir::new().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join(".gitignore"), "target/\n").unwrap();
    assert!(git(dir.path(), &repo, &["init", "-q"]));
    assert!(git(dir.path(), &repo, &["add", "."]));
    assert!(git(dir.path(), &repo, &["commit", "-q", "-m", "root"]));
    let daemon = open(&dir);
    let a = register_in(&daemon, "a", &repo).await;
    tokio::spawn(crate::watcher::run(
        daemon.clone(),
        std::time::Duration::from_millis(50),
        std::time::Duration::from_millis(50),
    ));
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;

    // Joining is the first entry.
    let entries = journal_of(&daemon, &repo, None, None, None).await;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].kind, JournalKind::Join);
    assert_eq!(entries[0].seq, 1);
    assert!(
        entries[0].summary.contains("branch main"),
        "{}",
        entries[0].summary
    );

    // Two files edited under leases, released with no summary: one
    // synthesised entry naming both, with the ledger range.
    let lib = repo.join("src/lib.rs");
    let main = repo.join("src/main.rs");
    for path in [&lib, &main] {
        assert!(matches!(
            claim(&daemon, "a", &format!("path:{}", path.display())).await,
            Response::Lease { .. }
        ));
    }
    std::fs::write(&lib, "fn a() {}\n").unwrap();
    std::fs::write(&main, "fn main() {}\n").unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    let Response::Leases { leases } = daemon
        .handle(Request::ReleaseAll {
            agent: "a".to_owned(),
            summary: None,
            summary_source: SummarySource::Explicit,
            only_automatic: false,
        })
        .await
    else {
        panic!("release_all failed");
    };
    assert_eq!(leases.len(), 2);
    let entries = journal_of(&daemon, &repo, Some("release"), None, None).await;
    assert_eq!(entries.len(), 1, "{entries:?}");
    let release = &entries[0];
    assert_eq!(release.summary_source, SummarySource::Synthesised);
    assert_eq!(
        release.summary,
        "edited 2 files under src/: lib.rs, main.rs"
    );
    assert_eq!(
        release.paths,
        vec![PathBuf::from("src/lib.rs"), PathBuf::from("src/main.rs")]
    );
    assert_eq!(release.paths_total, 2);
    assert!(release.changes.is_some_and(|(lo, hi)| lo <= hi));
    assert_eq!(release.resources.len(), 2);
    assert_eq!(release.agent, Some(a.id.clone()));
    assert!(release.head_after.is_some());

    // The barrier: a change made right before the release, with no
    // wait for the watcher's debounce, is still in the entry.
    assert!(matches!(
        claim(&daemon, "a", &format!("path:{}", lib.display())).await,
        Response::Lease { .. }
    ));
    std::fs::write(&lib, "fn a() { /* parser */ }\n").unwrap();
    let Response::Leases { .. } = daemon
        .handle(Request::ReleaseAll {
            agent: "a".to_owned(),
            summary: Some("rewrote the parser".to_owned()),
            summary_source: SummarySource::Explicit,
            only_automatic: false,
        })
        .await
    else {
        panic!("release_all failed");
    };
    let entries = journal_of(&daemon, &repo, Some("release"), None, None).await;
    assert_eq!(entries.len(), 2);
    let explicit = &entries[1];
    assert_eq!(explicit.summary_source, SummarySource::Explicit);
    assert_eq!(explicit.summary, "rewrote the parser");
    assert_eq!(
        explicit.paths,
        vec![PathBuf::from("src/lib.rs")],
        "barrier flushed the watcher"
    );
    assert!(explicit.seq > release.seq);

    // Nothing changed and nothing said: no entry.
    assert!(matches!(
        claim(&daemon, "a", &format!("path:{}", main.display())).await,
        Response::Lease { .. }
    ));
    daemon
        .handle(Request::ReleaseAll {
            agent: "a".to_owned(),
            summary: None,
            summary_source: SummarySource::Explicit,
            only_automatic: false,
        })
        .await;
    assert_eq!(
        journal_of(&daemon, &repo, Some("release"), None, None)
            .await
            .len(),
        2
    );

    // Nothing held, but something said: the words are kept.
    daemon
        .handle(Request::ReleaseAll {
            agent: "a".to_owned(),
            summary: Some("  reviewed the plan, no edits  ".to_owned()),
            summary_source: SummarySource::Explicit,
            only_automatic: false,
        })
        .await;
    let entries = journal_of(&daemon, &repo, Some("release"), None, None).await;
    assert_eq!(entries.len(), 3);
    let said = &entries[2];
    assert_eq!(said.summary, "reviewed the plan, no edits");
    assert_eq!(said.summary_source, SummarySource::Explicit);
    assert!(said.paths.is_empty() && said.resources.is_empty());

    // Notes, and the filters.
    let Response::JournalEntry { entry: note } = daemon
        .handle(Request::JournalAdd {
            agent: "a".to_owned(),
            summary: "lexer is next".to_owned(),
        })
        .await
    else {
        panic!("journal_add failed");
    };
    assert_eq!(note.kind, JournalKind::Note);
    assert_eq!(
        journal_of(&daemon, &repo, Some("note"), None, None)
            .await
            .len(),
        1
    );
    let by_path = journal_of(&daemon, &repo, None, Some(&lib.to_string_lossy()), None).await;
    assert_eq!(by_path.len(), 2, "both releases touched src/lib.rs");
    let by_dir = journal_of(&daemon, &repo, None, Some("src"), None).await;
    assert_eq!(by_dir.len(), 2);
    let grep = journal_of(&daemon, &repo, None, None, Some("parser")).await;
    assert_eq!(grep.len(), 1);
    assert_eq!(grep[0].seq, explicit.seq);
    assert!(matches!(
        daemon
            .handle(Request::Journal {
                project: repo.to_string_lossy().into_owned(),
                since_seq: None,
                until_seq: None,
                agent: None,
                branch: None,
                kind: Some("bogus".to_owned()),
                path: None,
                grep: None,
                limit: 50,
                digest: None,
            })
            .await,
        Response::Error {
            code: ErrorCode::Invalid,
            ..
        }
    ));

    // Leaving is journaled, and the ring survives a prune.
    daemon
        .handle(Request::Deregister {
            agent: "a".to_owned(),
        })
        .await;
    let all = journal_of(&daemon, &repo, None, None, None).await;
    assert_eq!(all.last().map(|e| e.kind), Some(JournalKind::Leave));
    let seqs: Vec<u64> = all.iter().map(|e| e.seq).collect();
    assert_eq!(
        seqs,
        (1..=seqs.len() as u64).collect::<Vec<_>>(),
        "dense per-project seqs"
    );
    let Response::Pruned { removed } = daemon
        .handle(Request::JournalPrune {
            project: repo.to_string_lossy().into_owned(),
            before_seq: Some(3),
            older_than_secs: None,
        })
        .await
    else {
        panic!("prune failed");
    };
    assert_eq!(removed, 2);
    assert_eq!(journal_of(&daemon, &repo, None, None, None).await[0].seq, 3);

    // A restart continues the seq and reloads the ring.
    drop(daemon);
    let daemon = open(&dir);
    register_in(&daemon, "b", &repo).await;
    let after = journal_of(&daemon, &repo, None, None, None).await;
    assert_eq!(after.last().map(|e| e.kind), Some(JournalKind::Join));
    assert_eq!(after.last().map(|e| e.seq), Some(seqs.len() as u64 + 1));
}

#[tokio::test]
async fn retention_prunes_in_batches_from_the_daemon_config() {
    if !have_git() {
        return;
    }
    let dir = TempDir::new().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("README"), "x\n").unwrap();
    assert!(git(dir.path(), &repo, &["init", "-q"]));
    assert!(git(dir.path(), &repo, &["add", "."]));
    assert!(git(dir.path(), &repo, &["commit", "-q", "-m", "root"]));
    let daemon = open(&dir);
    let a = register_in(&daemon, "a", &repo).await;
    let project = a.project.as_ref().expect("registered in a repo").id();
    // Ages fall as seqs rise, as they do in a real journal: the join is
    // 40 days old, then twelve notes from 37 days old down to 4, then
    // one fresh note.
    let head = journal_of(&daemon, &repo, None, None, None).await[0].seq;
    for i in 1..=12u64 {
        daemon
            .handle(Request::JournalAdd {
                agent: a.id.to_string(),
                summary: format!("old {i}"),
            })
            .await;
    }
    {
        let state = lock(&daemon.state);
        state
            .store
            .age_journal_for_test(&project, head, Utc::now() - Duration::days(40));
        for i in 1..=12u64 {
            state.store.age_journal_for_test(
                &project,
                head + i,
                Utc::now() - Duration::days(40 - 3 * i as i64),
            );
        }
    }
    daemon
        .handle(Request::JournalAdd {
            agent: a.id.to_string(),
            summary: "fresh".into(),
        })
        .await;
    assert_eq!(journal_of(&daemon, &repo, None, None, None).await.len(), 14);

    // No file: nothing is touched. A broken file: nothing is touched either.
    daemon.apply_journal_retention();
    assert_eq!(journal_of(&daemon, &repo, None, None, None).await.len(), 14);
    let config = daemon.home.join("agentd.toml");
    std::fs::write(&config, "[journal]\nretention = \"soon\"\n").unwrap();
    daemon.apply_journal_retention();
    assert_eq!(journal_of(&daemon, &repo, None, None, None).await.len(), 14);
    assert!(lock(&daemon.state).config_notice.is_some());

    // One tick prunes everything older than the window (the batch is
    // larger than the backlog), the ring agrees, and the event says why.
    std::fs::write(&config, "[journal]\nretention = \"20d\"\n").unwrap();
    daemon.apply_journal_retention();
    assert!(lock(&daemon.state).config_notice.is_none());
    let left = journal_of(&daemon, &repo, None, None, None).await;
    let seqs: Vec<u64> = left.iter().map(|e| e.seq).collect();
    // The join and notes 1..=6 (37..22 days) go; 7..=12 (19..4) and the
    // fresh note stay.
    assert_eq!(seqs, (head + 7..=head + 13).collect::<Vec<_>>());
    let pruned = daemon
        .recent_events(50)
        .into_iter()
        .filter_map(|e| match e.kind {
            EventKind::JournalPruned {
                removed, reason, ..
            } => Some((removed, reason)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(pruned, vec![(7, "retention".to_owned())]);
    // A second tick finds nothing older and emits nothing.
    daemon.apply_journal_retention();
    assert_eq!(journal_of(&daemon, &repo, None, None, None).await.len(), 7);

    // A request by age uses the same boundary and says so.
    let Response::Pruned { removed } = daemon
        .handle(Request::JournalPrune {
            project: repo.to_string_lossy().into_owned(),
            before_seq: None,
            older_than_secs: Some(18 * 24 * 3600 + 3600),
        })
        .await
    else {
        panic!("prune failed");
    };
    assert_eq!(
        removed, 1,
        "only the 19-day note is older than 18 days and an hour"
    );
    assert_eq!(journal_of(&daemon, &repo, None, None, None).await.len(), 6);
    assert!(matches!(
        daemon
            .handle(Request::JournalPrune {
                project: repo.to_string_lossy().into_owned(),
                before_seq: None,
                older_than_secs: None,
            })
            .await,
        Response::Error {
            code: ErrorCode::Invalid,
            ..
        }
    ));
    // Agent `a` is live: vacuum waits for a quiet daemon unless forced.
    assert!(matches!(
        daemon.handle(Request::Vacuum { force: false }).await,
        Response::Error {
            code: ErrorCode::Conflict,
            ..
        }
    ));
    assert!(matches!(
        daemon.handle(Request::Vacuum { force: true }).await,
        Response::Vacuumed { before_bytes, after_bytes } if before_bytes > 0 && after_bytes > 0
    ));
}

async fn digest_for(
    daemon: &Arc<Daemon>,
    project: &str,
    reader: &str,
    since: Option<u64>,
    budget: DigestBudget,
    advance: bool,
) -> agentdocker_core::Digest {
    match daemon
        .handle(Request::Journal {
            project: project.to_owned(),
            since_seq: since,
            until_seq: None,
            agent: None,
            branch: None,
            kind: None,
            path: None,
            grep: None,
            limit: 50,
            digest: Some(DigestRequest {
                reader: reader.to_owned(),
                max_entries: budget.max_entries,
                max_chars: budget.max_chars,
                all_branches: false,
                advance,
            }),
        })
        .await
    {
        Response::Digest { digest, .. } => digest,
        other => panic!("unexpected {other:?}"),
    }
}

async fn note(daemon: &Arc<Daemon>, agent: &str, text: &str) -> u64 {
    match daemon
        .handle(Request::JournalAdd {
            agent: agent.to_owned(),
            summary: text.to_owned(),
        })
        .await
    {
        Response::JournalEntry { entry } => entry.seq,
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test]
async fn digests_follow_cursors_that_are_seeded_by_name() {
    if !have_git() {
        return;
    }
    let dir = TempDir::new().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    assert!(git(dir.path(), &repo, &["init", "-q"]));
    let daemon = open(&dir);
    let repo_ref = repo.to_string_lossy().into_owned();
    register_in(&daemon, "a", &repo).await;
    for i in 0..25 {
        note(&daemon, "a", &format!("note {i}")).await;
    }

    // A newcomer is told the twenty newest entries, not its own join.
    let b = register_in(&daemon, "b", &repo).await;
    let first = digest_for(
        &daemon,
        &repo_ref,
        "b",
        None,
        DigestBudget::SESSION_START,
        true,
    )
    .await;
    assert_eq!(
        (first.shown, first.collapsed, first.other_branches),
        (20, 0, 0),
        "{}",
        first.text
    );
    assert!(
        first
            .text
            .starts_with("Since you last looked (20 entries):\n")
    );
    assert!(first.text.contains("noted: \"note 24\""));
    assert!(!first.text.contains("noted: \"note 4\""), "{}", first.text);
    assert!(
        !first.text.contains("b [") || !first.text.contains("joined"),
        "{}",
        first.text
    );

    // Advanced: nothing new until something happens, then only that.
    let again = digest_for(
        &daemon,
        &repo_ref,
        "b",
        None,
        DigestBudget::SESSION_START,
        true,
    )
    .await;
    assert_eq!(again.text, "");
    assert_eq!(again.head_seq, first.head_seq);
    let seq = note(&daemon, "a", "parser is next").await;
    let prompt = digest_for(&daemon, &repo_ref, "b", None, DigestBudget::PROMPT, true).await;
    assert_eq!((prompt.shown, prompt.head_seq), (1, seq), "{}", prompt.text);
    assert!(prompt.text.contains("parser is next"));

    // An empty project means the reader's own; a since overrides the
    // cursor without losing it.
    let own = digest_for(
        &daemon,
        "",
        "b",
        Some(0),
        DigestBudget::SESSION_START,
        false,
    )
    .await;
    assert_eq!(
        own.collapsed + own.shown,
        27,
        "a's join and notes: {}",
        own.text
    );
    assert_eq!(
        digest_for(&daemon, &repo_ref, "b", None, DigestBudget::PROMPT, true)
            .await
            .text,
        ""
    );

    // The human has a cursor too.
    let user = digest_for(
        &daemon,
        &repo_ref,
        "user",
        None,
        DigestBudget::SESSION_START,
        true,
    )
    .await;
    assert_eq!(user.shown, 20, "{}", user.text);
    note(&daemon, "a", "for the human").await;
    let user = digest_for(
        &daemon,
        &repo_ref,
        "user",
        None,
        DigestBudget::SESSION_START,
        false,
    )
    .await;
    assert_eq!(user.shown, 1);
    assert!(user.text.contains("for the human"));

    // b leaves and comes back under the same name: its cursor carries
    // over, so it is not told what it already saw, nor its own leave.
    daemon
        .handle(Request::Deregister {
            agent: "b".to_owned(),
        })
        .await;
    note(&daemon, "a", "while b was away").await;
    let b2 = register_in(&daemon, "b", &repo).await;
    assert_ne!(b2.id, b.id);
    let resumed = digest_for(
        &daemon,
        &repo_ref,
        "b",
        None,
        DigestBudget::SESSION_START,
        true,
    )
    .await;
    assert_eq!(resumed.shown, 2, "{}", resumed.text);
    assert!(resumed.text.contains("for the human"));
    assert!(resumed.text.contains("while b was away"));
    assert!(!resumed.text.contains("left"), "{}", resumed.text);

    // A stranger's name inherits nothing, and neither does a namesake
    // in another project.
    let other = dir.path().join("other");
    std::fs::create_dir_all(&other).unwrap();
    assert!(git(dir.path(), &other, &["init", "-q"]));
    register_in(&daemon, "c", &other).await;
    note(&daemon, "c", "elsewhere").await;
    daemon
        .handle(Request::Deregister {
            agent: "c".to_owned(),
        })
        .await;
    register_in(&daemon, "c", &repo).await;
    let fresh = digest_for(
        &daemon,
        &repo_ref,
        "c",
        None,
        DigestBudget::SESSION_START,
        false,
    )
    .await;
    assert_eq!(
        fresh.shown, 20,
        "seeded from history, not from c's other-project cursor"
    );

    // A transcript tail describes released leases only: with none
    // held it writes nothing, unlike an explicit summary.
    let before = journal_of(&daemon, &repo, Some("release"), None, None)
        .await
        .len();
    daemon
        .handle(Request::ReleaseAll {
            agent: "a".to_owned(),
            summary: Some("I finished the parser.".to_owned()),
            summary_source: SummarySource::Transcript,
            only_automatic: false,
        })
        .await;
    assert_eq!(
        journal_of(&daemon, &repo, Some("release"), None, None)
            .await
            .len(),
        before
    );
    let src = repo.join("src.rs");
    assert!(matches!(
        claim(&daemon, "a", &format!("path:{}", src.display())).await,
        Response::Lease { .. }
    ));
    daemon
        .handle(Request::ReleaseAll {
            agent: "a".to_owned(),
            summary: Some("I finished the parser.".to_owned()),
            summary_source: SummarySource::Transcript,
            only_automatic: false,
        })
        .await;
    let releases = journal_of(&daemon, &repo, Some("release"), None, None).await;
    assert_eq!(releases.len(), before + 1);
    assert_eq!(
        releases.last().unwrap().summary_source,
        SummarySource::Transcript
    );
    assert_eq!(releases.last().unwrap().summary, "I finished the parser.");

    // Cursors survive a restart.
    drop(daemon);
    let daemon = open(&dir);
    let after = digest_for(
        &daemon,
        &repo_ref,
        "user",
        None,
        DigestBudget::SESSION_START,
        false,
    )
    .await;
    assert_eq!(after.shown, 6, "{}", after.text);
    assert!(
        !after.text.contains("note 24"),
        "already shown: {}",
        after.text
    );
}

#[tokio::test]
async fn head_moves_are_journaled_once_per_checkout() {
    if !have_git() {
        return;
    }
    let dir = TempDir::new().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    assert!(git(dir.path(), &repo, &["init", "-q"]));
    assert!(git(
        dir.path(),
        &repo,
        &["commit", "-q", "--allow-empty", "-m", "root"]
    ));
    let daemon = open(&dir);
    register_in(&daemon, "a", &repo).await;
    register_in(&daemon, "b", &repo).await;
    daemon.refresh_vcs(None).await;
    assert!(
        journal_of(&daemon, &repo, Some("commit"), None, None)
            .await
            .is_empty()
    );

    assert!(git(
        dir.path(),
        &repo,
        &["commit", "-q", "--allow-empty", "-m", "Add lexer"]
    ));
    daemon.refresh_vcs(None).await;
    daemon.refresh_vcs(None).await;
    let commits = journal_of(&daemon, &repo, Some("commit"), None, None).await;
    assert_eq!(commits.len(), 1, "two agents, one checkout, one entry");
    assert!(
        commits[0].summary.starts_with("committed "),
        "{}",
        commits[0].summary
    );
    assert!(
        commits[0].summary.ends_with(": Add lexer"),
        "{}",
        commits[0].summary
    );
    assert_eq!(
        commits[0].agent, None,
        "shared checkout, nobody holds the branch"
    );
    assert_eq!(commits[0].agent_name, "external");
    assert!(commits[0].head_before.is_some() && commits[0].head_after.is_some());

    assert!(git(dir.path(), &repo, &["checkout", "-q", "-b", "feature"]));
    daemon.refresh_vcs(None).await;
    let commits = journal_of(&daemon, &repo, Some("commit"), None, None).await;
    assert_eq!(commits.len(), 1, "same head, no new entry");
    assert!(git(
        dir.path(),
        &repo,
        &["commit", "-q", "--allow-empty", "-m", "On feature"]
    ));
    daemon.refresh_vcs(None).await;
    let commits = journal_of(&daemon, &repo, Some("commit"), None, None).await;
    assert_eq!(commits.len(), 2);
    assert_eq!(commits[1].branch.as_deref(), Some("feature"));
}
async fn reject_lease_transition(operation: &str) {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let owner = register(&daemon, "owner", None).await;
    let original = if operation == "new" {
        None
    } else {
        let Response::Lease { lease } = claim(&daemon, "owner", "task:atomic").await else {
            panic!()
        };
        Some(lease)
    };
    let before_events = daemon.recent_events(100);
    let (before_agent, before_seq) = {
        let state = lock(&daemon.state);
        (
            state.registry.get(&owner.id).unwrap().clone(),
            state.next_seq,
        )
    };
    let mut live = daemon.subscribe_events();
    lock(&daemon.state)
        .store
        .reject_event_for_test(if operation == "new" {
            "lease_claimed"
        } else {
            "lease_renewed"
        });
    let response = if operation == "renew" {
        daemon
            .handle(Request::Renew {
                agent: "owner".into(),
                lease: original.as_ref().unwrap().id.clone(),
                ttl_secs: 3600,
            })
            .await
    } else {
        daemon
            .handle(Request::Claim {
                agent: "owner".into(),
                resource: "task:atomic".into(),
                mode: LeaseMode::Exclusive,
                amount: None,
                ttl_secs: 3600,
                note: None,
                wait_secs: 0,
                automatic: false,
            })
            .await
    };
    assert!(
        matches!(
            response,
            Response::Error {
                code: ErrorCode::StorageUnavailable,
                ..
            }
        ),
        "{response:?}"
    );
    let state = lock(&daemon.state);
    let expected: Vec<_> = original.into_iter().collect();
    assert_eq!(
        state.store.load_leases().unwrap(),
        expected,
        "lease cannot commit without its replay event"
    );
    assert_eq!(
        state.leases.all().into_iter().cloned().collect::<Vec<_>>(),
        expected,
        "memory cannot advance after rollback"
    );
    assert_eq!(
        state.registry.get(&owner.id).unwrap(),
        &before_agent,
        "claim liveness is part of the failed transition"
    );
    assert_eq!(state.next_seq, before_seq);
    drop(state);
    assert_eq!(daemon.recent_events(100), before_events);
    assert!(live.try_recv().is_err());
}

#[tokio::test]
async fn rejected_claim_event_rolls_back_new_lease_and_liveness() {
    reject_lease_transition("new").await;
}

#[tokio::test]
async fn rejected_reclaim_event_rolls_back_renewal_and_liveness() {
    reject_lease_transition("reclaim").await;
}

#[tokio::test]
async fn rejected_renew_event_rolls_back_renewal_and_liveness() {
    reject_lease_transition("renew").await;
}

#[tokio::test]
async fn rejected_claim_row_rolls_back_its_liveness_and_event() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let owner = register(&daemon, "owner", None).await;
    let before = daemon.recent_events(100);
    let mut live = daemon.subscribe_events();
    lock(&daemon.state)
        .store
        .reject_lease_change_for_test("INSERT");
    assert!(matches!(
        claim(&daemon, "owner", "task:atomic").await,
        Response::Error {
            code: ErrorCode::StorageUnavailable,
            ..
        }
    ));
    let state = lock(&daemon.state);
    assert!(state.leases.is_empty());
    assert!(state.store.load_leases().unwrap().is_empty());
    assert_eq!(state.registry.get(&owner.id), Some(&owner));
    assert_eq!(state.store.load_agents().unwrap(), [owner]);
    drop(state);
    assert_eq!(daemon.recent_events(100), before);
    assert!(live.try_recv().is_err());
}

#[tokio::test]
async fn rejected_conflict_event_preserves_requester_liveness_and_held_lease() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    register(&daemon, "owner", None).await;
    let peer = register(&daemon, "peer", None).await;
    let Response::Lease { lease } = claim(&daemon, "owner", "task:atomic").await else {
        panic!()
    };
    let before = daemon.recent_events(100);
    let mut live = daemon.subscribe_events();
    lock(&daemon.state)
        .store
        .reject_event_for_test("lease_conflict");
    assert!(matches!(
        claim(&daemon, "peer", "task:atomic").await,
        Response::Error {
            code: ErrorCode::StorageUnavailable,
            ..
        }
    ));
    let state = lock(&daemon.state);
    assert_eq!(state.registry.get(&peer.id), Some(&peer));
    assert_eq!(
        state
            .store
            .load_agents()
            .unwrap()
            .into_iter()
            .find(|a| a.id == peer.id),
        Some(peer)
    );
    assert_eq!(state.leases.get(&lease.id), Some(&lease));
    assert_eq!(state.store.load_leases().unwrap(), [lease]);
    drop(state);
    assert_eq!(daemon.recent_events(100), before);
    assert!(live.try_recv().is_err());
}

#[tokio::test]
async fn rejected_release_deletion_keeps_memory_and_replay_protection() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    register(&daemon, "owner", None).await;
    let Response::Lease { lease } = claim(&daemon, "owner", "task:atomic").await else {
        panic!()
    };
    let before = daemon.recent_events(100);
    let mut live = daemon.subscribe_events();
    lock(&daemon.state)
        .store
        .reject_lease_change_for_test("DELETE");
    assert!(matches!(
        daemon
            .handle(Request::Release {
                agent: "owner".into(),
                lease: lease.id.clone(),
                summary: None,
                summary_source: SummarySource::Explicit
            })
            .await,
        Response::Error {
            code: ErrorCode::StorageUnavailable,
            ..
        }
    ));
    let state = lock(&daemon.state);
    assert_eq!(state.leases.get(&lease.id), Some(&lease));
    assert_eq!(state.store.load_leases().unwrap(), [lease]);
    drop(state);
    assert_eq!(daemon.recent_events(100), before);
    assert!(live.try_recv().is_err());
}

#[tokio::test]
async fn failed_liveness_write_does_not_advance_memory() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let owner = register(&daemon, "owner", None).await;
    let mut state = lock(&daemon.state);
    let mut before = state.registry.get(&owner.id).unwrap().clone();
    before.last_seen = Utc::now() - chrono::Duration::seconds(30);
    state.store.upsert_agent(&before).unwrap();
    *state.registry.get_mut(&owner.id).unwrap() = before.clone();
    state.store.reject_agent_writes_for_test();
    state.touch(&owner.id);
    assert!(state.write_failure().is_some());
    assert_eq!(state.registry.get(&owner.id).unwrap(), &before);
    assert_eq!(state.store.load_agents().unwrap(), [before]);
}

#[tokio::test]
async fn journal_release_event_failures_roll_back_leases_entries_and_publication() {
    for rejected in ["lease_released", "journal_appended"] {
        let dir = TempDir::new().unwrap();
        let checkout = dir.path().join("checkout");
        std::fs::create_dir(&checkout).unwrap();
        let daemon = open(&dir);
        register_in(&daemon, "owner", &checkout).await;
        register_in(&daemon, "competitor", &checkout).await;
        let Response::Lease { lease } = claim(&daemon, "owner", "task:durable").await else {
            panic!()
        };
        let before = daemon.recent_events(100);
        let mut live = daemon.subscribe_events();
        lock(&daemon.state).store.reject_event_for_test(rejected);
        assert!(matches!(
            daemon
                .handle(Request::ReleaseAll {
                    agent: "owner".into(),
                    summary: Some("release with durable summary".into()),
                    summary_source: SummarySource::Explicit,
                    only_automatic: false,
                })
                .await,
            Response::Error {
                code: ErrorCode::StorageUnavailable,
                ..
            }
        ));
        assert_eq!(
            lock(&daemon.state).leases.get(&lease.id),
            Some(&lease),
            "failed release must retain protection in memory"
        );
        assert!(
            live.try_recv().is_err(),
            "failed transaction must not publish"
        );
        assert!(matches!(
            claim(&daemon, "competitor", "task:durable").await,
            Response::Error {
                code: ErrorCode::StorageUnavailable,
                ..
            }
        ));
        drop(daemon);
        let restarted = open(&dir);
        assert_eq!(list_leases(&restarted).await, [lease]);
        assert!(
            journal_of(&restarted, &checkout, Some("release"), None, None)
                .await
                .is_empty()
        );
        assert_eq!(restarted.recent_events(100), before);
        assert!(matches!(
            claim(&restarted, "competitor", "task:durable").await,
            Response::Error {
                code: ErrorCode::Conflict,
                ..
            }
        ));
    }
}

#[tokio::test]
async fn journal_notes_and_cursors_do_not_survive_failed_replay_events() {
    for rejected in ["journal_appended", "journal_read"] {
        let dir = TempDir::new().unwrap();
        let checkout = dir.path().join("checkout");
        std::fs::create_dir(&checkout).unwrap();
        let daemon = open(&dir);
        let agent = register_in(&daemon, "owner", &checkout).await;
        let project = agent.project.as_ref().unwrap().id();
        let before = journal_of(&daemon, &checkout, None, None, None).await;
        let mut live = daemon.subscribe_events();
        {
            let mut state = lock(&daemon.state);
            state.store.reject_event_for_test(rejected);
            if rejected == "journal_read" {
                state.move_cursor("user", &project, 10);
                assert_eq!(state.store.journal_cursor("user", &project).unwrap(), None);
            } else {
                state.journal_add("owner", "must roll back".into());
            }
            assert!(state.write_failure().is_some());
        }
        assert!(live.try_recv().is_err());
        drop(daemon);
        let restarted = open(&dir);
        assert_eq!(
            journal_of(&restarted, &checkout, None, None, None).await,
            before
        );
        assert_eq!(
            lock(&restarted.state)
                .store
                .journal_cursor("user", &project)
                .unwrap(),
            None
        );
    }
}

#[tokio::test]
async fn journal_sequence_survives_pruning_every_entry_and_restart() {
    let dir = TempDir::new().unwrap();
    let checkout = dir.path().join("checkout");
    std::fs::create_dir(&checkout).unwrap();
    let daemon = open(&dir);
    let agent = register_in(&daemon, "owner", &checkout).await;
    let project = agent.project.unwrap().id();
    let last = journal_of(&daemon, &checkout, None, None, None)
        .await
        .last()
        .unwrap()
        .seq;
    {
        let mut state = lock(&daemon.state);
        state.move_cursor("user", &project, last);
        state.journal_prune(&project, last + 1, "test");
        for filtered in [false, true] {
            let mut query = JournalQuery::new(project.clone(), 200);
            if filtered {
                query.grep = Some("absent".into());
            }
            let Response::Journal {
                entries, head_seq, ..
            } = state.journal_query(query)
            else {
                panic!()
            };
            assert!(entries.is_empty());
            assert_eq!(
                head_seq,
                Some(last),
                "pruning preserves the durable snapshot head"
            );
        }
    }
    drop(daemon);
    let daemon = open(&dir);
    let Response::Journal {
        entries, head_seq, ..
    } = lock(&daemon.state).journal_query(JournalQuery::new(project.clone(), 200))
    else {
        panic!()
    };
    assert!(entries.is_empty());
    assert_eq!(head_seq, Some(last), "empty snapshot head survives restart");
    let Response::JournalEntry { entry } = daemon
        .handle(Request::JournalAdd {
            agent: "owner".into(),
            summary: "after prune".into(),
        })
        .await
    else {
        panic!()
    };
    assert!(entry.seq > last);
    assert_eq!(
        lock(&daemon.state)
            .store
            .journal_cursor("user", &project)
            .unwrap(),
        Some(last)
    );
}

#[tokio::test]
async fn release_summary_uses_lease_sequence_when_wall_clock_moves_backwards() {
    let dir = TempDir::new().unwrap();
    let checkout = dir.path().join("checkout");
    std::fs::create_dir(&checkout).unwrap();
    let daemon = open(&dir);
    let agent = register_in(&daemon, "owner", &checkout).await;
    let project = agent.project.unwrap();
    let change = |path: &str, at| Change {
        seq: 0,
        project: project.id(),
        checkout: Some(project.dir().to_path_buf()),
        worktree: None,
        path: path.into(),
        kind: ChangeKind::Modified,
        at,
        by: Attribution::External,
        head: None,
    };
    lock(&daemon.state)
        .store
        .append_change(&change("old", Utc::now() + Duration::hours(1)))
        .unwrap();
    let Response::Lease { lease } =
        claim(&daemon, "owner", &format!("path:{}", checkout.display())).await
    else {
        panic!()
    };
    assert_eq!(lease.change_seq, Some(1));
    let Response::Lease { lease: renewed } =
        claim(&daemon, "owner", &format!("path:{}", checkout.display())).await
    else {
        panic!()
    };
    assert_eq!(lease.change_seq, renewed.change_seq);
    lock(&daemon.state)
        .store
        .append_change(&change("new", lease.acquired_at - Duration::hours(1)))
        .unwrap();
    // The acquisition boundary is durable and survives daemon recovery.
    drop(daemon);
    let daemon = open(&dir);
    assert!(matches!(
        daemon
            .handle(Request::ReleaseAll {
                agent: "owner".into(),
                summary: None,
                summary_source: SummarySource::Explicit,
                only_automatic: false,
            })
            .await,
        Response::Leases { .. }
    ));
    let entries = journal_of(&daemon, &checkout, Some("release"), None, None).await;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].paths, [PathBuf::from("new")]);
    assert_eq!(entries[0].changes, Some((2, 2)));
}
#[tokio::test]
async fn task_release_skips_watcher_and_path_release_waits_for_pending_changes() {
    let dir = TempDir::new().unwrap();
    let checkout = dir.path().join("checkout");
    std::fs::create_dir(&checkout).unwrap();
    let daemon = open(&dir);
    let agent = register_in(&daemon, "owner", &checkout).await;
    let project = agent.project.unwrap();
    let (sender, mut receiver) = mpsc::channel(1);
    daemon.set_watcher_flush(sender);
    claim(&daemon, "owner", "task:unrelated").await;
    let release = || Request::ReleaseAll {
        agent: "owner".into(),
        summary: None,
        summary_source: SummarySource::Explicit,
        only_automatic: false,
    };
    assert!(matches!(
        tokio::time::timeout(
            std::time::Duration::from_millis(200),
            daemon.handle(release())
        )
        .await
        .unwrap(),
        Response::Leases { .. }
    ));
    assert!(receiver.try_recv().is_err());
    claim(&daemon, "owner", &format!("path:{}", checkout.display())).await;
    let worker = daemon.clone();
    let task = tokio::spawn(async move { worker.handle(release()).await });
    let ack = tokio::time::timeout(std::time::Duration::from_secs(1), receiver.recv())
        .await
        .unwrap()
        .unwrap();
    // Flush processing must be able to acquire state before release builds its summary.
    lock(&daemon.state)
        .store
        .append_change(&Change {
            seq: 0,
            project: project.id(),
            checkout: Some(project.dir().to_path_buf()),
            worktree: None,
            path: "pending.rs".into(),
            kind: ChangeKind::Modified,
            at: Utc::now(),
            by: Attribution::External,
            head: None,
        })
        .unwrap();
    ack.send(()).unwrap();
    assert!(matches!(task.await.unwrap(), Response::Leases { .. }));
    let entries = journal_of(&daemon, &checkout, Some("release"), None, None).await;
    assert_eq!(entries[0].paths, [PathBuf::from("pending.rs")]);
}

/// The read-before-write guard from the agent's side, as reported from a
/// real session: a file that is not there yet is never stale; under an
/// observed folder only a target that someone else changed is, as the
/// ledger says, never for the agent's own changes; and a difference
/// nothing recorded stays stale.
#[tokio::test]
async fn stale_judges_an_observed_folder_by_what_changed_and_by_whom() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let daemon = open(&home);
    let me = register_in(&daemon, "me", work.path()).await;
    let project = me.project.clone().unwrap();
    let root = project.dir().to_path_buf();
    watch_ledger(&daemon, &root);
    std::fs::create_dir_all(root.join("repro")).unwrap();
    std::fs::write(root.join("repro/a.txt"), "a").unwrap();
    std::fs::write(root.join("repro/b.txt"), "b").unwrap();
    assert!(matches!(
        daemon
            .handle(Request::Observe {
                agent: "me".into(),
                paths: vec!["repro".into()],
            })
            .await,
        Response::Reads { .. }
    ));
    let stale = |path: &'static str| {
        let daemon = daemon.clone();
        async move {
            match daemon
                .handle(Request::Stale {
                    agent: "me".into(),
                    paths: vec![path.into()],
                })
                .await
            {
                Response::Stale { stale } => stale,
                other => panic!("unexpected {other:?}"),
            }
        }
    };
    // A shell mkdir changes the folder; a file to create beneath it has
    // nothing to overwrite.
    std::fs::create_dir(root.join("repro/sub")).unwrap();
    assert!(stale("repro/sub/new.txt").await.is_empty());
    // A change nothing recorded stays stale.
    assert!(matches!(
        claim(
            &daemon,
            "me",
            &format!("path:{}", root.join("repro/a.txt").display())
        )
        .await,
        Response::Lease { .. }
    ));
    std::fs::write(root.join("repro/a.txt"), "a2").unwrap();
    assert_eq!(stale("repro/a.txt").await.len(), 1);
    // The ledger says who: the agent's own change to a.txt, someone
    // else's to b.txt.
    let change = |path: &str, by: Attribution| Change {
        seq: 0,
        project: project.id(),
        checkout: Some(root.clone()),
        worktree: None,
        path: path.into(),
        kind: ChangeKind::Modified,
        at: Utc::now(),
        by,
        head: None,
    };
    std::fs::write(root.join("repro/b.txt"), "b2").unwrap();
    daemon
        .record_fs_changes(
            vec![Observed {
                checkout: Checkout {
                    dir: root.clone(),
                    project: project.id(),
                    worktree: None,
                },
                path: "repro/a.txt".into(),
                kind: ChangeKind::Modified,
            }],
            vec![],
        )
        .await;
    lock(&daemon.state)
        .store
        .append_change(&change("repro/b.txt", Attribution::External))
        .unwrap();
    assert!(stale("repro/a.txt").await.is_empty(), "my own edit");
    let theirs = stale("repro/b.txt").await;
    assert_eq!(theirs.len(), 1);
    assert_eq!(
        theirs[0].path,
        root.join("repro/b.txt"),
        "names the file to reread"
    );
    assert!(stale("repro/sub/new.txt").await.is_empty());
}

async fn content_stale(daemon: &Daemon, path: &str) -> Vec<agentdocker_core::StalePath> {
    match daemon.stale("me", vec![path.into()]).await {
        Response::Stale { stale } => stale,
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn stale_content_proofs_distinguish_deleted_unseen_and_unchanged_children() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let daemon = open(&home);
    let me = register_in(&daemon, "me", work.path()).await;
    let root = me.project.unwrap().dir().to_path_buf();
    watch_ledger(&daemon, &root);
    std::fs::create_dir(root.join("repro")).unwrap();
    std::fs::write(root.join("repro/deleted"), "seen").unwrap();
    std::fs::write(root.join("repro/untouched"), "keep").unwrap();
    assert!(matches!(
        daemon.observe("me", vec!["repro".into()]).await,
        Response::Reads { .. }
    ));
    std::fs::remove_file(root.join("repro/deleted")).unwrap();
    // No notification has arrived. Reading the parent still observed its child.
    let removed = content_stale(&daemon, "repro/deleted").await;
    assert_eq!(
        removed.len(),
        1,
        "a directory observation must remember a deleted child"
    );
    assert_eq!(removed[0].path, root.join("repro/deleted"));
    assert!(removed[0].reason.contains("removed"));
    assert!(
        content_stale(&daemon, "repro/never-existed")
            .await
            .is_empty()
    );
    assert!(content_stale(&daemon, "repro/untouched").await.is_empty());
    daemon.observe("me", vec!["repro/deleted".into()]).await;
    assert!(
        content_stale(&daemon, "repro/deleted").await.is_empty(),
        "explicitly observed absence"
    );
    std::fs::write(root.join("repro/untouched"), "external").unwrap();
    assert_eq!(content_stale(&daemon, "repro/untouched").await.len(), 1);
    // Cache eviction does not erase the persistent read mark or manufacture freshness.
    lock(&daemon.state).content_proofs = read_content::Proofs::default();
    assert_eq!(content_stale(&daemon, "repro/untouched").await.len(), 1);
    let absent = content_stale(&daemon, "repro/never-existed").await;
    assert_eq!(absent.len(), 1);
    assert!(absent[0].reason.contains("absent"));
    assert!(
        !absent[0].reason.contains("removed"),
        "lost child history cannot prove deletion"
    );
    daemon
        .observe("me", vec!["repro/never-existed".into()])
        .await;
    assert!(
        content_stale(&daemon, "repro/never-existed")
            .await
            .is_empty()
    );
    std::fs::write(root.join("exact"), "seen").unwrap();
    daemon.observe("me", vec!["exact".into()]).await;
    lock(&daemon.state).content_proofs = read_content::Proofs::default();
    std::fs::remove_file(root.join("exact")).unwrap();
    assert!(
        content_stale(&daemon, "exact").await[0]
            .reason
            .contains("removed"),
        "an exact persisted mark still proves prior existence"
    );
}

#[tokio::test]
async fn stale_content_proofs_refuse_late_external_edits_after_own_and_sibling_rows() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let daemon = open(&home);
    let me = register_in(&daemon, "me", work.path()).await;
    let project = me.project.unwrap();
    let root = project.dir().to_path_buf();
    watch_ledger(&daemon, &root);
    std::fs::create_dir(root.join("repro")).unwrap();
    std::fs::write(root.join("repro/a"), "a1").unwrap();
    std::fs::write(root.join("repro/b"), "b1").unwrap();
    let checkout = Checkout {
        dir: root.clone(),
        project: project.id(),
        worktree: None,
    };
    let observed = |path: &str| Observed {
        checkout: checkout.clone(),
        path: path.into(),
        kind: ChangeKind::Modified,
    };
    daemon.observe("me", vec!["repro".into()]).await;
    assert!(matches!(
        claim(
            &daemon,
            "me",
            &format!("path:{}", root.join("repro/a").display())
        )
        .await,
        Response::Lease { .. }
    ));
    std::fs::write(root.join("repro/a"), "my edit").unwrap();
    daemon
        .record_fs_changes(vec![observed("repro/a")], vec![])
        .await;
    assert!(
        content_stale(&daemon, "repro/a").await.is_empty(),
        "content-bound own edit still works"
    );
    std::fs::write(root.join("repro/a"), "external, event delayed").unwrap();
    // The mock watcher acknowledges the flush without delivering this event.
    assert_eq!(
        content_stale(&daemon, "repro/a").await.len(),
        1,
        "an older own entry cannot clear newer bytes"
    );
    daemon.observe("me", vec!["repro".into()]).await;
    assert!(matches!(
        claim(
            &daemon,
            "me",
            &format!("path:{}", root.join("repro/b").display())
        )
        .await,
        Response::Lease { .. }
    ));
    std::fs::write(root.join("repro/b"), "own sibling").unwrap();
    daemon
        .record_fs_changes(vec![observed("repro/b")], vec![])
        .await;
    assert!(
        content_stale(&daemon, "repro/a").await.is_empty(),
        "unchanged target under edited parent"
    );
    std::fs::write(root.join("repro/a"), "external after sibling").unwrap();
    assert_eq!(
        content_stale(&daemon, "repro/a").await.len(),
        1,
        "sibling entries cannot explain a changed target"
    );
    daemon.observe("me", vec!["repro".into()]).await;
    std::fs::write(root.join("repro/a"), "own after reread").unwrap();
    daemon
        .record_fs_changes(vec![observed("repro/a")], vec![])
        .await;
    assert!(content_stale(&daemon, "repro/a").await.is_empty());
    lock(&daemon.state)
        .content_proofs
        .forget_read(&me.id, &root.join("repro"));
    assert_eq!(
        content_stale(&daemon, "repro/a").await.len(),
        1,
        "evicted baseline cannot borrow the retained own-change proof"
    );
}

#[tokio::test]
async fn stale_content_proofs_recheck_content_after_the_flush_wait() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let daemon = open(&home);
    let me = register_in(&daemon, "me", work.path()).await;
    let project = me.project.unwrap();
    let root = project.dir().to_path_buf();
    let (sender, mut flushes) = mpsc::channel::<oneshot::Sender<()>>(8);
    daemon.set_watcher_flush(sender);
    daemon.ledger_covers(&root, true);
    std::fs::write(root.join("file"), "read").unwrap();
    daemon.observe("me", vec!["file".into()]).await;
    assert!(matches!(
        claim(
            &daemon,
            "me",
            &format!("path:{}", root.join("file").display())
        )
        .await,
        Response::Lease { .. }
    ));
    std::fs::write(root.join("file"), "own edit").unwrap();
    daemon
        .record_fs_changes(
            vec![Observed {
                checkout: Checkout {
                    dir: root.clone(),
                    project: project.id(),
                    worktree: None,
                },
                path: "file".into(),
                kind: ChangeKind::Modified,
            }],
            vec![],
        )
        .await;
    let check = tokio::spawn({
        let daemon = daemon.clone();
        async move { content_stale(&daemon, "file").await }
    });
    let ack = flushes.recv().await.unwrap();
    std::fs::write(root.join("file"), "external during flush wait").unwrap();
    ack.send(()).unwrap();
    assert_eq!(
        check.await.unwrap().len(),
        1,
        "pre-flush content cannot authorize post-flush bytes"
    );
}

#[tokio::test]
async fn stale_content_proofs_allow_own_removal_but_not_a_recreated_directory() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let daemon = open(&home);
    let me = register_in(&daemon, "me", work.path()).await;
    let project = me.project.unwrap();
    let root = project.dir().to_path_buf();
    watch_ledger(&daemon, &root);
    std::fs::create_dir_all(root.join("folder/sub")).unwrap();
    std::fs::write(root.join("folder/sub/file"), "read").unwrap();
    daemon.observe("me", vec!["folder".into()]).await;
    assert!(matches!(
        claim(
            &daemon,
            "me",
            &format!("path:{}", root.join("folder").display())
        )
        .await,
        Response::Lease { .. }
    ));
    std::fs::remove_dir_all(root.join("folder/sub")).unwrap();
    daemon
        .record_fs_changes(
            vec![Observed {
                checkout: Checkout {
                    dir: root.clone(),
                    project: project.id(),
                    worktree: None,
                },
                path: "folder/sub".into(),
                kind: ChangeKind::Removed,
            }],
            vec![],
        )
        .await;
    assert!(
        content_stale(&daemon, "folder/sub").await.is_empty(),
        "a recorded own removal explains its descendants"
    );
    std::fs::create_dir(root.join("folder/sub")).unwrap();
    assert_eq!(
        content_stale(&daemon, "folder/sub").await.len(),
        1,
        "recreated parent cannot reuse the previous removal proof"
    );
}

#[tokio::test]
async fn stale_content_proofs_ignore_older_rows_within_the_same_sql_time_bucket() {
    use chrono::Timelike;
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let daemon = open(&home);
    let me = register_in(&daemon, "me", work.path()).await;
    let project = me.project.unwrap();
    let root = project.dir().to_path_buf();
    watch_ledger(&daemon, &root);
    std::fs::write(root.join("file"), "read").unwrap();
    let Response::Reads { mut reads } = daemon.observe("me", vec!["file".into()]).await else {
        panic!("observation failed")
    };
    // Pin the read inside a SQL millisecond bucket and safely in the past;
    // the older external row must be selected by SQL but excluded exactly.
    let captured_mark = reads[0].clone();
    reads[0].at = (Utc::now() - chrono::Duration::seconds(1))
        .with_nanosecond(100_000)
        .unwrap();
    {
        let mut state = lock(&daemon.state);
        let mark = reads[0].clone();
        let snapshot = state
            .content_proofs
            .baseline(&me.id, &captured_mark)
            .unwrap();
        state
            .store
            .put_document("reads", me.id.as_str(), &reads)
            .unwrap();
        state.content_proofs.observe(&me.id, mark.clone(), snapshot);
        state
            .ledger_since
            .insert(root.clone(), mark.at - chrono::Duration::seconds(1));
        state
            .store
            .append_change(&Change {
                seq: 0,
                project: project.id(),
                checkout: Some(root.clone()),
                worktree: None,
                path: "file".into(),
                kind: ChangeKind::Modified,
                at: mark.at - chrono::Duration::nanoseconds(1),
                by: Attribution::External,
                head: None,
            })
            .unwrap();
        let page = state
            .store
            .changes(&ChangesQuery {
                project: project.id(),
                since_seq: None,
                path: Some("file".into()),
                agent: None,
                limit: 500,
                after: Some(mark.at),
                before_seq: None,
            })
            .unwrap();
        assert_eq!(page.len(), 1, "fixture must expose the coarse SQL boundary");
        assert!(page[0].at < mark.at);
    }
    assert!(matches!(
        claim(
            &daemon,
            "me",
            &format!("path:{}", root.join("file").display())
        )
        .await,
        Response::Lease { .. }
    ));
    std::fs::write(root.join("file"), "own after read").unwrap();
    daemon
        .record_fs_changes(
            vec![Observed {
                checkout: Checkout {
                    dir: root.clone(),
                    project: project.id(),
                    worktree: None,
                },
                path: "file".into(),
                kind: ChangeKind::Modified,
            }],
            vec![],
        )
        .await;
    assert!(
        content_stale(&daemon, "file").await.is_empty(),
        "an external row before the read cannot invalidate this proven own edit"
    );
}

#[tokio::test]
async fn stale_content_proofs_accept_own_edits_larger_than_the_previous_batch_budget() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let daemon = open(&home);
    let me = register_in(&daemon, "me", work.path()).await;
    let project = me.project.unwrap();
    let root = project.dir().to_path_buf();
    watch_ledger(&daemon, &root);
    std::fs::write(root.join("medium"), vec![1u8; 9 * 1024 * 1024]).unwrap();
    daemon.observe("me", vec!["medium".into()]).await;
    assert!(matches!(
        claim(
            &daemon,
            "me",
            &format!("path:{}", root.join("medium").display())
        )
        .await,
        Response::Lease { .. }
    ));
    std::fs::write(root.join("medium"), vec![2u8; 9 * 1024 * 1024]).unwrap();
    daemon
        .record_fs_changes(
            vec![Observed {
                checkout: Checkout {
                    dir: root.clone(),
                    project: project.id(),
                    worktree: None,
                },
                path: "medium".into(),
                kind: ChangeKind::Modified,
            }],
            vec![],
        )
        .await;
    assert!(
        content_stale(&daemon, "medium").await.is_empty(),
        "matching own-write content must not require repeated rereads"
    );
    std::fs::write(root.join("medium"), vec![3u8; 9 * 1024 * 1024]).unwrap();
    assert_eq!(
        content_stale(&daemon, "medium").await.len(),
        1,
        "later unreported external bytes remain stale"
    );
}

/// A watcher for `checkout` that records nothing on its own and
/// confirms every flush: what the ledger holds is what a test put there.
fn watch_ledger(daemon: &Daemon, checkout: &Path) {
    let (sender, mut receiver) = mpsc::channel::<oneshot::Sender<()>>(8);
    daemon.set_watcher_flush(sender);
    daemon.ledger_covers(checkout, true);
    tokio::spawn(async move {
        while let Some(ack) = receiver.recv().await {
            let _ = ack.send(());
        }
    });
}

/// The ledger clears a changed observation only when it can have seen
/// every change behind it: a page cut short, a watcher that has not
/// confirmed its pending events, or a checkout nothing watched since the
/// read all leave the target stale, however innocent the page looks.
/// A file read and since removed stays stale; a path never seen does not.
#[tokio::test]
async fn stale_trusts_the_ledger_only_as_far_as_it_reaches() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let daemon = open(&home);
    let me = register_in(&daemon, "me", work.path()).await;
    let project = me.project.clone().unwrap();
    let root = project.dir().to_path_buf();
    std::fs::create_dir_all(root.join("repro")).unwrap();
    std::fs::write(root.join("repro/a.txt"), "a").unwrap();
    std::fs::write(root.join("repro/gone.txt"), "g").unwrap();
    let (sender, mut flushes) = mpsc::channel::<oneshot::Sender<()>>(8);
    daemon.set_watcher_flush(sender);
    daemon.ledger_covers(&root, true);
    let observe = |path: &'static str| {
        let daemon = daemon.clone();
        async move {
            assert!(matches!(
                daemon
                    .handle(Request::Observe {
                        agent: "me".into(),
                        paths: vec![path.into()],
                    })
                    .await,
                Response::Reads { .. }
            ));
        }
    };
    let stale = |path: &'static str| {
        let daemon = daemon.clone();
        tokio::spawn(async move {
            match daemon
                .handle(Request::Stale {
                    agent: "me".into(),
                    paths: vec![path.into()],
                })
                .await
            {
                Response::Stale { stale } => stale,
                other => panic!("unexpected {other:?}"),
            }
        })
    };
    let change = |checkout: &Path, path: &str, by: Attribution| Change {
        seq: 0,
        project: project.id(),
        checkout: Some(checkout.to_path_buf()),
        worktree: None,
        path: path.into(),
        kind: ChangeKind::Modified,
        at: Utc::now(),
        by,
        head: None,
    };
    let mine = || Attribution::Agent {
        agent: me.id.clone(),
        lease: agentdocker_core::LeaseId::generate(),
        note: None,
    };
    observe("repro").await;
    assert!(matches!(
        claim(
            &daemon,
            "me",
            &format!("path:{}", root.join("repro/a.txt").display())
        )
        .await,
        Response::Lease { .. }
    ));
    std::fs::write(root.join("repro/a.txt"), "a2").unwrap();
    lock(&daemon.state)
        .store
        .append_change(&change(&root, "repro/a.txt", mine()))
        .unwrap();
    // The watcher drains an external write it had not yet recorded
    // before it confirms the flush: the check must wait for it.
    let asked = stale("repro/a.txt");
    let ack = flushes.recv().await.unwrap();
    lock(&daemon.state)
        .store
        .append_change(&change(&root, "repro/a.txt", Attribution::External))
        .unwrap();
    ack.send(()).unwrap();
    assert_eq!(asked.await.unwrap().len(), 1, "delayed external write");
    // Reread, then only my own edit since: clear, once confirmed.
    observe("repro").await;
    std::fs::write(root.join("repro/a.txt"), "a3").unwrap();
    daemon
        .record_fs_changes(
            vec![Observed {
                checkout: Checkout {
                    dir: root.clone(),
                    project: project.id(),
                    worktree: None,
                },
                path: "repro/a.txt".into(),
                kind: ChangeKind::Modified,
            }],
            vec![],
        )
        .await;
    let asked = stale("repro/a.txt");
    flushes.recv().await.unwrap().send(()).unwrap();
    assert!(asked.await.unwrap().is_empty(), "my own edit, confirmed");
    // A watcher that never confirms proves nothing.
    let asked = stale("repro/a.txt");
    drop(flushes.recv().await.unwrap());
    assert_eq!(asked.await.unwrap().len(), 1, "unconfirmed flush");
    // Nor does a ledger that stopped watching the checkout.
    daemon.ledger_covers(&root, false);
    let asked = stale("repro/a.txt");
    flushes.recv().await.unwrap().send(()).unwrap();
    assert_eq!(asked.await.unwrap().len(), 1, "unwatched checkout");
    daemon.ledger_covers(&root, true);
    // A full page is partial even when another checkout's entries fill
    // it and the rest is my own.
    observe("repro").await;
    std::fs::write(root.join("repro/a.txt"), "a4").unwrap();
    {
        let state = lock(&daemon.state);
        let elsewhere = work.path().join("elsewhere");
        for _ in 0..500 {
            state
                .store
                .append_change(&change(&elsewhere, "repro/a.txt", Attribution::External))
                .unwrap();
        }
    }
    daemon
        .record_fs_changes(
            vec![Observed {
                checkout: Checkout {
                    dir: root.clone(),
                    project: project.id(),
                    worktree: None,
                },
                path: "repro/a.txt".into(),
                kind: ChangeKind::Modified,
            }],
            vec![],
        )
        .await;
    let asked = stale("repro/a.txt");
    flushes.recv().await.unwrap().send(()).unwrap();
    assert_eq!(asked.await.unwrap().len(), 1, "partial page");
    // A file I read that someone removed is stale; a path never seen
    // there is not.
    observe("repro/gone.txt").await;
    std::fs::remove_file(root.join("repro/gone.txt")).unwrap();
    lock(&daemon.state)
        .store
        .append_change(&Change {
            kind: ChangeKind::Removed,
            ..change(&root, "repro/gone.txt", Attribution::External)
        })
        .unwrap();
    let asked = stale("repro/gone.txt");
    flushes.recv().await.unwrap().send(()).unwrap();
    let gone = asked.await.unwrap();
    assert_eq!(gone.len(), 1, "read, then removed");
    assert_eq!(gone[0].path, root.join("repro/gone.txt"));
    assert!(gone[0].reason.contains("removed"), "{}", gone[0].reason);
    assert!(stale("repro/never.txt").await.unwrap().is_empty());
    // Looking again records the absence, and writing it back is fine.
    observe("repro/gone.txt").await;
    assert!(stale("repro/gone.txt").await.unwrap().is_empty());
}

#[tokio::test]
async fn a_full_watcher_flush_queue_cannot_block_release_indefinitely() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    register_in(&daemon, "owner", dir.path()).await;
    claim(&daemon, "owner", &format!("path:{}", dir.path().display())).await;
    let (sender, _receiver) = mpsc::channel(1);
    let (ack, _done) = oneshot::channel();
    sender.try_send(ack).unwrap();
    daemon.set_watcher_flush(sender);
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        daemon.handle(Request::ReleaseAll {
            agent: "owner".into(),
            summary: None,
            summary_source: SummarySource::Explicit,
            only_automatic: false,
        }),
    )
    .await
    .expect("queue admission must share the flush timeout");
    assert!(matches!(response, Response::Leases { .. }));
}
