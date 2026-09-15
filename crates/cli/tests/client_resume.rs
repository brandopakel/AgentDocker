//! What the CLI does when the daemon underneath it is replaced: a fake
//! daemon on a private socket answers each connection by script, so the
//! transfer window, the replaced-stream resubscription and the reload wait
//! are exercised exactly, with no timing left to a real handover.
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// One connection's script: what to send back for the request it opens
/// with, and whether to say `end` or just close afterwards.
#[derive(Clone)]
struct Answer {
    /// Responses, each written as one line.
    lines: Vec<Value>,
    /// Close the connection with nothing further; `false` says `end` first.
    silent_close: bool,
}

fn line(value: Value) -> String {
    format!("{value}\n")
}

/// Serve `answers` one connection each, recording the `op` of every
/// request seen. Stops accepting after the last answer.
fn fake_daemon(socket: &Path, answers: Vec<Answer>) -> Arc<Mutex<Vec<String>>> {
    let listener = UnixListener::bind(socket).unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for answer in answers {
            let Ok((stream, _)) = listener.accept() else {
                return;
            };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            let request: Value = serde_json::from_str(&request).unwrap();
            log.lock()
                .unwrap()
                .push(request["op"].as_str().unwrap_or("?").to_owned());
            let mut stream = stream;
            for value in &answer.lines {
                stream.write_all(line(value.clone()).as_bytes()).unwrap();
            }
            if !answer.silent_close {
                stream
                    .write_all(line(json!({"type":"end"})).as_bytes())
                    .unwrap();
            }
            // Close by drop.
        }
    });
    seen
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
        .prefix("ad-resume-")
        .tempdir_in("/tmp")
        .unwrap();
    let socket = tmp.path().join(format!("{name}.sock"));
    (tmp, socket)
}

fn pong() -> Value {
    json!({"type":"pong","version":"0.0.0-fake","uptime_secs":3})
}

fn transferring() -> Value {
    json!({"type":"error","code":"transferring","message":"coordination is being transferred"})
}

fn event(seq: u64) -> Value {
    json!({"type":"event","event":{"seq":seq,"at":"2026-09-15T00:00:00Z","kind":{"event":"from_a_newer_daemon"}}})
}

fn answer(lines: Vec<Value>, silent_close: bool) -> Answer {
    Answer {
        lines,
        silent_close,
    }
}

/// `transferring` is the daemon saying "not now, nothing applied": the
/// same request is sent again, unchanged, until the successor answers.
#[test]
fn a_transferring_answer_is_retried_until_a_daemon_answers() {
    let (_tmp, socket) = private_socket("retry");
    let seen = fake_daemon(
        &socket,
        vec![
            answer(vec![transferring()], true),
            answer(vec![transferring()], true),
            answer(vec![pong()], true),
        ],
    );
    let started = Instant::now();
    let output = cli(&socket, &["ping"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("agentd 0.0.0-fake up"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("handing over"));
    assert_eq!(*seen.lock().unwrap(), vec!["ping", "ping", "ping"]);
    assert!(
        started.elapsed() >= Duration::from_millis(450),
        "each retry waits its turn"
    );
}

/// A live stream that ends with nothing said, while a daemon still answers
/// on the socket, was a daemon replaced underneath it: the stream is
/// subscribed again on the successor and carries on until `end`.
#[test]
fn a_stream_ended_by_a_replaced_daemon_is_subscribed_again() {
    let (_tmp, socket) = private_socket("resume");
    let seen = fake_daemon(
        &socket,
        vec![
            // The predecessor: ready, one event, then gone.
            answer(vec![json!({"type":"events_ready"}), event(1)], true),
            // Is anyone there? The successor is.
            answer(vec![pong()], true),
            // The successor: ready, one more event, then the end.
            answer(vec![json!({"type":"events_ready"}), event(2)], false),
        ],
    );
    let output = cli(&socket, &["events"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout
            .matches("an event this version does not know")
            .count(),
        2,
        "both daemons' events reached the screen: {stdout}"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("was replaced"));
    assert_eq!(*seen.lock().unwrap(), vec!["events", "ping", "events"]);
}

/// The same silent end with nobody answering afterwards is the daemon
/// stopping: the stream ends, without a claim that it was replaced.
#[test]
fn a_stream_ended_by_a_stopped_daemon_ends() {
    let (_tmp, socket) = private_socket("stopped");
    let seen = fake_daemon(
        &socket,
        vec![answer(vec![json!({"type":"events_ready"}), event(1)], true)],
    );
    let output = std::thread::spawn({
        let socket = socket.clone();
        move || cli(&socket, &["events"])
    });
    // Nothing accepts after the first answer; take the path down as a
    // stopping daemon does.
    let deadline = Instant::now() + Duration::from_secs(5);
    while seen.lock().unwrap().is_empty() {
        assert!(Instant::now() < deadline, "the stream never subscribed");
        std::thread::sleep(Duration::from_millis(10));
    }
    std::fs::remove_file(&socket).unwrap();
    let output = output.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout
            .matches("an event this version does not know")
            .count(),
        1
    );
    assert!(!String::from_utf8_lossy(&output.stderr).contains("was replaced"));
}

/// `daemon reload` waits for a mutation still executing rather than
/// handing the user the daemon's `backpressure` to retry by hand.
#[test]
fn reload_waits_for_a_request_still_executing() {
    let (_tmp, socket) = private_socket("reload");
    let seen = fake_daemon(
        &socket,
        vec![
            answer(
                vec![
                    json!({"type":"error","code":"backpressure","message":"1 mutating request(s) still executing"}),
                ],
                true,
            ),
            answer(vec![json!({"type":"ok"})], true),
        ],
    );
    let output = cli(&socket, &["daemon", "reload"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("reloaded"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("still executing"));
    assert_eq!(*seen.lock().unwrap(), vec!["reload", "reload"]);
}

/// Any other refusal comes straight back with its message.
#[test]
fn reload_reports_a_refusal_at_once() {
    let (_tmp, socket) = private_socket("refused");
    fake_daemon(
        &socket,
        vec![answer(
            vec![
                json!({"type":"error","code":"unavailable","message":"live daemon reload is unavailable: gate off"}),
            ],
            true,
        )],
    );
    let output = cli(&socket, &["daemon", "reload"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("reload is unavailable"));
    assert!(output.stdout.is_empty());
}
