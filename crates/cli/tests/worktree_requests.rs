//! What `agentdocker worktree-create` and `worktree-remove` ask of the
//! daemon: a path only when one was named, made absolute where the command
//! runs (the daemon would read a relative one against its own folder), and
//! the request that fits how the worktree to remove was named. The daemon
//! here is a fake that records each request and answers from a list.
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
        .prefix("ad-wt-")
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

fn made() -> Value {
    json!({"type": "worktree", "path": "/work/app/.agentdocker/worktrees/agent-fix", "branch": "agent/fix"})
}

fn removed() -> Value {
    json!({"type": "worktree_removed", "path": "/work/fix", "branch_removed": true})
}

/// A folder to run in, with its resolved path: what a relative path named
/// there has to become.
fn folder() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let inside = tmp.path().join("app");
    std::fs::create_dir(&inside).unwrap();
    let resolved = inside.canonicalize().unwrap();
    (tmp, resolved)
}

/// `worktree-create` sends no path when none was named, so the daemon
/// chooses the project's folder, and sends a named one as the place it is
/// where the command runs.
#[test]
fn a_worktree_is_asked_for_where_the_command_runs() {
    let (_tmp, app) = folder();
    let (requests, output) = ask(
        &app,
        vec![made()],
        &["worktree-create", "--as", "worker", "--branch", "agent/fix"],
    );
    assert!(output.status.success(), "{output:?}");
    assert_eq!(requests.len(), 1, "{requests:?}");
    assert_eq!(requests[0]["op"], "worktree_create");
    assert_eq!(requests[0]["agent"], "worker");
    assert!(requests[0].get("path").is_none(), "{}", requests[0]);
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "/work/app/.agentdocker/worktrees/agent-fix",
        "the place the daemon chose is what the command prints"
    );

    let (requests, output) = ask(
        &app,
        vec![made()],
        &[
            "worktree-create",
            "--as",
            "worker",
            "--branch",
            "agent/fix",
            "../fix",
        ],
    );
    assert!(output.status.success(), "{output:?}");
    let beside = app.parent().unwrap().join("fix");
    assert_eq!(
        requests[0]["path"],
        beside.to_string_lossy().as_ref(),
        "{}",
        requests[0]
    );
}

/// `worktree-remove <agent>` is an ended session's own worktree;
/// `--path` and `--branch` name any worktree nobody works in, asked as
/// the caller. The two are different requests, and naming neither, or an
/// agent with either, asks nothing.
#[test]
fn a_worktree_is_removed_by_session_or_by_name() {
    let (_tmp, app) = folder();

    let (requests, output) = ask(&app, vec![removed()], &["worktree-remove", "ended"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        meant(requests),
        [json!({"op": "worktree_remove", "agent": "ended"})]
    );

    let (requests, output) = ask(
        &app,
        vec![removed()],
        &["worktree-remove", "--as", "worker", "--branch", "agent/fix"],
    );
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        meant(requests),
        [json!({"op": "worktree_remove_named", "agent": "worker", "branch": "agent/fix"})]
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("removed /work/fix and its branch"),
        "{output:?}"
    );

    let (requests, output) = ask(
        &app,
        vec![removed()],
        &["worktree-remove", "--as", "worker", "--path", "../fix"],
    );
    assert!(output.status.success(), "{output:?}");
    let beside = app.parent().unwrap().join("fix");
    assert_eq!(
        meant(requests),
        [json!({"op": "worktree_remove_named", "agent": "worker", "path": beside})]
    );

    // Refused before anything is asked: nobody listens here, so a command
    // that asked anyway would say it could not connect instead.
    for (refused, why) in [
        (&["worktree-remove"][..], "Usage"),
        (
            &["worktree-remove", "ended", "--branch", "agent/fix"],
            "Usage",
        ),
        (
            &[
                "worktree-remove",
                "--path",
                "../fix",
                "--branch",
                "agent/fix",
            ],
            "Usage",
        ),
        // Nobody to ask as: this test's environment names no session.
        (
            &["worktree-remove", "--branch", "agent/fix"],
            "say who asks",
        ),
    ] {
        let (requests, output) = ask(&app, Vec::new(), refused);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{refused:?}: {stderr}");
        assert!(stderr.contains(why), "{refused:?}: {stderr}");
        assert!(requests.is_empty(), "{refused:?}: {requests:?}");
    }
}
