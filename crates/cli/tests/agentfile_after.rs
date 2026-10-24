//! `after = "A exits 0"` on a real daemon: `agentdocker up` starts what
//! waits once the entry it names has exited with status zero, and never
//! starts it, nor anything waiting on it, when that entry exits otherwise,
//! is stopped, or fails to start — and an exit 0 counts only when `up` saw
//! its own event delivered, so one seen without the event stream (none
//! could be opened, or the one there was ended) does not. Every command and
//! endpoint belongs to the fixture; no installed daemon, provider account or
//! user config is used.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

struct RunningDaemon {
    child: Child,
    socket: PathBuf,
}

impl RunningDaemon {
    fn start(home: &Path, socket: &Path, extra_env: &[(&str, &str)]) -> Self {
        let log =
            agentdocker_host::dirs::private_file(&home.with_extension("daemon.log"), true, true)
                .unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_agentdocker"))
            .args(["daemon", "run"])
            // The harness acts as the person from wherever it runs, an
            // agent's session included.
            .env("AGENTDOCKER_PERSON_CHECK", "off")
            .arg("--home")
            .arg(home)
            .arg("--socket")
            .arg(socket)
            .env("AGENTDOCKER_HOME", home)
            .env("AGENTDOCKER_SOCKET", socket)
            .env("AGENTDOCKER_NO_AUTOSTART", "1")
            .env_remove("AGENTDOCKER_TOKEN_FILE")
            .env_remove("AGENTDOCKER_AGENT_ID")
            .env_remove("AGENTDOCKER_EXPERIMENTAL_RELOAD")
            .env_remove("AGENTDOCKER_RELOAD_CANDIDATE")
            .env("RUST_LOG", "warn")
            .envs(extra_env.iter().copied())
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log))
            .spawn()
            .unwrap();
        let mut running = Self {
            child,
            socket: socket.to_path_buf(),
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(
                running.child.try_wait().unwrap().is_none(),
                "daemon exited at startup"
            );
            if rpc(socket, json!({"op":"ping"})).is_ok_and(|response| response["type"] == "pong") {
                return running;
            }
            assert!(Instant::now() < deadline, "daemon did not become ready");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for RunningDaemon {
    fn drop(&mut self) {
        // Whoever serves the socket now — this child, or the successor a
        // reload handed it to — is told to shut down, and is waited for.
        let _ = rpc(&self.socket, json!({"op":"shutdown"}));
        let deadline = Instant::now() + Duration::from_secs(10);
        while (self.child.try_wait().ok().flatten().is_none()
            || rpc(&self.socket, json!({"op":"ping"})).is_ok())
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn rpc(socket: &Path, request: Value) -> std::io::Result<Value> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    writeln!(stream, "{request}")?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    serde_json::from_str(&line).map_err(std::io::Error::other)
}

/// A private home, socket and project directory under /tmp, short enough
/// for a Unix socket path, with a daemon serving them.
struct Fixture {
    // Fields drop in order: the daemon is shut down through its socket
    // before the directory holding it is removed.
    daemon: RunningDaemon,
    home: PathBuf,
    socket: PathBuf,
    work: PathBuf,
    _tmp: tempfile::TempDir,
}

impl Fixture {
    fn new(agentfile: &str) -> Self {
        Self::with_daemon_env(agentfile, &[])
    }

    fn with_daemon_env(agentfile: &str, extra_env: &[(&str, &str)]) -> Self {
        let tmp = tempfile::Builder::new()
            .prefix("ad-after-")
            .tempdir_in("/tmp")
            .unwrap();
        let home = tmp.path().join("home");
        let socket = tmp.path().join("d.sock");
        let work = tmp.path().join("work");
        std::fs::create_dir(&work).unwrap();
        std::fs::write(work.join("Agentfile.toml"), agentfile).unwrap();
        let daemon = RunningDaemon::start(&home, &socket, extra_env);
        Self {
            daemon,
            home,
            socket,
            work,
            _tmp: tmp,
        }
    }

    /// `agentdocker up` against this daemon, with nothing inherited from
    /// the surrounding session's routing or credentials.
    fn up(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_agentdocker"));
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("AGENTDOCKER_") {
                command.env_remove(key);
            }
        }
        command
            .args(["up", "-f"])
            .arg(self.work.join("Agentfile.toml"))
            .current_dir(&self.work)
            .env("AGENTDOCKER_HOME", &self.home)
            .env("AGENTDOCKER_SOCKET", &self.socket)
            .env("AGENTDOCKER_NO_AUTOSTART", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    /// Every agent on record, by name.
    fn records(&self) -> Vec<Value> {
        let response = rpc(&self.daemon.socket, json!({"op":"list","all":true})).unwrap();
        assert_eq!(response["type"], "agents", "{response}");
        response["agents"].as_array().unwrap().clone()
    }

    fn record(&self, name: &str) -> Option<Value> {
        self.records()
            .into_iter()
            .find(|agent| agent["spec"]["name"] == name)
    }
}

/// `up` for just these entries, run to the end.
fn up_only(fixture: &Fixture, names: &[&str]) -> (bool, String) {
    let mut command = fixture.up();
    command.args(names);
    let output = finish(command.spawn().unwrap(), Duration::from_secs(30));
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Wait until the daemon has `name` on record with this status.
fn wait_status(fixture: &Fixture, name: &str, status: Value) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while fixture.record(name).map(|agent| agent["status"].clone()) != Some(status.clone()) {
        assert!(
            Instant::now() < deadline,
            "{name} never reached {status}: {:?}",
            fixture.record(name)
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Wait for a child with a deadline, so a regression that hangs `up` fails
/// the test instead of the run.
fn finish(mut child: Child, within: Duration) -> Output {
    let deadline = Instant::now() + within;
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!(
                "up did not finish within {within:?}; stderr:\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_end(&mut stdout)
        .unwrap();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_end(&mut stderr)
        .unwrap();
    Output {
        status: child.wait().unwrap(),
        stdout,
        stderr,
    }
}

/// The line `up` printed for an entry with this state, if it did.
fn said<'a>(stderr: &'a str, name: &str, state: &str) -> Option<&'a str> {
    stderr.lines().find(|line| {
        line.split_whitespace().next() == Some(name)
            && line[name.len()..].trim_start().starts_with(state)
    })
}

#[test]
fn success_starts_what_waits_and_a_failure_stops_it_and_everything_after() {
    let fixture = Fixture::new(
        r#"
[agents.build]
command = ["true"]

[agents.test]
command = ["sh", "-c", "touch test-ran"]
after = "build exits 0"

[agents.broken]
command = ["false"]

[agents.deploy]
command = ["sh", "-c", "touch deploy-ran"]
after = ["test exits 0", "broken exits 0"]

[agents.announce]
command = ["sh", "-c", "touch announce-ran"]
after = "deploy exits 0"

[agents.missing]
command = ["/nonexistent/agentfile-after-missing"]

[agents.cleanup]
command = ["sh", "-c", "touch cleanup-ran"]
after = "missing exits 0"
"#,
    );
    let output = finish(fixture.up().spawn().unwrap(), Duration::from_secs(60));
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();

    assert!(!output.status.success(), "what was not started fails up");
    // `missing`, which could not be started, and the three behind failures.
    assert!(stderr.contains("4 agent(s) not started"), "{stderr}");

    // `test` waited, then ran once `build` exited 0.
    assert!(
        said(&stderr, "test", "waiting: build exits 0").is_some(),
        "{stderr}"
    );
    assert!(said(&stderr, "build", "exited 0").is_some(), "{stderr}");
    assert!(said(&stderr, "test", "started").is_some(), "{stderr}");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !fixture.work.join("test-ran").exists() {
        assert!(Instant::now() < deadline, "test never ran: {stderr}");
        std::thread::sleep(Duration::from_millis(20));
    }

    // `broken` exited 1: `deploy` is never started, even though `test`,
    // its other condition, may not have finished yet, and neither is
    // `announce`, which waits on `deploy`.
    assert!(said(&stderr, "broken", "exited 1").is_some(), "{stderr}");
    assert_eq!(
        said(&stderr, "deploy", "not started:"),
        Some(format!("{:<24} not started: broken exited 1", "deploy").as_str()),
        "{stderr}"
    );
    assert_eq!(
        said(&stderr, "announce", "not started:"),
        Some(
            format!(
                "{:<24} not started: deploy was not started: broken exited 1",
                "announce"
            )
            .as_str()
        ),
        "{stderr}"
    );

    // `missing` could not be started at all, so `cleanup` is not either.
    let cleanup = said(&stderr, "cleanup", "not started:").unwrap_or_default();
    assert!(
        cleanup.contains("not started: missing failed"),
        "{cleanup}: {stderr}"
    );

    // What was never started has no record and did nothing.
    for name in ["deploy", "announce", "cleanup"] {
        assert!(fixture.record(name).is_none(), "{name} was started");
        assert!(
            !fixture.work.join(format!("{name}-ran")).exists(),
            "{name} ran"
        );
    }
    // What did start is on stdout, one id a line.
    let started: Vec<&str> = stdout.lines().collect();
    for name in ["build", "test", "broken"] {
        let record = fixture.record(name).unwrap();
        assert!(
            started.contains(&record["id"].as_str().unwrap()),
            "{name}: {stdout}"
        );
    }
    assert_eq!(
        fixture.record("build").unwrap()["status"],
        json!({"state":"exited","code":0})
    );
    assert_eq!(
        fixture.record("broken").unwrap()["status"],
        json!({"state":"exited","code":1})
    );
}

#[test]
fn a_stop_stops_what_waits_even_when_the_stopped_agent_exits_0() {
    let fixture = Fixture::new(
        r#"
[agents.serve]
command = ["sh", "-c", "trap 'exit 0' TERM; touch serving; while :; do sleep 0.05; done"]

[agents.report]
command = ["sh", "-c", "touch report-ran"]
after = "serve exits 0"
"#,
    );
    let child = fixture.up().spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while !fixture.work.join("serving").exists() {
        assert!(Instant::now() < deadline, "serve never started");
        std::thread::sleep(Duration::from_millis(20));
    }
    let stopped = rpc(
        &fixture.socket,
        json!({"op":"stop","agent":"serve","force":false}),
    )
    .unwrap();
    assert_eq!(stopped["type"], "agent", "{stopped}");

    let output = finish(child, Duration::from_secs(30));
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(!output.status.success(), "{stderr}");
    assert!(
        said(&stderr, "report", "waiting: serve exits 0").is_some(),
        "{stderr}"
    );
    assert_eq!(
        said(&stderr, "report", "not started:"),
        Some(format!("{:<24} not started: serve was stopped", "report").as_str()),
        "{stderr}"
    );
    // It trapped the stop and exited 0: the exit status alone would have
    // read as success.
    assert_eq!(
        fixture.record("serve").unwrap()["status"],
        json!({"state":"exited","code":0})
    );
    assert!(fixture.record("report").is_none());
    assert!(!fixture.work.join("report-ran").exists());
}

/// What `up` prints for an entry whose dependency ended before it ran.
const BEFORE_THIS_RUN: &str = "ended before this run; its exit cannot be shown to be its own";

#[test]
fn a_dependency_stopped_before_this_run_does_not_let_what_waits_start() {
    let fixture = Fixture::new(
        r#"
[agents.serve]
command = ["sh", "-c", "trap 'exit 0' TERM; touch serving; while :; do sleep 0.05; done"]

[agents.report]
command = ["sh", "-c", "touch report-ran"]
after = "serve exits 0"
"#,
    );
    let (ok, stderr) = up_only(&fixture, &["serve"]);
    assert!(ok, "{stderr}");
    let deadline = Instant::now() + Duration::from_secs(20);
    while !fixture.work.join("serving").exists() {
        assert!(Instant::now() < deadline, "serve never started");
        std::thread::sleep(Duration::from_millis(20));
    }
    let stopped = rpc(
        &fixture.socket,
        json!({"op":"stop","agent":"serve","force":false}),
    )
    .unwrap();
    assert_eq!(stopped["type"], "agent", "{stopped}");
    // It traps the stop and exits 0, so its record reads like success.
    wait_status(&fixture, "serve", json!({"state":"exited","code":0}));

    let (ok, stderr) = up_only(&fixture, &["report"]);
    assert!(!ok, "{stderr}");
    assert_eq!(
        said(&stderr, "report", "not started:"),
        Some(format!("{:<24} not started: serve {BEFORE_THIS_RUN}", "report").as_str()),
        "{stderr}"
    );
    assert!(fixture.record("report").is_none(), "report was started");
    assert!(!fixture.work.join("report-ran").exists());
}

#[test]
fn an_earlier_exit_0_on_record_cannot_be_told_from_a_stop_so_it_fails_closed() {
    let fixture = Fixture::new(
        r#"
[agents.build]
command = ["true"]

[agents.test]
command = ["sh", "-c", "touch test-ran"]
after = "build exits 0"
"#,
    );
    let (ok, stderr) = up_only(&fixture, &["build"]);
    assert!(ok, "{stderr}");
    wait_status(&fixture, "build", json!({"state":"exited","code":0}));

    // `build` exited 0 by itself, but its record would read the same had
    // it been stopped and exited 0, so `up test` cannot take it as success.
    let (ok, stderr) = up_only(&fixture, &["test"]);
    assert!(!ok, "{stderr}");
    assert_eq!(
        said(&stderr, "test", "not started:"),
        Some(format!("{:<24} not started: build {BEFORE_THIS_RUN}", "test").as_str()),
        "{stderr}"
    );
    assert!(fixture.record("test").is_none(), "test was started");

    // Brought up together, the exit is watched, and `test` runs.
    let (ok, stderr) = up_only(&fixture, &[]);
    assert!(ok, "{stderr}");
    assert!(said(&stderr, "test", "started").is_some(), "{stderr}");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !fixture.work.join("test-ran").exists() {
        assert!(Instant::now() < deadline, "test never ran: {stderr}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn an_agentfile_whose_wait_could_never_end_starts_nothing() {
    let fixture = Fixture::new(
        r#"
[agents.a]
command = ["sh", "-c", "touch a-ran"]
after = "b exits 0"

[agents.b]
command = ["sh", "-c", "touch b-ran"]
after = "a exits 0"
"#,
    );
    let output = finish(fixture.up().spawn().unwrap(), Duration::from_secs(30));
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(!output.status.success());
    assert!(stderr.contains("is not a valid Agentfile"), "{stderr}");
    assert!(stderr.contains("cycle: a → b → a"), "{stderr}");
    assert!(fixture.records().is_empty(), "nothing was started");
}

/// What a running `up` has printed on stderr so far, read as it comes.
struct Followed {
    text: Arc<Mutex<String>>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl Followed {
    fn new(stderr: std::process::ChildStderr) -> Self {
        let text = Arc::new(Mutex::new(String::new()));
        let reader = std::thread::spawn({
            let text = text.clone();
            move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    let mut text = text.lock().unwrap();
                    text.push_str(&line);
                    text.push('\n');
                }
            }
        })
        .into();
        Self { text, reader }
    }

    fn text(&self) -> String {
        self.text.lock().unwrap().clone()
    }

    fn wait_for(&self, what: &str, within: Duration) {
        let deadline = Instant::now() + within;
        while !self.text().contains(what) {
            assert!(
                Instant::now() < deadline,
                "up never said {what:?}: {}",
                self.text()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Everything, once the process has ended and closed its stderr.
    fn finish(mut self) -> String {
        if let Some(reader) = self.reader.take() {
            reader.join().unwrap();
        }
        self.text()
    }
}

/// Wait for a child whose stderr is being followed.
fn wait_exit(child: &mut Child, within: Duration) -> std::process::ExitStatus {
    let deadline = Instant::now() + within;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            panic!("up did not finish within {within:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// What `up` prints for an entry whose dependency exited 0 while there was
/// no event stream to show the exit was its own.
const WITHOUT_EVENTS: &str =
    "exited 0 without its events being seen; the exit cannot be shown to be its own";

#[test]
fn an_exit_0_after_the_event_stream_ended_does_not_let_what_waits_start() {
    // A reload hands the daemon to a successor, which ends every event
    // stream without a word: `up` is left looking at records, where a stop
    // that a trapping agent turns into exit 0 is invisible.
    let fixture = Fixture::with_daemon_env(
        r#"
[agents.serve]
command = ["sh", "-c", "trap 'exit 0' TERM; touch serving; while :; do sleep 0.05; done"]

[agents.report]
command = ["sh", "-c", "touch report-ran"]
after = "serve exits 0"
"#,
        &[("AGENTDOCKER_EXPERIMENTAL_RELOAD", "1")],
    );
    let mut child = fixture.up().spawn().unwrap();
    let followed = Followed::new(child.stderr.take().unwrap());
    followed.wait_for("waiting: serve exits 0", Duration::from_secs(20));
    let deadline = Instant::now() + Duration::from_secs(20);
    while !fixture.work.join("serving").exists() {
        assert!(Instant::now() < deadline, "serve never started");
        std::thread::sleep(Duration::from_millis(20));
    }

    let reloaded = rpc(&fixture.socket, json!({"op":"reload"})).unwrap();
    assert_eq!(reloaded["type"], "ok", "{reloaded}");
    followed.wait_for("event stream ended", Duration::from_secs(20));
    // The agent survived the handover, and the successor stops it.
    assert_eq!(
        fixture.record("serve").unwrap()["status"]["state"],
        "running"
    );
    let stopped = rpc(
        &fixture.socket,
        json!({"op":"stop","agent":"serve","force":false}),
    )
    .unwrap();
    assert_eq!(stopped["type"], "agent", "{stopped}");
    wait_status(&fixture, "serve", json!({"state":"exited","code":0}));

    let status = wait_exit(&mut child, Duration::from_secs(30));
    drop(child);
    let stderr = followed.finish();
    assert!(!status.success(), "{stderr}");
    // Its record read exited 0. A once-a-second look that happens to land
    // in the moment it was `stopping` sees the stop instead; either way
    // the exit 0 does not count.
    let report = said(&stderr, "report", "not started:").unwrap_or_default();
    assert!(
        report.ends_with(&format!("not started: serve {WITHOUT_EVENTS}"))
            || report.ends_with("not started: serve was stopped"),
        "{stderr}"
    );
    assert!(fixture.record("report").is_none(), "report was started");
    assert!(!fixture.work.join("report-ran").exists());
}

/// A stand-in daemon whose answers the test scripts. Its event
/// subscription is either held open without a word (`ready: false`, a
/// subscription that never becomes ready) or acknowledged and then fed
/// whatever events the test pushes, in order. `list` answers nothing,
/// `run` a running agent of the spec it was sent (kept as the agent), and
/// `inspect` the agent as the test last set it. Runs are kept by name.
struct Scripted {
    stop: Arc<AtomicBool>,
    runs: Arc<Mutex<Vec<String>>>,
    inspects: Arc<std::sync::atomic::AtomicUsize>,
    agent: Arc<Mutex<Option<agentdocker_core::AgentRecord>>>,
    events: std::sync::mpsc::Sender<agentdocker_core::EventKind>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Scripted {
    fn serve(socket: &Path, ready: bool) -> Self {
        use agentdocker_core::{AgentRecord, AgentSpec, AgentStatus, Event, Response};
        let listener = UnixListener::bind(socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let runs = Arc::new(Mutex::new(Vec::new()));
        let inspects = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let agent: Arc<Mutex<Option<AgentRecord>>> = Arc::new(Mutex::new(None));
        let (events, pushed) = std::sync::mpsc::channel::<agentdocker_core::EventKind>();
        let worker = std::thread::spawn({
            let (stop, runs, inspects, agent) =
                (stop.clone(), runs.clone(), inspects.clone(), agent.clone());
            move || {
                let mut held = Vec::new();
                let mut subscriber: Option<UnixStream> = None;
                let line =
                    |response: &Response| format!("{}\n", serde_json::to_string(response).unwrap());
                while !stop.load(Ordering::Relaxed) {
                    while let Ok(kind) = pushed.try_recv() {
                        let frame = line(&Response::Event {
                            event: Event::new(kind, chrono::Utc::now()),
                        });
                        let subscriber = subscriber.as_mut().expect("pushed before subscribing");
                        subscriber.write_all(frame.as_bytes()).unwrap();
                    }
                    let stream = match listener.accept() {
                        Ok((stream, _)) => stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(5));
                            continue;
                        }
                        Err(error) => panic!("accept failed: {error}"),
                    };
                    stream.set_nonblocking(false).unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut request = String::new();
                    reader.read_line(&mut request).unwrap();
                    let request: Value = serde_json::from_str(&request).unwrap();
                    let mut stream = stream;
                    let answer = match request["op"].as_str().unwrap() {
                        "events" if ready => {
                            stream
                                .write_all(line(&Response::EventsReady).as_bytes())
                                .unwrap();
                            subscriber = Some(stream);
                            continue;
                        }
                        "events" | "resume_events" => {
                            held.push(stream);
                            continue;
                        }
                        "list" => json!({"type":"agents","agents":[]}),
                        "run" => {
                            let spec: AgentSpec =
                                serde_json::from_value(request["spec"].clone()).unwrap();
                            runs.lock().unwrap().push(spec.name.clone());
                            let mut record = AgentRecord::new(spec, true, chrono::Utc::now());
                            record.status = AgentStatus::Running;
                            *agent.lock().unwrap() = Some(record.clone());
                            json!({"type":"agent","agent":record})
                        }
                        "inspect" => {
                            inspects.fetch_add(1, Ordering::Relaxed);
                            let record = agent.lock().unwrap().clone().expect("inspect after run");
                            json!({"type":"agent","agent":record})
                        }
                        other => json!({"type":"error","code":"invalid","message":other}),
                    };
                    stream.write_all(format!("{answer}\n").as_bytes()).unwrap();
                }
            }
        });
        Self {
            stop,
            runs,
            inspects,
            agent,
            events,
            worker: Some(worker),
        }
    }

    /// Change the agent `inspect` answers with.
    fn set(&self, change: impl FnOnce(&mut agentdocker_core::AgentRecord)) {
        change(self.agent.lock().unwrap().as_mut().expect("run first"));
    }

    fn id(&self) -> agentdocker_core::AgentId {
        self.agent
            .lock()
            .unwrap()
            .as_ref()
            .expect("run first")
            .id
            .clone()
    }

    fn push(&self, kind: agentdocker_core::EventKind) {
        self.events.send(kind).unwrap();
    }

    fn runs(&self) -> Vec<String> {
        self.runs.lock().unwrap().clone()
    }

    fn wait_until(&self, what: &str, done: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !done(self) {
            assert!(Instant::now() < deadline, "never {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn finish(mut self) -> Vec<String> {
        self.stop.store(true, Ordering::Relaxed);
        self.worker
            .take()
            .unwrap()
            .join()
            .expect("the fake daemon served");
        self.runs()
    }
}

/// `up serve report` against a scripted daemon at `socket`.
fn up_scripted(tmp: &Path, socket: &Path) -> Child {
    let work = tmp.join("work");
    std::fs::create_dir(&work).unwrap();
    std::fs::write(
        work.join("Agentfile.toml"),
        r#"
[agents.serve]
command = ["sh", "-c", "serve"]
restart = "on-failure:3"

[agents.report]
command = ["sh", "-c", "report"]
after = "serve exits 0"
"#,
    )
    .unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_agentdocker"));
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("AGENTDOCKER_") {
            command.env_remove(key);
        }
    }
    command
        .args(["up", "-f"])
        .arg(work.join("Agentfile.toml"))
        .current_dir(&work)
        .env("AGENTDOCKER_HOME", tmp.join("home"))
        .env("AGENTDOCKER_SOCKET", socket)
        .env("AGENTDOCKER_NO_AUTOSTART", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn scratch() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("ad-after-")
        .tempdir_in("/tmp")
        .unwrap()
}

#[test]
fn an_exit_0_seen_after_the_subscription_timed_out_does_not_let_what_waits_start() {
    let tmp = scratch();
    let socket = tmp.path().join("d.sock");
    let daemon = Scripted::serve(&socket, false);
    let child = up_scripted(tmp.path(), &socket);
    daemon.wait_until("ran serve", |d| d.runs() == ["serve"]);
    // A stop and its exit 0 between two record reads: all a read can show
    // is the exit 0.
    daemon.set(|agent| agent.status = agentdocker_core::AgentStatus::Exited { code: Some(0) });
    let output = finish(child, Duration::from_secs(30));
    let runs = daemon.finish();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(!output.status.success(), "{stderr}");
    assert!(
        stderr.contains("no event stream from the daemon"),
        "{stderr}"
    );
    assert_eq!(runs, ["serve"], "report was never asked to run: {stderr}");
    assert_eq!(
        said(&stderr, "report", "not started:"),
        Some(format!("{:<24} not started: serve {WITHOUT_EVENTS}", "report").as_str()),
        "{stderr}"
    );
}

#[test]
fn a_record_read_ahead_of_the_events_cannot_let_what_waits_start() {
    use agentdocker_core::{AgentStatus, EventKind};
    let tmp = scratch();
    let socket = tmp.path().join("d.sock");
    let daemon = Scripted::serve(&socket, true);
    let child = up_scripted(tmp.path(), &socket);
    daemon.wait_until("ran serve", |d| d.runs() == ["serve"]);
    let id = daemon.id();

    // `serve` (on-failure) exits 1. By the time `up` reads its record it
    // has been restarted, stopped, trapped the stop and exited 0 — and
    // those events have not reached `up` yet.
    daemon.set(|agent| {
        agent.status = AgentStatus::Exited { code: Some(0) };
        agent.restarts = 1;
        agent.spec.restart = agentdocker_core::RestartPolicy::No;
    });
    let read = daemon.inspects.load(Ordering::Relaxed);
    daemon.push(EventKind::AgentExited {
        agent: id.clone(),
        status: AgentStatus::Exited { code: Some(1) },
    });
    daemon.wait_until("read serve's record", |d| {
        d.inspects.load(Ordering::Relaxed) > read
    });
    // Give a wrong decision time to show itself as a run of `report`.
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        daemon.runs(),
        ["serve"],
        "report started on a read-ahead exit 0"
    );

    for kind in [
        EventKind::AgentRestarted {
            agent: id.clone(),
            pid: None,
            attempt: 1,
        },
        EventKind::AgentRestartCleared { agent: id.clone() },
        EventKind::AgentStopping {
            agent: id.clone(),
            force: false,
        },
        EventKind::AgentExited {
            agent: id.clone(),
            status: AgentStatus::Exited { code: Some(0) },
        },
    ] {
        daemon.push(kind);
    }
    let output = finish(child, Duration::from_secs(30));
    let runs = daemon.finish();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(!output.status.success(), "{stderr}");
    assert_eq!(runs, ["serve"], "{stderr}");
    assert_eq!(
        said(&stderr, "report", "not started:"),
        Some(format!("{:<24} not started: serve was stopped", "report").as_str()),
        "{stderr}"
    );
}
