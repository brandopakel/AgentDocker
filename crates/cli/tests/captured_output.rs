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

mod support;

use std::path::Path;
use std::process::Command;

use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use support::*;

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

/// `ps --input-details` in a pipe for two browser agents, one heard from
/// a moment ago and one not for hours: what each row says of the state,
/// of whether it can be told something and of when it was made, and the
/// detail on stderr.
#[test]
fn ps_in_a_pipe_says_a_connector_gone_quiet_is_not_connected() {
    let connector = |id: &str, name: &str, last_seen: i64| {
        let mut agent = agent(id, name, "claude-in-chrome", false);
        agent.pid = None;
        agent.process_started_at = None;
        agent
            .spec
            .labels
            .insert("connector".to_owned(), "true".to_owned());
        agent.last_seen = ago(last_seen);
        agent
    };
    let agents = json!([
        connector(WRITER, "here", 30),
        connector(REVIEWER, "gone-quiet", 7350),
    ]);
    let captured = capture(
        vec![
            json!({"type":"agents", "agents": agents.clone()}),
            json!({"type":"agents", "agents": agents}),
            json!({"type":"activity", "activity": []}),
            usage(Vec::new()),
            json!({"type":"restarts", "outlooks": {}}),
        ],
        &["ps", "--no-discover", "--input-details"],
    );
    assert_eq!(captured.status, Some(0), "{}", captured.stderr);
    assert_eq!(captured.asked, PS_ASKS);
    assert_eq!(captured.text(), PS_CONNECTORS, "{:?}", captured.text());
    assert_eq!(
        captured.stderr, PS_CONNECTORS_DETAIL,
        "{:?}",
        captured.stderr
    );
}

const PS_CONNECTORS: &str = "\
AGENT ID       NAME         PROJECT   BRANCH   HEAD   RUNTIME            MODEL   STATUS          INPUT                      DOING   PID   CREATED
aaaaaaaaaaaa   here         -         -        -      claude-in-chrome   -       running         Waits until it checks in   -       -     2m ago
bbbbbbbbbbbb   gone-quiet   -         -        -      claude-in-chrome   -       not connected   Waits until it checks in   -       -     2m ago
";
const PS_CONNECTORS_DETAIL: &str = "\
\x20 here (aaaaaaaaaaaa): Waits until it checks in
    It reads messages only when it checks in; they are kept until then. It takes them the next time it is used.
  gone-quiet (bbbbbbbbbbbb): Waits until it checks in
    It reads messages only when it checks in; they are kept until then. It takes them the next time it is used.
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
    assert_eq!(captured.asked, ["file_get"]);
    assert_eq!(captured.stdout, text.as_bytes());
    assert_eq!(captured.stderr, "");
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
    assert_eq!(captured.stderr, "");
}

const GATE: &str = "\
unreviewed: nobody who counts has passed some-branch
gate=missing branch=some-branch head=<head>
";
