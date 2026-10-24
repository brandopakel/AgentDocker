//! What Codex's shared app-server says about the conversations it hosts.
//!
//! A Codex terminal (0.162 and later) runs its conversations in one shared
//! app-server per profile, reached at the profile's control socket. For a
//! live Codex record keyed by that profile and a conversation the server has
//! loaded, the daemon reads the conversation's status and records it as the
//! record's activity, as its own observation (`source: codex_server`), not
//! the agent's: an approval or a question it stopped for waits on the
//! person, a running turn is working, a loaded quiet conversation is idle.
//! Like herdr's, such an observation is not contact, lasts five minutes
//! whatever it says, is renewed while the server still says it, and while it
//! lasts stands over contact and over herdr's reading of the terminal. Only
//! reads: `thread/loaded/list`, `thread/read` and the server's own
//! `thread/status/changed`. A profile without the socket is never contacted,
//! and nothing changes for its records.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use agentdocker_core::{
    ActivityObservation, ActivitySource, AgentId, AgentRecord, ReportedActivity, Response,
    WaitReason,
};
use agentdocker_host::codex_control::{self, ThreadStatus};
use chrono::{DateTime, Utc};
use tracing::debug;

use super::{Daemon, State, lock};

/// How often each profile's conversations are read again.
const EVERY: std::time::Duration = std::time::Duration::from_secs(5);
/// An unchanged observation is renewed this long after it was made, inside
/// the five minutes a server observation lasts whatever it says.
const RENEW: chrono::Duration = chrono::Duration::minutes(4);
/// Who these observations are by.
const SOURCE: Option<ActivitySource> = Some(ActivitySource::CodexServer);

/// The stops for the person this watch reported, by record, with when: the
/// ones it clears again when the conversation is no longer loaded.
type Waits = HashMap<AgentId, DateTime<Utc>>;

/// Start watching every profile a live Codex record names.
pub(crate) fn spawn(daemon: Arc<Daemon>) {
    tokio::spawn(run(daemon, EVERY));
}

/// The record's conversation key in a shared server: its verified profile
/// and root thread. Only a live, unsupervised Codex record has one.
fn watched(record: &AgentRecord) -> Option<(PathBuf, String)> {
    if record.spec.runtime != "codex" || record.managed || !record.status.is_live() {
        return None;
    }
    let key = record.session_key()?;
    let profile = PathBuf::from(key.profile?);
    profile.is_absolute().then_some((profile, key.session))
}

/// One watch per profile that has a socket, started again when it ends;
/// a profile without one is looked at again on the next tick.
pub(crate) async fn run(daemon: Arc<Daemon>, every: std::time::Duration) {
    let mut watching: HashMap<PathBuf, tokio::task::JoinHandle<()>> = HashMap::new();
    let mut ticker = tokio::time::interval(every);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        watching.retain(|_, task| !task.is_finished());
        for profile in daemon.codex_profiles() {
            if watching.contains_key(&profile) {
                continue;
            }
            match codex_control::locate(&profile) {
                Ok(Some(_)) => {
                    let task = tokio::spawn(watch(daemon.clone(), profile.clone(), every));
                    watching.insert(profile, task);
                }
                Ok(None) => {}
                Err(error) => {
                    debug!(profile = %profile.display(), %error, "Codex control socket refused");
                }
            }
        }
    }
}

/// Read one profile's shared server until no record watches it or the
/// connection ends; then clear the stops it reported.
pub(crate) async fn watch(daemon: Arc<Daemon>, profile: PathBuf, every: std::time::Duration) {
    let mut waits = Waits::new();
    if let Err(error) = observe(&daemon, &profile, every, &mut waits).await {
        debug!(profile = %profile.display(), %error, "Codex control watch ended");
    }
    daemon.clear_codex_waits(&mut waits);
}

async fn observe(
    daemon: &Daemon,
    profile: &Path,
    every: std::time::Duration,
    waits: &mut Waits,
) -> std::io::Result<()> {
    let Some(mut session) = codex_control::Session::open(profile).await? else {
        return Ok(());
    };
    let mut ticker = tokio::time::interval(every);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let outcome = loop {
        tokio::select! {
            _ = ticker.tick() => {
                let watched = daemon.codex_watched(profile);
                if watched.is_empty() {
                    break Ok(());
                }
                match statuses(&mut session, watched).await {
                    Ok(statuses) => {
                        for (id, thread, status) in statuses {
                            daemon.observe_codex_thread(&id, profile, &thread, &status, waits);
                        }
                    }
                    Err(error) => break Err(error),
                }
            }
            change = session.next_change() => {
                let change = match change {
                    Ok(change) => change,
                    Err(error) => break Err(error),
                };
                // The announcement says when; a fresh read says what, so an
                // announcement that came before an answer already applied
                // never undoes it.
                let watched: Vec<_> = daemon
                    .codex_watched(profile)
                    .into_iter()
                    .filter(|(_, thread)| *thread == change.thread)
                    .collect();
                if !watched.is_empty() {
                    match status(&mut session, &change.thread).await {
                        Ok(Some(status)) => {
                            for (id, thread) in watched {
                                daemon.observe_codex_thread(&id, profile, &thread, &status, waits);
                            }
                        }
                        Ok(None) => {}
                        Err(error) => break Err(error),
                    }
                }
            }
        }
    };
    session.close().await;
    outcome
}

/// Each watched record's conversation status: read when the server has it
/// loaded, not loaded otherwise; one with nothing to say this round is left
/// out.
async fn statuses(
    session: &mut codex_control::Session,
    watched: Vec<(AgentId, String)>,
) -> std::io::Result<Vec<(AgentId, String, ThreadStatus)>> {
    let loaded: HashSet<String> = session.loaded().await?.into_iter().collect();
    let mut statuses = Vec::with_capacity(watched.len());
    for (id, thread) in watched {
        let status = if loaded.contains(&thread) {
            match status(session, &thread).await? {
                Some(status) => status,
                None => continue,
            }
        } else {
            ThreadStatus::NotLoaded
        };
        statuses.push((id, thread, status));
    }
    Ok(statuses)
}

/// One conversation's status from a fresh read. The server's word that it
/// is not loaded (it unloaded between the list and the read) is that status.
/// Any other answer about it alone (a refusal, a status this build does not
/// know, a child conversation, which is never a record's own) says nothing
/// this round, so one conversation never ends the profile's watch. Only a
/// failed connection is an error.
async fn status(
    session: &mut codex_control::Session,
    thread: &str,
) -> std::io::Result<Option<ThreadStatus>> {
    match session.read(thread).await {
        Ok(read) if read.parent.is_some() => Ok(None),
        Ok(read) => Ok(Some(read.status)),
        Err(error) if codex_control::not_loaded(&error, thread) => {
            Ok(Some(ThreadStatus::NotLoaded))
        }
        Err(error) if codex_control::answered(&error) => {
            debug!(thread, %error, "Codex conversation skipped this round");
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

impl Daemon {
    /// The profiles live Codex records name.
    fn codex_profiles(&self) -> BTreeSet<PathBuf> {
        let state = lock(&self.state);
        state
            .registry
            .live()
            .filter_map(watched)
            .map(|(profile, _)| profile)
            .collect()
    }

    /// The live Codex records keyed by `profile`, with their threads.
    fn codex_watched(&self, profile: &Path) -> Vec<(AgentId, String)> {
        let state = lock(&self.state);
        state
            .registry
            .live()
            .filter_map(|record| {
                let (theirs, thread) = watched(record)?;
                (theirs == profile).then(|| (record.id.clone(), thread))
            })
            .collect()
    }

    /// Record `status` as the record's activity when it says something new,
    /// or renew it before it lapses. A conversation that is no longer loaded
    /// ends a stop for the person this watch reported.
    pub(super) fn observe_codex_thread(
        &self,
        id: &AgentId,
        profile: &Path,
        thread: &str,
        status: &ThreadStatus,
        waits: &mut Waits,
    ) {
        lock(&self.state).observe_codex_thread(id, profile, thread, status, waits, Utc::now());
    }

    fn clear_codex_waits(&self, waits: &mut Waits) {
        let now = Utc::now();
        let mut state = lock(&self.state);
        let ids: Vec<AgentId> = waits.keys().cloned().collect();
        for id in ids {
            state.clear_codex_wait(&id, waits, now);
        }
    }
}

impl State {
    /// Record what Codex's shared server says of `id`'s conversation
    /// `thread` in `profile` as the daemon's own observation, while the
    /// record is still watched there and the word is news
    /// ([`says_already`]). It is not the agent's contact, and while it is
    /// current it stands over contact and keeps herdr's word off the record.
    /// A status that says nothing (not loaded, failed) ends the stop for the
    /// person this watch reported. Whether an observation was recorded.
    pub(super) fn observe_codex_thread(
        &mut self,
        id: &AgentId,
        profile: &Path,
        thread: &str,
        status: &ThreadStatus,
        waits: &mut Waits,
        now: DateTime<Utc>,
    ) -> bool {
        if self.fenced() {
            return false;
        }
        let Some(record) = self.registry.get(id) else {
            waits.remove(id);
            return false;
        };
        if watched(record)
            .as_ref()
            .map(|(p, t)| (p.as_path(), t.as_str()))
            != Some((profile, thread))
        {
            waits.remove(id);
            return false;
        }
        let Some((activity, reason)) = status.activity() else {
            self.clear_codex_wait(id, waits, now);
            return false;
        };
        if record.reported_activity.as_ref().is_some_and(|previous| {
            previous.observed_at >= now || says_already(previous, activity, reason, now)
        }) {
            return false;
        }
        let observation = ActivityObservation {
            activity,
            observed_at: now,
            reason,
            detail: None,
            source: SOURCE,
        };
        if !matches!(self.record_activity(id, observation, now), Response::Ok) {
            return false;
        }
        if activity == ReportedActivity::Waiting {
            waits.insert(id.clone(), now);
        } else {
            waits.remove(id);
        }
        true
    }

    /// End the stop for the person this watch reported for `id`, as the
    /// server's idle, while the record still holds that very observation:
    /// the server's wait, made when `waits` says. Once anything else has
    /// said what the agent is doing — a wait its hooks or herdr reported
    /// included — there is nothing of this watch's to end.
    pub(super) fn clear_codex_wait(&mut self, id: &AgentId, waits: &mut Waits, now: DateTime<Utc>) {
        let Some(at) = waits.remove(id) else {
            return;
        };
        if self.fenced() {
            return;
        }
        let ours = self.registry.get(id).is_some_and(|record| {
            record.status.is_live()
                && record
                    .reported_activity
                    .as_ref()
                    .is_some_and(|observation| {
                        observation.activity == ReportedActivity::Waiting
                            && observation.source == SOURCE
                            && observation.observed_at == at
                    })
        });
        if ours {
            let after = now.max(at + chrono::Duration::milliseconds(1));
            let _ = self.record_activity(
                id,
                ActivityObservation {
                    activity: ReportedActivity::Idle,
                    observed_at: after,
                    reason: None,
                    detail: None,
                    source: SOURCE,
                },
                after,
            );
        }
    }
}

/// Whether `previous` already says what the server says, so recording it
/// again is no news: the same activity and reason, not yet due for renewal,
/// and the server's own word — or a running turn the agent's own adapter
/// reported, which contact reads the same way, so a Codex whose hooks report
/// every tool run is not restated after each one. Anything else is recorded
/// as the server's: herdr's reading, so the server's word outranks it, and
/// the adapter's idle or wait, so contact no longer turns it into working.
fn says_already(
    previous: &ActivityObservation,
    activity: ReportedActivity,
    reason: Option<WaitReason>,
    now: DateTime<Utc>,
) -> bool {
    previous.activity == activity
        && previous.reason == reason
        && now - previous.observed_at < RENEW
        && match previous.source {
            Some(ActivitySource::CodexServer) => true,
            None => activity == ReportedActivity::Working,
            Some(ActivitySource::Herdr | ActivitySource::Other) => false,
        }
}

#[cfg(test)]
mod sources;

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use agentdocker_core::{AdapterKind, Request, WaitReason};
    use futures_util::{SinkExt, StreamExt};
    use serde_json::{Value, json};
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Mutex;
    use tokio::net::UnixListener;
    use tokio::sync::mpsc;
    use tokio_tungstenite::tungstenite::Message;

    /// A short root: a socket path may have only 104 bytes on macOS.
    fn root() -> tempfile::TempDir {
        let base = if cfg!(target_os = "macos") {
            PathBuf::from("/tmp")
        } else {
            std::env::temp_dir()
        };
        tempfile::Builder::new()
            .prefix("ct-")
            .tempdir_in(base)
            .unwrap()
    }

    /// What the fake shared server says: the loaded threads and each one's
    /// status, changed by the test as it goes.
    #[derive(Default)]
    struct Hosted {
        loaded: Vec<String>,
        status: HashMap<String, Value>,
        /// `thread/read` of these threads is refused with this error.
        refuse: HashMap<String, Value>,
        /// Every method asked, in order.
        asked: Vec<String>,
    }

    async fn serve(
        listener: UnixListener,
        home: PathBuf,
        hosted: Arc<Mutex<Hosted>>,
        mut announce: mpsc::Receiver<Value>,
    ) {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        loop {
            tokio::select! {
                note = announce.recv() => {
                    let Some(note) = note else { break };
                    if socket.send(Message::text(note.to_string())).await.is_err() {
                        break;
                    }
                }
                frame = socket.next() => {
                    let Some(Ok(Message::Text(text))) = frame else { break };
                    let request: Value = serde_json::from_str(&text).unwrap();
                    let Some(id) = request.get("id").cloned() else { continue };
                    let method = request["method"].as_str().unwrap().to_owned();
                    let params = &request["params"];
                    let reply = {
                        let mut hosted = hosted.lock().unwrap();
                        hosted.asked.push(method.clone());
                        let result = match method.as_str() {
                            "initialize" => json!({"codexHome": home,
                                "userAgent": "codex_cli_rs/0.163.0 (test)",
                                "platformFamily": "unix", "platformOs": "test"}),
                            "thread/loaded/list" => json!({"data": hosted.loaded, "nextCursor": null}),
                            "thread/read" => {
                                let thread = params["threadId"].as_str().unwrap();
                                if let Some(error) = hosted.refuse.get(thread) {
                                    json!({"error": error})
                                } else {
                                    json!({"thread": {"id": thread, "cwd": "/checkout",
                                        "parentThreadId": null,
                                        "status": hosted.status.get(thread).cloned()
                                            .unwrap_or(json!({"type": "notLoaded"}))}})
                                }
                            }
                            other => panic!("a read-only watch asked {other}"),
                        };
                        match result.get("error") {
                            Some(error) => json!({"id": id, "error": error}),
                            None => json!({"id": id, "result": result}),
                        }
                    };
                    if socket.send(Message::text(reply.to_string())).await.is_err() {
                        break;
                    }
                }
            }
        }
    }

    fn activity(daemon: &Daemon, id: &AgentId) -> Option<(ReportedActivity, Option<WaitReason>)> {
        lock(&daemon.state)
            .registry
            .get(id)
            .and_then(|r| r.reported_activity.as_ref())
            .map(|o| (o.activity, o.reason))
    }

    async fn until(daemon: &Daemon, id: &AgentId, want: (ReportedActivity, Option<WaitReason>)) {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        while activity(daemon, id) != Some(want) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "wanted {want:?}, the record says {:?}",
                activity(daemon, id)
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    const THREAD: &str = "0199a6f1-0000-7000-8000-00000000000a";

    /// A daemon with one Codex conversation keyed by a profile whose shared
    /// server is the fake, one keyed by another profile, and the fake itself.
    struct Fixture {
        _root: tempfile::TempDir,
        daemon: Arc<Daemon>,
        profile: PathBuf,
        checkout: PathBuf,
        agent: AgentId,
        other: AgentId,
        hosted: Arc<Mutex<Hosted>>,
        announce: mpsc::Sender<Value>,
        server: tokio::task::JoinHandle<()>,
    }

    async fn fixture(status: Value) -> Fixture {
        let root = root();
        std::fs::create_dir_all(root.path().join("home")).unwrap();
        let daemon = Arc::new(
            Daemon::open(root.path().join("home"), root.path().join("home/sock")).unwrap(),
        );
        let checkout = root.path().join("checkout");
        std::fs::create_dir_all(&checkout).unwrap();
        let profile = root.path().join("profile");
        let control = profile.join(codex_control::DIRECTORY);
        std::fs::create_dir_all(&control).unwrap();
        std::fs::set_permissions(&control, std::fs::Permissions::from_mode(0o700)).unwrap();
        let profile = profile.canonicalize().unwrap();
        // A profile with no shared server at all.
        let elsewhere = root.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let attach = |profile: &Path, thread: &str| Request::SessionAttach {
            runtime: "codex".into(),
            profile: profile.display().to_string(),
            session: thread.into(),
            workdir: checkout.clone(),
            adapter: AdapterKind::Hooks,
        };
        let Response::Agent { agent } = daemon.handle(attach(&profile, THREAD)).await else {
            panic!("the conversation's keyed record");
        };
        let Response::Agent { agent: other } = daemon
            .handle(attach(
                &elsewhere.canonicalize().unwrap(),
                "0199a6f1-0000-7000-8000-00000000000b",
            ))
            .await
        else {
            panic!("another profile's record");
        };
        let hosted = Arc::new(Mutex::new(Hosted {
            loaded: vec![THREAD.into()],
            status: [(THREAD.to_owned(), status)].into(),
            ..Hosted::default()
        }));
        let (announce, announced) = mpsc::channel(8);
        let listener = UnixListener::bind(codex_control::socket_path(&profile)).unwrap();
        let server = tokio::spawn(serve(listener, profile.clone(), hosted.clone(), announced));
        Fixture {
            _root: root,
            daemon,
            profile,
            checkout,
            agent: agent.id,
            other: other.id,
            hosted,
            announce,
            server,
        }
    }

    impl Fixture {
        /// Another conversation of the same profile, loaded with `status`.
        async fn another(&self, thread: &str, status: Value) -> AgentId {
            let Response::Agent { agent } = self
                .daemon
                .handle(Request::SessionAttach {
                    runtime: "codex".into(),
                    profile: self.profile.display().to_string(),
                    session: thread.into(),
                    workdir: self.checkout.clone(),
                    adapter: AdapterKind::Hooks,
                })
                .await
            else {
                panic!("another conversation's keyed record");
            };
            let mut hosted = self.hosted.lock().unwrap();
            hosted.loaded.push(thread.into());
            hosted.status.insert(thread.into(), status);
            agent.id
        }

        fn status(&self, status: Value) {
            self.hosted
                .lock()
                .unwrap()
                .status
                .insert(THREAD.into(), status);
        }

        /// Only reads were asked of the shared server.
        fn only_reads(&self) {
            let asked = self.hosted.lock().unwrap().asked.clone();
            assert!(
                asked.iter().all(|m| matches!(
                    m.as_str(),
                    "initialize" | "thread/loaded/list" | "thread/read"
                )),
                "only reads: {asked:?}"
            );
        }
    }

    fn approval() -> Value {
        json!({"type": "active", "activeFlags": ["waitingOnApproval"]})
    }

    fn question() -> Value {
        json!({"type": "active", "activeFlags": ["waitingOnUserInput"]})
    }

    /// A conversation the shared server hosts feeds its keyed record's
    /// standing: an approval and a question wait on the person, a running
    /// turn is working, a quiet conversation is idle, and one no longer
    /// loaded ends the stop this watch reported. Another profile's record,
    /// whose profile has no shared server, is never touched.
    #[tokio::test]
    async fn a_hosted_conversations_status_becomes_its_records_activity() {
        let f = fixture(approval()).await;
        let watching = tokio::spawn(run(f.daemon.clone(), std::time::Duration::from_millis(50)));
        let (daemon, id) = (&f.daemon, &f.agent);
        until(
            daemon,
            id,
            (ReportedActivity::Waiting, Some(WaitReason::Permission)),
        )
        .await;
        {
            let state = lock(&daemon.state);
            let record = state.registry.get(id).unwrap();
            assert!(
                matches!(
                    state.activity_of(record, Utc::now()),
                    agentdocker_core::Activity::Waiting {
                        reason: Some(WaitReason::Permission),
                        ..
                    }
                ),
                "the record waits on the person"
            );
        }
        f.status(question());
        until(
            daemon,
            id,
            (ReportedActivity::Waiting, Some(WaitReason::Question)),
        )
        .await;
        f.status(json!({"type": "active", "activeFlags": []}));
        until(daemon, id, (ReportedActivity::Working, None)).await;
        f.status(json!({"type": "idle"}));
        until(daemon, id, (ReportedActivity::Idle, None)).await;
        // A stop for the person ends with the conversation's unloading.
        f.status(approval());
        until(
            daemon,
            id,
            (ReportedActivity::Waiting, Some(WaitReason::Permission)),
        )
        .await;
        f.hosted.lock().unwrap().loaded.clear();
        until(daemon, id, (ReportedActivity::Idle, None)).await;
        assert_eq!(
            activity(daemon, &f.other),
            None,
            "another profile is untouched"
        );
        f.only_reads();
        watching.abort();
        f.server.abort();
    }

    /// Between reads, the server's own `thread/status/changed` moves the
    /// record at once, by reading the conversation again; a change for a
    /// conversation no record watches reads nothing. When the connection
    /// ends, the stop it reported ends too.
    #[tokio::test]
    async fn the_servers_announcement_moves_the_record_between_reads() {
        let f = fixture(approval()).await;
        // One read at the start, then none for an hour: only the
        // announcement can say anything new.
        let watching = tokio::spawn(watch(
            f.daemon.clone(),
            f.profile.clone(),
            std::time::Duration::from_secs(3600),
        ));
        let (daemon, id) = (&f.daemon, &f.agent);
        until(
            daemon,
            id,
            (ReportedActivity::Waiting, Some(WaitReason::Permission)),
        )
        .await;
        let reads = || {
            f.hosted
                .lock()
                .unwrap()
                .asked
                .iter()
                .filter(|m| *m == "thread/read")
                .count()
        };
        let before = reads();
        f.announce
            .send(json!({"method": "thread/status/changed", "params": {
                "threadId": "0199a6f1-0000-7000-8000-0000000000ff", "status": {"type": "idle"}}}))
            .await
            .unwrap();
        f.status(question());
        f.announce
            .send(json!({"method": "thread/status/changed",
                "params": {"threadId": THREAD, "status": question()}}))
            .await
            .unwrap();
        until(
            daemon,
            id,
            (ReportedActivity::Waiting, Some(WaitReason::Question)),
        )
        .await;
        assert_eq!(
            reads(),
            before + 1,
            "one read, for the watched conversation"
        );
        // The shared server goes away: the watch ends and takes back the
        // stop it reported, since nobody is waiting in a gone server.
        f.server.abort();
        tokio::time::timeout(std::time::Duration::from_secs(10), watching)
            .await
            .expect("the watch ends with its connection")
            .unwrap();
        until(daemon, id, (ReportedActivity::Idle, None)).await;
        f.only_reads();
    }

    /// One conversation's read refused does not end the profile's watch: the
    /// server's "thread not loaded" for it is that status (its stop for the
    /// person ends), any other refusal skips it for the round, and another
    /// conversation waiting on the person keeps waiting and keeps being read.
    #[tokio::test]
    async fn a_refused_read_of_one_conversation_leaves_the_others_standing() {
        let f = fixture(approval()).await;
        let second = "0199a6f1-0000-7000-8000-00000000000c";
        let waiting = f.another(second, question()).await;
        let watching = tokio::spawn(run(f.daemon.clone(), std::time::Duration::from_millis(50)));
        let daemon = &f.daemon;
        until(
            daemon,
            &f.agent,
            (ReportedActivity::Waiting, Some(WaitReason::Permission)),
        )
        .await;
        until(
            daemon,
            &waiting,
            (ReportedActivity::Waiting, Some(WaitReason::Question)),
        )
        .await;
        // The first unloads between the list and the read, as Codex answers it.
        f.hosted.lock().unwrap().refuse.insert(
            THREAD.into(),
            json!({"code": -32600, "message": format!("thread not loaded: {THREAD}")}),
        );
        until(daemon, &f.agent, (ReportedActivity::Idle, None)).await;
        assert_eq!(
            activity(daemon, &waiting),
            Some((ReportedActivity::Waiting, Some(WaitReason::Question))),
            "the other conversation still waits on the person"
        );
        // Any other refusal says nothing about it this round, and the watch
        // goes on reading the other.
        f.status(approval());
        f.hosted.lock().unwrap().refuse.insert(
            THREAD.into(),
            json!({"code": -32603, "message": "failed to read thread"}),
        );
        let reads = || f.hosted.lock().unwrap().asked.len();
        let before = reads();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        while reads() < before + 12 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the watch stopped reading"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(
            activity(daemon, &f.agent),
            Some((ReportedActivity::Idle, None))
        );
        assert_eq!(
            activity(daemon, &waiting),
            Some((ReportedActivity::Waiting, Some(WaitReason::Question)))
        );
        f.hosted
            .lock()
            .unwrap()
            .status
            .insert(second.into(), json!({"type": "idle"}));
        until(daemon, &waiting, (ReportedActivity::Idle, None)).await;
        f.only_reads();
        watching.abort();
        f.server.abort();
    }
}
