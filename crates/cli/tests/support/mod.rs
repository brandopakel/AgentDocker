//! The harness the output tests share: the real binary, run against a
//! daemon that answers from a list, with its streams where a test puts
//! them, and the made-up agents, cards and usage the tests answer with.
#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::{Command, Stdio};
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
pub fn serve(
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
pub struct Captured {
    pub stdout: Vec<u8>,
    pub stderr: String,
    pub status: Option<i32>,
    /// The `op` of each request it made, in order.
    pub asked: Vec<String>,
}

impl Captured {
    pub fn text(&self) -> String {
        String::from_utf8(self.stdout.clone()).expect("stdout is UTF-8")
    }
}

/// Where one of a command's output streams goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    /// A pipe: what a script reads.
    Pipe,
    /// A terminal of this many columns: what a person reads.
    Terminal(u16),
}

/// How a command is run.
#[derive(Clone, Copy)]
pub struct Run<'a> {
    /// The folder it runs in; its own empty home when none is given. A
    /// command that asks git something needs a checkout to ask.
    pub folder: Option<&'a Path>,
    /// What is set in its environment, beside the home and the socket.
    pub env: &'a [(&'a str, &'a str)],
    pub stdout: Stream,
    pub stderr: Stream,
}

impl Run<'_> {
    /// Both streams in pipes, nothing set: what a script gets.
    pub const PIPED: Run<'static> = Run {
        folder: None,
        env: &[],
        stdout: Stream::Pipe,
        stderr: Stream::Pipe,
    };
}

/// One command, its output in a pipe, against a daemon with `answers` to
/// give, with nothing inherited from the surrounding session.
pub fn capture(answers: Vec<Value>, args: &[&str]) -> Captured {
    run(Run::PIPED, answers, args)
}

/// [`capture`], run in `folder`.
pub fn capture_in(folder: Option<&Path>, answers: Vec<Value>, args: &[&str]) -> Captured {
    run(
        Run {
            folder,
            ..Run::PIPED
        },
        answers,
        args,
    )
}

/// Written into a terminal after the command has ended, to mark where
/// what it printed ends.
const END: &[u8] = b"\x1e\x1eend of what was printed\x1e\x1e";

/// Everything a terminal's far end is sent up to [`END`], with the line
/// endings a terminal adds taken out again.
///
/// It reads to a mark and not to the terminal's closing: what a command
/// printed just before it ended can be thrown away by a terminal whose
/// near end closes before the far end has read it. The mark is written
/// behind it, by this process, into a near end it kept open.
fn read_terminal(master: std::os::fd::OwnedFd) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut master = std::fs::File::from(master);
        let mut said = Vec::new();
        let mut buffer = [0u8; 4096];
        while !said.ends_with(END) {
            match master.read(&mut buffer) {
                // The near end closed with no mark: what there is.
                Ok(0) | Err(_) => break,
                Ok(read) => said.extend_from_slice(&buffer[..read]),
            }
        }
        if said.ends_with(END) {
            said.truncate(said.len() - END.len());
        }
        let mut out = Vec::with_capacity(said.len());
        let mut bytes = said.iter().copied().peekable();
        while let Some(byte) = bytes.next() {
            if byte == b'\r' && bytes.peek() == Some(&b'\n') {
                continue;
            }
            out.push(byte);
        }
        out
    })
}

/// One command against a daemon with `answers` to give, each of its
/// streams where `how` puts it, with nothing inherited from the
/// surrounding session but what `how` sets.
pub fn run(how: Run<'_>, answers: Vec<Value>, args: &[&str]) -> Captured {
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
    command
        .args(args)
        .current_dir(how.folder.unwrap_or(home.path()))
        .env("AGENTDOCKER_HOME", home.path())
        .env("AGENTDOCKER_SOCKET", &socket)
        .env("AGENTDOCKER_NO_AUTOSTART", "1")
        .envs(how.env.iter().copied())
        .stdin(Stdio::null());
    // A stream at a terminal is given a terminal's near end, of the
    // width asked for; the far end is read beside the wait, and this
    // process keeps a near end of its own to write the mark into.
    let terminal = |stream: Stream| match stream {
        Stream::Pipe => (Stdio::piped(), None),
        Stream::Terminal(columns) => {
            let mut pty = agentdocker_host::pty::Pty::open().expect("a terminal pair");
            pty.resize(columns, 24).expect("the terminal's size is set");
            let near = pty.take_slave().expect("the terminal's near end");
            let kept = std::fs::File::from(near.try_clone().expect("a second near end"));
            (
                Stdio::from(near),
                Some((read_terminal(pty.into_master()), kept)),
            )
        }
    };
    let (stdout, stdout_terminal) = terminal(how.stdout);
    let (stderr, stderr_terminal) = terminal(how.stderr);
    let child = command.stdout(stdout).stderr(stderr).spawn().unwrap();
    // The command holds this process's copies of the near ends until it
    // is dropped.
    drop(command);
    let output = child.wait_with_output().unwrap();
    let read = |terminal: Option<(std::thread::JoinHandle<Vec<u8>>, std::fs::File)>,
                piped: Vec<u8>| match terminal {
        Some((reader, mut kept)) => {
            kept.write_all(END).expect("the mark is written");
            let said = reader.join().expect("the terminal was read");
            drop(kept);
            said
        }
        None => piped,
    };
    let stdout = read(stdout_terminal, output.stdout);
    let stderr = read(stderr_terminal, output.stderr);
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
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr),
    );
    Captured {
        stdout,
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        status: output.status.code(),
        asked,
    }
}

/// A moment `seconds` before now, placed in the middle of its minute so
/// that "2m ago" stays "2m ago" for as long as a test runs.
pub fn ago(seconds: i64) -> DateTime<Utc> {
    Utc::now() - chrono::Duration::seconds(seconds)
}

pub fn agent(id: &str, name: &str, runtime: &str, managed: bool) -> AgentRecord {
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

pub const WRITER: &str = "aaaaaaaaaaaa0000000000000000aaaa";
pub const REVIEWER: &str = "bbbbbbbbbbbb0000000000000000bbbb";
pub const PROJECT: &str = "cccccccccccc0000000000000000cccc";

pub fn card(key: &str, title: &str, column: &str, assignee: Option<&str>) -> Value {
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
pub fn usage(rows: Vec<agentdocker_core::usage::report::Row>) -> Value {
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
pub fn tokens_of(agent: &str, input: u64, output: u64) -> agentdocker_core::usage::report::Row {
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
pub const PS_ASKS: [&str; 5] = ["list", "list", "activity", "usage", "restarts"];

pub fn ps_answers(report: Value) -> Vec<Value> {
    let agents = json!([
        agent(WRITER, "writer", "claude-code", true),
        agent(REVIEWER, "reviewer", "codex", false),
    ]);
    vec![
        json!({"type":"agents", "agents": agents.clone()}),
        json!({"type":"agents", "agents": agents}),
        json!({"type":"activity", "activity": []}),
        report,
        json!({"type":"restarts", "outlooks": {}}),
    ]
}

pub fn ps(report: Value) -> Captured {
    capture(ps_answers(report), &["ps", "--no-discover"])
}
