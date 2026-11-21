//! What the first four commands print for a person: at a terminal, and
//! in a pipe where a person asked for it.
//!
//! Each test runs the real binary against a daemon that answers from a
//! list. A terminal here is a real one, a pair of this many columns, so
//! what is tested is what the command decides from its own stream; a
//! view's text at each width, with colour and without, is held by the
//! views' own tests, beside them. What a script reads is held by the
//! captures (`captured_output.rs`), which this change does not touch.
#![cfg(unix)]

mod support;

use serde_json::{Value, json};
use support::*;

const TERM: (&str, &str) = ("TERM", "xterm-256color");

/// Stdout at a terminal of `columns`, stderr in a pipe.
fn at_a_terminal(columns: u16) -> Run<'static> {
    Run {
        env: &[TERM],
        stdout: Stream::Terminal(columns),
        ..Run::PIPED
    }
}

/// What is printed, without its escapes.
fn strip(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find('\u{1b}') {
        out.push_str(&rest[..start]);
        let end = rest[start..]
            .find('m')
            .map_or(rest.len(), |m| start + m + 1);
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

fn columns_of(line: &str) -> usize {
    use unicode_width::UnicodeWidthStr;
    line.width()
}

fn day() -> Value {
    usage(vec![tokens_of(WRITER, 12_000, 3_400)])
}

fn leases_answer() -> Vec<Value> {
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
            "acquired_at": ago(3600),
            "change_seq": 9,
            // Expired: a word that does not change while the test runs.
            "expires_at": ago(30),
        },
    ]})]
}

/// `ps` at a terminal: for a person without being asked, fitted to the
/// terminal's own width and in colour, by the same five requests.
#[test]
fn ps_at_a_terminal_is_for_a_person() {
    let said = run(
        at_a_terminal(80),
        ps_answers(day()),
        &["ps", "--no-discover"],
    );
    assert_eq!(said.status, Some(0), "{}", said.stderr);
    assert_eq!(said.asked, PS_ASKS);
    let text = said.text();
    assert!(text.contains("\u{1b}[32mrunning\u{1b}[0m"), "{text:?}");
    assert!(text.starts_with("\u{1b}[1mAGENT ID\u{1b}[0m"), "{text:?}");
    let plain = strip(&text);
    assert!(plain.contains("\naaaaaaaaaaaa   writer "), "{plain}");
    assert!(plain.contains("\nbbbbbbbbbbbb   reviewer "), "{plain}");
    assert!(plain.contains("not shown at this width: "), "{plain}");
    for line in plain.lines().take(3) {
        assert!(columns_of(line) <= 80, "{line:?}");
    }

    // A wider terminal, the same answers: nothing is left out.
    let wide = run(
        at_a_terminal(200),
        ps_answers(day()),
        &["ps", "--no-discover"],
    );
    let wide = strip(&wide.text());
    assert!(!wide.contains("not shown at this width"), "{wide}");
    assert!(
        wide.lines()
            .next()
            .unwrap()
            .ends_with("TOKENS 24H   PID    CREATED"),
        "{wide}"
    );
}

/// At a terminal, `--format plain` is what a pipe gets, byte for byte;
/// and in a pipe nothing changes unless a person asks.
#[test]
fn plain_at_a_terminal_is_what_a_pipe_gets() {
    let piped = capture(ps_answers(day()), &["ps", "--no-discover"]);
    let asked_for = run(
        at_a_terminal(80),
        ps_answers(day()),
        &["ps", "--no-discover", "--format", "plain"],
    );
    assert_eq!(asked_for.status, Some(0), "{}", asked_for.stderr);
    assert_eq!(asked_for.text(), piped.text());
    assert!(!piped.text().contains('\u{1b}'));
    // A terminal on stderr does not make stdout one: each stream is
    // decided for itself.
    let stderr_only = run(
        Run {
            env: &[TERM],
            stderr: Stream::Terminal(80),
            ..Run::PIPED
        },
        ps_answers(day()),
        &["ps", "--no-discover"],
    );
    assert_eq!(stderr_only.text(), piped.text());
    // Nor does an environment that asks for colour, or a flag: what a
    // script reads is never decorated.
    for (env, flags) in [
        (&[("CLICOLOR_FORCE", "1"), ("COLUMNS", "40")][..], &[][..]),
        (&[][..], &["--color", "always"][..]),
        (
            &[("CLICOLOR_FORCE", "1")][..],
            &["--format", "plain", "--color", "always"][..],
        ),
    ] {
        let mut args = vec!["ps", "--no-discover"];
        args.extend(flags);
        let forced = run(Run { env, ..Run::PIPED }, ps_answers(day()), &args);
        assert_eq!(forced.text(), piped.text(), "{env:?} {flags:?}");
    }
}

/// In a pipe a person can ask for their own look: `--format human`,
/// fitted to `COLUMNS` where it says, with colour only when asked.
#[test]
fn a_person_can_ask_for_their_look_in_a_pipe() {
    let human = |env: &[(&str, &str)], flags: &[&str]| {
        let mut args = vec!["ps", "--no-discover", "--format", "human"];
        args.extend(flags);
        let said = run(Run { env, ..Run::PIPED }, ps_answers(day()), &args);
        assert_eq!(said.status, Some(0), "{}", said.stderr);
        said.text()
    };
    // No width known: nothing is fitted, and a pipe has no colour.
    let unfitted = human(&[], &[]);
    assert!(
        !unfitted.contains('\u{1b}') && !unfitted.contains("not shown"),
        "{unfitted}"
    );
    // COLUMNS is the width where there is no terminal to ask.
    let fitted = human(&[("COLUMNS", "70")], &[]);
    assert!(fitted.contains("not shown at this width: "), "{fitted}");
    for line in fitted.lines().take(3) {
        assert!(columns_of(line) <= 70, "{line:?}");
    }
    // Colour: the flag, then NO_COLOR over CLICOLOR_FORCE.
    assert!(human(&[], &["--color", "always"]).contains("\u{1b}[32mrunning"));
    assert!(human(&[("CLICOLOR_FORCE", "1")], &[]).contains("\u{1b}[32mrunning"));
    assert!(!human(&[("CLICOLOR_FORCE", "1"), ("NO_COLOR", "1")], &[]).contains('\u{1b}'));
    assert!(!human(&[("CLICOLOR_FORCE", "1")], &["--color", "never"]).contains('\u{1b}'));
    assert!(human(&[("NO_COLOR", "1")], &["--color", "always"]).contains('\u{1b}'));
}

/// At a terminal the environment's word on colour holds: NO_COLOR and a
/// dumb terminal have none, and the table is still fitted.
#[test]
fn a_terminal_without_colour_is_still_fitted() {
    for env in [&[TERM, ("NO_COLOR", "1")][..], &[("TERM", "dumb")][..]] {
        let said = run(
            Run {
                env,
                stdout: Stream::Terminal(80),
                ..Run::PIPED
            },
            ps_answers(day()),
            &["ps", "--no-discover"],
        );
        let text = said.text();
        assert!(!text.contains('\u{1b}'), "{env:?}: {text:?}");
        assert!(
            text.contains("not shown at this width: "),
            "{env:?}: {text}"
        );
    }
}

/// When one of the reads `ps` makes beside the agents fails, a person is
/// told which, and a script's output is what it has always been.
#[test]
fn a_person_is_told_what_could_not_be_read() {
    let mut answers = ps_answers(day());
    answers[2] = json!({"type":"error", "code":"internal", "message":"the daemon could not say"});
    let said = run(
        at_a_terminal(120),
        answers.clone(),
        &["ps", "--no-discover"],
    );
    assert_eq!(said.status, Some(0), "{}", said.stderr);
    assert!(
        strip(&said.text()).contains(
            "could not be read: what each is doing (not known, which is not the same as none)"
        ),
        "{}",
        said.text()
    );
    let piped = capture(answers, &["ps", "--no-discover"]);
    assert_eq!(
        piped.text(),
        capture(ps_answers(day()), &["ps", "--no-discover"]).text()
    );
}

/// `leases` at a terminal: the time left in its tone, by its one request.
#[test]
fn leases_at_a_terminal_are_for_a_person() {
    let said = run(at_a_terminal(100), leases_answer(), &["leases"]);
    assert_eq!(said.status, Some(0), "{}", said.stderr);
    assert_eq!(said.asked, ["leases"]);
    let text = said.text();
    assert!(
        text.contains("\u{1b}[1m\u{1b}[31mexpired\u{1b}[0m"),
        "{text:?}"
    );
    assert_eq!(
        strip(&text),
        "\
LEASE ID    RESOURCE                 HOLDER         MODE        EXPIRES   NOTE
lease-one   path:/work/app/src/api   aaaaaaaaaaaa   exclusive   in 4m     rewriting the router
lease-two   task:local-build         bbbbbbbbbbbb   exclusive   expired
"
    );
    let none = run(
        at_a_terminal(100),
        vec![json!({"type":"leases", "leases": []})],
        &["leases"],
    );
    assert_eq!(none.text(), "no leases\n");
}

fn tasks_answer() -> Vec<Value> {
    vec![
        json!({
            "type":"tasks",
            "tasks": [
                card("AD-1", "Give the router one error type", "in_progress", Some(WRITER)),
                card("AD-2", "Say which lease is in the way", "ready", None),
            ],
            "more": true,
        }),
        json!({"type":"agents", "agents": [agent(WRITER, "writer", "claude-code", true)]}),
    ]
}

/// `task list` at a terminal: each lane in its tone, the holder by name,
/// by the same two requests; and what follows the table follows it.
#[test]
fn the_board_at_a_terminal_is_for_a_person() {
    let args = ["task", "list", "--project", "/work/app", "--as", WRITER];
    let said = run(at_a_terminal(100), tasks_answer(), &args);
    assert_eq!(said.status, Some(0), "{}", said.stderr);
    assert_eq!(said.asked, ["tasks", "list"]);
    let text = said.text();
    assert!(text.contains("\u{1b}[36min_progress\u{1b}[0m"), "{text:?}");
    assert_eq!(
        strip(&text),
        "\
CARD   LANE          IN LANE   TITLE                            HOLDER
AD-1   in_progress             Give the router one error type   writer
AD-2   ready                   Say which lease is in the way    -
and more: the board goes on; --offset 2 shows the next page
"
    );
}

/// `task show` at a terminal: the card for a person, the holder by name,
/// which is one more read than a script's.
#[test]
fn a_card_at_a_terminal_is_for_a_person() {
    let created = "2026-01-02T03:04:05Z";
    let mut task = card(
        "AD-1",
        "Give the router one error type",
        "in_progress",
        Some(WRITER),
    );
    task["description"] = json!("Three error enums say the same thing.");
    let detail = json!({
        "type": "task_detail",
        "task": task,
        "board": {
            "project": PROJECT,
            "slug": "main",
            "name": "Main",
            "template": "software",
            "lanes": [
                {"slug": "ready", "name": "Ready", "category": "ready"},
                {"slug": "in_progress", "name": "In progress", "category": "in_progress"},
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
        "history": [],
        "blockers": [],
        "children": [],
    });
    let names = json!({"type":"agents", "agents": [agent(WRITER, "writer", "claude-code", true)]});
    let said = run(
        Run {
            env: &[TERM, ("TZ", "UTC")],
            stdout: Stream::Terminal(100),
            ..Run::PIPED
        },
        vec![detail.clone(), names],
        &["task", "show", "AD-1"],
    );
    assert_eq!(said.status, Some(0), "{}", said.stderr);
    assert_eq!(said.asked, ["task_get", "list"]);
    let text = said.text();
    assert!(
        text.starts_with(
            "\u{1b}[1mAD-1\u{1b}[0m  \u{1b}[1mGive the router one error type\u{1b}[0m\n"
        ),
        "{text:?}"
    );
    assert_eq!(
        strip(&text),
        "\
AD-1  Give the router one error type
  In progress · held by writer
  board main

Done when
  It is done when it is done.

Description
  Three error enums say the same thing.

Comments
  2026-01-02 03:04 +00:00  writer
    Mapped: three enums, one of them unused.
"
    );
    // A script's card is as it was, by its one request.
    let piped = capture(vec![detail], &["task", "show", "AD-1"]);
    assert_eq!(piped.asked, ["task_get"]);
    assert!(
        piped
            .text()
            .starts_with("AD-1  in_progress  Give the router one error type  (aaaaaaaaaaaa)\n")
    );
}

/// One process nobody registered, as the daemon answers `discover`.
fn discovered() -> Value {
    json!({"type":"processes", "processes": [{
        "pid": 5151,
        "ppid": 1,
        "runtime": "claude-code",
        "command": "claude",
    }]})
}

/// `ps` with discovery on asks six things: the process nobody registered
/// is asked for between the agents and what each is doing.
fn ps_with_discovery() -> Vec<Value> {
    let mut answers = ps_answers(day());
    answers.insert(2, discovered());
    answers
}

/// Plain output at a terminal is the pipe's bytes with a process nobody
/// registered in the table too: that row used to be set back with escapes
/// whenever stdout was a terminal, whatever was asked.
#[test]
fn plain_at_a_terminal_is_the_pipes_bytes_with_discovery_on() {
    let piped = capture(ps_with_discovery(), &["ps"]);
    assert_eq!(piped.status, Some(0), "{}", piped.stderr);
    assert_eq!(
        piped.asked,
        ["list", "list", "discover", "activity", "usage", "restarts"]
    );
    assert!(piped.text().contains("unadopted"), "{}", piped.text());
    let at_terminal = run(
        at_a_terminal(200),
        ps_with_discovery(),
        &["ps", "--format", "plain", "--color", "never"],
    );
    assert_eq!(at_terminal.text(), piped.text());
    assert!(!at_terminal.text().contains('\u{1b}'));
    // For a person the row is there, set back, with the process id that
    // `adopt` takes.
    let human = run(at_a_terminal(200), ps_with_discovery(), &["ps"]);
    assert!(
        human.text().contains("\u{1b}[2m5151\u{1b}[0m"),
        "{:?}",
        human.text()
    );
}

/// A name is somebody's data on both streams: in the table it is written
/// out, and the readiness detail under `--input-details` names the agent
/// in plain words (core makes the name plain before it is said, in a
/// pipe as at a terminal), so a line break or a carriage return in a name
/// can break or overwrite neither.
#[test]
fn a_name_cannot_break_the_detail_said_about_it() {
    let agents = json!([agent(WRITER, "wri\nter\rX", "claude-code", true)]);
    let answers = || {
        vec![
            json!({"type":"agents", "agents": agents.clone()}),
            json!({"type":"agents", "agents": agents.clone()}),
            json!({"type":"activity", "activity": []}),
            usage(Vec::new()),
            json!({"type":"restarts", "outlooks": {}}),
        ]
    };
    let args = ["ps", "--no-discover", "--input-details"];
    let said = run(
        Run {
            env: &[TERM],
            stdout: Stream::Terminal(160),
            stderr: Stream::Terminal(160),
            ..Run::PIPED
        },
        answers(),
        &args,
    );
    assert_eq!(said.status, Some(0), "{}", said.stderr);
    let table = strip(&said.text());
    assert!(table.contains("wri\\nter\\rX"), "{table:?}");
    assert!(!table.contains("ter\rX"), "{table:?}");
    let detail = "  writerX (aaaaaaaaaaaa): Can't receive messages\n";
    assert!(said.stderr.starts_with(detail), "{:?}", said.stderr);
    assert_eq!(said.stderr.lines().count(), 2, "{:?}", said.stderr);
    assert!(!said.stderr.contains('\r'), "{:?}", said.stderr);
    // In a pipe the detail is the same words, as it always was.
    let piped = capture(answers(), &args);
    assert_eq!(piped.stderr, said.stderr);
}

/// A card's moments are each in the offset the reader's zone had at that
/// moment: a summer comment read in winter keeps summer's offset.
#[test]
fn a_summer_moment_keeps_summers_offset() {
    let mut task = card("AD-1", "Give the router one error type", "ready", None);
    task["acceptance"] = json!("");
    let comment = |id: &str, at: &str| {
        json!({
            "id": id,
            "task": "task-AD-1",
            "project": PROJECT,
            "author": "user",
            "body": "noted",
            "created_at": at,
        })
    };
    let detail = json!({
        "type": "task_detail",
        "task": task,
        "board": {
            "project": PROJECT,
            "slug": "main",
            "name": "Main",
            "template": "software",
            "lanes": [{"slug": "ready", "name": "Ready", "category": "ready"}],
            "pull_source": "ready",
            "pull_target": "ready",
            "key_prefix": "AD",
            "last_key": 2,
            "created_at": "2026-01-02T03:04:05Z",
            "updated_at": "2026-01-02T03:04:05Z",
        },
        "comments": [
            comment("summer", "2026-07-15T12:00:00Z"),
            comment("winter", "2026-11-15T12:00:00Z"),
        ],
        "history": [],
        "blockers": [],
        "children": [],
    });
    let said = run(
        Run {
            env: &[TERM, ("TZ", "America/Los_Angeles")],
            stdout: Stream::Terminal(100),
            ..Run::PIPED
        },
        vec![detail, json!({"type":"agents", "agents": []})],
        &["task", "show", "AD-1"],
    );
    assert_eq!(said.status, Some(0), "{}", said.stderr);
    let text = strip(&said.text());
    assert!(text.contains("  2026-07-15 05:00 -07:00  user\n"), "{text}");
    assert!(text.contains("  2026-11-15 04:00 -08:00  user\n"), "{text}");
}

/// A terminal that says it has no columns is laid out for as the
/// narrowest, not as one with all the room there is.
#[test]
fn a_terminal_of_no_width_is_the_narrowest() {
    let said = run(
        at_a_terminal(0),
        ps_answers(day()),
        &["ps", "--no-discover"],
    );
    assert_eq!(said.status, Some(0), "{}", said.stderr);
    let text = strip(&said.text());
    // Too narrow for the table: a block an agent, nothing left out.
    assert!(text.starts_with("aaaaaaaaaaaa\n  NAME"), "{text}");
    assert!(!text.contains("AGENT ID"), "{text}");
    assert!(
        text.contains("  STATUS") && text.contains("running"),
        "{text}"
    );
}
