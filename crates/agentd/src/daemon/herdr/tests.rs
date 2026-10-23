use super::*;
use agentdocker_core::multiplexer::{Evidence, Session};
use agentdocker_core::{
    Activity, ActivityObservation, AdapterContact, AdapterKind, ReportedActivity,
};
use tempfile::TempDir;

fn open(dir: &TempDir) -> Arc<Daemon> {
    let home = dir.path().to_path_buf();
    Arc::new(Daemon::open(home.clone(), home.join("sock")).unwrap())
}

/// A running agent recorded in a herdr session (`herdr` names it; `None`
/// is the default session), naming `provider` as its provider session.
async fn in_herdr(
    daemon: &Arc<Daemon>,
    name: &str,
    provider: Option<&str>,
    herdr: Option<&str>,
) -> AgentRecord {
    let mut spec = AgentSpec {
        name: name.to_owned(),
        runtime: "gemini".to_owned(),
        ..AgentSpec::default()
    };
    if let Some(provider) = provider {
        spec.labels
            .insert("session_id".to_owned(), provider.to_owned());
    }
    let Response::Agent { agent } = daemon
        .handle(Request::Register {
            spec,
            pid: None,
            session: None,
        })
        .await
    else {
        panic!("registered");
    };
    let mut state = lock(&daemon.state);
    let record = state.registry.get_mut(&agent.id).unwrap();
    record.process_started_at = Some(Utc::now() - Duration::hours(1));
    record.session = Some(Session {
        kind: "herdr".to_owned(),
        session: herdr.map(str::to_owned),
        pane: Some("w1:p1".to_owned()),
        evidence: Evidence::Environment,
        socket: None,
    });
    let record = record.clone();
    state.store.upsert_agent(&record).unwrap();
    record
}

/// Mark the agent's hooks as having run in its current process.
fn hooks_ran(daemon: &Daemon, agent: &AgentId) {
    let mut state = lock(&daemon.state);
    let record = state.registry.get_mut(agent).unwrap();
    record.adapter_contacts.insert(
        AdapterKind::Hooks,
        AdapterContact {
            process_started_at: record.process_started_at.unwrap(),
            observed_at: Utc::now(),
        },
    );
}

fn sockets_in(dir: &Path) -> SocketOf {
    let dir = dir.to_path_buf();
    Arc::new(move |name: Option<&str>| agentdocker_host::multiplexer::herdr_socket_in(&dir, name))
}

fn observation(daemon: &Daemon, agent: &AgentId) -> Option<ActivityObservation> {
    lock(&daemon.state)
        .registry
        .get(agent)
        .unwrap()
        .reported_activity
        .clone()
}

/// Which agents herdr is asked about, and on which socket: only those
/// with no hooks in their current process, and only by provider session.
#[tokio::test]
async fn herdr_is_asked_only_about_hookless_agents_by_their_provider_session() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let socket_of = sockets_in(Path::new("/h"));
    let default = PathBuf::from("/h/herdr.sock");
    let backend = PathBuf::from("/h/sessions/backend/herdr.sock");

    let plain = in_herdr(&daemon, "plain", Some("thread-1"), None).await;
    let named = in_herdr(&daemon, "named", Some("thread-2"), Some("backend")).await;
    // Known only by name: a pane is never matched by a display name.
    in_herdr(&daemon, "w1:p1", None, None).await;
    let hooked = in_herdr(&daemon, "hooked", Some("thread-3"), None).await;
    hooks_ran(&daemon, &hooked.id);
    // Hooks that ran in an earlier process say nothing of this one.
    let earlier = in_herdr(&daemon, "earlier", Some("thread-4"), None).await;
    hooks_ran(&daemon, &earlier.id);
    lock(&daemon.state)
        .registry
        .get_mut(&earlier.id)
        .unwrap()
        .process_started_at = Some(Utc::now());
    // Two agents claiming one provider session: neither is read.
    in_herdr(&daemon, "twin-a", Some("thread-5"), None).await;
    in_herdr(&daemon, "twin-b", Some("thread-5"), None).await;
    // A session name that is a path names no socket.
    in_herdr(&daemon, "escaping", Some("thread-6"), Some("../x")).await;
    // In tmux, not herdr.
    let tmux = in_herdr(&daemon, "tmux", Some("thread-7"), None).await;
    lock(&daemon.state)
        .registry
        .get_mut(&tmux.id)
        .unwrap()
        .session
        .as_mut()
        .unwrap()
        .kind = "tmux".to_owned();
    // Finished.
    let gone = in_herdr(&daemon, "gone", Some("thread-8"), None).await;
    lock(&daemon.state)
        .registry
        .get_mut(&gone.id)
        .unwrap()
        .status = AgentStatus::Exited { code: Some(0) };

    assert_eq!(
        daemon.herdr_watches(&socket_of),
        Watches::from([
            (
                default,
                BTreeMap::from([
                    ("thread-1".to_owned(), plain.id.clone()),
                    ("thread-4".to_owned(), earlier.id.clone()),
                ])
            ),
            (
                backend,
                BTreeMap::from([("thread-2".to_owned(), named.id.clone())])
            ),
        ])
    );
}

/// What herdr says is recorded once per change, renewed only when due,
/// and only for an agent herdr is still asked about; it stands over
/// contact until it expires, and it is not contact itself.
#[tokio::test]
async fn herdr_observations_outrank_contact_until_they_expire() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let socket_of = sockets_in(Path::new("/h"));
    let socket = Path::new("/h/herdr.sock");
    let agent = in_herdr(&daemon, "quiet", Some("thread-1"), None).await;
    let now = Utc::now();
    let mut events = daemon.subscribe_events();
    let mut state = lock(&daemon.state);
    let seen_before = state.registry.get(&agent.id).unwrap().last_seen;

    assert!(state.report_from_herdr(&agent.id, socket, "thread-1", Status::Idle, &socket_of, now));
    let Ok(Event {
        kind: EventKind::AgentActivityReported { observation, .. },
        ..
    }) = events.try_recv()
    else {
        panic!("the observation is announced");
    };
    assert_eq!(observation.source, Some(ActivitySource::Herdr));
    assert_eq!(
        state.registry.get(&agent.id).unwrap().last_seen,
        seen_before,
        "herdr seeing the agent is not the agent's contact"
    );

    // The same word again is not news until it is due for renewal; a
    // different word is.
    let soon = now + Duration::seconds(30);
    assert!(!state.report_from_herdr(
        &agent.id,
        socket,
        "thread-1",
        Status::Done,
        &socket_of,
        soon
    ));
    assert!(events.try_recv().is_err());
    assert!(state.report_from_herdr(
        &agent.id,
        socket,
        "thread-1",
        Status::Idle,
        &socket_of,
        now + Duration::seconds(61)
    ));
    assert!(!state.report_from_herdr(
        &agent.id,
        socket,
        "thread-1",
        Status::Unknown,
        &socket_of,
        now + Duration::seconds(90)
    ));
    // Another socket or another session is not this agent's pane.
    assert!(!state.report_from_herdr(
        &agent.id,
        Path::new("/h/sessions/x/herdr.sock"),
        "thread-1",
        Status::Working,
        &socket_of,
        now + Duration::seconds(91)
    ));
    assert!(!state.report_from_herdr(
        &agent.id,
        socket,
        "thread-9",
        Status::Working,
        &socket_of,
        now + Duration::seconds(91)
    ));

    // Its MCP server polling afterwards is presence, not work.
    let renewed = now + Duration::seconds(61);
    let record = state.registry.get_mut(&agent.id).unwrap();
    record.last_seen = renewed + Duration::seconds(5);
    let record = record.clone();
    assert!(matches!(
        state.activity_of(&record, renewed + Duration::seconds(10)),
        Activity::Idle { .. }
    ));
    // Past its expiry herdr is no longer saying, and contact counts again.
    let record = state.registry.get_mut(&agent.id).unwrap();
    record.last_seen = renewed + Duration::seconds(300);
    let record = record.clone();
    assert!(matches!(
        state.activity_of(&record, renewed + Duration::seconds(301)),
        Activity::Working { .. }
    ));

    // Once its hooks run in this process, they speak for it instead.
    drop(state);
    hooks_ran(&daemon, &agent.id);
    assert!(!lock(&daemon.state).report_from_herdr(
        &agent.id,
        socket,
        "thread-1",
        Status::Working,
        &socket_of,
        now + Duration::seconds(200)
    ));
}

/// Only the daemon observes for an agent: a client cannot pass its report
/// off as herdr's.
#[tokio::test]
async fn a_client_report_cannot_name_a_source() {
    let dir = TempDir::new().unwrap();
    let daemon = open(&dir);
    let agent = in_herdr(&daemon, "claimant", Some("thread-1"), None).await;
    let response = daemon
        .handle(Request::ReportActivity {
            agent: agent.id.to_string(),
            observation: ActivityObservation {
                activity: ReportedActivity::Idle,
                observed_at: Utc::now(),
                reason: None,
                detail: None,
                source: Some(ActivitySource::Herdr),
            },
        })
        .await;
    assert!(matches!(
        response,
        Response::Error {
            code: ErrorCode::Invalid,
            ..
        }
    ));
    assert_eq!(observation(&daemon, &agent.id), None);
}

#[cfg(unix)]
mod socket {
    use super::*;
    use agentdocker_core::WaitReason;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::sync::mpsc;

    fn activity(daemon: &Daemon, agent: &AgentId) -> Activity {
        let state = lock(&daemon.state);
        state.activity_of(state.registry.get(agent).unwrap(), Utc::now())
    }

    async fn until(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !done() {
            assert!(std::time::Instant::now() < deadline, "timed out: {what}");
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    /// A herdr server as far as the daemon talks to one: `pane.list`
    /// answers with the panes it is given, `events.subscribe` acknowledges
    /// and then pushes what the test sends. It records every method it is
    /// asked, each subscription's request, and each subscription the
    /// client closed.
    struct Fake {
        panes: Arc<Mutex<Value>>,
        methods: Arc<Mutex<Vec<String>>>,
        subscriptions: Arc<Mutex<Vec<Value>>>,
        pushers: Arc<Mutex<Vec<mpsc::UnboundedSender<String>>>>,
        closed: Arc<AtomicUsize>,
        _server: tokio::task::JoinHandle<()>,
    }

    impl Fake {
        fn start(path: &Path, panes: Value) -> Self {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let listener = tokio::net::UnixListener::bind(path).unwrap();
            let fake = Self {
                panes: Arc::new(Mutex::new(panes)),
                methods: Arc::default(),
                subscriptions: Arc::default(),
                pushers: Arc::default(),
                closed: Arc::default(),
                _server: tokio::spawn(async {}),
            };
            let (panes, methods, subscriptions, pushers, closed) = (
                fake.panes.clone(),
                fake.methods.clone(),
                fake.subscriptions.clone(),
                fake.pushers.clone(),
                fake.closed.clone(),
            );
            let server = tokio::spawn(async move {
                while let Ok((stream, _)) = listener.accept().await {
                    let (panes, methods, subscriptions, pushers, closed) = (
                        panes.clone(),
                        methods.clone(),
                        subscriptions.clone(),
                        pushers.clone(),
                        closed.clone(),
                    );
                    tokio::spawn(async move {
                        let (read, mut write) = stream.into_split();
                        let mut read = BufReader::new(read);
                        let mut line = String::new();
                        if read.read_line(&mut line).await.unwrap_or(0) == 0 {
                            return;
                        }
                        let request: Value = serde_json::from_str(&line).unwrap();
                        let id = request["id"].clone();
                        let method = request["method"].as_str().unwrap_or("").to_owned();
                        methods.lock().unwrap().push(method.clone());
                        match method.as_str() {
                            "pane.list" => {
                                let panes = panes.lock().unwrap().clone();
                                let reply = json!({"id": id, "result": {"type": "pane_list", "panes": panes}});
                                let _ = write.write_all(format!("{reply}\n").as_bytes()).await;
                            }
                            "events.subscribe" => {
                                subscriptions
                                    .lock()
                                    .unwrap()
                                    .push(request["params"]["subscriptions"].clone());
                                let ack =
                                    json!({"id": id, "result": {"type": "subscription_started"}});
                                let _ = write.write_all(format!("{ack}\n").as_bytes()).await;
                                let (push, mut pushed) = mpsc::unbounded_channel::<String>();
                                pushers.lock().unwrap().push(push);
                                let mut rest = String::new();
                                loop {
                                    tokio::select! {
                                        next = pushed.recv() => match next {
                                            Some(line) => {
                                                let _ = write.write_all(format!("{line}\n").as_bytes()).await;
                                            }
                                            None => return,
                                        },
                                        read = read.read_line(&mut rest) => {
                                            if read.unwrap_or(0) == 0 {
                                                closed.fetch_add(1, Ordering::SeqCst);
                                                return;
                                            }
                                        }
                                    }
                                }
                            }
                            _ => {
                                let reply = json!({"id": id, "error": {"code": "unknown_method", "message": "no"}});
                                let _ = write.write_all(format!("{reply}\n").as_bytes()).await;
                            }
                        }
                    });
                }
            });
            Self {
                _server: server,
                ..fake
            }
        }

        fn set_panes(&self, panes: Value) {
            *self.panes.lock().unwrap() = panes;
        }

        /// One line to every open subscription.
        fn push(&self, line: Value) {
            self.pushers
                .lock()
                .unwrap()
                .retain(|push| push.send(line.to_string()).is_ok());
        }

        /// herdr ends every subscription.
        fn end_subscriptions(&self) {
            self.pushers.lock().unwrap().clear();
        }

        fn subscribed(&self) -> Vec<Value> {
            self.subscriptions.lock().unwrap().clone()
        }

        fn methods(&self) -> Vec<String> {
            self.methods.lock().unwrap().clone()
        }
    }

    fn pane(id: &str, status: &str, session: Option<&str>) -> Value {
        json!({
            "pane_id": id,
            "terminal_id": "term_1",
            "workspace_id": "w1",
            "tab_id": "w1:t1",
            "focused": false,
            "revision": 0,
            "agent": "gemini",
            "agent_status": status,
            "agent_session": session.map(|value| json!({
                "source": "herdr:gemini", "agent": "gemini", "kind": "id", "value": value
            })),
        })
    }

    fn status(pane: &str, status: &str) -> Value {
        json!({"event": "pane.agent_status_changed", "data": {
            "agent": "gemini", "agent_status": status, "pane_id": pane, "workspace_id": "w1"
        }})
    }

    fn detected(pane: &str) -> Value {
        json!({"event": "pane_agent_detected", "data": {
            "agent": "gemini", "pane_id": pane, "type": "pane_agent_detected", "workspace_id": "w1"
        }})
    }

    fn fast() -> Timing {
        use std::time::Duration;
        Timing {
            tick: Duration::from_millis(20),
            refresh: Duration::from_millis(400),
            seek: Duration::from_millis(100),
            settle: Duration::from_millis(20),
            timeout: Duration::from_secs(2),
            backoff: (Duration::from_millis(20), Duration::from_millis(200)),
        }
    }

    /// The whole path, against a herdr that answers as 0.9.3 does: an agent
    /// with no hooks in a herdr pane shows what herdr says of the pane
    /// holding its provider session; a status in another pane is not its;
    /// only reads reach herdr; and once its hooks run, nobody needs the
    /// subscription and it is closed.
    #[tokio::test]
    async fn a_hookless_agent_in_a_herdr_pane_shows_what_herdr_says() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let herdr_dir = dir.path().join("herdr");
        let fake = Fake::start(
            &herdr_dir.join("herdr.sock"),
            json!([
                pane("w1:p1", "working", Some("thread-1")),
                pane("w1:p2", "blocked", None),
            ]),
        );
        let agent = in_herdr(&daemon, "quiet", Some("thread-1"), None).await;
        let bystander = in_herdr(&daemon, "w1:p2", None, None).await;
        let task = tokio::spawn(run(daemon.clone(), fast(), sockets_in(&herdr_dir)));

        until("working", || {
            matches!(activity(&daemon, &agent.id), Activity::Working { .. })
        })
        .await;
        assert_eq!(
            observation(&daemon, &agent.id).unwrap().source,
            Some(ActivitySource::Herdr)
        );
        assert_eq!(
            fake.subscribed(),
            [json!([
                {"type": "pane.agent_detected"},
                {"type": "pane.agent_status_changed", "pane_id": "w1:p1"},
            ])],
            "one subscription, to the pane holding the session"
        );

        fake.push(status("w1:p1", "blocked"));
        until("waiting", || {
            activity(&daemon, &agent.id)
                == Activity::Waiting {
                    since: observation(&daemon, &agent.id).unwrap().observed_at,
                    reason: Some(WaitReason::Other),
                    judged: None,
                    detail: None,
                }
        })
        .await;
        fake.push(status("w1:p1", "done"));
        until("idle", || {
            matches!(activity(&daemon, &agent.id), Activity::Idle { .. })
        })
        .await;
        // A status in a pane that does not hold the session is not this
        // agent's, nor the bystander's, whose name is the pane's.
        let before = observation(&daemon, &agent.id);
        fake.push(status("w1:p2", "working"));
        fake.push(status("w1:p1", "done"));
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(observation(&daemon, &agent.id), before);
        assert_eq!(observation(&daemon, &bystander.id), None);
        assert!(
            daemon
                .recent_events(100)
                .iter()
                .filter(|event| matches!(&event.kind, EventKind::AgentActivityReported { .. }))
                .count()
                >= 3,
            "working, waiting and idle were each announced"
        );

        // Its hooks run: nobody needs herdr any more, and the subscription
        // is closed rather than left open.
        hooks_ran(&daemon, &agent.id);
        until("subscription closed", || {
            fake.closed.load(Ordering::SeqCst) == 1
        })
        .await;
        fake.push(status("w1:p1", "working"));
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(observation(&daemon, &agent.id), before);
        assert!(
            fake.methods()
                .iter()
                .all(|method| method == "pane.list" || method == "events.subscribe"),
            "only reads reach herdr: {:?}",
            fake.methods()
        );
        task.abort();
    }

    /// A pane moved to another workspace gets a new id: the detection is
    /// followed to it, a status from the old id is not attributed, and a
    /// subscription herdr ends is opened again.
    #[tokio::test]
    async fn a_moved_pane_and_an_ended_subscription_are_followed() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let herdr_dir = dir.path().join("herdr");
        let fake = Fake::start(
            &herdr_dir.join("sessions/backend/herdr.sock"),
            json!([pane("w1:p1", "idle", Some("thread-1"))]),
        );
        let agent = in_herdr(&daemon, "mover", Some("thread-1"), Some("backend")).await;
        let task = tokio::spawn(run(daemon.clone(), fast(), sockets_in(&herdr_dir)));
        until("idle", || {
            matches!(activity(&daemon, &agent.id), Activity::Idle { .. })
        })
        .await;

        fake.set_panes(json!([
            pane("w1:p2", "unknown", None),
            pane("w2:p4", "working", Some("thread-1")),
        ]));
        fake.push(detected("w2:p4"));
        until("working in the moved pane", || {
            matches!(activity(&daemon, &agent.id), Activity::Working { .. })
        })
        .await;
        until("subscribed to the moved pane", || {
            fake.subscribed().last()
                == Some(&json!([
                    {"type": "pane.agent_detected"},
                    {"type": "pane.agent_status_changed", "pane_id": "w2:p4"},
                ]))
        })
        .await;
        fake.push(status("w1:p1", "blocked"));
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(matches!(
            activity(&daemon, &agent.id),
            Activity::Working { .. }
        ));
        fake.push(status("w2:p4", "blocked"));
        until("waiting", || {
            matches!(activity(&daemon, &agent.id), Activity::Waiting { .. })
        })
        .await;

        let subscribed = fake.subscribed().len();
        fake.end_subscriptions();
        until("subscribed again", || fake.subscribed().len() > subscribed).await;
        fake.set_panes(json!([pane("w2:p4", "idle", Some("thread-1"))]));
        fake.push(status("w2:p4", "idle"));
        until("idle again", || {
            matches!(activity(&daemon, &agent.id), Activity::Idle { .. })
        })
        .await;
        task.abort();
    }

    /// No herdr there: nothing is recorded, and the attempts back off
    /// rather than spin.
    #[tokio::test]
    async fn an_absent_herdr_is_retried_with_backoff() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let herdr_dir = dir.path().join("herdr");
        let agent = in_herdr(&daemon, "alone", Some("thread-1"), None).await;
        let task = tokio::spawn(run(daemon.clone(), fast(), sockets_in(&herdr_dir)));
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert_eq!(observation(&daemon, &agent.id), None);
        // herdr starts: the next attempt finds it.
        let _fake = Fake::start(
            &herdr_dir.join("herdr.sock"),
            json!([pane("w1:p1", "working", Some("thread-1"))]),
        );
        until("working", || {
            matches!(activity(&daemon, &agent.id), Activity::Working { .. })
        })
        .await;
        task.abort();
    }

    /// A line longer than the bound is refused, not buffered without end.
    #[tokio::test]
    async fn a_line_is_read_only_up_to_its_bound() {
        let mut reader = BufReader::new(&b"short\nlonger than eight\n"[..]);
        assert_eq!(
            read_line(&mut reader, 8).await.unwrap().as_deref(),
            Some("short")
        );
        assert!(read_line(&mut reader, 8).await.is_err());
        let mut ended = BufReader::new(&b"whole\n"[..]);
        assert_eq!(
            read_line(&mut ended, 8).await.unwrap().as_deref(),
            Some("whole")
        );
        assert_eq!(read_line(&mut ended, 8).await.unwrap(), None);
        let mut cut = BufReader::new(&b"cut"[..]);
        assert!(read_line(&mut cut, 8).await.is_err());
    }
}
