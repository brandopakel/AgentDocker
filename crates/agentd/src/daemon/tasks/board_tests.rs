//! Tags, search, plans, files and rules on a real daemon.
use super::*;
use agentdocker_core::rule::{RuleAction, Trigger};
use agentdocker_core::{PlanStatus, TagColor};
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

fn create(from: &str, project: &str, title: &str, lane: Option<&str>, tags: &[&str]) -> Request {
    Request::TaskCreate {
        from: from.to_owned(),
        project: Some(project.to_owned()),
        title: title.to_owned(),
        acceptance: String::new(),
        column: None,
        links: Vec::new(),
        board: None,
        lane: lane.map(str::to_owned),
        description: String::new(),
        priority: None,
        size: None,
        due: None,
        external_blocker: String::new(),
        parent: None,
        blocked_by: Vec::new(),
        tags: tags.iter().map(|t| (*t).to_owned()).collect(),
        plan: None,
    }
}

fn card(response: Response) -> Task {
    match response {
        Response::Task { task } => task,
        other => panic!("{other:?}"),
    }
}

struct Fixture {
    _dir: TempDir,
    daemon: Arc<Daemon>,
    work: String,
    agent: AgentRecord,
    peer: AgentRecord,
}

async fn fixture() -> Fixture {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let work = dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let agent = register(&daemon, "alice", &work).await;
    let peer = register(&daemon, "bob", &work).await;
    assert!(matches!(
        daemon
            .handle(Request::Me {
                workdir: Some(work.clone()),
            })
            .await,
        Response::Agent { .. }
    ));
    Fixture {
        work: work.display().to_string(),
        _dir: dir,
        daemon,
        agent,
        peer,
    }
}

/// The person's new words become tags, coloured; an agent's unknown tag
/// is refused with the catalog named, until somebody makes it; a rename
/// follows the tag onto every card, and a delete takes it off.
#[tokio::test]
async fn tags_are_the_projects_and_follow_their_cards() {
    let f = fixture().await;
    let d = &f.daemon;
    let filed = card(
        d.handle(create(HUMAN, &f.work, "Fix login", None, &["UI"]))
            .await,
    );
    assert_eq!(filed.tags, ["ui"]);
    let Response::Tags { tags } = d
        .handle(Request::Tags {
            project: Some(f.work.clone()),
        })
        .await
    else {
        panic!()
    };
    assert_eq!(tags.len(), 1);
    assert_ne!(tags[0].color, TagColor::Gray, "a new tag starts coloured");
    let refused = d
        .handle(create(
            f.agent.id.as_str(),
            &f.work,
            "Wire API",
            None,
            &["api"],
        ))
        .await;
    assert!(
        matches!(&refused, Response::Error { code: ErrorCode::Invalid, message, .. } if message.contains("ui")),
        "{refused:?}"
    );
    assert!(matches!(
        d.handle(Request::TagSave {
            from: f.agent.id.to_string(),
            project: None,
            name: "api".into(),
            color: Some(TagColor::Teal),
            rename: None,
        })
        .await,
        Response::Tag { .. }
    ));
    card(
        d.handle(create(
            f.agent.id.as_str(),
            &f.work,
            "Wire API",
            None,
            &["api"],
        ))
        .await,
    );
    // Renaming is the person's; it follows the tag onto the card.
    assert!(matches!(
        d.handle(Request::TagSave {
            from: f.agent.id.to_string(),
            project: None,
            name: "ui".into(),
            color: None,
            rename: Some("frontend".into()),
        })
        .await,
        Response::Error {
            code: ErrorCode::Forbidden,
            ..
        }
    ));
    assert!(matches!(
        d.handle(Request::TagSave {
            from: HUMAN.into(),
            project: Some(f.work.clone()),
            name: "ui".into(),
            color: None,
            rename: Some("frontend".into()),
        })
        .await,
        Response::Tag { .. }
    ));
    let Response::TaskDetail { task, .. } = d
        .handle(Request::TaskGet {
            task: filed.id.to_string(),
        })
        .await
    else {
        panic!()
    };
    assert_eq!(task.tags, ["frontend"]);
    assert!(matches!(
        d.handle(Request::TagDelete {
            from: HUMAN.into(),
            project: Some(f.work.clone()),
            name: "frontend".into(),
        })
        .await,
        Response::Ok
    ));
    let Response::TaskDetail { task, .. } = d
        .handle(Request::TaskGet {
            task: filed.id.to_string(),
        })
        .await
    else {
        panic!()
    };
    assert!(task.tags.is_empty());
}

/// Search forgives a slip and answers a key exactly.
#[tokio::test]
async fn search_finds_cards_by_words_and_keys() {
    let f = fixture().await;
    let d = &f.daemon;
    let login = card(
        d.handle(create(HUMAN, &f.work, "Fix the login page", None, &[]))
            .await,
    );
    card(
        d.handle(create(HUMAN, &f.work, "Write release notes", None, &[]))
            .await,
    );
    let search = |query: &str| {
        let d = d.clone();
        let project = f.work.clone();
        let query = query.to_owned();
        async move {
            match d
                .handle(Request::TaskSearch {
                    query,
                    project: Some(project),
                    archived: false,
                    limit: 10,
                })
                .await
            {
                Response::Tasks { tasks, .. } => tasks,
                other => panic!("{other:?}"),
            }
        }
    };
    let found = search("logn page").await;
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, login.id);
    let found = search(&login.key).await;
    assert_eq!(found.len(), 1);
    assert!(search("billing").await.is_empty());
    assert!(matches!(
        d.handle(Request::TaskSearch {
            query: "a".into(),
            project: None,
            archived: false,
            limit: 10,
        })
        .await,
        Response::Error {
            code: ErrorCode::Invalid,
            ..
        }
    ));
}

/// An agent writes a plan; a card carries it out by its title; the plan
/// lists the card, and the card's detail names the plan.
#[tokio::test]
async fn plans_are_written_and_carried_out_by_cards() {
    let f = fixture().await;
    let d = &f.daemon;
    let Response::Plan { plan, .. } = d
        .handle(Request::PlanSave {
            from: f.agent.id.to_string(),
            project: None,
            plan: None,
            title: Some("Auth rewrite".into()),
            body: Some("# Auth\n\nTokens, then sessions.".into()),
            status: None,
        })
        .await
    else {
        panic!()
    };
    assert_eq!(plan.status, PlanStatus::Draft);
    let mut request = create(f.agent.id.as_str(), &f.work, "Tokens", None, &[]);
    if let Request::TaskCreate { plan: words, .. } = &mut request {
        *words = Some("auth rewrite".into());
    }
    let filed = card(d.handle(request).await);
    assert_eq!(filed.plan.as_deref(), Some(plan.id.as_str()));
    let Response::Plan { plan: read, tasks } = d
        .handle(Request::PlanGet {
            plan: plan.id.clone(),
        })
        .await
    else {
        panic!()
    };
    assert!(read.body.contains("Tokens, then sessions"));
    assert_eq!(tasks.len(), 1);
    let Response::Plans { plans } = d
        .handle(Request::Plans {
            project: Some(f.work.clone()),
            archived: false,
        })
        .await
    else {
        panic!()
    };
    assert!(plans[0].body.is_empty(), "lists leave the body out");
    let Response::TaskDetail { plan: named, .. } = d
        .handle(Request::TaskGet {
            task: filed.id.to_string(),
        })
        .await
    else {
        panic!()
    };
    assert_eq!(named.map(|p| p.id), Some(plan.id.clone()));
    // Archived, it leaves the list unless asked for.
    assert!(matches!(
        d.handle(Request::PlanSave {
            from: HUMAN.into(),
            project: None,
            plan: Some(plan.id.clone()),
            title: None,
            body: None,
            status: Some(PlanStatus::Archived),
        })
        .await,
        Response::Plan { .. }
    ));
    let Response::Plans { plans } = d
        .handle(Request::Plans {
            project: Some(f.work.clone()),
            archived: false,
        })
        .await
    else {
        panic!()
    };
    assert!(plans.is_empty());
}

/// A file is kept, linked to a card, read back as text or bytes, a new
/// version under the same name, unlinked, and deleted only by its
/// uploader or the person.
#[tokio::test]
async fn files_are_kept_linked_and_deleted_by_their_owner() {
    let f = fixture().await;
    let d = &f.daemon;
    let filed = card(d.handle(create(HUMAN, &f.work, "Design", None, &[])).await);
    let Response::File { file } = d
        .handle(Request::FilePut {
            from: f.agent.id.to_string(),
            project: None,
            name: "notes.md".into(),
            media_type: None,
            text: Some("# Notes".into()),
            base64: None,
            task: Some(filed.key.clone()),
        })
        .await
    else {
        panic!()
    };
    assert_eq!(file.media_type, "text/markdown");
    assert_eq!(file.tasks, std::slice::from_ref(&filed.id));
    let Response::Files { files } = d
        .handle(Request::Files {
            project: None,
            task: Some(filed.id.to_string()),
        })
        .await
    else {
        panic!()
    };
    assert_eq!(files.len(), 1);
    let Response::FileContent { text, .. } = d
        .handle(Request::FileGet {
            file: file.id.clone(),
        })
        .await
    else {
        panic!()
    };
    assert_eq!(text.as_deref(), Some("# Notes"));
    // The same name is a new version of the same file.
    let Response::File { file: again } = d
        .handle(Request::FilePut {
            from: f.agent.id.to_string(),
            project: None,
            name: "notes.md".into(),
            media_type: None,
            text: Some("# Notes, v2".into()),
            base64: None,
            task: None,
        })
        .await
    else {
        panic!()
    };
    assert_eq!(again.id, file.id);
    assert_eq!(
        again.tasks,
        std::slice::from_ref(&filed.id),
        "its links kept"
    );
    // Bytes that are not text come back as base64.
    let Response::File { file: image } = d
        .handle(Request::FilePut {
            from: f.agent.id.to_string(),
            project: None,
            name: "shot.png".into(),
            media_type: None,
            text: None,
            base64: Some("iVBORw0KGgo=".into()),
            task: None,
        })
        .await
    else {
        panic!()
    };
    let Response::FileContent { text, base64, .. } = d
        .handle(Request::FileGet {
            file: image.id.clone(),
        })
        .await
    else {
        panic!()
    };
    assert!(text.is_none());
    assert_eq!(base64.as_deref(), Some("iVBORw0KGgo="));
    assert!(matches!(
        d.handle(Request::FilePut {
            from: f.agent.id.to_string(),
            project: None,
            name: "../escape".into(),
            media_type: None,
            text: Some("x".into()),
            base64: None,
            task: None,
        })
        .await,
        Response::Error {
            code: ErrorCode::Invalid,
            ..
        }
    ));
    let Response::File { file: unlinked } = d
        .handle(Request::FileLink {
            from: f.agent.id.to_string(),
            file: file.id.clone(),
            task: filed.id.to_string(),
            unlink: true,
        })
        .await
    else {
        panic!()
    };
    assert!(unlinked.tasks.is_empty());
    assert!(matches!(
        d.handle(Request::FileDelete {
            from: f.peer.id.to_string(),
            file: file.id.clone(),
        })
        .await,
        Response::Error {
            code: ErrorCode::Forbidden,
            ..
        }
    ));
    assert!(matches!(
        d.handle(Request::FileDelete {
            from: HUMAN.into(),
            file: file.id.clone(),
        })
        .await,
        Response::Ok
    ));
    assert!(matches!(
        d.handle(Request::FileGet {
            file: file.id.clone()
        })
        .await,
        Response::Error {
            code: ErrorCode::NotFound,
            ..
        }
    ));
}

/// Rules are the person's. An enter rule comments on a card the person
/// moves into its lane (and not on a rule's own move); an idle rule acts
/// once a stay; a move rule's move does not set off another rule.
#[tokio::test]
async fn rules_act_on_entry_and_on_idle_cards_once() {
    let f = fixture().await;
    let d = &f.daemon;
    let rule = |from: &str, name: &str, trigger: Trigger, action: RuleAction| Request::RuleCreate {
        from: from.to_owned(),
        project: Some(f.work.clone()),
        board: None,
        name: name.to_owned(),
        trigger,
        action,
    };
    assert!(matches!(
        d.handle(rule(
            f.agent.id.as_str(),
            "Nope",
            Trigger::Enter {
                lane: "review".into()
            },
            RuleAction::Archive
        ))
        .await,
        Response::Error {
            code: ErrorCode::Forbidden,
            ..
        }
    ));
    let Response::Rule { rule: noted } = d
        .handle(rule(
            HUMAN,
            "Review note",
            Trigger::Enter {
                lane: "Review".into(),
            },
            RuleAction::Comment {
                text: "Ready for eyes".into(),
            },
        ))
        .await
    else {
        panic!()
    };
    assert_eq!(
        noted.trigger,
        Trigger::Enter {
            lane: "review".into()
        },
        "a lane's name made its slug"
    );
    let filed = card(
        d.handle(create(HUMAN, &f.work, "Ship it", Some("ready"), &[]))
            .await,
    );
    card(
        d.handle(Request::TaskMove {
            agent: HUMAN.into(),
            task: filed.id.to_string(),
            column: None,
            to: Some("review".into()),
            after: None,
            before: None,
            comment: None,
        })
        .await,
    );
    let comments = |d: &Arc<Daemon>, id: &TaskId| {
        let d = d.clone();
        let id = id.to_string();
        async move {
            match d.handle(Request::TaskGet { task: id }).await {
                Response::TaskDetail { comments, task, .. } => (comments, task),
                other => panic!("{other:?}"),
            }
        }
    };
    let (said, task) = comments(d, &filed.id).await;
    assert_eq!(said.len(), 1);
    assert_eq!(said[0].body, "Ready for eyes");
    assert_eq!(said[0].author, "rule:Review note");
    assert!(task.fired.contains(&noted.id));

    // An idle rule: Review for three days moves it back to Ready. Its
    // move does not set off the enter rule on Ready.
    assert!(matches!(
        d.handle(rule(
            HUMAN,
            "Stale review",
            Trigger::Idle {
                lane: "review".into(),
                days: 3
            },
            RuleAction::Move {
                lane: "ready".into()
            },
        ))
        .await,
        Response::Rule { .. }
    ));
    assert!(matches!(
        d.handle(rule(
            HUMAN,
            "Ready note",
            Trigger::Enter {
                lane: "ready".into()
            },
            RuleAction::Comment {
                text: "Back for the taking".into(),
            },
        ))
        .await,
        Response::Rule { .. }
    ));
    let starts = lock(&d.state).sweep_rules(Utc::now());
    assert!(starts.is_empty());
    assert_eq!(
        comments(d, &filed.id).await.1.lane.as_deref(),
        Some("review"),
        "not idle yet"
    );
    let later = Utc::now() + chrono::Duration::days(4);
    lock(&d.state).sweep_rules(later);
    let (said, task) = comments(d, &filed.id).await;
    assert_eq!(task.lane.as_deref(), Some("ready"));
    assert_eq!(said.len(), 1, "a rule's move sets off no rule");
    // Off, a rule does nothing.
    let Response::Rules { rules } = d
        .handle(Request::Rules {
            project: Some(f.work.clone()),
            board: None,
        })
        .await
    else {
        panic!()
    };
    assert_eq!(rules.len(), 3);
    assert!(matches!(
        d.handle(Request::RuleUpdate {
            from: HUMAN.into(),
            rule: noted.id.clone(),
            name: None,
            trigger: None,
            action: None,
            enabled: Some(false),
        })
        .await,
        Response::Rule { rule } if !rule.enabled
    ));
    assert!(matches!(
        d.handle(Request::RuleDelete {
            from: HUMAN.into(),
            rule: noted.id.clone(),
        })
        .await,
        Response::Ok
    ));
}

/// Starting an agent on a card is the person's; an unknown runtime says
/// so and leaves the card free to start on again.
#[tokio::test]
async fn an_agent_is_started_on_a_card_only_by_the_person() {
    let f = fixture().await;
    let d = &f.daemon;
    let filed = card(
        d.handle(create(HUMAN, &f.work, "Port it", Some("ready"), &[]))
            .await,
    );
    let start = |from: &str| Request::TaskStart {
        from: from.to_owned(),
        task: filed.id.to_string(),
        runtime: "no-such-runtime".into(),
        args: Vec::new(),
        workdir: None,
    };
    assert!(matches!(
        d.handle(start(f.agent.id.as_str())).await,
        Response::Error {
            code: ErrorCode::Forbidden,
            ..
        }
    ));
    assert!(matches!(
        d.handle(start(HUMAN)).await,
        Response::Error {
            code: ErrorCode::NotFound,
            ..
        }
    ));
    assert!(
        lock(&d.state).starting_cards.is_empty(),
        "free to start on again"
    );
}

/// A card that waits, on an open card or on something off the board, is
/// not started on by the person or by a start rule, as a pull would not
/// take it; once what it waits on clears, both start on it.
#[tokio::test]
async fn a_waiting_card_is_started_on_only_once_it_clears() {
    let f = fixture().await;
    let d = &f.daemon;
    let first = card(
        d.handle(create(HUMAN, &f.work, "Schema first", None, &[]))
            .await,
    );
    let mut filing = create(HUMAN, &f.work, "Port it", Some("ready"), &[]);
    if let Request::TaskCreate {
        blocked_by,
        external_blocker,
        ..
    } = &mut filing
    {
        *blocked_by = vec![first.id.to_string()];
        *external_blocker = "the vendor's sign-off".into();
    }
    let waiting = card(d.handle(filing).await);
    let start = || Request::TaskStart {
        from: HUMAN.into(),
        task: waiting.id.to_string(),
        runtime: "no-such-runtime".into(),
        args: Vec::new(),
        workdir: None,
    };
    let refused = d.handle(start()).await;
    assert!(
        matches!(&refused, Response::Error { code: ErrorCode::Conflict, message, .. }
            if message.contains(&first.handle())),
        "{refused:?}"
    );
    assert!(matches!(
        d.handle(Request::RuleCreate {
            from: HUMAN.into(),
            project: Some(f.work.clone()),
            board: None,
            name: "Work Ready".into(),
            trigger: Trigger::Enter {
                lane: "ready".into()
            },
            action: RuleAction::Start {
                runtime: "no-such-runtime".into(),
                args: Vec::new(),
                max: 2,
            },
        })
        .await,
        Response::Rule { .. }
    ));
    assert!(
        lock(&d.state).sweep_rules(Utc::now()).is_empty(),
        "a start rule launches nothing on it"
    );

    // The card it waits on ends; the thing off the board still holds it.
    card(
        d.handle(Request::TaskMove {
            agent: HUMAN.into(),
            task: first.id.to_string(),
            column: None,
            to: Some("done".into()),
            after: None,
            before: None,
            comment: None,
        })
        .await,
    );
    let refused = d.handle(start()).await;
    assert!(
        matches!(&refused, Response::Error { code: ErrorCode::Conflict, message, .. }
            if message.contains("off the board")),
        "{refused:?}"
    );
    assert!(lock(&d.state).sweep_rules(Utc::now()).is_empty());

    // Cleared: the person's start gets as far as the runtime, and the
    // rule's sweep starts on it.
    card(
        d.handle(Request::TaskUpdate {
            agent: HUMAN.into(),
            task: waiting.id.to_string(),
            title: None,
            acceptance: None,
            assignee: None,
            links: None,
            description: None,
            priority: None,
            size: None,
            due: None,
            external_blocker: Some(String::new()),
            parent: None,
            blocked_by: None,
            tags: None,
            plan: None,
        })
        .await,
    );
    assert!(matches!(
        d.handle(start()).await,
        Response::Error {
            code: ErrorCode::NotFound,
            ..
        }
    ));
    let starts = lock(&d.state).sweep_rules(Utc::now());
    assert_eq!(
        starts.iter().map(|s| &s.task).collect::<Vec<_>>(),
        [&waiting.id]
    );
}

/// The shared manual/rule path launches the provider's adapter before handing
/// it a card. These executables capture their actual argv/environment; they do
/// not run providers, read accounts, or rely on the host's PATH inventory.
#[cfg(unix)]
#[tokio::test]
async fn card_starts_launch_the_input_adapter_and_queue_the_assignment() {
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;
    for (runtime, rule_started) in [("codex", false), ("claude-code", true)] {
        let f = fixture().await;
        let d = &f.daemon;
        let filed = card(
            d.handle(create(
                HUMAN,
                &f.work,
                "Receive this card",
                Some("ready"),
                &[],
            ))
            .await,
        );
        let bin = f._dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let daemon_exe = bin.join("agentd");
        let cli = bin.join("agentdocker");
        let provider = bin.join(runtime);
        std::fs::write(&daemon_exe, "fixture").unwrap();
        let script = "#!/bin/sh\nprintf '%s\\n' \"$AGENTDOCKER_CODEX_INPUT\" \"$AGENTDOCKER_CLAUDE_CHANNEL_INPUT\" \"$@\" > launch.txt\nexec /bin/sleep 30\n";
        for path in [&cli, &provider] {
            std::fs::write(path, script).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut start = rules::Start {
            rule: None,
            task: filed.id.clone(),
            runtime: runtime.into(),
            args: vec!["--model".into(), "chosen-model".into()],
            from: HUMAN.into(),
            workdir: Some(f.work.clone().into()),
        };
        if rule_started {
            let Response::Rule { rule } = d
                .handle(Request::RuleCreate {
                    from: HUMAN.into(),
                    project: Some(f.work.clone()),
                    board: None,
                    name: "Receive Ready".into(),
                    trigger: Trigger::Enter {
                        lane: "ready".into(),
                    },
                    action: RuleAction::Start {
                        runtime: runtime.into(),
                        args: start.args.clone(),
                        max: 1,
                    },
                })
                .await
            else {
                panic!("rule was not created")
            };
            start.rule = Some(rule);
        }
        let Response::Agent { agent } = d
            .start_on_card_command(&start, provider.clone(), Ok(daemon_exe))
            .await
        else {
            panic!("card did not start")
        };
        let output = PathBuf::from(&f.work).join("launch.txt");
        let captured = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(text) = std::fs::read_to_string(&output)
                    && text.contains("chosen-model")
                {
                    break text;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        let (assigned, queued) = {
            let mut state = lock(&d.state);
            (
                state.resolve_task(filed.id.as_str()).unwrap(),
                state.inboxes.get(&agent.id).map_or(0, |queue| queue.len()),
            )
        };
        // Always stop our fixture before assertions can fail, including the
        // negative control with input preparation disabled.
        let stopped = d
            .handle(Request::Stop {
                agent: agent.id.to_string(),
                force: false,
            })
            .await;
        assert!(!matches!(stopped, Response::Error { .. }), "{stopped:?}");
        let captured = captured.expect("fixture command did not write its arguments");
        let lines: Vec<_> = captured.lines().collect();
        assert_eq!(assigned.assignee.as_ref(), Some(&agent.id));
        assert_eq!(queued, 1, "the card is durably queued once");
        if runtime == "codex" {
            assert_eq!(lines[..4], ["1", "", "codex-input", "--"]);
            assert_eq!(lines[4], provider.to_str().unwrap());
            assert_eq!(lines[5..], ["-c", "model=\"chosen-model\""]);
        } else {
            assert_eq!(lines[..3], ["", "1", "--mcp-config"]);
            let config: serde_json::Value = serde_json::from_str(lines[3]).unwrap();
            assert_eq!(
                config["mcpServers"]["agentdocker"]["command"],
                cli.to_str().unwrap()
            );
            assert_eq!(
                config["mcpServers"]["agentdocker"]["args"],
                serde_json::json!(["mcp", "--runtime", "claude-code", "--claude-channel"])
            );
            assert_eq!(
                lines[4..],
                [
                    "--dangerously-load-development-channels",
                    "server:agentdocker",
                    "--model",
                    "chosen-model"
                ]
            );
            assert_eq!(assigned.fired, [start.rule.unwrap().id]);
        }
    }
}

#[tokio::test]
async fn card_start_input_failures_leave_the_card_and_registry_unchanged() {
    let f = fixture().await;
    let d = &f.daemon;
    let filed = card(
        d.handle(create(HUMAN, &f.work, "Leave me free", Some("ready"), &[]))
            .await,
    );
    let before = lock(&d.state).registry.all().count();
    let bin = f._dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let daemon_exe = bin.join(format!("agentd{}", std::env::consts::EXE_SUFFIX));
    let cli = bin.join(format!("agentdocker{}", std::env::consts::EXE_SUFFIX));
    std::fs::write(&daemon_exe, "fixture").unwrap();
    for (runtime, args, installed, code) in [
        ("codex", vec![], false, ErrorCode::Unavailable),
        ("codex", vec!["resume"], true, ErrorCode::Invalid),
        ("claude-code", vec!["--print"], true, ErrorCode::Invalid),
        ("aider", vec![], true, ErrorCode::Unavailable),
    ] {
        if installed {
            std::fs::write(&cli, "fixture").unwrap();
        }
        let start = rules::Start {
            rule: None,
            task: filed.id.clone(),
            runtime: runtime.into(),
            args: args.into_iter().map(str::to_owned).collect(),
            from: HUMAN.into(),
            workdir: Some(f.work.clone().into()),
        };
        let response = d
            .start_on_card_command(&start, bin.join(runtime), Ok(daemon_exe.clone()))
            .await;
        assert!(
            matches!(&response, Response::Error { code: actual, .. } if *actual == code),
            "{response:?}"
        );
        let mut state = lock(&d.state);
        assert_eq!(state.resolve_task(filed.id.as_str()).unwrap(), filed);
        assert_eq!(state.registry.all().count(), before);
        assert!(state.leases.all().is_empty());
    }
}
