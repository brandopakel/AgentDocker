//! The managed Codex bridge, end to end, against a mock app-server and a
//! fixture daemon: a queued message becomes a turn, the receipt is proved
//! from the provider's own user item, the message is acknowledged, and the
//! loop keeps serving. No installed daemon, provider account or user
//! configuration is used.
//!
//! This binary is its own mock: launched as `<self> app-server --stdio`
//! with `AGENTDOCKER_TEST_APP_SERVER_TRANSCRIPT` set, it speaks the subset of
//! the Codex app-server protocol the bridge uses over stdio and writes every
//! frame it receives to the transcript, so `harness = false` keeps the
//! test runner's own output off that stream. Launched by the native
//! launcher with `AGENTDOCKER_TEST_NATIVE_CODEX` set, it is both of that
//! launcher's Codex children instead: the dedicated WebSocket app-server and
//! the full-screen terminal program.
#[cfg(unix)]
use serde_json::{Value, json};
#[cfg(unix)]
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::time::{Duration, Instant};

const TRANSCRIPT: &str = "AGENTDOCKER_TEST_APP_SERVER_TRANSCRIPT";
/// The version the mock claims in `initialize`; 0.160.0 unless set.
const VERSION: &str = "AGENTDOCKER_TEST_APP_SERVER_VERSION";
/// Set on a native launch: this binary is then that launch's Codex server
/// or terminal program. Its value is the fixture's directory.
#[cfg(unix)]
const NATIVE: &str = "AGENTDOCKER_TEST_NATIVE_CODEX";

fn main() {
    #[cfg(unix)]
    if std::env::var_os(NATIVE).is_some() {
        let arguments: Vec<String> = std::env::args().skip(1).collect();
        if arguments.iter().any(|argument| argument == "app-server") {
            std::process::exit(native::serve(&arguments));
        }
        if arguments.iter().any(|argument| argument == "--remote") {
            std::process::exit(native::terminal(&arguments));
        }
    }
    if std::env::args().nth(1).as_deref() == Some("app-server")
        && std::env::var_os(TRANSCRIPT).is_some()
    {
        std::process::exit(mock::serve());
    }
    if std::env::args().nth(1).as_deref() == Some("idle-codex") {
        // The existing Codex session of the receiver test: alive and idle,
        // its executable this binary, until its stdin closes.
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        return;
    }
    let tests: &[(&str, fn())] = &[
        (
            "a_queued_message_becomes_a_turn_with_a_receipt_proved_from_the_providers_item",
            a_queued_message_becomes_a_turn_with_a_receipt_proved_from_the_providers_item,
        ),
        (
            "a_refused_turn_leaves_the_message_queued_and_reports_the_provider",
            a_refused_turn_leaves_the_message_queued_and_reports_the_provider,
        ),
        (
            "an_app_server_older_than_the_floor_is_refused_before_any_thread_is_touched",
            an_app_server_older_than_the_floor_is_refused_before_any_thread_is_touched,
        ),
        (
            "an_existing_codex_session_is_fed_through_its_native_queue_with_a_proved_receipt",
            an_existing_codex_session_is_fed_through_its_native_queue_with_a_proved_receipt,
        ),
        (
            "a_shared_server_conversation_is_fed_through_its_control_socket_once",
            a_shared_server_conversation_is_fed_through_its_control_socket_once,
        ),
        (
            "a_message_during_a_running_turn_is_steered_into_it_once",
            a_message_during_a_running_turn_is_steered_into_it_once,
        ),
        (
            "a_steer_refused_for_a_changed_turn_is_delivered_once_by_the_native_queue",
            a_steer_refused_for_a_changed_turn_is_delivered_once_by_the_native_queue,
        ),
        (
            "a_turn_waiting_on_approval_takes_no_steer",
            a_turn_waiting_on_approval_takes_no_steer,
        ),
        (
            "a_steer_whose_answer_is_lost_is_reconciled_without_a_resend",
            a_steer_whose_answer_is_lost_is_reconciled_without_a_resend,
        ),
        (
            "the_shared_server_listing_never_starts_a_daemon",
            the_shared_server_listing_never_starts_a_daemon,
        ),
        (
            "the_native_launcher_writes_nothing_over_the_codex_screen",
            the_native_launcher_writes_nothing_over_the_codex_screen,
        ),
    ];
    // The libtest command line that `cargo test` and nextest speak:
    // `--list --format terse` wants one `name: test` line per test and
    // nothing else; `--exact <name> --nocapture` runs that test alone;
    // `--ignored` selects ignored tests, of which there are none.
    let mut arguments = std::env::args().skip(1);
    let (mut list, mut exact, mut ignored_only) = (false, false, false);
    let mut filters = Vec::new();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--list" => list = true,
            "--exact" => exact = true,
            "--ignored" => ignored_only = true,
            "--format" | "--test-threads" | "--skip" | "--logfile" | "-Z" | "--color" => {
                arguments.next();
            }
            other if other.starts_with('-') => {}
            other => filters.push(other.to_owned()),
        }
    }
    let selected: Vec<&(&str, fn())> = tests
        .iter()
        .filter(|(name, _)| {
            filters.is_empty()
                || filters.iter().any(|wanted| {
                    if exact {
                        name == wanted
                    } else {
                        name.contains(wanted.as_str())
                    }
                })
        })
        .collect();
    if list {
        if !ignored_only {
            for (name, _) in &selected {
                println!("{name}: test");
            }
        }
        return;
    }
    if ignored_only {
        println!("\nrunning 0 tests\n\ntest result: ok. 0 passed; 0 failed");
        return;
    }
    println!("\nrunning {} tests", selected.len());
    let (mut passed, mut failed) = (0, 0);
    for (name, test) in selected {
        print!("test {name} ... ");
        match std::panic::catch_unwind(test) {
            Ok(()) => {
                println!("ok");
                passed += 1;
            }
            Err(_) => {
                println!("FAILED");
                failed += 1;
            }
        }
    }
    println!(
        "\ntest result: {}. {passed} passed; {failed} failed",
        if failed == 0 { "ok" } else { "FAILED" }
    );
    if failed > 0 {
        std::process::exit(1);
    }
}

/// The app-server both loops talk to: enough of the protocol to start or
/// read a thread, take a turn or a native queue entry, say which user item
/// carried the input, and end the turn. A turn whose text asks for a
/// refusal is refused with the provider's structured usage-limit error.
mod mock {
    use super::TRANSCRIPT;
    use serde_json::{Value, json};
    use std::io::{BufRead, Write};

    struct Server {
        cwd: String,
        turns: Vec<String>,
        /// `{turnId, item}` entries in the order they happened.
        items: Vec<Value>,
    }

    impl Server {
        /// A turn the TUI runs for `text`: the user item that proves the
        /// receipt, and the turn's completion.
        fn run_turn(&mut self, text: &str) -> (String, String) {
            let turn = format!("turn-{}", self.turns.len() + 1);
            let item = format!("item-{}", self.turns.len() + 1);
            self.turns.push(turn.clone());
            self.items.push(
                json!({"turnId": turn, "item": {"type": "userMessage", "id": item,
                "content": [{"type": "text", "text": text, "text_elements": []}]}}),
            );
            (turn, item)
        }

        fn answer(&mut self, id: &Value, method: &str, params: &Value) -> Vec<Value> {
            let ok = |result: Value| vec![json!({"id": id, "result": result})];
            let refused = |code: i64, message: String, data: Value| {
                vec![json!({"id": id, "error": {"code": code, "message": message, "data": data}})]
            };
            match method {
                "initialize" => ok(json!({
                    "userAgent": format!("codex_cli_rs/{} (Mock 1.0.0; test) mock-app-server",
                        std::env::var(super::VERSION).unwrap_or_else(|_| "0.160.0".into())),
                    "codexHome": std::env::var("CODEX_HOME").unwrap_or_default(),
                    "platformFamily": "unix", "platformOs": "linux",
                })),
                "config/read" => ok(json!({"config": {"mcp_servers": {}}})),
                "hooks/list" => ok(json!({"data": [{"cwd": params["cwds"][0], "errors": []}]})),
                "thread/start" => ok(json!({"thread": {"id": "thread-1", "cwd": params["cwd"]}})),
                "thread/resume" | "thread/read" => {
                    ok(json!({"thread": {"id": params["threadId"], "cwd": self.cwd}}))
                }
                "thread/items/list" => {
                    let wanted = params["turnId"].as_str().map(str::to_owned);
                    let limit = params["limit"].as_u64().unwrap_or(50) as usize;
                    let data: Vec<Value> = self
                        .items
                        .iter()
                        .rev()
                        .filter(|entry| {
                            wanted.as_deref().is_none_or(|turn| entry["turnId"] == turn)
                        })
                        .take(limit)
                        .cloned()
                        .collect();
                    ok(json!({"data": data}))
                }
                "thread/turns/list" => ok(json!({"data": self.turns.iter().rev()
                    .map(|turn| json!({"id": turn, "status": "completed"})).collect::<Vec<_>>()})),
                // The TUI takes every queue entry as soon as it is added.
                "thread/queue/list" => ok(json!({"data": []})),
                "thread/queue/add" => {
                    let text = params["input"][0]["text"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned();
                    let submission = json!({"id": format!("queued-{}", self.turns.len() + 1),
                        "clientUserMessageId": params["clientUserMessageId"], "input": params["input"]});
                    self.run_turn(&text);
                    ok(json!({"queuedSubmission": submission}))
                }
                "turn/start" => {
                    let text = params["input"][0]["text"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned();
                    let body = serde_json::from_str::<Value>(&text)
                        .ok()
                        .and_then(|envelope| {
                            envelope["agentdocker_message"]["payload"]["text"]
                                .as_str()
                                .map(str::to_owned)
                        })
                        .unwrap_or_default();
                    if body.contains("refuse") {
                        return refused(
                            -32000,
                            "usage limit reached".into(),
                            json!({"codexErrorInfo": "usageLimitExceeded"}),
                        );
                    }
                    let (turn, item) = self.run_turn(&text);
                    let mut frames = ok(json!({"turn": {"id": turn}}));
                    frames.push(json!({"method": "item/started", "params": {
                        "threadId": "thread-1", "turnId": turn,
                        "item": {"type": "userMessage", "id": item,
                            "content": [{"type": "text", "text": text, "text_elements": []}]}}}));
                    frames.push(json!({"method": "turn/completed", "params": {
                        "threadId": "thread-1", "turn": {"id": turn, "status": "completed"}}}));
                    frames
                }
                "turn/steer" => refused(-32600, "no active turn to steer".into(), Value::Null),
                other => refused(
                    -32601,
                    format!("{other} is not supported by the mock"),
                    Value::Null,
                ),
            }
        }
    }

    pub fn serve() -> i32 {
        let transcript = std::env::var_os(TRANSCRIPT).expect("transcript path");
        let mut log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(transcript)
            .expect("transcript file");
        let stdin = std::io::stdin();
        let mut out = std::io::stdout().lock();
        let mut server = Server {
            cwd: std::env::current_dir().expect("cwd").display().to_string(),
            turns: Vec::new(),
            items: Vec::new(),
        };
        for line in stdin.lock().lines() {
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }
            writeln!(log, "{line}").expect("transcript write");
            let value: Value = serde_json::from_str(&line).expect("a JSON frame");
            let Some(id) = value.get("id") else {
                // `initialized` and other notifications need no answer.
                continue;
            };
            let method = value["method"].as_str().unwrap_or_default().to_owned();
            for frame in server.answer(id, &method, &value["params"]) {
                writeln!(out, "{frame}").expect("stdout");
            }
            out.flush().expect("stdout flush");
        }
        0
    }
}

#[cfg(unix)]
mod fixture {
    use serde_json::{Value, json};
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    pub struct RunningDaemon {
        child: Child,
        pub socket: PathBuf,
    }

    impl RunningDaemon {
        pub fn start(home: &Path, socket: &Path) -> Self {
            let log = agentdocker_host::dirs::private_file(
                &home.with_extension("daemon.log"),
                true,
                true,
            )
            .unwrap();
            // The executable is the daemon as `agentdocker daemon run`.
            let child = Command::new(env!("CARGO_BIN_EXE_agentdocker"))
                .args(["daemon", "run"])
                // The harness acts as the person from wherever it runs,
                // an agent's session included.
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
                .env_remove("AGENTDOCKER_AGENT_NAME")
                .env_remove("AGENTDOCKER_CLAUDE_CHANNEL_INPUT")
                .env("RUST_LOG", "warn")
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
                if rpc(socket, json!({"op":"ping"}))
                    .is_ok_and(|response| response["type"] == "pong")
                {
                    return running;
                }
                assert!(Instant::now() < deadline, "daemon did not become ready");
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

    /// A root short enough for the Unix sockets made under it. The
    /// receiver listens at `<home>/codex-queue/<32 hex>/resolve.sock`,
    /// which under macOS's per-user temporary directory (`/var/folders/…`)
    /// runs past the 104 bytes a socket path may have; `/tmp` keeps it
    /// well within, on every Unix.
    pub fn root() -> tempfile::TempDir {
        let base = if cfg!(target_os = "macos") {
            PathBuf::from("/tmp")
        } else {
            std::env::temp_dir()
        };
        tempfile::Builder::new()
            .prefix("codex-")
            .tempdir_in(base)
            .expect("a temporary root")
    }

    pub fn rpc(socket: &Path, request: Value) -> std::io::Result<Value> {
        let mut stream = UnixStream::connect(socket)?;
        stream.set_read_timeout(Some(Duration::from_secs(15)))?;
        stream.set_write_timeout(Some(Duration::from_secs(3)))?;
        writeln!(stream, "{request}")?;
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line)?;
        serde_json::from_str(&line).map_err(std::io::Error::other)
    }

    /// Ask until `done` says so, or fail with the last answer after `within`.
    pub fn wait_for(
        socket: &Path,
        request: Value,
        within: Duration,
        what: &str,
        done: impl Fn(&Value) -> bool,
    ) -> Value {
        let deadline = Instant::now() + within;
        let mut last = Value::Null;
        while Instant::now() < deadline {
            if let Ok(response) = rpc(socket, request.clone()) {
                if done(&response) {
                    return response;
                }
                last = response;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("{what} did not happen within {within:?}; last answer {last}");
    }
}

/// A checkout, a provider home, the mock executable and the launch spec the
/// real `run --codex-input` composes, started under the fixture daemon.
#[cfg(unix)]
struct Bridge {
    daemon: fixture::RunningDaemon,
    _root: tempfile::TempDir,
    transcript: PathBuf,
    agent: String,
}

#[cfg(unix)]
impl Bridge {
    fn start() -> Self {
        let bridge = Self::launch(None);
        bridge.wait_for_agent(
            Duration::from_secs(30),
            "the bridge reporting ready",
            |agent| agent["input_delivery"]["paused"] == false,
        );
        bridge
    }

    /// Launch under the fixture daemon, with the mock claiming `version`
    /// when given, and wait for nothing.
    fn launch(version: Option<&str>) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let root = fixture::root();
        let home = root.path().join("home");
        let socket = root.path().join("agentd.sock");
        let checkout = root.path().join("checkout");
        let codex_home = root.path().join("codex-home");
        std::fs::create_dir_all(&checkout).unwrap();
        std::fs::create_dir_all(&codex_home).unwrap();
        let transcript = root.path().join("app-server.jsonl");
        let mock = root.path().join("codex");
        std::fs::write(
            &mock,
            format!(
                "#!/bin/sh\nexec \"{}\" \"$@\"\n",
                std::env::current_exe().unwrap().display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&mock, std::fs::Permissions::from_mode(0o755)).unwrap();
        let daemon = fixture::RunningDaemon::start(&home, &socket);
        let mut spec = agentdocker_core::AgentSpec {
            name: "bridge".into(),
            runtime: "codex".into(),
            command: vec![mock.display().to_string()],
            workdir: Some(checkout.canonicalize().unwrap()),
            ..Default::default()
        };
        spec.env
            .insert("CODEX_HOME".into(), codex_home.display().to_string());
        spec.env
            .insert(TRANSCRIPT.into(), transcript.display().to_string());
        if let Some(version) = version {
            spec.env.insert(VERSION.into(), version.into());
        }
        agentdocker_host::provider_input::enable_codex_input(
            &mut spec,
            Path::new(env!("CARGO_BIN_EXE_agentdocker")),
        )
        .unwrap();
        let started = fixture::rpc(&socket, json!({"op": "run", "spec": spec})).unwrap();
        assert_eq!(started["type"], "agent", "{started}");
        let agent = started["agent"]["id"].as_str().unwrap().to_owned();
        Self {
            daemon,
            _root: root,
            transcript,
            agent,
        }
    }

    fn wait_for_agent(&self, within: Duration, what: &str, done: impl Fn(&Value) -> bool) -> Value {
        fixture::wait_for(
            &self.daemon.socket,
            json!({"op": "inspect", "agent": self.agent}),
            within,
            what,
            |response| response["type"] == "agent" && done(&response["agent"]),
        )["agent"]
            .clone()
    }

    fn send(&self, text: &str) -> String {
        let sent = fixture::rpc(
            &self.daemon.socket,
            json!({"op": "send", "from": "user", "to": self.agent, "kind": "chat",
                "payload": {"text": text}}),
        )
        .unwrap();
        assert_eq!(sent["type"], "sent", "{sent}");
        sent["message"].as_str().unwrap().to_owned()
    }

    fn inbox(&self) -> Vec<Value> {
        let inbox = fixture::rpc(
            &self.daemon.socket,
            json!({"op": "inbox", "agent": self.agent, "drain": false}),
        )
        .unwrap();
        assert_eq!(inbox["type"], "messages", "{inbox}");
        inbox["messages"].as_array().unwrap().clone()
    }

    fn transcript(&self) -> Vec<Value> {
        std::fs::read_to_string(&self.transcript)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn stop(self) {
        let stopped = fixture::rpc(
            &self.daemon.socket,
            json!({"op": "stop", "agent": self.agent, "force": false}),
        )
        .unwrap();
        assert_ne!(stopped["type"], "error", "{stopped}");
        self.wait_for_agent(Duration::from_secs(15), "the bridge ending", |agent| {
            agent["status"] != "running" && agent["status"] != "stopping"
        });
    }
}

#[cfg(unix)]
fn a_queued_message_becomes_a_turn_with_a_receipt_proved_from_the_providers_item() {
    let bridge = Bridge::start();
    let first = bridge.send("hello from the test");
    let agent = bridge.wait_for_agent(
        Duration::from_secs(20),
        "the first message's receipt",
        |agent| agent["input_delivery"]["received"]["messages"][0] == first.as_str(),
    );
    let receipt = &agent["input_delivery"]["received"]["receipt"];
    assert_eq!(receipt["thread"], "thread-1", "{receipt}");
    assert_eq!(receipt["turn"], "turn-1", "{receipt}");
    assert_eq!(receipt["item"], "item-1", "{receipt}");
    assert_eq!(
        agent["spec"]["labels"]["session_id"], "thread-1",
        "the accepted turn binds the conversation to the managed record"
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while !bridge.inbox().is_empty() {
        assert!(
            Instant::now() < deadline,
            "the received message leaves the queue"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let second = bridge.send("and again");
    let agent = bridge.wait_for_agent(
        Duration::from_secs(20),
        "the second message's receipt",
        |agent| agent["input_delivery"]["received"]["messages"][0] == second.as_str(),
    );
    assert_eq!(
        agent["input_delivery"]["received"]["receipt"]["turn"], "turn-2",
        "the loop keeps serving after a completed turn"
    );
    let transcript = bridge.transcript();
    let turns: Vec<&Value> = transcript
        .iter()
        .filter(|frame| frame["method"] == "turn/start")
        .collect();
    assert_eq!(turns.len(), 2, "{transcript:?}");
    let input = turns[0]["params"]["input"][0]["text"].as_str().unwrap();
    let envelope: Value = serde_json::from_str(input).unwrap();
    assert_eq!(
        envelope["agentdocker_message"]["payload"]["text"],
        "hello from the test"
    );
    assert_eq!(envelope["agentdocker_message"]["id"], first.as_str());
    assert_eq!(
        envelope["agentdocker_message"]["to"]["value"],
        bridge.agent.as_str(),
        "the message carries its attribution into the turn"
    );
    assert_eq!(turns[0]["params"]["threadId"], "thread-1");
    assert_eq!(turns[0]["params"]["clientUserMessageId"], first.as_str());
    assert!(
        transcript
            .iter()
            .any(|frame| frame["method"] == "initialize"
                && frame["params"]["clientInfo"]["name"] == "agentdocker_codex_input"),
        "{transcript:?}"
    );
    assert!(
        transcript
            .iter()
            .any(|frame| frame["method"] == "hooks/list"),
        "every turn is preceded by the hook preflight"
    );
    // One read: the bridge may take the second message between two, which
    // emptied the inbox under its own index (seen on CI).
    let inbox = bridge.inbox();
    assert!(
        inbox
            .first()
            .is_none_or(|message| message["id"] != second.as_str())
    );
    bridge.stop();
}

/// The server's error reply to `turn/start` is its word that no turn
/// started: the message stays queued, the provider's structured reason is
/// on the record, and nothing is retained as needing inspection.
#[cfg(unix)]
fn a_refused_turn_leaves_the_message_queued_and_reports_the_provider() {
    let bridge = Bridge::start();
    let refused = bridge.send("please refuse this one");
    let agent = bridge.wait_for_agent(
        Duration::from_secs(20),
        "the provider's refusal on the record",
        |agent| agent["provider_availability"]["issue"].is_object(),
    );
    let kind = agent["provider_availability"]["issue"]["kind"].to_string();
    assert!(kind.to_lowercase().contains("usage"), "{agent}");
    assert!(
        agent["input_delivery"]["received"].is_null(),
        "no receipt is claimed for a refused turn: {agent}"
    );
    let inbox = bridge.inbox();
    assert_eq!(inbox.len(), 1, "{inbox:?}");
    assert_eq!(inbox[0]["id"], refused.as_str());
    std::thread::sleep(Duration::from_millis(1500));
    let transcript = bridge.transcript();
    assert!(
        transcript
            .iter()
            .any(|frame| frame["method"] == "turn/start"),
        "{transcript:?}"
    );
    assert!(
        !transcript
            .iter()
            .any(|frame| frame["method"] == "thread/items/list"),
        "a refused turn is not a lost receipt to go looking for: {transcript:?}"
    );
    assert_eq!(bridge.inbox()[0]["id"], refused.as_str(), "still queued");
    bridge.stop();
}

/// An app-server older than the floor is refused at `initialize`, with
/// both versions in the pause reason, before any thread is started.
#[cfg(unix)]
fn an_app_server_older_than_the_floor_is_refused_before_any_thread_is_touched() {
    let bridge = Bridge::launch(Some("0.153.4"));
    let agent = bridge.wait_for_agent(
        Duration::from_secs(30),
        "the bridge pausing on the old server",
        |agent| agent["input_delivery"]["paused"] == true,
    );
    let reason = agent["input_delivery"]["pause_reason"].to_string();
    assert!(
        reason.contains("0.153.4") && reason.contains("0.154.0"),
        "{reason}"
    );
    let transcript = bridge.transcript();
    assert!(
        transcript
            .iter()
            .any(|frame| frame["method"] == "initialize"),
        "{transcript:?}"
    );
    assert!(
        !transcript
            .iter()
            .any(|frame| frame["method"] == "thread/start"),
        "{transcript:?}"
    );
}

/// The receiver for an existing Codex session, end to end: a session this
/// binary stands in for is registered with its pid and conversation, the
/// receiver binds the queue, a message goes in through `thread/queue/add`,
/// the receipt is proved from the user item the session's own turn carried,
/// and the message is acknowledged. The receiver never starts a turn.
#[cfg(unix)]
fn an_existing_codex_session_is_fed_through_its_native_queue_with_a_proved_receipt() {
    use std::process::{Command, Stdio};
    let root = fixture::root();
    let home = root.path().join("home");
    let socket = root.path().join("agentd.sock");
    let checkout = root.path().join("checkout");
    let codex_home = root.path().join("codex-home");
    std::fs::create_dir_all(&checkout).unwrap();
    std::fs::create_dir_all(&codex_home).unwrap();
    let checkout = checkout.canonicalize().unwrap();
    let transcript = root.path().join("app-server.jsonl");
    let daemon = fixture::RunningDaemon::start(&home, &socket);
    let exe = std::env::current_exe().unwrap().canonicalize().unwrap();
    let mut session = Command::new(&exe)
        .arg("idle-codex")
        .current_dir(&checkout)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = session.id();
    let born = agentdocker_host::procinfo::start_time(pid).expect("the session's birth");
    let mut spec = agentdocker_core::AgentSpec {
        name: "existing".into(),
        runtime: "codex".into(),
        workdir: Some(checkout.clone()),
        ..Default::default()
    };
    spec.labels.insert("session_id".into(), "thread-1".into());
    let registered =
        fixture::rpc(&socket, json!({"op": "register", "spec": spec, "pid": pid})).unwrap();
    assert_eq!(registered["type"], "agent", "{registered}");
    let agent = registered["agent"]["id"].as_str().unwrap().to_owned();
    let log = agentdocker_host::dirs::private_file(&root.path().join("receiver.log"), true, true)
        .unwrap();
    let mut receiver = Command::new(env!("CARGO_BIN_EXE_agentdocker"))
        .arg("--socket")
        .arg(&socket)
        .arg("codex-queue")
        .args(["--agent", &agent, "--pid", &pid.to_string()])
        .args(["--started-at", &born.to_rfc3339(), "--thread", "thread-1"])
        .arg("--profile")
        .arg(&codex_home)
        .arg("--cwd")
        .arg(&checkout)
        .arg("--program")
        .arg(&exe)
        .env("AGENTDOCKER_HOME", &home)
        .env("AGENTDOCKER_SOCKET", &socket)
        .env("AGENTDOCKER_NO_AUTOSTART", "1")
        .env(TRANSCRIPT, &transcript)
        .env_remove("AGENTDOCKER_AGENT_ID")
        .env_remove("AGENTDOCKER_AGENT_NAME")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone().unwrap()))
        .stderr(Stdio::from(log))
        .spawn()
        .unwrap();
    let inspect = json!({"op": "inspect", "agent": agent});
    let bound = fixture::wait_for(
        &socket,
        inspect.clone(),
        Duration::from_secs(30),
        "the receiver binding the queue and reporting ready",
        |response| {
            response["type"] == "agent"
                && response["agent"]["input_binding"].is_object()
                && response["agent"]["input_delivery"]["paused"] == false
        },
    );
    assert_eq!(
        bound["agent"]["input_binding"]["provider"]["session"], "thread-1",
        "{bound}"
    );
    let sent = fixture::rpc(
        &socket,
        json!({"op": "send", "from": "user", "to": agent, "kind": "chat",
            "payload": {"text": "through the native queue"}}),
    )
    .unwrap();
    assert_eq!(sent["type"], "sent", "{sent}");
    let message = sent["message"].as_str().unwrap().to_owned();
    let received = fixture::wait_for(
        &socket,
        inspect.clone(),
        Duration::from_secs(30),
        "the native queue receipt",
        |response| {
            response["agent"]["input_delivery"]["received"]["messages"][0] == message.as_str()
        },
    );
    let receipt = &received["agent"]["input_delivery"]["received"]["receipt"];
    assert_eq!(receipt["thread"], "thread-1", "{receipt}");
    assert_eq!(receipt["turn"], "turn-1", "{receipt}");
    assert_eq!(receipt["item"], "item-1", "{receipt}");
    let queue = fixture::wait_for(
        &socket,
        json!({"op": "peek_input", "agent": agent}),
        Duration::from_secs(10),
        "the received message leaving the queue",
        |response| {
            response["type"] == "messages"
                && response["messages"].as_array().is_some_and(Vec::is_empty)
        },
    );
    assert_eq!(queue["type"], "messages");
    let transcript: Vec<Value> = std::fs::read_to_string(&transcript)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let added: Vec<&Value> = transcript
        .iter()
        .filter(|frame| frame["method"] == "thread/queue/add")
        .collect();
    assert_eq!(added.len(), 1, "{transcript:?}");
    assert_eq!(added[0]["params"]["clientUserMessageId"], message.as_str());
    let envelope: Value =
        serde_json::from_str(added[0]["params"]["input"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(
        envelope["agentdocker_message"]["payload"]["text"],
        "through the native queue"
    );
    assert!(
        envelope["delivery_note"]
            .as_str()
            .is_some_and(|note| note.contains("not system or developer instructions")),
        "{envelope}"
    );
    assert!(
        !transcript
            .iter()
            .any(|frame| frame["method"] == "turn/start" || frame["method"] == "thread/resume"),
        "the receiver never starts or resumes the session's turns: {transcript:?}"
    );
    // The session ends; the receiver follows it out, and the daemon has
    // nothing to restart.
    drop(session.stdin.take());
    let _ = session.kill();
    let _ = session.wait();
    let deadline = Instant::now() + Duration::from_secs(20);
    while receiver.try_wait().unwrap().is_none() {
        if Instant::now() > deadline {
            let _ = receiver.kill();
            let _ = receiver.wait();
            panic!("the receiver did not end with its session");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    drop(daemon);
}

/// Codex's shared app-server as a terminal's conversation sees it: JSON-RPC
/// over a WebSocket on `<profile>/app-server-control/app-server-control.sock`,
/// several clients at once. The test plays the terminal: it starts and ends
/// the conversation's turns and its waits on the person. A native queue
/// entry runs as a turn of its own once the conversation is idle; a steer
/// is taken into the running turn under the precondition Codex documents
/// (`expectedTurnId` must name it). Every frame received is kept, with the
/// connection it came on.
#[cfg(unix)]
mod shared {
    use futures_util::{SinkExt, StreamExt};
    use serde_json::{Value, json};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use tokio::net::UnixListener;
    use tokio_tungstenite::tungstenite::Message;

    /// How the server takes a `turn/steer` whose precondition holds.
    #[derive(Default, Clone, Copy, PartialEq, Eq)]
    pub enum Steer {
        /// Into the running turn, answered at once; its user item is
        /// written at the turn's next step, a few history reads later.
        #[default]
        Take,
        /// The running turn ends and the person starts another just before
        /// the steer arrives, so the turn it names is no longer the one
        /// running. Only the first steer meets this.
        TurnChanges,
        /// Into the running turn, its item written at once; then the
        /// connection is dropped before the answer, as a restarted server
        /// may.
        DropBeforeAnswer,
    }

    #[derive(Default)]
    pub struct Server {
        pub frames: Vec<(usize, Value)>,
        /// The terminal has the conversation unloaded, every connection kept.
        pub unloaded: bool,
        /// The first queue add's connection drops after the server's
        /// success, and its entry runs only once a later client has looked
        /// at the queue.
        pub drop_first_add: bool,
        pub steer: Steer,
        /// The running turn waits on the person's approval.
        pub waiting_on_approval: bool,
        running: Option<String>,
        queued: Vec<Value>,
        dropped: bool,
        run: bool,
        turns: Vec<String>,
        items: Vec<Value>,
        written: usize,
        /// Steered user items not yet written, and how many history reads
        /// pass before they are.
        steered: Vec<Value>,
        steered_after: usize,
    }

    enum Reply {
        Answer(Value),
        AnswerThenDrop(Value),
        Refuse(i64, String),
        Drop,
    }

    impl Server {
        fn next_turn(&mut self) -> String {
            let turn = format!("turn-{}", self.turns.len() + 1);
            self.turns.push(turn.clone());
            turn
        }

        fn user_item(&mut self, turn: &str, input: &Value) -> Value {
            self.written += 1;
            json!({"turnId": turn, "item": {"type": "userMessage",
                "id": format!("item-{}", self.written), "content": input}})
        }

        /// The person starts a turn in the terminal.
        pub fn start_turn(&mut self) -> String {
            assert!(self.running.is_none(), "a turn is already running");
            let turn = self.next_turn();
            self.running = Some(turn.clone());
            turn
        }

        /// The running turn ends, with whatever was steered into it
        /// written by then; the conversation is idle.
        pub fn end_turn(&mut self) {
            self.items.append(&mut self.steered);
            self.running = None;
            self.waiting_on_approval = false;
        }

        pub fn running(&self) -> Option<String> {
            self.running.clone()
        }

        /// Each queued entry runs as its own turn.
        fn take_queue(&mut self) {
            for submission in std::mem::take(&mut self.queued) {
                let turn = self.next_turn();
                let item = self.user_item(&turn, &submission["input"]);
                self.items.push(item);
            }
        }

        fn status(&self) -> Value {
            match &self.running {
                None => json!({"type": "idle"}),
                Some(_) if self.waiting_on_approval => {
                    json!({"type": "active", "activeFlags": ["waitingOnApproval"]})
                }
                Some(_) => json!({"type": "active", "activeFlags": []}),
            }
        }

        fn answer(&mut self, home: &str, cwd: &str, method: &str, params: &Value) -> Reply {
            let result = match method {
                "initialize" => json!({"codexHome": home,
                    "userAgent": "codex_cli_rs/0.163.0 (Mock 1.0.0; test) shared-app-server",
                    "platformFamily": "unix", "platformOs": "test"}),
                "config/read" => json!({"config": {"mcp_servers": {}}}),
                "thread/loaded/list" if self.unloaded => json!({"data": [], "nextCursor": null}),
                "thread/loaded/list" => json!({"data": ["thread-1"], "nextCursor": null}),
                "thread/read" => json!({"thread": {"id": params["threadId"], "cwd": cwd,
                    "parentThreadId": null, "status": self.status()}}),
                "thread/queue/list" => {
                    if self.dropped {
                        self.run = true;
                    }
                    json!({"data": self.queued, "nextCursor": null})
                }
                "thread/items/list" => {
                    if !self.steered.is_empty() {
                        if self.steered_after == 0 {
                            self.items.append(&mut self.steered);
                        } else {
                            self.steered_after -= 1;
                        }
                    }
                    if self.run || (!self.drop_first_add && self.running.is_none()) {
                        self.take_queue();
                        self.run = false;
                    }
                    let limit = params["limit"].as_u64().unwrap_or(50) as usize;
                    json!({"data": self.items.iter().rev().take(limit).cloned().collect::<Vec<_>>(),
                        "nextCursor": null})
                }
                "thread/turns/list" => {
                    let limit = params["limit"].as_u64().unwrap_or(50) as usize;
                    let running = self.running.clone();
                    json!({"data": self.turns.iter().rev().take(limit).map(|turn| json!({
                        "id": turn, "items": [], "itemsView": "notLoaded",
                        "status": if running.as_deref() == Some(turn.as_str()) { "inProgress" } else { "completed" },
                    })).collect::<Vec<_>>(), "nextCursor": null})
                }
                "thread/queue/add" => {
                    let submission = json!({"id": format!("queued-{}", self.frames.len()),
                        "clientUserMessageId": params["clientUserMessageId"],
                        "input": params["input"]});
                    self.queued.push(submission.clone());
                    let result = json!({"queuedSubmission": submission});
                    // Only the first add loses its connection.
                    if self.drop_first_add && !self.dropped {
                        self.dropped = true;
                        return Reply::AnswerThenDrop(result);
                    }
                    result
                }
                "turn/steer" => {
                    if self.steer == Steer::TurnChanges && self.running.is_some() {
                        self.end_turn();
                        self.start_turn();
                        self.steer = Steer::Take;
                    }
                    let expected = params["expectedTurnId"].as_str().unwrap_or_default();
                    let Some(running) = self.running.clone() else {
                        return Reply::Refuse(-32600, "no active turn to steer".into());
                    };
                    if expected != running {
                        return Reply::Refuse(
                            -32600,
                            format!("expected active turn id `{expected}` but found `{running}`"),
                        );
                    }
                    let item = self.user_item(&running, &params["input"]);
                    if self.steer == Steer::DropBeforeAnswer {
                        self.items.push(item);
                        return Reply::Drop;
                    }
                    self.steered.push(item);
                    self.steered_after = 2;
                    json!({"turnId": running})
                }
                other => return Reply::Refuse(-32601, format!("{other} is not served here")),
            };
            Reply::Answer(result)
        }
    }

    pub async fn serve(
        listener: UnixListener,
        home: PathBuf,
        cwd: PathBuf,
        server: Arc<Mutex<Server>>,
    ) {
        let home = home.display().to_string();
        let cwd = cwd.display().to_string();
        let mut connection = 0;
        while let Ok((stream, _)) = listener.accept().await {
            connection += 1;
            let (server, home, cwd) = (server.clone(), home.clone(), cwd.clone());
            tokio::spawn(async move {
                let Ok(mut socket) = tokio_tungstenite::accept_async(stream).await else {
                    return;
                };
                while let Some(Ok(Message::Text(text))) = socket.next().await {
                    let request: Value = serde_json::from_str(&text).unwrap();
                    let Some(id) = request.get("id").cloned() else {
                        continue;
                    };
                    let method = request["method"].as_str().unwrap_or_default().to_owned();
                    let reply = {
                        let mut server = server.lock().unwrap();
                        server.frames.push((connection, request.clone()));
                        server.answer(&home, &cwd, &method, &request["params"])
                    };
                    let (frame, drop) = match reply {
                        Reply::Answer(result) => (json!({"id": id, "result": result}), false),
                        Reply::AnswerThenDrop(result) => {
                            (json!({"id": id, "result": result}), true)
                        }
                        Reply::Refuse(code, message) => (
                            json!({"id": id, "error": {"code": code, "message": message}}),
                            false,
                        ),
                        Reply::Drop => return,
                    };
                    if socket.send(Message::text(frame.to_string())).await.is_err() || drop {
                        return;
                    }
                }
            });
        }
    }
}

/// A terminal's conversation in the shared server, the fixture daemon, the
/// terminal's process (registered as proved to run it) and the receiver
/// started for it with `--control`, bound and ready.
#[cfg(unix)]
struct Terminal {
    server: std::sync::Arc<std::sync::Mutex<shared::Server>>,
    session: std::process::Child,
    receiver: std::process::Child,
    agent: String,
    daemon: fixture::RunningDaemon,
    runtime: tokio::runtime::Runtime,
    _root: tempfile::TempDir,
}

#[cfg(unix)]
impl Terminal {
    fn start(server: shared::Server) -> Self {
        use std::os::unix::fs::PermissionsExt;
        use std::process::{Command, Stdio};
        use std::sync::{Arc, Mutex};
        let root = fixture::root();
        let home = root.path().join("home");
        let socket = root.path().join("agentd.sock");
        let checkout = root.path().join("checkout");
        let codex_home = root.path().join("codex-home");
        let control = codex_home.join("app-server-control");
        std::fs::create_dir_all(&checkout).unwrap();
        std::fs::create_dir_all(&control).unwrap();
        std::fs::set_permissions(&control, std::fs::Permissions::from_mode(0o700)).unwrap();
        let checkout = checkout.canonicalize().unwrap();
        let codex_home = codex_home.canonicalize().unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let server = Arc::new(Mutex::new(server));
        let listener = {
            let _entered = runtime.enter();
            tokio::net::UnixListener::bind(control.join("app-server-control.sock")).unwrap()
        };
        runtime.spawn(shared::serve(
            listener,
            codex_home.clone(),
            checkout.clone(),
            server.clone(),
        ));
        let daemon = fixture::RunningDaemon::start(&home, &socket);
        let exe = std::env::current_exe().unwrap().canonicalize().unwrap();
        let session = Command::new(&exe)
            .arg("idle-codex")
            .current_dir(&checkout)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let pid = session.id();
        let born = agentdocker_host::procinfo::start_time(pid).expect("the terminal's birth");
        let mut spec = agentdocker_core::AgentSpec {
            name: "terminal".into(),
            runtime: "codex".into(),
            workdir: Some(checkout.clone()),
            ..Default::default()
        };
        spec.labels.insert("session_id".into(), "thread-1".into());
        spec.labels.insert(
            agentdocker_core::agent::PROFILE_LABEL.into(),
            codex_home.display().to_string(),
        );
        let registered =
            fixture::rpc(&socket, json!({"op": "register", "spec": spec, "pid": pid})).unwrap();
        assert_eq!(registered["type"], "agent", "{registered}");
        let agent = registered["agent"]["id"].as_str().unwrap().to_owned();
        let log =
            agentdocker_host::dirs::private_file(&root.path().join("receiver.log"), true, true)
                .unwrap();
        let receiver = Command::new(env!("CARGO_BIN_EXE_agentdocker"))
            .arg("--socket")
            .arg(&socket)
            .arg("codex-queue")
            .args(["--agent", &agent, "--pid", &pid.to_string()])
            .args(["--started-at", &born.to_rfc3339(), "--thread", "thread-1"])
            .arg("--profile")
            .arg(&codex_home)
            .arg("--cwd")
            .arg(&checkout)
            .arg("--program")
            .arg(&exe)
            .arg("--control")
            .env("AGENTDOCKER_HOME", &home)
            .env("AGENTDOCKER_SOCKET", &socket)
            .env("AGENTDOCKER_NO_AUTOSTART", "1")
            .env_remove("AGENTDOCKER_AGENT_ID")
            .env_remove("AGENTDOCKER_AGENT_NAME")
            .env_remove(TRANSCRIPT)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log))
            .spawn()
            .unwrap();
        let terminal = Self {
            server,
            session,
            receiver,
            agent,
            daemon,
            runtime,
            _root: root,
        };
        let bound = terminal.wait_for_agent(
            Duration::from_secs(30),
            "the receiver binding the queue through the control socket",
            |agent| {
                agent["input_binding"].is_object() && agent["input_delivery"]["paused"] == false
            },
        );
        let launch = bound["input_binding"]["launch"]["args"]
            .as_array()
            .unwrap()
            .clone();
        assert!(
            launch.iter().any(|a| a == "--control"),
            "a restarted receiver reaches the same shared server: {launch:?}"
        );
        terminal
    }

    fn socket(&self) -> &Path {
        &self.daemon.socket
    }

    fn server(&self) -> std::sync::MutexGuard<'_, shared::Server> {
        self.server.lock().unwrap()
    }

    fn wait_for_agent(&self, within: Duration, what: &str, done: impl Fn(&Value) -> bool) -> Value {
        fixture::wait_for(
            self.socket(),
            json!({"op": "inspect", "agent": self.agent}),
            within,
            what,
            |response| response["type"] == "agent" && done(&response["agent"]),
        )["agent"]
            .clone()
    }

    fn send(&self, text: &str) -> String {
        let sent = fixture::rpc(
            self.socket(),
            json!({"op": "send", "from": "user", "to": self.agent, "kind": "chat",
                "payload": {"text": text}}),
        )
        .unwrap();
        assert_eq!(sent["type"], "sent", "{sent}");
        sent["message"].as_str().unwrap().to_owned()
    }

    /// The receipt the daemon records once `message` is received.
    fn received(&self, message: &str, within: Duration, what: &str) -> Value {
        self.wait_for_agent(within, what, |agent| {
            agent["input_delivery"]["received"]["messages"][0] == message
        })["input_delivery"]["received"]["receipt"]
            .clone()
    }

    /// Whether `message` still waits in the agent's queue.
    fn queued(&self, message: &str) -> bool {
        let queue = fixture::rpc(
            self.socket(),
            json!({"op": "peek_input", "agent": self.agent}),
        )
        .unwrap();
        assert_eq!(queue["type"], "messages", "{queue}");
        queue["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|queued| queued["id"] == message)
    }

    fn wait_queue_empty(&self) {
        fixture::wait_for(
            self.socket(),
            json!({"op": "peek_input", "agent": self.agent}),
            Duration::from_secs(10),
            "the received message leaving the queue",
            |response| {
                response["type"] == "messages"
                    && response["messages"].as_array().is_some_and(Vec::is_empty)
            },
        );
    }

    fn frames(&self) -> Vec<(usize, Value)> {
        self.server().frames.clone()
    }

    /// Every request for `method` the server received, with its connection.
    fn requests(&self, method: &str) -> Vec<(usize, Value)> {
        self.frames()
            .into_iter()
            .filter(|(_, frame)| frame["method"] == method)
            .collect()
    }

    /// Wait until the server has received `count` requests `done` picks.
    fn wait_for_requests(
        &self,
        count: usize,
        within: Duration,
        what: &str,
        done: impl Fn(&Value) -> bool,
    ) {
        let deadline = Instant::now() + within;
        while self
            .frames()
            .iter()
            .filter(|(_, frame)| done(frame))
            .count()
            < count
        {
            assert!(
                Instant::now() < deadline,
                "{what} did not happen within {within:?}: {:?}",
                self.frames()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// The terminal ends; the receiver follows it out.
    fn finish(mut self) {
        drop(self.session.stdin.take());
        let _ = self.session.kill();
        let _ = self.session.wait();
        let deadline = Instant::now() + Duration::from_secs(20);
        while self.receiver.try_wait().unwrap().is_none() {
            if Instant::now() > deadline {
                let _ = self.receiver.kill();
                let _ = self.receiver.wait();
                panic!("the receiver did not end with its terminal");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let Self {
            daemon, runtime, ..
        } = self;
        drop(daemon);
        runtime.shutdown_background();
    }
}

/// The envelope a queue add or a steer carried.
#[cfg(unix)]
fn envelope(request: &Value) -> Value {
    serde_json::from_str(request["params"]["input"][0]["text"].as_str().unwrap()).unwrap()
}

/// A terminal's conversation that runs in Codex's shared app-server, end to
/// end: the receiver reaches it through the profile's control socket, binds
/// the queue, adds the message once with `thread/queue/add`, and when that
/// connection drops after the server's success it reconnects and proves the
/// receipt from the turn's own user item, never adding it again. The
/// daemon reads the same server for the conversation's status. When the
/// terminal unloads the conversation, connections kept, the next message is
/// not offered (delivery pauses) until it is loaded again, then once.
#[cfg(unix)]
fn a_shared_server_conversation_is_fed_through_its_control_socket_once() {
    let mut server = shared::Server::default();
    server.drop_first_add = true;
    let terminal = Terminal::start(server);
    let message = terminal.send("through the shared server");
    // The dropped connection pauses delivery; the receiver comes back
    // after its reconnect delay and finds the turn that took the entry.
    let receipt = terminal.received(
        &message,
        Duration::from_secs(75),
        "the receipt after the reconnect",
    );
    assert_eq!(receipt["thread"], "thread-1", "{receipt}");
    assert_eq!(receipt["turn"], "turn-1", "{receipt}");
    assert_eq!(receipt["item"], "item-1", "{receipt}");
    terminal.wait_queue_empty();
    // The daemon reads the same server: the quiet conversation is idle.
    terminal.wait_for_agent(
        Duration::from_secs(20),
        "the conversation's status from the shared server",
        |agent| agent["reported_activity"]["activity"] == "idle",
    );
    let added = terminal.requests("thread/queue/add");
    assert_eq!(
        added.len(),
        1,
        "added once, across the reconnect: {:?}",
        terminal.frames()
    );
    assert_eq!(
        added[0].1["params"]["clientUserMessageId"],
        message.as_str()
    );
    assert_eq!(added[0].1["params"]["threadId"], "thread-1");
    let reconnected = terminal
        .requests("thread/items/list")
        .iter()
        .any(|(connection, _)| *connection > added[0].0);
    assert!(
        reconnected,
        "the receipt was read on a later connection: {:?}",
        terminal.frames()
    );
    // The terminal unloads the conversation; every connection stays.
    terminal.server().unloaded = true;
    let second = terminal.send("while it is unloaded");
    terminal.wait_for_agent(
        Duration::from_secs(20),
        "delivery pausing on the unloaded conversation",
        |agent| {
            agent["input_delivery"]["paused"] == true
                && agent["input_delivery"]["pause_reason"]
                    .as_str()
                    .is_some_and(|reason| reason.contains("has not loaded"))
        },
    );
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(
        terminal.requests("thread/queue/add").len(),
        1,
        "nothing is offered to an unloaded conversation"
    );
    terminal.server().unloaded = false;
    terminal.wait_for_agent(
        Duration::from_secs(30),
        "the second message's receipt once it is loaded again",
        |agent| {
            agent["input_delivery"]["received"]["messages"][0] == second.as_str()
                && agent["input_delivery"]["paused"] == false
        },
    );
    let added = terminal.requests("thread/queue/add");
    assert_eq!(added.len(), 2, "the second message added once: {added:?}");
    assert_eq!(added[1].1["params"]["clientUserMessageId"], second.as_str());
    let frames = terminal.frames();
    assert!(
        !frames.iter().any(|(_, frame)| matches!(
            frame["method"].as_str(),
            Some("turn/start" | "turn/steer" | "thread/resume" | "thread/start")
        )),
        "an idle conversation is never steered, and its turns are its terminal's: {frames:?}"
    );
    terminal.finish();
}

/// A message that arrives while the terminal's turn runs is steered into
/// that turn: one `turn/steer` naming it as `expectedTurnId` and carrying
/// the message id, received only on the turn's own user item, which is
/// written a few reads after the steer was answered. A second message in
/// the same turn is steered into it after the first is received. Once the
/// turn has ended the next goes by the native queue. Nothing goes twice.
#[cfg(unix)]
fn a_message_during_a_running_turn_is_steered_into_it_once() {
    let terminal = Terminal::start(shared::Server::default());
    let turn = terminal.server().start_turn();
    let first = terminal.send("while the turn runs");
    let receipt = terminal.received(&first, Duration::from_secs(30), "the steered receipt");
    assert_eq!(receipt["thread"], "thread-1", "{receipt}");
    assert_eq!(receipt["turn"], turn.as_str(), "{receipt}");
    assert_eq!(receipt["item"], "item-1", "{receipt}");
    let steers = terminal.requests("turn/steer");
    assert_eq!(steers.len(), 1, "{:?}", terminal.frames());
    let params = &steers[0].1["params"];
    assert_eq!(params["threadId"], "thread-1");
    assert_eq!(params["expectedTurnId"], turn.as_str());
    assert_eq!(params["clientUserMessageId"], first.as_str());
    let carried = envelope(&steers[0].1);
    assert_eq!(carried["agentdocker_message"]["id"], first.as_str());
    assert_eq!(
        carried["agentdocker_message"]["payload"]["text"],
        "while the turn runs"
    );
    assert!(terminal.requests("thread/queue/add").is_empty());
    let second = terminal.send("while it still runs");
    let receipt = terminal.received(
        &second,
        Duration::from_secs(30),
        "the second steered receipt",
    );
    assert_eq!(receipt["turn"], turn.as_str(), "{receipt}");
    assert_eq!(receipt["item"], "item-2", "{receipt}");
    let steers = terminal.requests("turn/steer");
    assert_eq!(steers.len(), 2, "{:?}", terminal.frames());
    assert_eq!(steers[1].1["params"]["expectedTurnId"], turn.as_str());
    assert_eq!(
        steers[1].1["params"]["clientUserMessageId"],
        second.as_str()
    );
    terminal.server().end_turn();
    let third = terminal.send("once it is idle");
    let receipt = terminal.received(&third, Duration::from_secs(30), "the queued receipt");
    assert_eq!(receipt["turn"], "turn-2", "{receipt}");
    terminal.wait_queue_empty();
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(
        terminal.requests("turn/steer").len(),
        2,
        "steered once each"
    );
    let added = terminal.requests("thread/queue/add");
    assert_eq!(added.len(), 1, "{added:?}");
    assert_eq!(added[0].1["params"]["clientUserMessageId"], third.as_str());
    terminal.finish();
}

/// The terminal's turn ends, and the person starts another, between the
/// receiver's read and its steer: the server refuses the steer for the turn
/// it named, its word that nothing was submitted, so the message goes by
/// the native queue instead, once, and is received in the turn that runs it
/// at idle.
#[cfg(unix)]
fn a_steer_refused_for_a_changed_turn_is_delivered_once_by_the_native_queue() {
    let mut server = shared::Server::default();
    server.steer = shared::Steer::TurnChanges;
    let terminal = Terminal::start(server);
    let read = terminal.server().start_turn();
    let message = terminal.send("between the read and the steer");
    terminal.wait_for_requests(
        1,
        Duration::from_secs(30),
        "the native queue add after the refusal",
        |frame| frame["method"] == "thread/queue/add",
    );
    let steers = terminal.requests("turn/steer");
    assert_eq!(steers.len(), 1, "{:?}", terminal.frames());
    assert_eq!(steers[0].1["params"]["expectedTurnId"], read.as_str());
    let persons = terminal.server().running().expect("the person's turn");
    assert_ne!(persons, read);
    // Queued behind the person's turn: not received until it ends.
    std::thread::sleep(Duration::from_secs(3));
    assert!(terminal.queued(&message), "it waits for idle");
    terminal.server().end_turn();
    let receipt = terminal.received(
        &message,
        Duration::from_secs(30),
        "the receipt from the native queue",
    );
    assert_eq!(receipt["turn"], "turn-3", "{receipt}");
    terminal.wait_queue_empty();
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(
        terminal.requests("turn/steer").len(),
        1,
        "never steered again"
    );
    let added = terminal.requests("thread/queue/add");
    assert_eq!(added.len(), 1, "added once: {added:?}");
    assert_eq!(
        added[0].1["params"]["clientUserMessageId"],
        message.as_str()
    );
    terminal.finish();
}

/// While the running turn waits on the person's approval nothing is
/// steered into it, or queued; once the approval is answered and the turn
/// runs on, the message is steered into it.
#[cfg(unix)]
fn a_turn_waiting_on_approval_takes_no_steer() {
    let terminal = Terminal::start(shared::Server::default());
    let turn = {
        let mut server = terminal.server();
        let turn = server.start_turn();
        server.waiting_on_approval = true;
        turn
    };
    let before = terminal.frames().len();
    let message = terminal.send("while it waits on an approval");
    // The receiver has looked at the conversation twice since.
    let looked = |frames: &[(usize, Value)]| {
        frames[before..]
            .iter()
            .filter(|(_, frame)| {
                frame["method"] == "thread/turns/list" && frame["params"]["limit"] == 1
            })
            .count()
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    while looked(&terminal.frames()) < 2 {
        assert!(Instant::now() < deadline, "the receiver did not look");
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(terminal.requests("turn/steer").is_empty(), "no steer");
    assert!(
        terminal.requests("thread/queue/add").is_empty(),
        "nothing queued"
    );
    assert!(terminal.queued(&message));
    terminal.server().waiting_on_approval = false;
    let receipt = terminal.received(
        &message,
        Duration::from_secs(30),
        "the steered receipt after the approval",
    );
    assert_eq!(receipt["turn"], turn.as_str(), "{receipt}");
    let steers = terminal.requests("turn/steer");
    assert_eq!(steers.len(), 1, "{steers:?}");
    assert_eq!(steers[0].1["params"]["expectedTurnId"], turn.as_str());
    assert!(terminal.requests("thread/queue/add").is_empty());
    terminal.finish();
}

/// The connection drops after the steer was sent and before its answer.
/// The receiver reconnects and proves the receipt from the steered turn's
/// own user item, never steering or queueing the message again.
#[cfg(unix)]
fn a_steer_whose_answer_is_lost_is_reconciled_without_a_resend() {
    let mut server = shared::Server::default();
    server.steer = shared::Steer::DropBeforeAnswer;
    let terminal = Terminal::start(server);
    let turn = terminal.server().start_turn();
    let message = terminal.send("answer lost");
    let receipt = terminal.received(
        &message,
        Duration::from_secs(75),
        "the receipt after the reconnect",
    );
    assert_eq!(receipt["turn"], turn.as_str(), "{receipt}");
    assert_eq!(receipt["item"], "item-1", "{receipt}");
    terminal.wait_queue_empty();
    std::thread::sleep(Duration::from_secs(3));
    let steers = terminal.requests("turn/steer");
    assert_eq!(steers.len(), 1, "steered once: {:?}", terminal.frames());
    assert_eq!(
        steers[0].1["params"]["clientUserMessageId"],
        message.as_str()
    );
    assert!(
        terminal.requests("thread/queue/add").is_empty(),
        "never queued"
    );
    let reconnected = terminal
        .requests("thread/items/list")
        .iter()
        .any(|(connection, _)| *connection > steers[0].0);
    assert!(
        reconnected,
        "the receipt was read on a later connection: {:?}",
        terminal.frames()
    );
    terminal.finish();
}

/// `codex-threads` is a diagnostic: with no daemon running it lists what the
/// shared server has loaded, names no record, and starts no daemon, although
/// nothing in its environment turns autostart off.
#[cfg(unix)]
fn the_shared_server_listing_never_starts_a_daemon() {
    use std::os::unix::fs::PermissionsExt;
    use std::process::{Command, Stdio};
    use std::sync::{Arc, Mutex};
    let root = fixture::root();
    let home = root.path().join("home");
    let socket = root.path().join("agentd.sock");
    let checkout = root.path().join("checkout");
    let codex_home = root.path().join("codex-home");
    let control = codex_home.join("app-server-control");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&checkout).unwrap();
    std::fs::create_dir_all(&control).unwrap();
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::set_permissions(&control, std::fs::Permissions::from_mode(0o700)).unwrap();
    let codex_home = codex_home.canonicalize().unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let server = Arc::new(Mutex::new(shared::Server::default()));
    let listener = {
        let _entered = runtime.enter();
        tokio::net::UnixListener::bind(control.join("app-server-control.sock")).unwrap()
    };
    runtime.spawn(shared::serve(
        listener,
        codex_home.clone(),
        checkout.canonicalize().unwrap(),
        server.clone(),
    ));
    let started = Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_agentdocker"))
        .args(["codex-threads", "--json", "--profile"])
        .arg(&codex_home)
        .env("AGENTDOCKER_HOME", &home)
        .env("AGENTDOCKER_SOCKET", &socket)
        .env_remove("AGENTDOCKER_NO_AUTOSTART")
        .env_remove("AGENTDOCKER_TOKEN_FILE")
        .env_remove("AGENTDOCKER_AGENT_ID")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let daemon_started = socket.exists();
    if daemon_started {
        // Stop what the listing should never have started.
        let _ = fixture::rpc(&socket, json!({"op": "shutdown"}));
    }
    assert!(!daemon_started, "the listing started a daemon");
    assert!(output.status.success(), "{output:?}");
    let rows: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(rows[0]["thread"], "thread-1", "{rows}");
    assert_eq!(rows[0]["status"], "idle", "{rows}");
    assert!(rows[0]["agent"].is_null(), "no daemon, no record: {rows}");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "it did not wait for a daemon to start: {:?}",
        started.elapsed()
    );
    let frames = server.lock().unwrap().frames.clone();
    assert!(
        frames.iter().all(|(_, frame)| matches!(
            frame["method"].as_str(),
            Some("initialize" | "thread/loaded/list" | "thread/read")
        )),
        "only reads: {frames:?}"
    );
    runtime.shutdown_background();
}

/// The two children `agentdocker codex` starts, as this binary plays them.
///
/// The dedicated app-server answers what the launcher and its receiver read
/// over the authenticated WebSocket: it is empty until the terminal program
/// starts a thread, which it announces to every connection, the launcher's
/// birth observer among them. The terminal program marks on the terminal
/// where its screen begins and ends, starts that thread, and holds the
/// screen until the fixture's `quit` file appears. Either one leaves when
/// the launcher that started it is gone.
#[cfg(unix)]
mod native {
    use super::NATIVE;
    use futures_util::{SinkExt, StreamExt};
    use serde_json::{Value, json};
    use std::{
        io::Write,
        path::PathBuf,
        sync::{Arc, Mutex},
        time::{Duration, Instant},
    };
    use tokio::sync::mpsc::UnboundedSender;
    use tokio_tungstenite::tungstenite::{
        Message,
        client::IntoClientRequest,
        handshake::server::{ErrorResponse, Request, Response},
        http::{HeaderValue, StatusCode},
    };

    /// What the terminal program writes when its screen begins and ends.
    /// Everything between the two on the terminal is the program's.
    pub const STARTED: &str = "[fake codex screen begins]";
    pub const ENDED: &str = "[fake codex screen ends]";

    fn option<'a>(arguments: &'a [String], name: &str) -> &'a str {
        let at = arguments
            .iter()
            .position(|argument| argument == name)
            .unwrap_or_else(|| panic!("{name} missing from {arguments:?}"));
        &arguments[at + 1]
    }

    /// Leave when the launcher is gone, so a failed test leaves nothing.
    fn orphan_watch() {
        let launcher = std::os::unix::process::parent_id();
        std::thread::spawn(move || {
            loop {
                if std::os::unix::process::parent_id() != launcher {
                    std::process::exit(0);
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        });
    }

    #[derive(Default)]
    struct State {
        thread: Option<Value>,
        connections: Vec<UnboundedSender<Value>>,
    }

    impl State {
        fn answer(&mut self, method: &str) -> Result<Value, String> {
            Ok(match method {
                "initialize" => json!({
                    "userAgent": "codex_cli_rs/0.163.0 (Mock 1.0.0; test) native-app-server",
                    "codexHome": std::env::var("CODEX_HOME").unwrap_or_default(),
                    "platformFamily": "unix", "platformOs": "test"}),
                "thread/loaded/list" => json!({
                    "data": self.thread.iter().map(|thread| thread["id"].clone()).collect::<Vec<_>>(),
                    "nextCursor": null}),
                "thread/start" => {
                    let now = chrono::Utc::now().timestamp();
                    let cwd = std::env::current_dir().unwrap().canonicalize().unwrap();
                    let thread = json!({"id": "thread-native", "sessionId": "thread-native",
                        "cwd": cwd, "createdAt": now, "updatedAt": now, "preview": "",
                        "status": {"type": "idle"}, "threadSource": "user",
                        "forkedFromId": null, "parentThreadId": null, "ephemeral": false,
                        "turns": []});
                    self.thread = Some(thread.clone());
                    let started = json!({"method": "thread/started", "params": {"thread": thread}});
                    self.connections
                        .retain(|connection| connection.send(started.clone()).is_ok());
                    json!({"thread": thread})
                }
                "thread/read" => match &self.thread {
                    Some(thread) => json!({"thread": thread}),
                    None => return Err("no thread".into()),
                },
                "thread/queue/list" => json!({"data": [], "nextCursor": null}),
                other => return Err(format!("{other} is not served here")),
            })
        }
    }

    /// `codex app-server --listen ws://127.0.0.1:<port> --ws-auth
    /// capability-token --ws-token-file <file>`.
    pub fn serve(arguments: &[String]) -> i32 {
        orphan_watch();
        let address = option(arguments, "--listen")
            .strip_prefix("ws://")
            .unwrap()
            .to_owned();
        let token = std::fs::read_to_string(option(arguments, "--ws-token-file")).unwrap();
        let expected = format!("Bearer {}", token.trim());
        let state = Arc::new(Mutex::new(State::default()));
        tokio::runtime::Runtime::new().unwrap().block_on(async move {
            let listener = tokio::net::TcpListener::bind(&address).await.unwrap();
            while let Ok((stream, _)) = listener.accept().await {
                let (state, expected) = (state.clone(), expected.clone());
                tokio::spawn(async move {
                    // The handshake callback's shape is tungstenite's.
                    #[allow(clippy::result_large_err)]
                    let authorized = |request: &Request, response: Response| {
                        let offered = request
                            .headers()
                            .get("authorization")
                            .and_then(|value| value.to_str().ok());
                        if offered == Some(expected.as_str()) {
                            Ok(response)
                        } else {
                            let mut refused = ErrorResponse::new(None);
                            *refused.status_mut() = StatusCode::UNAUTHORIZED;
                            Err(refused)
                        }
                    };
                    let Ok(mut socket) = tokio_tungstenite::accept_hdr_async(stream, authorized).await
                    else {
                        return;
                    };
                    let (outgoing, mut queued) = tokio::sync::mpsc::unbounded_channel();
                    state.lock().unwrap().connections.push(outgoing.clone());
                    loop {
                        tokio::select! {
                            incoming = socket.next() => {
                                let Some(Ok(message)) = incoming else { return };
                                let Message::Text(text) = message else {
                                    if message.is_close() { return }
                                    continue;
                                };
                                let request: Value = serde_json::from_str(&text).unwrap();
                                let Some(id) = request.get("id").cloned() else { continue };
                                let method = request["method"].as_str().unwrap_or_default().to_owned();
                                // Answered after whatever the request announced,
                                // on this connection as on every other.
                                let reply = match state.lock().unwrap().answer(&method) {
                                    Ok(result) => json!({"id": id, "result": result}),
                                    Err(message) => json!({"id": id,
                                        "error": {"code": -32601, "message": message}}),
                                };
                                let _ = outgoing.send(reply);
                            }
                            frame = queued.recv() => {
                                let Some(frame) = frame else { return };
                                if socket.send(Message::text(frame.to_string())).await.is_err() {
                                    return;
                                }
                            }
                        }
                    }
                });
            }
        });
        0
    }

    /// `codex --no-alt-screen --remote ws://127.0.0.1:<port>
    /// --remote-auth-token-env <name>`: the full-screen program.
    pub fn terminal(arguments: &[String]) -> i32 {
        orphan_watch();
        let fixture = PathBuf::from(std::env::var_os(NATIVE).unwrap());
        let url = option(arguments, "--remote").to_owned();
        let token = std::env::var(option(arguments, "--remote-auth-token-env")).unwrap();
        let mut screen = std::io::stdout();
        writeln!(screen, "{STARTED}").unwrap();
        screen.flush().unwrap();
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let mut request = url.as_str().into_client_request().unwrap();
            request.headers_mut().insert(
                "authorization",
                HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
            );
            let stream = tokio::net::TcpStream::connect(url.strip_prefix("ws://").unwrap())
                .await
                .unwrap();
            let (mut socket, _) = tokio_tungstenite::client_async(request, stream)
                .await
                .unwrap();
            for (id, method) in [(1, "initialize"), (2, "thread/start")] {
                socket
                    .send(Message::text(
                        json!({"id": id, "method": method, "params": {}}).to_string(),
                    ))
                    .await
                    .unwrap();
                loop {
                    let Some(Ok(Message::Text(text))) = socket.next().await else {
                        panic!("the server closed before answering {method}");
                    };
                    if serde_json::from_str::<Value>(&text).unwrap()["id"] == id {
                        break;
                    }
                }
            }
            let deadline = Instant::now() + Duration::from_secs(60);
            while !fixture.join("quit").exists() && Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        });
        writeln!(screen, "{ENDED}").unwrap();
        screen.flush().unwrap();
        0
    }
}

/// `agentdocker codex` as a person runs it, end to end against the fixture
/// daemon, with this binary as both Codex children and a file as the
/// terminal (standard output and error both, as a terminal would be). Once
/// the Codex screen is up, nothing AgentDocker writes reaches the terminal:
/// readiness shows on the agent's record and in the launch's own log, and
/// the receiver's output is its own log's, never the screen's.
#[cfg(unix)]
fn the_native_launcher_writes_nothing_over_the_codex_screen() {
    use std::process::{Command, Stdio};
    let root = fixture::root();
    let home = root.path().join("home");
    let socket = root.path().join("agentd.sock");
    let checkout = root.path().join("checkout");
    let codex_home = root.path().join("codex-home");
    std::fs::create_dir_all(&checkout).unwrap();
    std::fs::create_dir_all(&codex_home).unwrap();
    let checkout = checkout.canonicalize().unwrap();
    let codex_home = codex_home.canonicalize().unwrap();
    let daemon = fixture::RunningDaemon::start(&home, &socket);
    let exe = std::env::current_exe().unwrap().canonicalize().unwrap();
    let terminal = root.path().join("terminal");
    let screen = std::fs::File::create(&terminal).unwrap();
    let mut launch = Command::new(env!("CARGO_BIN_EXE_agentdocker"))
        .arg("--socket")
        .arg(&socket)
        .arg("codex-native")
        .arg("--program")
        .arg(&exe)
        .arg("--profile")
        .arg(&codex_home)
        .arg("--cwd")
        .arg(&checkout)
        .arg("--name")
        .arg("native")
        .env("AGENTDOCKER_HOME", &home)
        .env("AGENTDOCKER_SOCKET", &socket)
        .env("AGENTDOCKER_NO_AUTOSTART", "1")
        .env(NATIVE, root.path())
        .env_remove("AGENTDOCKER_AGENT_ID")
        .env_remove("AGENTDOCKER_AGENT_NAME")
        .env_remove("AGENTDOCKER_TOKEN_FILE")
        .env_remove("AGENTDOCKER_TRACE_NATIVE_STARTUP")
        .env_remove(TRANSCRIPT)
        .stdin(Stdio::null())
        .stdout(Stdio::from(screen.try_clone().unwrap()))
        .stderr(Stdio::from(screen))
        .spawn()
        .unwrap();
    let shown = || std::fs::read_to_string(&terminal).unwrap_or_default();
    // Ready as the daemon tells it: the receiver bound the agent's queue.
    let deadline = Instant::now() + Duration::from_secs(45);
    let agent = loop {
        let listed = fixture::rpc(&socket, json!({"op": "list"})).unwrap();
        let bound = listed["agents"].as_array().and_then(|agents| {
            agents
                .iter()
                .find(|agent| {
                    agent["spec"]["name"] == "native" && agent["input_binding"].is_object()
                })
                .cloned()
        });
        if let Some(agent) = bound {
            break agent;
        }
        if Instant::now() >= deadline || launch.try_wait().unwrap().is_some() {
            let _ = launch.kill();
            let _ = launch.wait();
            panic!(
                "the native launch never became ready; terminal:\n{}\nagents: {listed}",
                shown()
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let id = agent["id"].as_str().unwrap().to_owned();
    // Whatever the launcher would say once ready has had its chance.
    std::thread::sleep(Duration::from_millis(500));
    std::fs::write(root.path().join("quit"), "").unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = launch.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = launch.kill();
            let _ = launch.wait();
            panic!(
                "the native launch did not end with its terminal:\n{}",
                shown()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let shown = shown();
    let begins = shown
        .find(native::STARTED)
        .unwrap_or_else(|| panic!("the screen never began:\n{shown}"));
    let ends = shown
        .find(native::ENDED)
        .unwrap_or_else(|| panic!("the screen never ended:\n{shown}"));
    let under_the_screen = shown[begins + native::STARTED.len()..ends].trim_matches('\n');
    assert_eq!(
        under_the_screen, "",
        "AgentDocker wrote over the Codex screen:\n{shown}"
    );
    assert!(status.success(), "{status:?}:\n{shown}");
    // Where readiness went instead: the launch's own log.
    let launches: Vec<_> = std::fs::read_dir(home.join("codex-native"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(launches.len(), 1, "{launches:?}");
    let logged = std::fs::read_to_string(launches[0].join("launcher.log")).unwrap();
    assert!(
        logged.contains(&format!("native input ready: {id}")),
        "{logged}"
    );
    assert!(launches[0].join("receiver.log").exists());
    drop(daemon);
}

#[cfg(not(unix))]
#[allow(dead_code)]
fn the_native_launcher_writes_nothing_over_the_codex_screen() {}
#[cfg(not(unix))]
#[allow(dead_code)]
fn the_shared_server_listing_never_starts_a_daemon() {}
#[cfg(not(unix))]
#[allow(dead_code)]
fn a_shared_server_conversation_is_fed_through_its_control_socket_once() {}
#[cfg(not(unix))]
#[allow(dead_code)]
fn a_message_during_a_running_turn_is_steered_into_it_once() {}
#[cfg(not(unix))]
#[allow(dead_code)]
fn a_steer_refused_for_a_changed_turn_is_delivered_once_by_the_native_queue() {}
#[cfg(not(unix))]
#[allow(dead_code)]
fn a_turn_waiting_on_approval_takes_no_steer() {}
#[cfg(not(unix))]
#[allow(dead_code)]
fn a_steer_whose_answer_is_lost_is_reconciled_without_a_resend() {}
#[cfg(not(unix))]
#[allow(dead_code)]
fn a_queued_message_becomes_a_turn_with_a_receipt_proved_from_the_providers_item() {}
#[cfg(not(unix))]
#[allow(dead_code)]
fn an_existing_codex_session_is_fed_through_its_native_queue_with_a_proved_receipt() {}
#[cfg(not(unix))]
#[allow(dead_code)]
fn a_refused_turn_leaves_the_message_queued_and_reports_the_provider() {}
#[cfg(not(unix))]
#[allow(dead_code)]
fn an_app_server_older_than_the_floor_is_refused_before_any_thread_is_touched() {}
