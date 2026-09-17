//! A command's exit status says what class of thing went wrong, so an
//! agent driving the command line can branch on it without parsing text.
//! The daemon here is a fake that answers one request with one error.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::{Value, json};

/// Answer the one request with `answer`, then `end`. The worker is joined
/// by the test, so a failure inside it fails the test, and every read and
/// write is bounded.
struct FakeDaemon(Option<std::thread::JoinHandle<()>>);

impl FakeDaemon {
    fn serve(socket: &Path, answer: Value) -> Self {
        let listener = UnixListener::bind(socket).unwrap();
        Self(Some(std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("one connection");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            reader.read_line(&mut request).expect("a request line");
            assert!(request.contains("\"op\""), "{request}");
            let mut stream = stream;
            stream
                .write_all(format!("{answer}\n").as_bytes())
                .expect("answer written");
            stream
                .write_all(format!("{}\n", json!({"type":"end"})).as_bytes())
                .expect("end written");
        })))
    }

    fn finish(mut self) {
        self.0
            .take()
            .unwrap()
            .join()
            .expect("the fake daemon served");
    }
}

/// The command under a private home and socket, with nothing inherited
/// from the surrounding session's routing or credentials.
fn cli(home: &Path, socket: &Path, args: &[&str]) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_agentdocker"));
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("AGENTDOCKER_") {
            command.env_remove(key);
        }
    }
    command
        .args(args)
        .env("AGENTDOCKER_HOME", home)
        .env("AGENTDOCKER_SOCKET", socket)
        .env("AGENTDOCKER_NO_AUTOSTART", "1")
        .output()
        .unwrap()
}

fn private_socket(name: &str) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::Builder::new()
        .prefix("ad-exit-")
        .tempdir_in("/tmp")
        .unwrap();
    let socket = tmp.path().join(format!("{name}.sock"));
    (tmp, socket)
}

/// Each class of daemon answer is its own exit status, the words and
/// details still go to stderr, a usage error is the parser's own 2, and
/// no daemon at all is unexpected rather than any class of answer.
#[test]
fn the_exit_status_is_the_class_of_what_went_wrong() {
    for (code, status, message) in [
        ("not_found", 3, "no agent matches ghost"),
        ("ambiguous", 3, "more than one agent matches g"),
        ("conflict", 4, "the card is held by somebody"),
        ("deadlock", 4, "waiting would deadlock"),
        ("forbidden", 5, "only the card's assignee moves it"),
        ("paused", 5, "the project is paused: lunch"),
        ("storage_unavailable", 6, "storage failed"),
        ("unavailable", 6, "not now"),
        ("internal", 1, "something unexpected"),
    ] {
        let (tmp, socket) = private_socket(code);
        let daemon = FakeDaemon::serve(
            &socket,
            json!({"type":"error","code":code,"message":message,"details":{"why":code}}),
        );
        let output = cli(tmp.path(), &socket, &["inspect", "ghost"]);
        daemon.finish();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(status), "{code}: {stderr}");
        assert!(stderr.contains(message), "{code}: {stderr}");
        assert!(stderr.contains("\"why\""), "{code}: details kept: {stderr}");
    }
    let (tmp, socket) = private_socket("usage");
    let output = cli(tmp.path(), &socket, &["inspect"]);
    assert_eq!(output.status.code(), Some(2), "usage is the parser's own");
    // No daemon at all is not the daemon's answer: unexpected, with the
    // connection's own words.
    let (tmp, socket) = private_socket("nobody");
    let output = cli(tmp.path(), &socket, &["inspect", "ghost"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("cannot reach agentd"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let (tmp, socket) = private_socket("ok");
    let daemon = FakeDaemon::serve(
        &socket,
        json!({"type":"pong","version":"0.0.0-fake","uptime_secs":3}),
    );
    let output = cli(tmp.path(), &socket, &["ping"]);
    daemon.finish();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
