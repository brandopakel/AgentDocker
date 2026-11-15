//! What `agentdocker plan edit` asks of the daemon. An edit of a plan's
//! text names the revision it was written from, and a daemon that does
//! not hold an edit to its revision would take the request and write the
//! text over whatever is there: so the command asks first, and sends such
//! an edit only to a daemon that says it holds it. The daemon here is a
//! fake that records each request and answers from a list.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;
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
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
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

/// One command against a fake daemon that has `answers` to give, run in
/// `folder` with nothing inherited from the surrounding session: what it
/// asked, and how it ended.
fn ask(folder: &Path, answers: Vec<Value>, args: &[&str]) -> (Vec<Value>, std::process::Output) {
    let home = tempfile::Builder::new()
        .prefix("ad-plan-")
        .tempdir_in("/tmp")
        .unwrap();
    let socket = home.path().join("d.sock");
    let daemon = serve(&socket, answers);
    let mut command = Command::new(env!("CARGO_BIN_EXE_agentdocker"));
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("AGENTDOCKER_") {
            command.env_remove(key);
        }
    }
    let output = command
        .args(args)
        .current_dir(folder)
        .env("AGENTDOCKER_HOME", home.path())
        .env("AGENTDOCKER_SOCKET", &socket)
        .env("AGENTDOCKER_NO_AUTOSTART", "1")
        .output()
        .unwrap();
    // The listener goes with the fake daemon's thread; a command that sent
    // nothing leaves it waiting out its bound, which is the answer wanted.
    (daemon.join().expect("the fake daemon served"), output)
}

/// The requests as the command meant them: without what the client adds
/// to every request it sends.
fn meant(requests: Vec<Value>) -> Vec<Value> {
    requests
        .into_iter()
        .map(|mut request| {
            if let Some(fields) = request.as_object_mut() {
                fields.remove("handover_retry");
            }
            request
        })
        .collect()
}

fn pong(features: &[&str]) -> Value {
    json!({"type": "pong", "version": "test", "uptime_secs": 1, "features": features})
}

fn plan(revision: u64) -> Value {
    json!({"type": "plan", "tasks": [], "plan": {
        "id": "abc123def456", "project": "p", "title": "Where the work stands", "body": "",
        "status": "draft", "created_by": "worker",
        "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-01T00:00:00Z",
        "revision": revision,
    }})
}

/// A folder to run in, holding the text of an edit.
fn folder() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("plan.md"), "## Lead\nmerged\n").unwrap();
    tmp
}

const EDIT: [&str; 9] = [
    "plan",
    "edit",
    "abc123def456",
    "--as",
    "worker",
    "--body",
    "plan.md",
    "--revision",
    "0",
];

/// A daemon that lists no such feature is asked and sent nothing more:
/// it would write the text whatever revision the edit names, and two
/// writers who both read its plans as revision zero would both be told
/// they had saved.
#[test]
fn an_edit_of_a_plans_text_is_not_sent_to_a_daemon_that_ignores_its_revision() {
    let tmp = folder();
    for features in [&[][..], &["fresh_claim", "digest_max_bytes"]] {
        // One answer only: an edit sent after it would find nobody listening,
        // and the command would say that instead of the refusal below.
        let (requests, output) = ask(tmp.path(), vec![pong(features)], &EDIT);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{features:?}: {stderr}");
        assert!(
            stderr.contains("Nothing was sent"),
            "{features:?}: {stderr}"
        );
        assert!(stderr.contains("daemon restart"), "{features:?}: {stderr}");
        assert_eq!(meant(requests), [json!({"op": "ping"})], "{features:?}");
    }
}

/// A daemon that says it holds an edit to its revision gets it, with the
/// revision named: zero too, which is what a plan stored before
/// revisions were kept is at.
#[test]
fn an_edit_of_a_plans_text_names_its_revision_to_a_daemon_that_holds_it() {
    let tmp = folder();
    let (requests, output) = ask(
        tmp.path(),
        vec![pong(&["fresh_claim", "plan_revision"]), plan(1)],
        &EDIT,
    );
    assert!(output.status.success(), "{output:?}");
    let requests = meant(requests);
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert_eq!(requests[0], json!({"op": "ping"}));
    assert_eq!(requests[1]["op"], "plan_save");
    assert_eq!(requests[1]["plan"], "abc123def456");
    assert_eq!(requests[1]["from"], "worker");
    assert_eq!(requests[1]["body"], "## Lead\nmerged\n");
    assert_eq!(requests[1]["revision"], 0);
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("revision 1"),
        "the answer's revision is shown: {output:?}"
    );
}

/// A change of status replaces no text: it is sent without the question
/// and names no revision.
#[test]
fn a_change_of_a_plans_status_asks_nothing_first() {
    let tmp = folder();
    let (requests, output) = ask(
        tmp.path(),
        vec![plan(3)],
        &[
            "plan",
            "edit",
            "abc123def456",
            "--as",
            "worker",
            "--status",
            "done",
        ],
    );
    assert!(output.status.success(), "{output:?}");
    let requests = meant(requests);
    assert_eq!(requests.len(), 1, "{requests:?}");
    assert_eq!(requests[0]["op"], "plan_save");
    assert_eq!(requests[0]["status"], "done");
    assert!(requests[0].get("revision").is_none(), "{}", requests[0]);
    assert!(
        requests[0].get("body").is_none_or(Value::is_null),
        "{}",
        requests[0]
    );
}
