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
//! test runner's own output off that stream.
#[cfg(unix)]
use serde_json::{Value, json};
#[cfg(unix)]
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::time::{Duration, Instant};

const TRANSCRIPT: &str = "AGENTDOCKER_TEST_APP_SERVER_TRANSCRIPT";
/// The version the mock claims in `initialize`; 0.160.0 unless set.
const VERSION: &str = "AGENTDOCKER_TEST_APP_SERVER_VERSION";

fn main() {
    if std::env::args().nth(1).as_deref() == Some("app-server")
        && std::env::var_os(TRANSCRIPT).is_some()
    {
        std::process::exit(mock::serve());
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
    ];
    let filter = std::env::args()
        .skip(1)
        .find(|argument| !argument.starts_with("--"));
    let (mut passed, mut failed) = (0, 0);
    for (name, test) in tests {
        if filter
            .as_deref()
            .is_some_and(|wanted| !name.contains(wanted))
        {
            continue;
        }
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

/// The app-server the bridge talks to: enough of the protocol to start a
/// thread, accept a turn, say which user item carried the input and end
/// the turn. A turn whose text asks for a refusal is refused with the
/// provider's structured usage-limit error.
mod mock {
    use super::TRANSCRIPT;
    use serde_json::{Value, json};
    use std::io::{BufRead, Write};

    pub fn serve() -> i32 {
        let transcript = std::env::var_os(TRANSCRIPT).expect("transcript path");
        let mut log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(transcript)
            .expect("transcript file");
        let stdin = std::io::stdin();
        let mut out = std::io::stdout().lock();
        let cwd = std::env::current_dir().expect("cwd").display().to_string();
        let mut turns = Vec::new();
        for line in stdin.lock().lines() {
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }
            writeln!(log, "{line}").expect("transcript write");
            let value: Value = serde_json::from_str(&line).expect("a JSON frame");
            let Some(id) = value.get("id").cloned() else {
                // `initialized` and other notifications need no answer.
                continue;
            };
            let params = value["params"].clone();
            let mut frames = Vec::new();
            let mut reply = |result: Value| frames.push(json!({"id": id, "result": result}));
            match value["method"].as_str().unwrap_or_default() {
                "initialize" => reply(json!({
                    "userAgent": format!("codex_cli_rs/{} (Mock 1.0.0; test) mock-app-server",
                        std::env::var(super::VERSION).unwrap_or_else(|_| "0.160.0".into())),
                    "codexHome": std::env::var("CODEX_HOME").unwrap_or_default(),
                    "platformFamily": "unix", "platformOs": "linux",
                })),
                "config/read" => reply(json!({"config": {"mcp_servers": {}}})),
                "hooks/list" => reply(json!({"data": [{"cwd": params["cwds"][0], "errors": []}]})),
                "thread/start" => {
                    reply(json!({"thread": {"id": "thread-1", "cwd": params["cwd"]}}))
                }
                "thread/resume" => reply(json!({"thread": {"id": params["threadId"], "cwd": cwd}})),
                "thread/items/list" => reply(json!({"data": []})),
                "thread/turns/list" => reply(json!({"data": turns.iter().rev().map(|turn: &String|
                    json!({"id": turn, "status": "completed"})).collect::<Vec<_>>()})),
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
                        frames.push(json!({"id": id, "error": {
                            "code": -32000, "message": "usage limit reached",
                            "data": {"codexErrorInfo": "usageLimitExceeded"}}}));
                    } else {
                        let turn = format!("turn-{}", turns.len() + 1);
                        let item = format!("item-{}", turns.len() + 1);
                        turns.push(turn.clone());
                        frames.push(json!({"id": id, "result": {"turn": {"id": turn}}}));
                        frames.push(json!({"method": "item/started", "params": {
                            "threadId": "thread-1", "turnId": turn,
                            "item": {"type": "userMessage", "id": item,
                                "content": [{"type": "text", "text": text, "text_elements": []}]}}}));
                        frames.push(json!({"method": "turn/completed", "params": {
                            "threadId": "thread-1", "turn": {"id": turn, "status": "completed"}}}));
                    }
                }
                "turn/steer" => frames.push(json!({"id": id, "error": {
                    "code": -32600, "message": "no active turn to steer"}})),
                other => frames.push(json!({"id": id, "error": {
                    "code": -32601, "message": format!("{other} is not supported by the mock")}})),
            }
            for frame in frames {
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
        let root = tempfile::tempdir().unwrap();
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
    assert!(bridge.inbox().is_empty() || bridge.inbox()[0]["id"] != second.as_str());
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

#[cfg(not(unix))]
#[allow(dead_code)]
fn a_queued_message_becomes_a_turn_with_a_receipt_proved_from_the_providers_item() {}
#[cfg(not(unix))]
#[allow(dead_code)]
fn a_refused_turn_leaves_the_message_queued_and_reports_the_provider() {}
#[cfg(not(unix))]
#[allow(dead_code)]
fn an_app_server_older_than_the_floor_is_refused_before_any_thread_is_touched() {}
