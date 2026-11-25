//! Where `agentdocker authorship` and `agentdocker verdicts` read a pull
//! request number: in the repository of the project read, as the daemon
//! names that project, whatever the working directory, `GH_REPO` or Git's
//! own variables say. The daemon here is a fake that records each request
//! and answers from a list; `gh` is a fake that reads the pull request from
//! `pr.json` in the directory it runs in, and answers a branch of its own
//! when `GH_REPO`, `GIT_DIR` or `GIT_WORK_TREE` reaches it.
#![cfg(unix)]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use agentdocker_core::gate::{AuthorshipCoverage, Unreceipted};
use agentdocker_core::project::{ProjectRef, ProjectSource};
use agentdocker_core::{AgentRecord, AgentSpec, AgentStatus, ProjectId, Response};
use serde_json::{Value, json};

/// Answer each request in turn from `answers` until they run out or the
/// command is `done`, and hand back the requests as they were read; every
/// wait is bounded. A command that must send no further request is served
/// an answer for one all the same, so that one it sends is seen here rather
/// than failing for want of a daemon.
fn serve(
    socket: &Path,
    answers: Vec<Value>,
    done: Arc<AtomicBool>,
) -> std::thread::JoinHandle<Vec<Value>> {
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
                        if done.load(Ordering::SeqCst) || std::time::Instant::now() >= deadline {
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

/// Git in `dir`, in no repository the environment names.
fn git(dir: &Path, args: &[&str]) {
    let mut command = Command::new("git");
    for name in agentdocker_host::project::REPOSITORY_ENV {
        command.env_remove(name);
    }
    let status = command
        .args(["-c", "user.email=t@example.invalid", "-c", "user.name=T"])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

/// What pull request 7 is in the repository at `dir`.
fn pull_request(dir: &Path, branch: &str) {
    std::fs::write(
        dir.join("pr.json"),
        format!("{{\"headRefName\": \"{branch}\"}}"),
    )
    .unwrap();
}

fn fingerprint(dir: &Path) -> String {
    agentdocker_host::project::fingerprint(dir, Duration::from_secs(10)).expect("a fingerprint")
}

/// A committed repository whose pull request 7 is `branch`; its project's
/// id, its repository's fingerprint, as the daemon gives it. Its root
/// commit is its own: two that differed in nothing, made within a second of
/// each other, would be one commit and so one project.
fn repository(dir: &Path, branch: &str) -> String {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["commit", "-q", "--allow-empty", "-m", branch]);
    pull_request(dir, branch);
    fingerprint(dir)
}

/// Another clone of `from`, whose own pull request 7 is `branch`.
fn clone(from: &Path, to: &Path, branch: &str) {
    let (from, parent) = (from.to_str().unwrap(), to.parent().unwrap());
    git(parent, &["clone", "-q", from, to.to_str().unwrap()]);
    pull_request(to, branch);
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
    root: PathBuf,
    here: PathBuf,
    here_id: String,
    there: PathBuf,
    there_id: String,
    bin: PathBuf,
    sockets: PathBuf,
    _tmp: tempfile::TempDir,
}

/// Two projects whose pull request 7 names different branches, and the
/// fake `gh`.
fn setting() -> Setting {
    let tmp = tempfile::Builder::new()
        .prefix("ad-pr-")
        .tempdir_in("/tmp")
        .unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let here = root.join("here");
    let there = root.join("there");
    let here_id = repository(&here, "feature/a");
    let there_id = repository(&there, "feature/b");
    assert_ne!(here_id, there_id, "two projects");
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let gh = bin.join("gh");
    std::fs::write(
        &gh,
        "#!/bin/sh\n\
         [ -n \"$GH_REPO\" ] && { echo '{\"headRefName\": \"from-gh-repo\"}'; exit 0; }\n\
         [ -n \"$GIT_DIR$GIT_WORK_TREE\" ] && { echo '{\"headRefName\": \"from-git-env\"}'; exit 0; }\n\
         cat \"$PWD/pr.json\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    let sockets = root.join("s");
    std::fs::create_dir_all(&sockets).unwrap();
    Setting {
        root,
        here,
        here_id,
        there,
        there_id,
        bin,
        sockets,
        _tmp: tmp,
    }
}

/// The command run in `cwd`, as a hook might run it: `GH_REPO` naming
/// another repository and `GIT_DIR` the one it is not in, the fake `gh`
/// first on its `PATH`, against a fake daemon answering `answers`. Its
/// output and the requests it sent.
fn run(
    setting: &Setting,
    name: &str,
    cwd: &Path,
    args: &[&str],
    answers: Vec<Value>,
) -> (Output, Vec<Value>) {
    let socket = setting.sockets.join(format!("{name}.sock"));
    let done = Arc::new(AtomicBool::new(false));
    let daemon = serve(&socket, answers, done.clone());
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
    let elsewhere = if cwd.starts_with(&setting.there) {
        &setting.here
    } else {
        &setting.there
    };
    let output = command
        .args(args)
        .current_dir(cwd)
        .env("PATH", path)
        .env("GH_REPO", "someone/elsewhere")
        .env("GIT_DIR", elsewhere.join(".git"))
        .env("AGENTDOCKER_HOME", setting.sockets.join(name))
        .env("AGENTDOCKER_SOCKET", &socket)
        .env("AGENTDOCKER_NO_AUTOSTART", "1")
        .output()
        .unwrap();
    done.store(true, Ordering::SeqCst);
    (output, daemon.join().expect("the fake daemon served"))
}

fn agents(records: Vec<AgentRecord>) -> Value {
    answer(Response::Agents {
        agents: records,
        aliases: BTreeMap::new(),
    })
}

fn ended(mut record: AgentRecord) -> AgentRecord {
    record.status = AgentStatus::Exited { code: Some(0) };
    record
}

fn named(project: &str) -> [&str; 7] {
    [
        "authorship",
        "7",
        "--as",
        "agent-b",
        "--project",
        project,
        "--json",
    ]
}

/// An agent of the other project, asking from this checkout with no
/// project named: its own project's pull request and receipts, for both
/// commands, never this checkout's, `GH_REPO`'s or `GIT_DIR`'s.
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
            &setting.here,
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
    // Its root now holds another repository: nothing is read there.
    let moved = answer(Response::Agent {
        agent: record(&setting.here, &setting.there_id),
    });
    let (output, requests) = run(
        &setting,
        "moved",
        &setting.there,
        &["authorship", "7", "--as", "agent-b", "--json"],
        vec![moved, receipts(&setting.there_id)],
    );
    assert!(!output.status.success(), "{output:?}");
    assert_eq!(requests.len(), 1, "no read: {requests:?}");
}

/// The person, with no project named: this checkout's project and pull
/// request, whatever repository the environment names.
#[test]
fn the_person_s_pull_request_is_read_in_this_checkout() {
    let setting = setting();
    // Below its root, where Git, told of another repository, would answer
    // the directory itself.
    let below = setting.here.join("below");
    std::fs::create_dir_all(&below).unwrap();
    let (output, requests) = run(
        &setting,
        "person",
        &below,
        &["authorship", "7", "--as", "user", "--json"],
        vec![receipts(&setting.here_id)],
    );
    assert!(output.status.success(), "{output:?}");
    assert_eq!(requests.len(), 1, "{requests:?}");
    assert_eq!(requests[0]["branch"], "feature/a");
    assert_eq!(requests[0]["project"], setting.here.to_str().unwrap());
}

/// A project named by its full id or a unique prefix is the daemon's to
/// resolve, and its pull request is read in the root its records give; a
/// prefix the daemon finds ambiguous sends no read.
#[test]
fn a_named_project_s_pull_request_is_read_in_its_root() {
    let setting = setting();
    let listed = agents(vec![record(&setting.there, &setting.there_id)]);
    for selector in [setting.there_id.clone(), setting.there_id[..8].to_owned()] {
        let (output, requests) = run(
            &setting,
            &format!("named-{}", selector.len()),
            &setting.here,
            &named(&selector),
            vec![
                listed.clone(),
                agents(vec![record(&setting.here, &setting.here_id)]),
                receipts(&setting.there_id),
            ],
        );
        assert!(output.status.success(), "{selector}: {output:?}");
        assert_eq!(requests[0]["op"], "list", "{requests:?}");
        assert_eq!(requests[0]["project"], selector.as_str());
        assert_eq!(
            requests[1]["project"],
            setting.here.to_str().unwrap(),
            "whose this directory is, is the daemon's to say"
        );
        assert_eq!(requests[2]["branch"], "feature/b", "{selector}");
        assert_eq!(requests[2]["project"], setting.there_id.as_str());
    }
    let (output, requests) = run(
        &setting,
        "ambiguous",
        &setting.here,
        &named("a"),
        vec![
            json!({
                "type": "error",
                "code": "ambiguous",
                "message": "project reference a is ambiguous",
            }),
            receipts(&setting.there_id),
        ],
    );
    assert!(!output.status.success(), "{output:?}");
    assert_eq!(requests.len(), 1, "no read after the refusal: {requests:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("ambiguous"),
        "{output:?}"
    );
}

/// Clones of one repository are one project, with a pull request 7 of its
/// own in each. From one of them its own is read, whichever record is
/// listed first and whether or not a record names it; from outside, none is
/// guessed and no read is sent.
#[test]
fn a_project_of_several_clones_is_read_in_the_caller_s_or_not_at_all() {
    let setting = setting();
    let fork = setting.root.join("a-fork");
    clone(&setting.there, &fork, "feature/of-the-fork");
    assert_eq!(fingerprint(&fork), setting.there_id, "one project");
    let unnamed = setting.root.join("unnamed");
    clone(&setting.there, &unnamed, "feature/of-no-record");
    let listed = agents(vec![
        ended(record(&fork, &setting.there_id)),
        record(&setting.there, &setting.there_id),
    ]);
    for (cwd, branch) in [
        (&setting.there, "feature/b"),
        (&fork, "feature/of-the-fork"),
        (&unnamed, "feature/of-no-record"),
    ] {
        let (output, requests) = run(
            &setting,
            branch.rsplit('/').next().unwrap(),
            cwd,
            &named(&setting.there_id),
            // The daemon says the directory is the same project's.
            vec![listed.clone(), listed.clone(), receipts(&setting.there_id)],
        );
        assert!(output.status.success(), "{branch}: {output:?}");
        assert_eq!(requests[2]["branch"], branch);
        assert_eq!(requests[2]["project"], setting.there_id.as_str());
    }
    let (output, requests) = run(
        &setting,
        "outside",
        &setting.here,
        &named(&setting.there_id),
        vec![
            listed,
            agents(vec![record(&setting.here, &setting.here_id)]),
            receipts(&setting.there_id),
        ],
    );
    assert!(!output.status.success(), "{output:?}");
    assert_eq!(requests.len(), 2, "no read: {requests:?}");
    let said = String::from_utf8_lossy(&output.stderr).into_owned();
    for expected in [
        fork.to_str().unwrap(),
        setting.there.to_str().unwrap(),
        "--project",
    ] {
        assert!(said.contains(expected), "{expected} in {said}");
    }
}

/// A root a record gives that is gone, or that now holds another
/// repository, is not the project's: the one that still is, is read.
#[test]
fn a_root_that_is_no_longer_the_project_s_is_passed_over() {
    let setting = setting();
    let gone = setting.root.join("a-gone");
    let taken = setting.root.join("a-taken");
    repository(&taken, "feature/of-another");
    let here = agents(vec![record(&setting.here, &setting.here_id)]);
    for (name, stale) in [("gone", &gone), ("taken", &taken)] {
        let (output, requests) = run(
            &setting,
            name,
            &setting.here,
            &named(&setting.there_id),
            vec![
                agents(vec![
                    ended(record(stale, &setting.there_id)),
                    record(&setting.there, &setting.there_id),
                ]),
                here.clone(),
                receipts(&setting.there_id),
            ],
        );
        assert!(output.status.success(), "{name}: {output:?}");
        assert_eq!(requests[2]["branch"], "feature/b", "{name}");
    }
    let (output, requests) = run(
        &setting,
        "none-left",
        &setting.here,
        &named(&setting.there_id),
        vec![
            agents(vec![ended(record(&gone, &setting.there_id))]),
            here,
            receipts(&setting.there_id),
        ],
    );
    assert!(!output.status.success(), "{output:?}");
    assert_eq!(requests.len(), 2, "no read: {requests:?}");
}

/// A repository that only took another's history in is not that project's
/// checkout: the daemon says whose the directory is, and the named
/// project's own root is read.
#[test]
fn a_repository_that_took_a_history_in_is_not_its_project_s() {
    let setting = setting();
    git(
        &setting.here,
        &["fetch", "-q", setting.there.to_str().unwrap(), "main"],
    );
    git(
        &setting.here,
        &[
            "merge",
            "-q",
            "--allow-unrelated-histories",
            "-m",
            "in",
            "FETCH_HEAD",
        ],
    );
    let (output, requests) = run(
        &setting,
        "took-in",
        &setting.here,
        &named(&setting.there_id),
        vec![
            agents(vec![record(&setting.there, &setting.there_id)]),
            agents(vec![record(&setting.here, &setting.here_id)]),
            receipts(&setting.there_id),
        ],
    );
    assert!(output.status.success(), "{output:?}");
    assert_eq!(requests[2]["branch"], "feature/b");
}

/// A repository is its project's through what moves the oldest root of
/// whatever is checked out: an orphan branch, a shallow clone deepened, an
/// unrelated history merged in, and a first commit made after the daemon
/// first saw it. Read for the agent whose project it is, with no project
/// named, and by the project's id from its own checkout.
#[test]
fn a_repository_stays_its_project_s_whatever_its_head_comes_to() {
    let setting = setting();
    let recorded = |root: &Path, fingerprint: Option<&str>| {
        let mut record = record(root, fingerprint.unwrap_or_default());
        record.project.as_mut().unwrap().fingerprint = fingerprint.map(str::to_owned);
        record
    };
    let mut repositories: Vec<(&str, PathBuf, Option<String>)> = Vec::new();

    let unborn = setting.root.join("unborn");
    std::fs::create_dir_all(&unborn).unwrap();
    git(&unborn, &["init", "-q", "-b", "main"]);
    git(
        &unborn,
        &["commit", "-q", "--allow-empty", "-m", "born later"],
    );
    repositories.push(("unborn", unborn, None));

    let orphan = setting.root.join("orphan");
    let id = repository(&orphan, "replaced below");
    git(&orphan, &["checkout", "-q", "--orphan", "pages"]);
    git(
        &orphan,
        &["commit", "-q", "--allow-empty", "-m", "a root of its own"],
    );
    repositories.push(("orphan", orphan, Some(id)));

    let long = setting.root.join("long");
    repository(&long, "the oldest");
    git(
        &long,
        &["commit", "-q", "--allow-empty", "-m", "the newest"],
    );
    let deepened = setting.root.join("deepened");
    git(
        &setting.root,
        &[
            "clone",
            "-q",
            "--depth",
            "1",
            &format!("file://{}", long.display()),
            deepened.to_str().unwrap(),
        ],
    );
    let id = fingerprint(&deepened);
    git(&deepened, &["fetch", "-q", "--unshallow"]);
    assert_ne!(fingerprint(&deepened), id, "its oldest root moved");
    repositories.push(("deepened", deepened, Some(id)));

    let merged = setting.root.join("merged");
    let id = repository(&merged, "merged into");
    git(
        &merged,
        &["fetch", "-q", setting.there.to_str().unwrap(), "main"],
    );
    git(
        &merged,
        &[
            "merge",
            "-q",
            "--allow-unrelated-histories",
            "-m",
            "in",
            "FETCH_HEAD",
        ],
    );
    repositories.push(("merged", merged, Some(id)));

    for (name, root, fingerprint) in repositories {
        let branch = format!("feature/of-{name}");
        pull_request(&root, &branch);
        let record = recorded(&root, fingerprint.as_deref());
        let project = record.project.as_ref().unwrap().id().to_string();
        for command in ["authorship", "verdicts"] {
            let read = match command {
                "verdicts" => answer(Response::Verdicts {
                    verdicts: Vec::new(),
                    retained: 2000,
                }),
                _ => receipts(&project),
            };
            let (output, requests) = run(
                &setting,
                &format!("{name}-{command}"),
                &root,
                &[command, "7", "--as", "agent-b", "--json"],
                vec![
                    answer(Response::Agent {
                        agent: record.clone(),
                    }),
                    read,
                ],
            );
            assert!(output.status.success(), "{name} {command}: {output:?}");
            assert_eq!(requests[1]["branch"], branch.as_str(), "{name} {command}");
        }
        let listed = agents(vec![record]);
        let (output, requests) = run(
            &setting,
            &format!("{name}-named"),
            &root,
            &named(&project),
            vec![listed.clone(), listed, receipts(&project)],
        );
        assert!(output.status.success(), "{name} named: {output:?}");
        assert_eq!(requests[2]["branch"], branch.as_str(), "{name} named");
    }
}
