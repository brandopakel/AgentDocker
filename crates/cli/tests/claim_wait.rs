//! What `agentdocker claim` asks of the daemon: the wait it was given, in
//! full, on its first ask, and a fresh lease only from a daemon that says
//! it knows one. The daemon here is a fake that records each request and
//! answers from a list.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::{Value, json};

/// Answer each request in turn from `answers`, and hand back the requests
/// as they were read. Every accept, read and write is bounded, so a
/// command that sends fewer requests than expected fails the test rather
/// than hanging it.
fn serve(socket: &Path, answers: Vec<Value>) -> std::thread::JoinHandle<Vec<Value>> {
    let listener = UnixListener::bind(socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    std::thread::spawn(move || {
        let mut requests = Vec::new();
        for answer in answers {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            let stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if std::time::Instant::now() >= deadline {
                            // The command sent no further request.
                            return requests;
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("accept failed: {error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            reader.read_line(&mut request).expect("a request line");
            requests.push(serde_json::from_str(&request).expect("a request is JSON"));
            let mut stream = stream;
            stream
                .write_all(format!("{answer}\n{}\n", json!({"type":"end"})).as_bytes())
                .expect("answer and end written");
        }
        requests
    })
}

/// The command under a private home and socket, with nothing inherited
/// from the surrounding session.
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
        .prefix("ad-claim-")
        .tempdir_in("/tmp")
        .unwrap();
    let socket = tmp.path().join(format!("{name}.sock"));
    (tmp, socket)
}

fn held_by_another() -> Value {
    json!({
        "type": "error",
        "code": "conflict",
        "message": "task:slot is held by builder (abc; exclusive on task:slot)",
        "details": {"held_by": []},
    })
}

/// Behind another agent's lease, a claim asks the daemon to wait for all
/// of the time it was given: `--wait 1` is one second, not what is left of
/// it after the command built its request, which rounds down to none and
/// would have the daemon refuse at once.
#[test]
fn a_claim_asks_for_its_whole_wait_behind_another_holder() {
    for wait in ["1", "2", "600"] {
        let (tmp, socket) = private_socket("wait");
        let daemon = serve(&socket, vec![held_by_another()]);
        let output = cli(
            tmp.path(),
            &socket,
            &["claim", "task:slot", "--as", "worker", "--wait", wait],
        );
        let requests = daemon.join().expect("the fake daemon served");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(4), "{wait}: {stderr}");
        assert_eq!(requests.len(), 1, "{wait}: {requests:?}");
        assert_eq!(requests[0]["op"], "claim", "{wait}");
        assert_eq!(
            requests[0]["wait_secs"].as_u64(),
            wait.parse().ok(),
            "{wait}: {}",
            requests[0]
        );
        assert!(requests[0].get("fresh").is_none(), "a plain claim");
    }
}

/// A fresh claim is asked only of a daemon that says it knows one. One
/// that does not is sent nothing but the question: it would renew the
/// lease another worker holds and answer as if it had granted it. One
/// that does gets the claim, marked fresh, with its whole wait.
#[test]
fn a_fresh_claim_is_asked_only_of_a_daemon_that_knows_it() {
    let pong = |features: Value| json!({"type": "pong", "version": "test", "uptime_secs": 1, "features": features});

    let (tmp, socket) = private_socket("old");
    // One answer only: a claim sent after it would find nobody listening,
    // and the command would say so instead of the refusal below.
    let daemon = serve(&socket, vec![pong(json!([]))]);
    let output = cli(
        tmp.path(),
        &socket,
        &[
            "claim",
            "task:slot",
            "--as",
            "worker",
            "--fresh",
            "--wait",
            "1",
        ],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_ne!(output.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains("does not know a fresh claim"), "{stderr}");
    assert!(stderr.contains("Nothing was claimed"), "{stderr}");
    drop(output);
    let requests = daemon.join().expect("the fake daemon served");
    assert_eq!(requests.len(), 1, "{requests:?}");
    assert_eq!(requests[0]["op"], "ping");

    let (tmp, socket) = private_socket("new");
    let daemon = serve(
        &socket,
        vec![pong(json!(["fresh_claim"])), held_by_another()],
    );
    let output = cli(
        tmp.path(),
        &socket,
        &[
            "claim",
            "task:slot",
            "--as",
            "worker",
            "--fresh",
            "--wait",
            "1",
        ],
    );
    let requests = daemon.join().expect("the fake daemon served");
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Another agent holds it: the daemon's own conflict, not asked again.
    assert_eq!(output.status.code(), Some(4), "{stderr}");
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert_eq!(requests[1]["op"], "claim");
    assert_eq!(requests[1]["fresh"], true);
    assert_eq!(requests[1]["wait_secs"], 1);
}
