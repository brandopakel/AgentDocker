//! What the commands print today, captured: the bytes on stdout, the
//! bytes on stderr and the exit status of a command run with its output
//! in a pipe, against a daemon that answers from a list.
//!
//! These are what a script or an agent reads. They are pinned here before
//! any command is given a new look for a terminal, so that the new look
//! cannot change them: a piped invocation that exists today prints what it
//! prints today, and ends with the status it ends with today. A capture is
//! changed only by a change that means to change what a script reads, and
//! then on purpose.
//!
//! Times are the one thing a capture cannot hold still, since a table
//! says "2m ago". Each fixture's times are set from the test's own clock
//! so that the words come out the same on every run.
//!
//! A capture also holds the requests a command makes, every one: the
//! daemon here answers from its list and records a request it has no
//! answer for, which fails the test. A command reads some things only to
//! say more (a holder's name, the day's tokens) and says less when the
//! read fails; without that, what it says with half its answers missing
//! would be pinned as what it says.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use agentdocker_core::{AgentRecord, AgentSpec, AgentStatus};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};

/// Answer each request in turn from `answers` for as long as the command
/// runs, and hand back every request it made.
///
/// A request beyond the answers is recorded like the rest and answered
/// with an error, so a command cannot make a request a test does not
/// know of: one that fails quietly would otherwise be pinned as what the
/// command prints, when it is what the command prints with half its
/// answers missing. `done` is set when the command has exited; every
/// accept, read and write is bounded.
fn serve(
    socket: &Path,
    answers: Vec<Value>,
    done: Arc<AtomicBool>,
) -> std::thread::JoinHandle<Vec<Value>> {
    let listener = UnixListener::bind(socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    std::thread::spawn(move || {
        let mut requests = Vec::new();
        let mut answers = answers.into_iter();
        loop {
            // Read before the accept: a request made just before the
            // command exited is still taken on the last pass.
            let exited = done.load(Ordering::SeqCst);
            let stream = match listener.accept() {
                Ok((stream, _)) => stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if exited {
                        return requests;
                    }
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                }
                Err(error) => panic!("accept failed: {error}"),
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
            let answer = answers.next().unwrap_or_else(|| {
                json!({
                    "type": "error",
                    "code": "internal",
                    "message": "this test has no answer for that request",
                })
            });
            let mut stream = stream;
            // The command may have gone by now, if it did not wait for
            // this answer; that it asked is what is recorded.
            let _ = stream.write_all(format!("{answer}\n{}\n", json!({"type":"end"})).as_bytes());
        }
    })
}

/// What a command printed and how it ended.
struct Captured {
    stdout: Vec<u8>,
    stderr: String,
    status: Option<i32>,
    /// The `op` of each request it made, in order.
    asked: Vec<String>,
}

impl Captured {
    fn text(&self) -> String {
        String::from_utf8(self.stdout.clone()).expect("stdout is UTF-8")
    }
}

/// One command, its output in a pipe, against a daemon with `answers` to
/// give, with nothing inherited from the surrounding session.
fn capture(answers: Vec<Value>, args: &[&str]) -> Captured {
    capture_in(None, answers, args)
}

/// [`capture`], run in `folder` when one is given: a command that asks
/// git something needs a checkout to ask.
fn capture_in(folder: Option<&Path>, answers: Vec<Value>, args: &[&str]) -> Captured {
    let home = tempfile::Builder::new()
        .prefix("ad-cap-")
        .tempdir_in("/tmp")
        .unwrap();
    let socket = home.path().join("d.sock");
    let expected = answers.len();
    let done = Arc::new(AtomicBool::new(false));
    let daemon = serve(&socket, answers, done.clone());
    let mut command = Command::new(env!("CARGO_BIN_EXE_agentdocker"));
    for (key, _) in std::env::vars_os() {
        let key = key.to_string_lossy().into_owned();
        if key.starts_with("AGENTDOCKER_")
            || ["NO_COLOR", "CLICOLOR", "CLICOLOR_FORCE", "COLUMNS"].contains(&key.as_str())
        {
            command.env_remove(key);
        }
    }
    let output = command
        .args(args)
        .current_dir(folder.unwrap_or(home.path()))
        .env("AGENTDOCKER_HOME", home.path())
        .env("AGENTDOCKER_SOCKET", &socket)
        .env("AGENTDOCKER_NO_AUTOSTART", "1")
        .output()
        .unwrap();
    done.store(true, Ordering::SeqCst);
    let asked: Vec<String> = daemon
        .join()
        .expect("the fake daemon served")
        .iter()
        .map(|request| request["op"].as_str().unwrap_or("?").to_owned())
        .collect();
    assert_eq!(
        asked.len(),
        expected,
        "the command made {} requests and the test has {expected} answers: {asked:?}\nstdout: {}\nstderr: {}",
        asked.len(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    Captured {
        stdout: output.stdout,
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        status: output.status.code(),
        asked,
    }
}

/// A moment `seconds` before now, placed in the middle of its minute so
/// that "2m ago" stays "2m ago" for as long as a test runs.
fn ago(seconds: i64) -> DateTime<Utc> {
    Utc::now() - chrono::Duration::seconds(seconds)
}

fn agent(id: &str, name: &str, runtime: &str, managed: bool) -> AgentRecord {
    let mut agent = AgentRecord::new(
        AgentSpec {
            name: name.into(),
            runtime: runtime.into(),
            ..Default::default()
        },
        managed,
        ago(150),
    );
    agent.id = id.into();
    agent.status = AgentStatus::Running;
    agent.pid = Some(4242);
    agent.process_started_at = Some(ago(150));
    agent
}

const WRITER: &str = "aaaaaaaaaaaa0000000000000000aaaa";
const REVIEWER: &str = "bbbbbbbbbbbb0000000000000000bbbb";
const PROJECT: &str = "cccccccccccc0000000000000000cccc";

fn card(key: &str, title: &str, column: &str, assignee: Option<&str>) -> Value {
    let mut card = json!({
        "id": format!("task-{key}"),
        "project": PROJECT,
        "key": key,
        "title": title,
        "acceptance": "It is done when it is done.",
        "column": column,
        "lane": column,
        "rank": "a0",
        "size": "s",
        "tags": ["cli"],
        "created_by": "user",
        "created_at": ago(7350),
        "updated_at": ago(150),
        "lane_since": ago(150),
    });
    if let Some(assignee) = assignee {
        card["assignee"] = json!(assignee);
    }
    card
}

/// A day's usage report as the daemon answers it, with `rows` in it.
fn usage(rows: Vec<agentdocker_core::usage::report::Row>) -> Value {
    use agentdocker_core::usage::report::{Collection, Group, Overhead, Report, ReportCoverage};
    let now = Utc::now();
    let day = chrono::Duration::hours(24);
    let mut answer = serde_json::to_value(Report {
        rows,
        by: Group::Agent,
        as_of: now,
        effective_since: now - day,
        effective_until: now,
        coverage: ReportCoverage {
            retained_since: now - day,
            history_truncated: false,
            future_until_clamped: false,
            includes_current_hour: true,
            source_gaps: 0,
            collection: Collection::default(),
            tracking: None,
        },
        overhead: Overhead::default(),
        context: Default::default(),
    })
    .unwrap();
    answer["type"] = json!("usage");
    answer
}

/// One agent's tokens over the day: every sample said its count.
fn tokens_of(agent: &str, input: u64, output: u64) -> agentdocker_core::usage::report::Row {
    use agentdocker_core::usage::report::{CounterReports, Row};
    use agentdocker_core::usage::{CounterReport, Coverage};
    let counted = |sum| CounterReport {
        sum: Some(sum),
        known_samples: 4,
        coverage: Coverage::Complete,
    };
    Row {
        key: Some(agent.to_owned()),
        samples: 4,
        counters: CounterReports {
            input_tokens: counted(input),
            cache_read_input_tokens: CounterReport::default(),
            cache_write_input_tokens: CounterReport::default(),
            output_tokens: counted(output),
            reasoning_output_tokens: CounterReport::default(),
        },
        usage_source: None,
        cost_usd_micros: CounterReport::default(),
        cache_read_share_percent: None,
        estimated_cost: None,
    }
}

/// What `ps --no-discover` asks, in order: the agents shown, every agent
/// (whose provider's state a shown one may depend on), what each is
/// doing, the day's tokens, and whether an ended one comes back.
const PS_ASKS: [&str; 5] = ["list", "list", "activity", "usage", "restarts"];

fn ps(report: Value) -> Captured {
    let agents = json!([
        agent(WRITER, "writer", "claude-code", true),
        agent(REVIEWER, "reviewer", "codex", false),
    ]);
    capture(
        vec![
            json!({"type":"agents", "agents": agents.clone()}),
            json!({"type":"agents", "agents": agents}),
            json!({"type":"activity", "activity": []}),
            report,
            json!({"type":"restarts", "outlooks": {}}),
        ],
        &["ps", "--no-discover"],
    )
}

/// `ps` in a pipe: the table as it is, with the five requests it makes.
#[test]
fn ps_in_a_pipe_prints_what_it_prints_today() {
    let captured = ps(usage(Vec::new()));
    assert_eq!(captured.status, Some(0), "{}", captured.stderr);
    assert_eq!(captured.asked, PS_ASKS);
    assert_eq!(captured.text(), PS, "{:?}", captured.text());
    assert_eq!(captured.stderr, "");
}

const PS: &str = "\
AGENT ID       NAME       PROJECT   BRANCH   HEAD   RUNTIME       MODEL   STATUS    INPUT                    DOING   PID    CREATED
aaaaaaaaaaaa   writer     -         -        -      claude-code   -       running   Can't receive messages   -       4242   2m ago
bbbbbbbbbbbb   reviewer   -         -        -      codex         -       running   Can't receive messages   -       4242   2m ago
";

/// `ps` in a pipe where a provider reported tokens: the column that is
/// only there when there is something to put in it.
#[test]
fn ps_in_a_pipe_has_a_tokens_column_when_tokens_were_reported() {
    let captured = ps(usage(vec![tokens_of(WRITER, 12_000, 3_400)]));
    assert_eq!(captured.status, Some(0), "{}", captured.stderr);
    assert_eq!(captured.asked, PS_ASKS);
    assert_eq!(captured.text(), PS_TOKENS, "{:?}", captured.text());
    assert_eq!(captured.stderr, "");
}

const PS_TOKENS: &str = "\
AGENT ID       NAME       PROJECT   BRANCH   HEAD   RUNTIME       MODEL   STATUS    INPUT                    DOING   TOKENS 24H   PID    CREATED
aaaaaaaaaaaa   writer     -         -        -      claude-code   -       running   Can't receive messages   -       15.4K        4242   2m ago
bbbbbbbbbbbb   reviewer   -         -        -      codex         -       running   Can't receive messages   -       -            4242   2m ago
";

/// `leases` in a pipe.
#[test]
fn leases_in_a_pipe_prints_what_it_prints_today() {
    let captured = capture(
        vec![json!({"type":"leases", "leases": [
            {
                "id": "lease-one",
                "resource": "path:/work/app/src/api",
                "holder": WRITER,
                "mode": "exclusive",
                "acquired_at": ago(150),
                "change_seq": 7,
                "expires_at": ago(-270),
                "note": "rewriting the router",
            },
            {
                "id": "lease-two",
                "resource": "task:local-build",
                "holder": REVIEWER,
                "mode": "exclusive",
                "acquired_at": ago(30),
                "change_seq": 9,
                "expires_at": ago(-7350),
            },
        ]})],
        &["leases"],
    );
    assert_eq!(captured.status, Some(0), "{}", captured.stderr);
    assert_eq!(captured.asked, ["leases"]);
    assert_eq!(captured.text(), LEASES, "{:?}", captured.text());
    assert_eq!(captured.stderr, "");
}

const LEASES: &str = "\
LEASE ID    RESOURCE                 HOLDER         MODE        EXPIRES   NOTE
lease-one   path:/work/app/src/api   aaaaaaaaaaaa   exclusive   in 4m     rewriting the router
lease-two   task:local-build         bbbbbbbbbbbb   exclusive   in 2h
";

/// `task list` in a pipe.
#[test]
fn task_list_in_a_pipe_prints_what_it_prints_today() {
    let captured = capture(
        vec![
            json!({
                "type":"tasks",
                "tasks": [
                    card("AD-1", "Give the router one error type", "in_progress", Some(WRITER)),
                    card("AD-2", "Say which lease is in the way", "ready", None),
                ],
                "more": false,
                "times": [{
                    "task": "task-AD-1",
                    "stays": [{"lane": "in_progress", "from": ago(150)}],
                    "lanes": [{
                        "lane": "in_progress", "seconds": 150, "stays": 1,
                        "current": true, "finished": 0, "left": 0,
                    }],
                }],
            }),
            // Whose card it is is said by name: the agents are read for it.
            json!({"type":"agents", "agents": [
                agent(WRITER, "writer", "claude-code", true),
            ]}),
        ],
        &["task", "list", "--project", "/work/app", "--as", WRITER],
    );
    assert_eq!(captured.status, Some(0), "{}", captured.stderr);
    assert_eq!(captured.asked, ["tasks", "list"]);
    assert_eq!(captured.text(), TASK_LIST, "{:?}", captured.text());
    assert_eq!(captured.stderr, "");
}

const TASK_LIST: &str = "\
CARD   LANE          IN LANE   TITLE                            HOLDER
AD-1   in_progress   2m        Give the router one error type   writer
AD-2   ready                   Say which lease is in the way    -
";

/// `task show` in a pipe.
#[test]
fn task_show_in_a_pipe_prints_what_it_prints_today() {
    let created: DateTime<Utc> = "2026-01-02T03:04:05Z".parse().unwrap();
    let mut task = card(
        "AD-1",
        "Give the router one error type",
        "in_progress",
        Some(WRITER),
    );
    task["description"] = json!("Three error enums say the same thing.");
    task["priority"] = json!("high");
    task["links"] = json!([{"kind": "pr", "target": "#12"}]);
    task["created_at"] = json!(created);
    task["updated_at"] = json!(created);
    task["lane_since"] = json!(created);
    let captured = capture(
        vec![json!({
            "type": "task_detail",
            "task": task,
            "board": {
                "project": PROJECT,
                "slug": "main",
                "name": "Main",
                "template": "software",
                "lanes": [
                    {"slug": "backlog", "name": "Backlog", "category": "backlog"},
                    {"slug": "ready", "name": "Ready", "category": "ready"},
                    {"slug": "in_progress", "name": "In progress", "category": "in_progress"},
                    {"slug": "done", "name": "Done", "category": "done"},
                ],
                "pull_source": "ready",
                "pull_target": "in_progress",
                "key_prefix": "AD",
                "last_key": 2,
                "created_at": created,
                "updated_at": created,
            },
            "comments": [{
                "id": "comment-one",
                "task": "task-AD-1",
                "project": PROJECT,
                "author": WRITER,
                "body": "Mapped: three enums, one of them unused.",
                "created_at": created,
            }],
            "history": [{
                "id": "entry-one",
                "task": "task-AD-1",
                "project": PROJECT,
                "at": created,
                "by": WRITER,
                "action": {"kind": "pulled", "lane": "in_progress"},
            }],
            "blockers": [],
            "children": [],
        })],
        &["task", "show", "AD-1"],
    );
    assert_eq!(captured.status, Some(0), "{}", captured.stderr);
    assert_eq!(captured.asked, ["task_get"]);
    assert_eq!(captured.text(), TASK_SHOW, "{:?}", captured.text());
    assert_eq!(captured.stderr, "");
}

const TASK_SHOW: &str = "\
AD-1  in_progress  Give the router one error type  (aaaaaaaaaaaa)
    pr:#12
  board main · lane In progress
  done when: It is done when it is done.

Three error enums say the same thing.

  2026-01-02 03:04 aaaaaaaaaaaa0000000000000000aaaa: Mapped: three enums, one of them unused.
  2026-01-02 03:04 aaaaaaaaaaaa0000000000000000aaaa {\"kind\":\"pulled\",\"lane\":\"in_progress\"}
";

/// `inspect` prints the record itself as JSON, with no envelope: the keys
/// at its root are the record's.
#[test]
fn inspect_prints_the_record_as_json_at_the_root() {
    let record = agent(WRITER, "writer", "claude-code", true);
    // Read against the record's own run, so the record is read once.
    let outlook = agentdocker_core::restart::Outlook {
        run: agentdocker_core::restart::RunEvidence::of(&record),
        view: None,
    };
    let captured = capture(
        vec![
            json!({"type":"agent", "agent": record.clone()}),
            json!({"type":"restarts", "outlooks": {WRITER: outlook}}),
        ],
        &["inspect", "writer"],
    );
    assert_eq!(captured.status, Some(0), "{}", captured.stderr);
    assert_eq!(captured.asked, ["inspect", "restarts"]);
    let printed: Value =
        serde_json::from_slice(&captured.stdout).expect("stdout is one JSON value");
    let sent = serde_json::to_value(&record).unwrap();
    for (key, value) in sent.as_object().unwrap() {
        assert_eq!(&printed[key], value, "{key}");
    }
    assert_eq!(printed["id"], WRITER);
    assert!(
        printed.get("agent").is_none(),
        "no envelope round the record"
    );
    assert!(
        captured.text().starts_with("{\n  \""),
        "pretty-printed, two spaces: {:?}",
        &captured.text()[..captured.text().len().min(40)]
    );
    assert_eq!(captured.stderr, "");
}

/// `file get` writes the file's own bytes and nothing else: no heading,
/// no newline of its own, and bytes that are not UTF-8 as they are.
#[test]
fn file_get_writes_the_files_bytes_and_nothing_else() {
    use base64::Engine;
    let created: DateTime<Utc> = "2026-01-02T03:04:05Z".parse().unwrap();
    let file = |name: &str, size: usize| {
        json!({
            "id": "file-one",
            "project": PROJECT,
            "name": name,
            "media_type": "application/octet-stream",
            "size": size,
            "sha256": "0".repeat(64),
            "created_by": "user",
            "created_at": created,
            "updated_at": created,
        })
    };
    // Not UTF-8, with a NUL, an escape and no final newline.
    let bytes: &[u8] = &[0xff, 0xfe, 0x00, 0x1b, b'[', b'1', b'm', 0x80, b'a'];
    let captured = capture(
        vec![json!({
            "type": "file_content",
            "file": file("blob.bin", bytes.len()),
            "base64": base64::engine::general_purpose::STANDARD.encode(bytes),
        })],
        &["file", "get", "file-one"],
    );
    assert_eq!(captured.status, Some(0), "{}", captured.stderr);
    assert_eq!(captured.asked, ["file_get"]);
    assert_eq!(captured.stdout, bytes);
    assert_eq!(captured.stderr, "");

    let text = "line one\nline two, no newline after it";
    let captured = capture(
        vec![json!({
            "type": "file_content",
            "file": file("notes.txt", text.len()),
            "text": text,
        })],
        &["file", "get", "file-one"],
    );
    assert_eq!(captured.status, Some(0), "{}", captured.stderr);
    assert_eq!(captured.stdout, text.as_bytes());
}

/// `gate` ends with status 1 when the branch is not approved, and says
/// why on stdout: a script reads the status. It is the same status an
/// unexpected failure has, and stays so.
#[test]
fn gate_ends_with_one_when_the_branch_is_not_approved() {
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    let head = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(here)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_owned();
    assert_eq!(head.len(), 40, "this test runs in a checkout: {head:?}");
    let captured = capture_in(
        Some(here),
        vec![json!({"type":"review_gate", "gate": {"state": "missing"}})],
        &[
            "gate",
            "some-branch",
            "--head",
            "HEAD",
            "--as",
            WRITER,
            "--project",
            "/work/app",
        ],
    );
    assert_eq!(captured.status, Some(1), "{}", captured.stderr);
    assert_eq!(captured.asked, ["review_gate"]);
    assert_eq!(
        captured.text(),
        GATE.replace("<head>", &head),
        "{:?}",
        captured.text()
    );
}

const GATE: &str = "\
unreviewed: nobody who counts has passed some-branch
gate=missing branch=some-branch head=<head>
";
