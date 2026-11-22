//! Where `agentdocker authorship` and `agentdocker verdicts` read a pull
//! request number: in the repository of the project read, as the daemon
//! names that project, whatever the working directory or `GH_REPO` say.
//! The daemon here is a fake that records each request and answers from a
//! list; `gh` is a fake that reads the pull request from `pr.json` in the
//! directory it runs in, and answers a branch of its own when `GH_REPO`
//! reaches it.
#![cfg(unix)]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

use agentdocker_core::gate::{AuthorshipCoverage, Unreceipted};
use agentdocker_core::project::{ProjectRef, ProjectSource};
use agentdocker_core::{AgentRecord, AgentSpec, ProjectId, Response};
use serde_json::{Value, json};

/// Answer each request in turn from `answers`, and hand back the requests
/// as they were read; every wait is bounded.
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

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(["-c", "user.email=t@example.invalid", "-c", "user.name=T"])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

/// A committed repository whose pull request 7 is `branch`; its project's
/// id, its repository's fingerprint, as the daemon gives it.
fn repository(dir: &Path, branch: &str) -> String {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["commit", "-q", "--allow-empty", "-m", "first"]);
    std::fs::write(
        dir.join("pr.json"),
        format!("{{\"headRefName\": \"{branch}\"}}"),
    )
    .unwrap();
    agentdocker_host::project::fingerprint(dir, Duration::from_secs(10)).expect("a fingerprint")
}

/// An agent of the project at `root`, as the daemon records it.
fn record(root: &Path, fingerprint: &str) -> AgentRecord {
    let mut record = AgentRecord::new(
        AgentSpec {
            name: "agent-b".into(),
            runtime: "codex".into(),
            ..AgentSpec::default()
        },
        false,
        chrono::Utc::now(),
    );
    record.project = Some(ProjectRef {
        root: root.to_path_buf(),
        worktree: None,
        fingerprint: Some(fingerprint.to_owned()),
        source: ProjectSource::Git,
    });
    record
}

fn answer(response: Response) -> Value {
    serde_json::to_value(response).unwrap()
}

fn receipts(project: &str) -> Value {
    answer(Response::Authorship {
        project: ProjectId::from(project),
        branch: "refs/heads/feature/b".into(),
        receipts: Vec::new(),
        next: None,
        coverage: AuthorshipCoverage {
            retained: 20_000,
            capture_since: None,
            pruned_through: None,
            oldest_retained: None,
            seed_skipped: 0,
            page_may_miss: false,
            unreceipted: Unreceipted::Counted { count: 0 },
        },
    })
}

struct Setting {
    _tmp: tempfile::TempDir,
    here: PathBuf,
    there: PathBuf,
    there_id: String,
    bin: PathBuf,
    sockets: PathBuf,
}

/// Two committed repositories whose pull request 7 names different
/// branches, a working directory in the first, and the fake `gh`.
fn setting() -> Setting {
    let tmp = tempfile::Builder::new()
        .prefix("ad-pr-")
        .tempdir_in("/tmp")
        .unwrap();
    let here = tmp.path().join("here");
    let there = tmp.path().join("there");
    repository(&here, "feature/a");
    let there_id = repository(&there, "feature/b");
    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let gh = bin.join("gh");
    std::fs::write(
        &gh,
        "#!/bin/sh\n[ -n \"$GH_REPO\" ] && { echo '{\"headRefName\": \"from-gh-repo\"}'; exit 0; }\ncat \"$PWD/pr.json\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    let sockets = tmp.path().join("s");
    std::fs::create_dir_all(&sockets).unwrap();
    Setting {
        here,
        there,
        there_id,
        bin,
        sockets,
        _tmp: tmp,
    }
}

/// The command from `setting.here`, its `GH_REPO` naming another
/// repository, the fake `gh` first on its `PATH`, against a fake daemon
/// answering `answers`: its output and the requests it sent.
fn run(setting: &Setting, name: &str, args: &[&str], answers: Vec<Value>) -> (Output, Vec<Value>) {
    let socket = setting.sockets.join(format!("{name}.sock"));
    let daemon = serve(&socket, answers);
    let mut command = Command::new(env!("CARGO_BIN_EXE_agentdocker"));
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("AGENTDOCKER_") {
            command.env_remove(key);
        }
    }
    let path = format!(
        "{}:{}",
        setting.bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = command
        .args(args)
        .current_dir(&setting.here)
        .env("PATH", path)
        .env("GH_REPO", "someone/elsewhere")
        .env("AGENTDOCKER_HOME", setting.sockets.join(name))
        .env("AGENTDOCKER_SOCKET", &socket)
        .env("AGENTDOCKER_NO_AUTOSTART", "1")
        .output()
        .unwrap();
    (output, daemon.join().expect("the fake daemon served"))
}

/// An agent of the other project, asking from this checkout with no
/// project named: its own project's pull request and receipts, for both
/// commands, never this checkout's and never `GH_REPO`'s.
#[test]
fn an_agent_s_pull_request_is_read_in_its_own_project() {
    let setting = setting();
    let agent = answer(Response::Agent {
        agent: record(&setting.there, &setting.there_id),
    });
    for (command, read) in [
        ("authorship", receipts(&setting.there_id)),
        (
            "verdicts",
            answer(Response::Verdicts {
                verdicts: Vec::new(),
                retained: 2000,
            }),
        ),
    ] {
        let (output, requests) = run(
            &setting,
            command,
            &[command, "7", "--as", "agent-b", "--json"],
            vec![agent.clone(), read],
        );
        assert!(output.status.success(), "{command}: {output:?}");
        assert_eq!(requests.len(), 2, "{command}: {requests:?}");
        assert_eq!(requests[0]["op"], "inspect", "{requests:?}");
        assert_eq!(requests[1]["op"], command, "{requests:?}");
        assert_eq!(requests[1]["branch"], "feature/b", "{command}");
        assert_eq!(
            requests[1]["project"],
            setting.there_id.as_str(),
            "{command}"
        );
    }
}

/// A project named by its full id or a unique prefix is the daemon's to
/// resolve, and its pull request is read in the root its records give; a
/// prefix the daemon finds ambiguous sends no read.
#[test]
fn a_named_project_s_pull_request_is_read_in_its_root() {
    let setting = setting();
    let agents = answer(Response::Agents {
        agents: vec![record(&setting.there, &setting.there_id)],
        aliases: BTreeMap::new(),
    });
    for selector in [setting.there_id.clone(), setting.there_id[..8].to_owned()] {
        let (output, requests) = run(
            &setting,
            &format!("named-{}", selector.len()),
            &[
                "authorship",
                "7",
                "--as",
                "agent-b",
                "--project",
                &selector,
                "--json",
            ],
            vec![agents.clone(), receipts(&setting.there_id)],
        );
        assert!(output.status.success(), "{selector}: {output:?}");
        assert_eq!(requests[0]["op"], "list", "{requests:?}");
        assert_eq!(requests[0]["project"], selector.as_str());
        assert_eq!(requests[1]["branch"], "feature/b", "{selector}");
        assert_eq!(requests[1]["project"], setting.there_id.as_str());
    }
    let (output, requests) = run(
        &setting,
        "ambiguous",
        &[
            "authorship",
            "7",
            "--as",
            "agent-b",
            "--project",
            "a",
            "--json",
        ],
        vec![json!({
            "type": "error",
            "code": "ambiguous",
            "message": "project reference a is ambiguous",
        })],
    );
    assert!(!output.status.success(), "{output:?}");
    assert_eq!(requests.len(), 1, "no read after the refusal: {requests:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("ambiguous"),
        "{output:?}"
    );
}
