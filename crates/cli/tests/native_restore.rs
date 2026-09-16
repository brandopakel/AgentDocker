//! Real daemon/CLI restart acceptance. Every command and endpoint belongs to
//! the fixture; no installed daemon, provider account or user config is used.
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

struct RunningDaemon {
    child: Child,
    socket: PathBuf,
}

impl RunningDaemon {
    fn start(home: &Path, socket: &Path) -> Self {
        Self::start_with(home, socket, &[])
    }

    fn start_with(home: &Path, socket: &Path, extra_env: &[(&str, &str)]) -> Self {
        let log =
            agentdocker_host::dirs::private_file(&home.with_extension("daemon.log"), true, true)
                .unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_agentd"))
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

    fn stop(&mut self) {
        let _ = rpc(&self.socket, json!({"op":"shutdown"}));
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                self.child.kill().unwrap();
                self.child.wait().unwrap();
                panic!("fixture daemon failed to shut down cleanly");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for RunningDaemon {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = rpc(&self.socket, json!({"op":"shutdown"}));
            let deadline = Instant::now() + Duration::from_secs(10);
            while self.child.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn rpc(socket: &Path, request: Value) -> std::io::Result<Value> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    writeln!(stream, "{request}")?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    serde_json::from_str(&line).map_err(std::io::Error::other)
}

#[test]
fn build_info_reports_compiled_contract_without_opening_state() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("absent-state");
    let socket = tmp.path().join("absent.sock");
    // The bounded host runner kills a regression that accidentally starts a daemon.
    let output = agentdocker_host::command::run(
        tmp.path(),
        &[
            env!("CARGO_BIN_EXE_agentd").to_owned(),
            "--build-info".into(),
            "--home".into(),
            home.display().to_string(),
            "--socket".into(),
            socket.display().to_string(),
        ],
        Duration::from_secs(5),
    )
    .unwrap();
    assert!(output.success, "{}", output.text);
    let value: Value = serde_json::from_str(&output.text).unwrap();
    assert_eq!(value["format"], 1);
    assert_eq!(value["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(value["os"], std::env::consts::OS);
    assert_eq!(value["arch"], std::env::consts::ARCH);
    assert!(
        value["state_schema"]
            .as_u64()
            .is_some_and(|schema| (1..=u32::MAX as u64).contains(&schema))
    );
    assert!(!home.exists());
    assert!(!socket.exists());
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
}

/// One batch and one terminal agent, each waiting for a go-file before it
/// prints again and records that it survived. Returns name, id, pid, log.
type Fixture = (&'static str, String, Value, PathBuf);

fn start_batch_and_terminal(home: &Path, socket: &Path, work: &Path) -> Vec<Fixture> {
    start_batch_and_terminal_then(home, socket, work, |_| "exec sleep 30".to_owned())
}

/// The same fixtures, each ending with `tail(name)` once released.
fn start_batch_and_terminal_then(
    home: &Path,
    socket: &Path,
    work: &Path,
    tail: impl Fn(&str) -> String,
) -> Vec<Fixture> {
    let mut agents = Vec::new();
    for tty in [false, true] {
        let name = if tty { "terminal" } else { "batch" };
        let script = format!(
            "printf '{name}-before\\n'; while ! test -f {name}-go; do sleep 0.05; done; \
             printf '{name}-after\\n'; printf survived > {name}-survived; {}",
            tail(name)
        );
        let response = rpc(
            socket,
            json!({"op":"run", "spec": {
                "name":name, "workdir":work, "tty":tty, "command":["sh", "-c", script]
            }}),
        )
        .unwrap();
        assert_eq!(response["type"], "agent", "{response}");
        let id = response["agent"]["id"].as_str().unwrap().to_owned();
        let log = home.join("logs").join(format!("{id}.log"));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !std::fs::read_to_string(&log)
            .is_ok_and(|text| text.contains(&format!("{name}-before")))
        {
            assert!(Instant::now() < deadline, "fixture never produced output");
            std::thread::sleep(Duration::from_millis(20));
        }
        agents.push((name, id, response["agent"]["pid"].clone(), log));
    }
    agents
}

/// Let each fixture proceed and prove it still runs, still logs through
/// the daemon at `socket`, and is still the same process.
fn release_and_check_survivors(socket: &Path, work: &Path, agents: &[Fixture]) {
    for (name, id, pid, log) in agents {
        std::fs::write(work.join(format!("{name}-go")), b"").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !work.join(format!("{name}-survived")).exists()
            || !std::fs::read_to_string(log)
                .is_ok_and(|text| text.contains(&format!("{name}-after")))
        {
            assert!(
                Instant::now() < deadline,
                "{name} lost execution or logging"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let inspected = rpc(socket, json!({"op":"inspect", "agent":id})).unwrap();
        assert_eq!(&inspected["agent"]["pid"], pid);
        assert_eq!(inspected["agent"]["status"]["state"], "running");
    }
}

#[test]
fn refused_reload_preserves_real_batch_and_terminal_processes_and_logs() {
    let tmp = tempfile::Builder::new()
        .prefix("ad-reload-")
        .tempdir_in("/tmp")
        .unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let home = root.join("state");
    let socket = root.join("host.sock");
    let work = root.join("work");
    std::fs::create_dir(&work).unwrap();
    let mut daemon = RunningDaemon::start(&home, &socket);
    let agents = start_batch_and_terminal(&home, &socket, &work);
    let response = rpc(&socket, json!({"op":"reload"})).unwrap();
    assert_eq!(response["type"], "error", "{response}");
    assert_eq!(response["code"], "unavailable", "{response}");
    assert!(daemon.child.try_wait().unwrap().is_none());
    let cli = Command::new(env!("CARGO_BIN_EXE_agentdocker"))
        .args(["daemon", "reload"])
        .env("AGENTDOCKER_HOME", &home)
        .env("AGENTDOCKER_SOCKET", &socket)
        .env("AGENTDOCKER_NO_AUTOSTART", "0")
        .env_remove("AGENTDOCKER_TOKEN_FILE")
        .output()
        .unwrap();
    assert!(!cli.status.success());
    assert!(String::from_utf8_lossy(&cli.stderr).contains("reload is unavailable"));
    assert!(cli.stdout.is_empty());
    release_and_check_survivors(&socket, &work, &agents);
    daemon.stop();
    // The gate opens for exactly `1`: `0` and an empty value refuse too.
    for value in ["0", ""] {
        let gated_home = root.join(format!("gated{}", value.len()));
        let gated_socket = root.join(format!("gated{}.sock", value.len()));
        let mut gated = RunningDaemon::start_with(
            &gated_home,
            &gated_socket,
            &[("AGENTDOCKER_EXPERIMENTAL_RELOAD", value)],
        );
        let response = rpc(&gated_socket, json!({"op":"reload"})).unwrap();
        assert_eq!(response["type"], "error", "value {value:?}: {response}");
        assert_eq!(
            response["code"], "unavailable",
            "value {value:?}: {response}"
        );
        assert!(gated.child.try_wait().unwrap().is_none());
        gated.stop();
    }
    let absent_home = root.join("not-created");
    let cli = Command::new(env!("CARGO_BIN_EXE_agentdocker"))
        .args(["daemon", "reload"])
        .env("AGENTDOCKER_HOME", &absent_home)
        .env("AGENTDOCKER_SOCKET", root.join("absent.sock"))
        .env("AGENTDOCKER_NO_AUTOSTART", "0")
        .env_remove("AGENTDOCKER_TOKEN_FILE")
        .output()
        .unwrap();
    assert!(!cli.status.success());
    assert!(!absent_home.exists(), "reload must not autostart a daemon");
}

/// With the gate set, a reload hands the listener, lock and container
/// endpoint to a successor started from the same executable, waits for it
/// to serve, and leaves. Agents keep their processes and their logs; the
/// successor can be reloaded in its turn; and shutting it down ends the
/// chain.
#[test]
fn enabled_reload_hands_real_processes_to_a_successor_and_leaves() {
    let tmp = tempfile::Builder::new()
        .prefix("ad-reload-on-")
        .tempdir_in("/tmp")
        .unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let home = root.join("state");
    let socket = root.join("host.sock");
    let work = root.join("work");
    std::fs::create_dir(&work).unwrap();
    let mut daemon =
        RunningDaemon::start_with(&home, &socket, &[("AGENTDOCKER_EXPERIMENTAL_RELOAD", "1")]);
    let agents = start_batch_and_terminal_then(&home, &socket, &work, |name| {
        format!(
            "while ! test -f {name}-done; do sleep 0.05; done; exit {}",
            if name == "batch" { 7 } else { 3 }
        )
    });
    let container = home.join("container.sock");
    let deadline = Instant::now() + Duration::from_secs(5);
    while UnixStream::connect(&container).is_err() {
        assert!(
            Instant::now() < deadline,
            "container endpoint never came up"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    // A mutation still executing refuses the offer with its own code, so
    // a caller can tell "try again shortly" from "cannot".
    let validator = rpc(
        &socket,
        json!({"op":"register", "spec": {"name":"validator", "workdir":work}, "pid":null, "session":null}),
    )
    .unwrap();
    assert_eq!(validator["type"], "agent", "{validator}");
    let validator_id = validator["agent"]["id"].as_str().unwrap().to_owned();
    let validating = std::thread::spawn({
        let socket = socket.clone();
        move || {
            let mut stream = UnixStream::connect(&socket).unwrap();
            // The command says when it is running, so the reload below is
            // issued while the validation is admitted, not before it.
            let request = json!({"op":"validate", "agent":validator_id,
                "command":["sh","-c","touch validating; sleep 2"], "timeout_secs":30});
            writeln!(stream, "{request}").unwrap();
            let mut line = String::new();
            BufReader::new(stream).read_line(&mut line).unwrap();
            serde_json::from_str::<Value>(&line).unwrap()
        }
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while !work.join("validating").exists() {
        assert!(Instant::now() < deadline, "the validation never started");
        std::thread::sleep(Duration::from_millis(20));
    }
    let busy = rpc(&socket, json!({"op":"reload"})).unwrap();
    assert_eq!(busy["type"], "error", "{busy}");
    assert_eq!(busy["code"], "backpressure", "{busy}");
    assert_eq!(validating.join().unwrap()["type"], "validation");

    // A live event stream opened on the first daemon follows the chain:
    // each replacement ends its stream without a word, and it subscribes
    // again on whichever daemon answers next.
    let mut following = Command::new(env!("CARGO_BIN_EXE_agentdocker"))
        .args(["events"])
        .env("AGENTDOCKER_HOME", &home)
        .env("AGENTDOCKER_SOCKET", &socket)
        .env("AGENTDOCKER_NO_AUTOSTART", "1")
        .env_remove("AGENTDOCKER_TOKEN_FILE")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let followed = Followed::new(following.stdout.take().unwrap());
    // Subscribed before the reload, proven by a marker event it has
    // printed: the offer is then the next thing it sees.
    followed.wait_for_marker(&socket, &work, "marker-1");
    // A checked cursor taken from the first daemon: the log identity and
    // sequence it names must still be the successors' after the switches,
    // so a client that resumes from it replays every event since without
    // a gap.
    let cursor_before = {
        let mut stream = UnixStream::connect(&socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        writeln!(stream, "{}", json!({"op":"resume_events", "after":null})).unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
        let ready: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(ready["type"], "events_ready_at", "{ready}");
        ready["cursor"].clone()
    };

    let response = rpc(&socket, json!({"op":"reload"})).unwrap();
    assert_eq!(response["type"], "ok", "{response}");
    // The predecessor leaves once the successor serves, without stopping
    // anything.
    let deadline = Instant::now() + Duration::from_secs(10);
    while daemon.child.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "the predecessor did not leave");
        std::thread::sleep(Duration::from_millis(20));
    }
    let pong = rpc(&socket, json!({"op":"ping"})).unwrap();
    assert_eq!(pong["type"], "pong", "{pong}");
    release_and_check_survivors(&socket, &work, &agents);
    // Writes resumed under the successor.
    let later = rpc(
        &socket,
        json!({"op":"run", "spec": {
            "name":"later", "workdir":work, "command":["sh", "-c", "exit 0"]
        }}),
    )
    .unwrap();
    assert_eq!(later["type"], "agent", "{later}");

    // The successor holds what it was given and can hand it on in turn,
    // this time through the CLI. The stream has joined the successor
    // first, proven by a marker event, so it sees this handover as well.
    followed.wait_for_marker(&socket, &work, "marker-2");
    let cli = Command::new(env!("CARGO_BIN_EXE_agentdocker"))
        .args(["daemon", "reload"])
        .env("AGENTDOCKER_HOME", &home)
        .env("AGENTDOCKER_SOCKET", &socket)
        .env("AGENTDOCKER_NO_AUTOSTART", "1")
        .env_remove("AGENTDOCKER_TOKEN_FILE")
        .output()
        .unwrap();
    assert!(
        cli.status.success(),
        "{}",
        String::from_utf8_lossy(&cli.stderr)
    );
    let pong = rpc(&socket, json!({"op":"ping"})).unwrap();
    assert_eq!(pong["type"], "pong", "{pong}");
    assert!(
        pong["restricted"] != Value::Null,
        "the successor reports its container endpoint: {pong}"
    );
    for (_, id, pid, _) in &agents {
        let inspected = rpc(&socket, json!({"op":"inspect", "agent":id})).unwrap();
        assert_eq!(&inspected["agent"]["pid"], pid);
        assert_eq!(inspected["agent"]["status"]["state"], "running");
    }
    // The container endpoint travelled with the listener: the third daemon
    // answers on it without having bound it.
    UnixStream::connect(&container).expect("container endpoint inherited across two handovers");

    // Released for good, each fixture ends with its exact exit under the
    // third daemon, its log complete from before the first reload to the
    // end, since the session owner never changed hands.
    for (name, id, _, log) in &agents {
        std::fs::write(work.join(format!("{name}-done")), b"").unwrap();
        let expected = if *name == "batch" { 7 } else { 3 };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let inspected = rpc(&socket, json!({"op":"inspect", "agent":id})).unwrap();
            if inspected["agent"]["status"]["state"] == "exited" {
                assert_eq!(
                    inspected["agent"]["status"]["code"], expected,
                    "{inspected}"
                );
                break;
            }
            assert!(
                Instant::now() < deadline,
                "{name} did not exit: {inspected}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let text = std::fs::read_to_string(log).unwrap();
        assert!(
            text.contains(&format!("{name}-before")) && text.contains(&format!("{name}-after")),
            "{name} log incomplete: {text:?}"
        );
    }

    // Resumed from the first daemon's cursor on the third: the same log,
    // every sequence number since in order, both handovers among them.
    {
        let mut stream = UnixStream::connect(&socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        writeln!(
            stream,
            "{}",
            json!({"op":"resume_events", "after":cursor_before})
        )
        .unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let ready: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(ready["type"], "events_ready_at", "{ready}");
        assert_eq!(
            ready["cursor"], cursor_before,
            "the cursor is the daemon's too"
        );
        let mut expected_seq = cursor_before["seq"].as_u64().unwrap() + 1;
        let mut kinds = Vec::new();
        loop {
            line.clear();
            assert!(
                reader.read_line(&mut line).unwrap() > 0,
                "replay ended early"
            );
            let frame: Value = serde_json::from_str(&line).unwrap();
            match frame["type"].as_str().unwrap() {
                "event_at" => {
                    assert_eq!(frame["cursor"]["log"], cursor_before["log"], "{frame}");
                    assert_eq!(frame["cursor"]["seq"], expected_seq, "contiguous: {frame}");
                    expected_seq += 1;
                    kinds.push(frame["event"]["kind"]["event"].as_str().unwrap().to_owned());
                }
                "events_caught_up" => break,
                other => panic!("unexpected {other} in checked replay: {frame}"),
            }
        }
        let count = |kind: &str| kinds.iter().filter(|k| k.as_str() == kind).count();
        assert!(count("daemon_transfer_offered") >= 2, "{kinds:?}");
        assert!(count("daemon_transfer_accepted") >= 2, "{kinds:?}");
    }

    // The third daemon is nobody's child here; stop it over the socket and
    // wait for it to take the socket path down with it.
    let response = rpc(&socket, json!({"op":"shutdown"})).unwrap();
    assert_eq!(response["type"], "ok", "{response}");
    let deadline = Instant::now() + Duration::from_secs(10);
    while socket.exists() {
        assert!(Instant::now() < deadline, "the successor did not stop");
        std::thread::sleep(Duration::from_millis(20));
    }
    // With the last daemon gone, the stream ends for good. It saw both
    // offers and said twice that it had moved on.
    let status = following.wait().unwrap();
    let mut stderr = String::new();
    following
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    // The reader has seen EOF once the process is gone; join it so the
    // snapshot below is everything the stream printed.
    let stdout = followed.finish();
    assert!(status.success(), "{stderr}");
    assert_eq!(
        stderr.matches("was replaced").count(),
        2,
        "stderr: {stderr}\nstdout: {stdout}"
    );
    assert!(
        stdout.matches("transfer offered").count() >= 2,
        "the stream saw each daemon's offer: {stdout}"
    );
}

/// The stdout of a following `agentdocker events`, read as it arrives, so
/// a test can wait for what the stream has actually printed.
struct Followed {
    text: std::sync::Arc<std::sync::Mutex<String>>,
    reader: std::thread::JoinHandle<()>,
}

impl Followed {
    fn new(stdout: std::process::ChildStdout) -> Self {
        let text = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let reader = std::thread::spawn({
            let text = text.clone();
            move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    text.lock().unwrap().push_str(&line);
                    text.lock().unwrap().push('\n');
                }
            }
        });
        Self { text, reader }
    }

    fn text(&self) -> String {
        self.text.lock().unwrap().clone()
    }

    /// Everything the stream printed, once it has closed.
    fn finish(self) -> String {
        let Self { text, reader } = self;
        reader.join().unwrap();
        let printed = text.lock().unwrap();
        printed.clone()
    }

    /// Register a throwaway agent named `marker` and wait until the
    /// stream has printed its start (by the agent's short id, which is
    /// what the line carries): from then on the stream is known to be
    /// live on whichever daemon answers the socket.
    ///
    /// A marker emitted before the stream subscribed is never seen (the
    /// stream is live, not replayed), so markers are registered again
    /// until one is.
    fn wait_for_marker(&self, socket: &Path, work: &Path, marker: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        for attempt in 1.. {
            let name = format!("{marker}-{attempt}");
            let registered = rpc(
                socket,
                json!({"op":"register", "spec": {"name":name, "workdir":work}, "pid":null, "session":null}),
            )
            .unwrap();
            assert_eq!(registered["type"], "agent", "{registered}");
            let id = registered["agent"]["id"].as_str().unwrap().to_owned();
            let short = &id[..12.min(id.len())];
            let seen_by = Instant::now() + Duration::from_millis(300);
            while Instant::now() < seen_by {
                if self.text().contains(short) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            assert!(
                Instant::now() < deadline,
                "the stream never printed a {marker}: {}",
                self.text()
            );
        }
    }
}

#[test]
fn restored_first_instruction_can_coordinate_and_its_first_edit_is_observed() {
    let tmp = tempfile::Builder::new()
        .prefix("ad-restore-")
        .tempdir_in("/tmp")
        .unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let home = root.join("state");
    let socket = root.join("host.sock");
    let work = root.join("work");
    std::fs::create_dir(&work).unwrap();
    std::fs::write(work.join("Agentfile.toml"), "").unwrap();
    std::fs::write(work.join("first-edit"), "initial").unwrap();
    let mut daemon = RunningDaemon::start(&home, &socket);
    let response = rpc(&socket, json!({"op":"run", "spec": {
        "name":"first-instruction", "workdir":work, "restore":true,
        "command":["sh", "-c",
          "\"$1\" inspect \"$AGENTDOCKER_AGENT_ID\" >/dev/null || exit 81; printf coordinated > first-edit; exec sleep 30",
          "fixture", env!("CARGO_BIN_EXE_agentdocker")]
    }})).unwrap();
    assert_eq!(response["type"], "agent", "{response}");
    let id = response["agent"]["id"].as_str().unwrap();
    let old_pid = response["agent"]["pid"].as_u64().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while std::fs::read_to_string(work.join("first-edit")).unwrap() != "coordinated" {
        if Instant::now() >= deadline {
            let record = rpc(&socket, json!({"op":"inspect", "agent":id}));
            let log = std::fs::read_to_string(home.join("logs").join(format!("{id}.log")));
            let retained = tmp.keep();
            panic!(
                "initial fixture did not coordinate; record={record:?}; log={log:?}; retained={retained:?}"
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let claimed = rpc(
        &socket,
        json!({"op":"claim", "agent":id,
        "resource": format!("path:{}", work.join("first-edit").display()), "ttl_secs":300}),
    )
    .unwrap();
    assert_eq!(claimed["type"], "lease", "{claimed}");
    // Wait for the initial write to reach the ledger, so an old/debounced
    // observation cannot satisfy the restoration assertion after restart.
    let deadline = Instant::now() + Duration::from_secs(5);
    let before_restart = loop {
        let changes = rpc(&socket, json!({"op":"changes", "project":work})).unwrap();
        let entries = changes["changes"].as_array().unwrap();
        if entries.iter().any(|entry| {
            entry["path"]
                .as_str()
                .is_some_and(|path| path.ends_with("first-edit"))
        }) {
            break entries
                .iter()
                .filter_map(|entry| entry["seq"].as_u64())
                .max()
                .unwrap();
        }
        assert!(
            Instant::now() < deadline,
            "initial edit did not reach the ledger: {changes}"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    daemon.stop();
    std::fs::write(work.join("first-edit"), "while-down").unwrap();
    // Legacy modes are migrated when the same installation is reopened.
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::set_permissions(
        home.join("state.db"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    daemon = RunningDaemon::start(&home, &socket);
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let changes = rpc(
            &socket,
            json!({"op":"changes", "project":work, "agent":id, "since_seq":before_restart}),
        )
        .unwrap();
        let observed = changes["changes"].as_array().is_some_and(|entries| {
            entries.iter().any(|entry| {
                entry["seq"]
                    .as_u64()
                    .is_some_and(|seq| seq > before_restart)
                    && entry["path"]
                        .as_str()
                        .is_some_and(|p| p.ends_with("first-edit"))
            })
        });
        let content = std::fs::read_to_string(work.join("first-edit")).unwrap();
        if observed && content == "coordinated" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "restored first edit missing: {changes}; content={content}"
        );
        std::thread::sleep(Duration::from_millis(30));
    }
    let restored = rpc(&socket, json!({"op":"inspect", "agent":id})).unwrap();
    assert_ne!(restored["agent"]["pid"].as_u64(), Some(old_pid));
    for directory in [&home, &home.join("logs")] {
        assert_eq!(std::fs::metadata(directory).unwrap().mode() & 0o777, 0o700);
    }
    for file in [
        home.join("state.db"),
        home.join("state.db-wal"),
        home.join("state.db-shm"),
        home.join("logs").join(format!("{id}.log")),
    ] {
        assert_eq!(
            std::fs::metadata(&file).unwrap().mode() & 0o777,
            0o600,
            "{}",
            file.display()
        );
    }
    daemon.stop();
}

#[test]
fn service_install_preview_does_not_create_state_or_write_service_files() {
    let tmp = tempfile::tempdir_in("/tmp").unwrap();
    let home = tmp.path().join("not-created");
    let result = Command::new(env!("CARGO_BIN_EXE_agentdocker"))
        .args(["daemon", "install", "--dry-run"])
        .env("AGENTDOCKER_HOME", &home)
        .env("AGENTDOCKER_SOCKET", tmp.path().join("host.sock"))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        !home.exists(),
        "a preview must leave the state directory absent"
    );
}

#[test]
fn native_launch_and_restore_use_the_owning_daemon_context() {
    let temporary = tempfile::Builder::new()
        .prefix("ad-context-")
        .tempdir_in("/tmp")
        .unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let home = root.join("owning-state");
    let socket = root.join("owning.sock");
    let work = root.join("work");
    std::fs::create_dir(&work).unwrap();
    let mut daemon = RunningDaemon::start(&home, &socket);
    let response = rpc(&socket, json!({"op":"run", "spec": {
        "name":"context-fixture", "workdir":work, "restore":true,
        "env": {"AGENTDOCKER_HOME":root.join("unrelated-state"),
            "AGENTDOCKER_SOCKET":root.join("unrelated.sock"),
            "AGENTDOCKER_TOKEN_FILE":root.join("unrelated-token"),
            "AGENTDOCKER_NO_AUTOSTART":"0"},
        "command":["sh", "-c",
            "test \"$AGENTDOCKER_HOME\" = \"$2\" || exit 71; test \"$AGENTDOCKER_SOCKET\" = \"$3\" || exit 72; test \"${AGENTDOCKER_TOKEN_FILE+x}\" != x || exit 73; test \"$AGENTDOCKER_NO_AUTOSTART\" = 1 || exit 74; \"$1\" inspect \"$AGENTDOCKER_AGENT_ID\" >/dev/null || exit 75; printf passed > context; exec sleep 30",
            "fixture", env!("CARGO_BIN_EXE_agentdocker"),home,socket]
    }})).unwrap();
    assert_eq!(response["type"], "agent", "{response}");
    let id = response["agent"]["id"].as_str().unwrap();
    let first_pid = response["agent"]["pid"].as_u64().unwrap();
    for restored in [false, true] {
        if restored {
            daemon.stop();
            std::fs::remove_file(work.join("context")).unwrap();
            daemon = RunningDaemon::start(&home, &socket);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while !std::fs::read_to_string(work.join("context")).is_ok_and(|value| value == "passed") {
            assert!(
                Instant::now() < deadline,
                "native context failed (restored={restored}): {}",
                rpc(&socket, json!({"op":"inspect", "agent":id})).unwrap()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        if restored {
            assert_ne!(
                rpc(&socket, json!({"op":"inspect", "agent":id})).unwrap()["agent"]["pid"].as_u64(),
                Some(first_pid)
            );
        }
    }
    assert!(!root.join("unrelated-state").exists());
    assert!(!root.join("unrelated.sock").exists());
    daemon.stop();
}
