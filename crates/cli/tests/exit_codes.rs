//! A command's exit status says what class of thing went wrong, so an
//! agent driving the command line can branch on it without parsing text.
//! The daemon here is a fake that answers one request with one error.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

fn fake_daemon(socket: &Path, answer: Value) {
    let listener = UnixListener::bind(socket).unwrap();
    std::thread::spawn(move || {
        let Ok((stream, _)) = listener.accept() else {
            return;
        };
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut request = String::new();
        reader.read_line(&mut request).unwrap();
        let mut stream = stream;
        stream.write_all(format!("{answer}\n").as_bytes()).unwrap();
        stream
            .write_all(format!("{}\n", json!({"type":"end"})).as_bytes())
            .unwrap();
    });
}

fn cli(socket: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_agentdocker"))
        .args(args)
        .env("AGENTDOCKER_SOCKET", socket)
        .env("AGENTDOCKER_NO_AUTOSTART", "1")
        .env_remove("AGENTDOCKER_TOKEN_FILE")
        .env_remove("AGENTDOCKER_AGENT_ID")
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
/// details still go to stderr, and a usage error is the parser's own 2.
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
        let (_tmp, socket) = private_socket(code);
        fake_daemon(
            &socket,
            json!({"type":"error","code":code,"message":message,"details":{"why":code}}),
        );
        let output = cli(&socket, &["inspect", "ghost"]);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(status), "{code}: {stderr}");
        assert!(stderr.contains(message), "{code}: {stderr}");
        assert!(stderr.contains("\"why\""), "{code}: details kept: {stderr}");
    }
    let (_tmp, socket) = private_socket("usage");
    let output = cli(&socket, &["inspect"]);
    assert_eq!(output.status.code(), Some(2), "usage is the parser's own");
    // No daemon at all is not the daemon's answer: unexpected, with the
    // connection's own words.
    let (_tmp, socket) = private_socket("nobody");
    let output = cli(&socket, &["inspect", "ghost"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("cannot reach agentd"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let (_tmp, socket) = private_socket("ok");
    fake_daemon(
        &socket,
        json!({"type":"pong","version":"0.0.0-fake","uptime_secs":3}),
    );
    let output = cli(&socket, &["ping"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
