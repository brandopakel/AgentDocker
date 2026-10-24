//! What the watch records is the daemon's own observation, not the agent's:
//! it is not contact, it lasts five minutes, it stands over contact and
//! over herdr while it lasts, and the waits it ends are only its own.

use super::*;
use agentdocker_core::multiplexer::herdr::Status;
use agentdocker_core::multiplexer::{Evidence, Session};
use agentdocker_core::{Activity, AdapterKind, Event, EventKind, Request};
use chrono::Duration;
use tempfile::TempDir;
use tokio::sync::broadcast;

const THREAD: &str = "0199a6f1-0000-7000-8000-0000000000d1";

/// A daemon with one live Codex record keyed by a profile and a thread, as
/// a terminal Codex's first hook makes it.
struct Keyed {
    _dir: TempDir,
    daemon: Arc<Daemon>,
    profile: PathBuf,
    id: AgentId,
}

async fn keyed() -> Keyed {
    let dir = TempDir::new().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let daemon = Arc::new(Daemon::open(home.clone(), home.join("sock")).unwrap());
    let checkout = dir.path().join("checkout");
    std::fs::create_dir_all(&checkout).unwrap();
    let profile = dir.path().join("profile");
    let Response::Agent { agent } = daemon
        .handle(Request::SessionAttach {
            runtime: "codex".into(),
            profile: profile.display().to_string(),
            session: THREAD.into(),
            workdir: checkout,
            adapter: AdapterKind::Hooks,
        })
        .await
    else {
        panic!("the conversation's keyed record");
    };
    Keyed {
        _dir: dir,
        daemon,
        profile,
        id: agent.id,
    }
}

impl Keyed {
    /// What the watch makes of `status` at `now`; whether it was recorded.
    fn observe(&self, status: ThreadStatus, waits: &mut Waits, now: DateTime<Utc>) -> bool {
        lock(&self.daemon.state).observe_codex_thread(
            &self.id,
            &self.profile,
            THREAD,
            &status,
            waits,
            now,
        )
    }

    /// What the agent's own adapter reports, as its hooks or MCP server do.
    fn reports(&self, activity: ReportedActivity, reason: Option<WaitReason>, at: DateTime<Utc>) {
        let response = lock(&self.daemon.state).report_activity(
            self.id.as_str(),
            ActivityObservation {
                activity,
                observed_at: at,
                reason,
                detail: None,
                source: None,
            },
            at,
        );
        assert!(matches!(response, Response::Ok), "{response:?}");
    }

    /// What some other source said, recorded as the daemon records it.
    fn said_by(&self, source: ActivitySource, activity: ReportedActivity, at: DateTime<Utc>) {
        let response = lock(&self.daemon.state).record_activity(
            &self.id,
            ActivityObservation {
                activity,
                observed_at: at,
                reason: (activity == ReportedActivity::Waiting).then_some(WaitReason::Other),
                detail: None,
                source: Some(source),
            },
            at,
        );
        assert!(matches!(response, Response::Ok), "{response:?}");
    }

    fn observation(&self) -> ActivityObservation {
        lock(&self.daemon.state)
            .registry
            .get(&self.id)
            .unwrap()
            .reported_activity
            .clone()
            .expect("an observation")
    }

    fn said(&self) -> (ReportedActivity, Option<WaitReason>, Option<ActivitySource>) {
        let observation = self.observation();
        (observation.activity, observation.reason, observation.source)
    }

    fn activity(&self, now: DateTime<Utc>) -> Activity {
        let state = lock(&self.daemon.state);
        state.activity_of(state.registry.get(&self.id).unwrap(), now)
    }

    fn last_seen(&self) -> DateTime<Utc> {
        lock(&self.daemon.state)
            .registry
            .get(&self.id)
            .unwrap()
            .last_seen
    }

    /// Contact at `at`, as the daemon records a call from the agent.
    fn contact(&self, at: DateTime<Utc>) {
        lock(&self.daemon.state)
            .registry
            .get_mut(&self.id)
            .unwrap()
            .last_seen = at;
    }

    /// Its MCP server's call, through the daemon as any client makes it.
    async fn heartbeat(&self) {
        let response = self
            .daemon
            .handle(Request::Heartbeat {
                agent: self.id.to_string(),
            })
            .await;
        assert!(matches!(response, Response::Ok), "{response:?}");
    }

    /// Put the record in a herdr pane that holds its thread, with a process
    /// and no hooks run in it, so herdr is asked about it too.
    fn in_herdr(&self) {
        let mut state = lock(&self.daemon.state);
        let record = state.registry.get_mut(&self.id).unwrap();
        record.process_started_at = Some(Utc::now() - Duration::hours(1));
        record.session = Some(Session {
            kind: "herdr".to_owned(),
            session: None,
            pane: Some("w1:p1".to_owned()),
            evidence: Evidence::Environment,
        });
    }
}

fn approval() -> ThreadStatus {
    ThreadStatus::Active {
        waiting_on_approval: true,
        waiting_on_user_input: false,
    }
}

fn question() -> ThreadStatus {
    ThreadStatus::Active {
        waiting_on_approval: false,
        waiting_on_user_input: true,
    }
}

fn running() -> ThreadStatus {
    ThreadStatus::Active {
        waiting_on_approval: false,
        waiting_on_user_input: false,
    }
}

/// The observations announced since the last look.
fn announced(events: &mut broadcast::Receiver<Event>) -> Vec<ActivityObservation> {
    let mut seen = Vec::new();
    while let Ok(event) = events.try_recv() {
        if let EventKind::AgentActivityReported { observation, .. } = event.kind {
            seen.push(observation);
        }
    }
    seen
}

const SERVER: Option<ActivitySource> = Some(ActivitySource::CodexServer);

/// What the server says is recorded as its own and announced so; it is not
/// the agent's contact; and the agent's MCP server calling afterwards does
/// not turn a server-reported idle or wait into working while it lasts —
/// as it does an idle the agent's own adapter reported.
#[tokio::test]
async fn contact_does_not_turn_the_servers_idle_or_wait_into_working() {
    let k = keyed().await;
    let mut events = k.daemon.subscribe_events();
    let mut waits = Waits::new();

    for (status, want) in [
        (ThreadStatus::Idle, (ReportedActivity::Idle, None)),
        (
            approval(),
            (ReportedActivity::Waiting, Some(WaitReason::Permission)),
        ),
        (
            question(),
            (ReportedActivity::Waiting, Some(WaitReason::Question)),
        ),
    ] {
        let seen = k.last_seen();
        let at = Utc::now() - Duration::milliseconds(500);
        assert!(k.observe(status, &mut waits, at));
        assert_eq!(k.said(), (want.0, want.1, SERVER));
        let announced = announced(&mut events);
        assert_eq!(announced.len(), 1);
        assert_eq!(announced[0].source, SERVER, "announced as the server's");
        assert_eq!(
            k.last_seen(),
            seen,
            "the server's word is not the agent's contact"
        );
        k.heartbeat().await;
        assert!(k.last_seen() > at, "contact came after it");
        let activity = k.activity(Utc::now());
        match want {
            (ReportedActivity::Idle, _) => {
                assert!(matches!(activity, Activity::Idle { .. }), "{activity:?}")
            }
            (_, reason) => assert!(
                matches!(activity, Activity::Waiting { reason: r, .. } if r == reason),
                "{activity:?}"
            ),
        }
    }

    // The agent's own idle is what contact supersedes: that was the gap.
    k.reports(
        ReportedActivity::Idle,
        None,
        Utc::now() - Duration::milliseconds(300),
    );
    k.heartbeat().await;
    assert!(matches!(k.activity(Utc::now()), Activity::Working { .. }));
    // The server saying idle too restates it as its own, which contact
    // does not supersede.
    let seen = k.last_seen();
    assert!(k.observe(
        ThreadStatus::Idle,
        &mut waits,
        Utc::now() - Duration::milliseconds(200)
    ));
    assert_eq!(k.said(), (ReportedActivity::Idle, None, SERVER));
    assert_eq!(k.last_seen(), seen);
    k.heartbeat().await;
    assert!(matches!(k.activity(Utc::now()), Activity::Idle { .. }));
}

/// A server observation lasts five minutes whatever it says, a wait
/// included; an unchanged one is renewed only when due, and past its
/// expiry contact counts again.
#[tokio::test]
async fn a_server_observation_lasts_five_minutes_unless_renewed() {
    let k = keyed().await;
    let mut events = k.daemon.subscribe_events();
    let mut waits = Waits::new();
    let t0 = Utc::now() + Duration::seconds(1);

    assert!(k.observe(approval(), &mut waits, t0));
    k.contact(t0 + Duration::seconds(10));
    assert!(matches!(
        k.activity(t0 + Duration::seconds(299)),
        Activity::Waiting {
            reason: Some(WaitReason::Permission),
            ..
        }
    ));
    // Not due for renewal yet: nothing recorded, nothing announced.
    announced(&mut events);
    assert!(!k.observe(approval(), &mut waits, t0 + Duration::minutes(3)));
    assert!(announced(&mut events).is_empty());
    // At five minutes the server is no longer saying: the contact before
    // it is too old to be working, and fresh contact is working.
    assert_eq!(k.activity(t0 + Duration::minutes(5)), Activity::Unknown);
    k.contact(t0 + Duration::minutes(5));
    assert!(matches!(
        k.activity(t0 + Duration::minutes(5) + Duration::seconds(1)),
        Activity::Working { .. }
    ));

    // Renewed when due, it holds on.
    let t1 = t0 + Duration::minutes(10);
    assert!(k.observe(question(), &mut waits, t1));
    assert!(!k.observe(question(), &mut waits, t1 + Duration::seconds(239)));
    assert!(k.observe(question(), &mut waits, t1 + Duration::minutes(4)));
    k.contact(t1 + Duration::minutes(8));
    assert!(matches!(
        k.activity(t1 + Duration::minutes(8) + Duration::seconds(59)),
        Activity::Waiting {
            reason: Some(WaitReason::Question),
            ..
        }
    ));
    assert!(matches!(
        k.activity(t1 + Duration::minutes(9)),
        Activity::Working { .. }
    ));
}

/// The renewal and its restating are source-aware: the server's own word
/// is renewed only when due; the same word from herdr, or an idle or wait
/// the agent's adapter reported, is restated as the server's at once; a
/// running turn the adapter reported is left as it is until due.
#[tokio::test]
async fn renewal_is_by_the_servers_own_word() {
    let k = keyed().await;
    let mut waits = Waits::new();
    let t0 = Utc::now() + Duration::seconds(1);

    // The agent's adapter says it is running a turn, as its hooks do on
    // every tool run: the server saying the same is not restated...
    k.reports(ReportedActivity::Working, None, t0);
    assert!(!k.observe(running(), &mut waits, t0 + Duration::seconds(5)));
    assert_eq!(k.said(), (ReportedActivity::Working, None, None));
    // ...until it is due, like the server's own.
    assert!(k.observe(running(), &mut waits, t0 + Duration::minutes(4)));
    assert_eq!(k.said(), (ReportedActivity::Working, None, SERVER));

    // The adapter's idle is restated at once.
    let t1 = t0 + Duration::minutes(5);
    k.reports(ReportedActivity::Idle, None, t1);
    assert!(k.observe(ThreadStatus::Idle, &mut waits, t1 + Duration::seconds(1)));
    assert_eq!(k.said(), (ReportedActivity::Idle, None, SERVER));
    // Its own word again is not news.
    assert!(!k.observe(ThreadStatus::Idle, &mut waits, t1 + Duration::seconds(2)));

    // So is the adapter's wait, and the server's own wait is the watch's.
    let t2 = t1 + Duration::minutes(1);
    k.reports(ReportedActivity::Waiting, Some(WaitReason::Permission), t2);
    assert!(waits.is_empty());
    assert!(k.observe(approval(), &mut waits, t2 + Duration::seconds(1)));
    assert_eq!(
        k.said(),
        (
            ReportedActivity::Waiting,
            Some(WaitReason::Permission),
            SERVER
        )
    );
    assert_eq!(waits.get(&k.id), Some(&(t2 + Duration::seconds(1))));

    // herdr's word, the same as the server's, is restated as the server's
    // once the server's has lapsed and herdr has spoken since.
    let t3 = t2 + Duration::minutes(10);
    k.said_by(ActivitySource::Herdr, ReportedActivity::Idle, t3);
    assert!(k.observe(ThreadStatus::Idle, &mut waits, t3 + Duration::seconds(1)));
    assert_eq!(k.said(), (ReportedActivity::Idle, None, SERVER));
    assert!(
        waits.is_empty(),
        "a word other than a wait is no wait of its"
    );
}

/// The conversation unloading, failing, or the connection ending ends the
/// wait the watch reported — as the server's idle, which is not contact —
/// and never a wait the agent's adapter or herdr reported since.
#[tokio::test]
async fn unload_or_disconnect_ends_only_the_watchs_own_wait() {
    let k = keyed().await;
    let mut waits = Waits::new();
    let t0 = Utc::now() + Duration::seconds(1);
    let seen = k.last_seen();

    // Its own wait ends when the conversation unloads.
    assert!(k.observe(approval(), &mut waits, t0));
    assert!(!k.observe(
        ThreadStatus::NotLoaded,
        &mut waits,
        t0 + Duration::seconds(10)
    ));
    let ended = k.observation();
    assert_eq!(
        (ended.activity, ended.source, ended.observed_at),
        (ReportedActivity::Idle, SERVER, t0 + Duration::seconds(10))
    );
    assert_eq!(k.last_seen(), seen, "the end of a wait is not contact");
    assert!(waits.is_empty());

    // A wait the agent's adapter reported after the server's is not the
    // watch's to end when the conversation fails.
    assert!(k.observe(question(), &mut waits, t0 + Duration::seconds(20)));
    k.reports(
        ReportedActivity::Waiting,
        Some(WaitReason::Permission),
        t0 + Duration::seconds(25),
    );
    assert!(!k.observe(
        ThreadStatus::SystemError,
        &mut waits,
        t0 + Duration::seconds(30)
    ));
    assert_eq!(
        k.said(),
        (
            ReportedActivity::Waiting,
            Some(WaitReason::Permission),
            None
        )
    );

    // Its own wait ends when the connection does.
    assert!(k.observe(approval(), &mut waits, t0 + Duration::seconds(40)));
    let seen = k.last_seen();
    k.daemon.clear_codex_waits(&mut waits);
    assert_eq!(k.said(), (ReportedActivity::Idle, None, SERVER));
    assert_eq!(k.last_seen(), seen);
    assert!(waits.is_empty());

    // herdr's wait, once the server's word has lapsed, is not its to end.
    let t1 = t0 + Duration::minutes(1);
    assert!(k.observe(approval(), &mut waits, t1));
    k.said_by(
        ActivitySource::Herdr,
        ReportedActivity::Waiting,
        t1 + Duration::minutes(5),
    );
    assert_eq!(
        k.said(),
        (
            ReportedActivity::Waiting,
            Some(WaitReason::Other),
            Some(ActivitySource::Herdr)
        )
    );
    k.daemon.clear_codex_waits(&mut waits);
    assert_eq!(
        k.said(),
        (
            ReportedActivity::Waiting,
            Some(WaitReason::Other),
            Some(ActivitySource::Herdr)
        )
    );
    assert!(waits.is_empty());
}

/// Where herdr and Codex's shared server both speak for one agent, the
/// server's fresh word wins, whichever came first: it replaces herdr's,
/// and herdr's is not recorded over it until it lapses unrenewed.
#[tokio::test]
async fn a_fresh_server_status_wins_over_herdrs_reading_of_the_terminal() {
    let k = keyed().await;
    k.in_herdr();
    let socket_of: crate::daemon::herdr::SocketOf = Arc::new(|name: Option<&str>| {
        agentdocker_host::multiplexer::herdr_socket_in(Path::new("/h"), name)
    });
    let socket = Path::new("/h/herdr.sock");
    let herdr = |status: Status, at: DateTime<Utc>| {
        lock(&k.daemon.state).report_from_herdr(&k.id, socket, THREAD, status, &socket_of, at)
    };
    let mut waits = Waits::new();
    let t0 = Utc::now() + Duration::seconds(1);

    // herdr spoke first; the server's word replaces it.
    assert!(herdr(Status::Working, t0));
    assert_eq!(
        k.said(),
        (ReportedActivity::Working, None, Some(ActivitySource::Herdr))
    );
    assert!(k.observe(ThreadStatus::Idle, &mut waits, t0 + Duration::seconds(5)));
    assert_eq!(k.said(), (ReportedActivity::Idle, None, SERVER));

    // While it is current, herdr's word is not recorded over it, a new
    // reading or a renewal alike, nor through the recorder directly.
    for (status, after) in [
        (Status::Working, 10),
        (Status::Blocked, 70),
        (Status::Working, 304),
    ] {
        assert!(
            !herdr(status, t0 + Duration::seconds(after)),
            "herdr's {status:?} at {after}s"
        );
        assert_eq!(k.said(), (ReportedActivity::Idle, None, SERVER));
    }
    k.said_by(
        ActivitySource::Herdr,
        ReportedActivity::Working,
        t0 + Duration::seconds(200),
    );
    assert_eq!(k.said(), (ReportedActivity::Idle, None, SERVER));
    assert!(matches!(
        k.activity(t0 + Duration::seconds(200)),
        Activity::Idle { .. }
    ));

    // Unrenewed, it lapses five minutes on, and herdr speaks again.
    assert!(herdr(Status::Done, t0 + Duration::seconds(305)));
    assert_eq!(
        k.said(),
        (ReportedActivity::Idle, None, Some(ActivitySource::Herdr))
    );
    // The server's next word wins again, the same word included.
    assert!(k.observe(ThreadStatus::Idle, &mut waits, t0 + Duration::seconds(310)));
    assert_eq!(k.said(), (ReportedActivity::Idle, None, SERVER));
    assert!(!herdr(Status::Blocked, t0 + Duration::seconds(320)));
}
