//! `watch` on a real daemon replaying what a topic kept: a reader with no
//! cursor that subscribes after more messages were said than one watch
//! replays is shown the newest, told how many older ones were not
//! replayed and why, and is not told it fell behind the live stream.
//! Every command and endpoint belongs to the fixture; no installed daemon,
//! provider account or user config is used.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use agentdocker_core::topic::REPLAY_PER_TOPIC;
use serde_json::{Value, json};

struct RunningDaemon {
    child: Child,
    socket: PathBuf,
}

impl RunningDaemon {
    fn start(home: &Path, socket: &Path) -> Self {
        let log =
            agentdocker_host::dirs::private_file(&home.with_extension("daemon.log"), true, true)
                .unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_agentdocker"))
            .args(["daemon", "run"])
            .arg("--home")
            .arg(home)
            .arg("--socket")
            .arg(socket)
            // The harness acts as the person and as the reader from
            // wherever it runs, an agent's session included.
            .env("AGENTDOCKER_PERSON_CHECK", "off")
            .env("AGENTDOCKER_AGENT_CHECK", "off")
            .env("AGENTDOCKER_HOME", home)
            .env("AGENTDOCKER_SOCKET", socket)
            .env("AGENTDOCKER_NO_AUTOSTART", "1")
            .env_remove("AGENTDOCKER_TOKEN_FILE")
            .env_remove("AGENTDOCKER_AGENT_ID")
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
            if rpc(socket, json!({"op": "ping"})).is_ok_and(|response| response["type"] == "pong") {
                return running;
            }
            assert!(Instant::now() < deadline, "daemon did not become ready");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for RunningDaemon {
    fn drop(&mut self) {
        let _ = rpc(&self.socket, json!({"op": "shutdown"}));
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.child.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
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

#[test]
fn watch_explains_a_bounded_topic_replay_rather_than_a_live_lag() {
    // Short enough for a Unix socket path.
    let tmp = tempfile::Builder::new()
        .prefix("ad-topic-")
        .tempdir_in("/tmp")
        .unwrap();
    let home = tmp.path().join("home");
    let work = tmp.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let socket = tmp.path().join("d.sock");
    let daemon = RunningDaemon::start(&home, &socket);

    let registered = rpc(
        &socket,
        json!({"op": "register", "spec": {"name": "reader", "workdir": work}}),
    )
    .unwrap();
    assert_eq!(registered["type"], "agent", "{registered}");
    let over = 7;
    let said = REPLAY_PER_TOPIC + over;
    for n in 0..said {
        let sent = rpc(
            &socket,
            json!({"op": "send", "from": "user", "to": "topic:builds/status",
                   "kind": "notice", "payload": {"text": format!("build {n}")}}),
        )
        .unwrap();
        assert_eq!(sent["type"], "sent", "{sent}");
    }

    let mut watch = Command::new(env!("CARGO_BIN_EXE_agentdocker"))
        .args(["watch", "--as", "reader", "builds/#"])
        .env("AGENTDOCKER_SOCKET", &socket)
        .env("AGENTDOCKER_HOME", &home)
        .env("AGENTDOCKER_NO_AUTOSTART", "1")
        .env_remove("AGENTDOCKER_TOKEN_FILE")
        .env_remove("AGENTDOCKER_AGENT_ID")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = watch.stdout.take().unwrap();
    let (lines_tx, lines) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if lines_tx.send(line).is_err() {
                return;
            }
        }
    });
    let mut shown = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    while shown.len() < REPLAY_PER_TOPIC {
        let left = deadline.saturating_duration_since(Instant::now());
        match lines.recv_timeout(left) {
            Ok(line) => shown.push(line),
            Err(_) => panic!("watch showed {} of {REPLAY_PER_TOPIC} lines", shown.len()),
        }
    }
    let _ = watch.kill();
    let _ = watch.wait();
    let mut stderr = String::new();
    watch
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    drop(daemon);

    assert!(
        shown[0].contains(&format!("build {over}")),
        "the oldest replayed is the first kept past the cap: {}",
        shown[0]
    );
    assert!(
        shown
            .last()
            .unwrap()
            .contains(&format!("build {}", said - 1))
    );
    assert!(
        shown.iter().all(|line| line.contains("seq:")),
        "each topic line shows where it stands"
    );
    assert!(
        stderr.contains(&format!("{over} older kept topic message(s) not replayed"))
            && stderr.contains(&format!("newest {REPLAY_PER_TOPIC}")),
        "{stderr}"
    );
    assert!(
        !stderr.contains("live messages"),
        "a replay's bound is not a live lag: {stderr}"
    );
}
