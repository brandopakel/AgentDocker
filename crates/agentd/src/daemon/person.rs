//! Who may act as the person. The host socket trusts its owning OS user,
//! and an agent's tools run as that same user, so a request's word that
//! the person sent it is a claim. A request that changes something as the
//! person — naming them as its sender, or registering a person's record —
//! is refused when the process on the other end of the socket runs inside
//! an agent's session: under a provider's own process (`claude`, `codex`
//! and the rest), or under a live agent's. The kernel says which process
//! that is; the request cannot. Reads are not refused, and the daemon's own
//! in-process callers are trusted.
//!
//! Unverifiable ancestry authorises nothing: a peer the daemon cannot
//! trace is refused the person's word, never granted it.

use std::collections::HashMap;
use std::time::{Duration as StdDuration, Instant};

use super::*;
use agentdocker_core::{HUMAN, HUMAN_RUNTIME};
use agentdocker_host::procinfo::{self, Unseen};

/// Peers remembered at most; past it the memory starts again.
const REMEMBERED: usize = 256;
/// How long a peer found outside every agent's session is believed, and
/// then only while no agent has come to stand among its parents.
const OUTSIDE_FOR: StdDuration = StdDuration::from_secs(10);
/// How far up a peer's ancestry is walked.
const ANCESTRY: usize = 64;

/// The process on a connection, as the kernel named it when it connected:
/// its pid and its birth, so a pid that ends and is reused is not taken
/// for it.
pub(crate) type Peer = (u32, Option<DateTime<Utc>>);

/// What was found of each peer, by pid and birth.
#[derive(Default)]
pub(crate) struct Peers {
    found: HashMap<(u32, i64), Found>,
}

#[derive(Clone)]
enum Found {
    /// Inside an agent's session for good: a process does not leave the
    /// session it was started in.
    Inside(String),
    /// Outside, as of then, through these parents.
    Outside(Instant, Vec<u32>),
}

/// Who a request names as acting: its sender, the reader it acts for, the
/// agent it acts as. The person stands for every request whose sender is
/// theirs when it names nobody (an answer, a read mark, a merge), the old
/// `role` request (which only the person makes), a record registered or
/// adopted as a person, and a `me` that moves the person somewhere.
pub(crate) fn acting(request: &Request) -> Vec<String> {
    let Ok(serde_json::Value::Object(fields)) = serde_json::to_value(request) else {
        return Vec::new();
    };
    let op = fields.get("op").and_then(|op| op.as_str()).unwrap_or("");
    let text =
        |value: Option<&serde_json::Value>| value.and_then(|v| v.as_str()).map(str::to_owned);
    let mut names = Vec::new();
    for key in ["from", "agent", "reader"] {
        // A worktree's `from` is where it starts, not who asks.
        if key == "from" && op == "worktree_create" {
            continue;
        }
        match fields.get(key) {
            Some(serde_json::Value::String(name)) => names.push(name.clone()),
            None | Some(serde_json::Value::Null)
                if (key == "from" && op == "answer") || (key == "reader" && op == "mark_read") =>
            {
                names.push(HUMAN.to_owned());
            }
            _ => {}
        }
    }
    // A journal digest that moves a reader's cursor acts for that reader.
    if let Some(reader) = text(fields.get("digest").and_then(|d| d.get("reader"))) {
        names.push(reader);
    }
    // Attaching to an agent's terminal is the person's: its input is their
    // keyboard there.
    let person = op == "role"
        || op == "attach"
        || (op == "me" && fields.get("workdir").is_some_and(|w| !w.is_null()))
        || text(fields.get("runtime")).as_deref() == Some(HUMAN_RUNTIME)
        || text(fields.get("spec").and_then(|spec| spec.get("runtime"))).as_deref()
            == Some(HUMAN_RUNTIME);
    if person {
        names.push(HUMAN.to_owned());
    }
    names
}

/// Where a walk up a peer's parents ended.
#[derive(Debug, PartialEq, Eq)]
enum Walk {
    Inside(String),
    Outside(Vec<u32>),
}

/// Walk up from `peer`, itself included. Inside an agent's session when a
/// process on the way is a provider's session process, a Codex server, or
/// a live agent's own process — by its pid *and* its birth, so a reused
/// pid is not taken for the agent. Outside when the chain reaches the
/// first process, a process that has ended, another user's, or a "parent"
/// born after its child (its pid was reused). This user's process that
/// still runs and cannot be read authorises nothing.
fn walk(
    peer: u32,
    table: &[procinfo::Process],
    agents: &HashMap<u32, DateTime<Utc>>,
    born: impl Fn(u32) -> Option<DateTime<Utc>>,
    unseen: impl Fn(u32) -> Unseen,
) -> Result<Walk, String> {
    let mut chain = Vec::new();
    let mut pid = peer;
    let mut child: Option<DateTime<Utc>> = None;
    for _ in 0..ANCESTRY {
        if pid <= 1 {
            return Ok(Walk::Outside(chain));
        }
        let Some(process) = table.iter().find(|p| p.pid == pid) else {
            return match unseen(pid) {
                Unseen::Ended | Unseen::OtherUser => Ok(Walk::Outside(chain)),
                Unseen::Unreadable => Err(format!(
                    "process {pid}, which sent it or is among its parents, could not be read"
                )),
            };
        };
        let Some(birth) = born(pid) else {
            // A row whose birth cannot be read proves nothing about whose it
            // is: past it only when it has ended or is another user's.
            return match unseen(pid) {
                Unseen::Ended | Unseen::OtherUser => Ok(Walk::Outside(chain)),
                Unseen::Unreadable => Err(format!(
                    "process {pid}, among its parents, has a birth that could not be read"
                )),
            };
        };
        let birth = Some(birth);
        if let (Some(birth), Some(child)) = (birth, child)
            && birth > child
        {
            return Ok(Walk::Outside(chain));
        }
        if birth.is_some() && agents.get(&pid) == birth.as_ref() {
            return Ok(Walk::Inside(format!("an agent's process ({pid})")));
        }
        if let Some(runtime) = procinfo::session_runtime_of(&process.argv) {
            return Ok(Walk::Inside(format!("a {runtime} session ({pid})")));
        }
        if procinfo::is_codex_binary(&process.argv) {
            return Ok(Walk::Inside(format!("a Codex server ({pid})")));
        }
        chain.push(pid);
        if process.ppid == pid {
            return Ok(Walk::Outside(chain));
        }
        child = birth.or(child);
        pid = process.ppid;
    }
    Err("its process ancestry is deeper than can be traced".to_owned())
}

fn refusal(reason: &str) -> Response {
    Response::error(
        ErrorCode::Forbidden,
        format!(
            "this acts as the person, and it was sent from inside {reason}; only the person does this, from their own terminal or the app"
        ),
    )
}

/// What the kernel says the process on a connection is running: its
/// executable and its arguments.
pub(crate) struct Running {
    executable: Option<std::path::PathBuf>,
    argv: Option<Vec<String>>,
}

impl Running {
    fn of(pid: u32) -> Self {
        Self {
            executable: procinfo::executable_path_of(pid).ok(),
            argv: procinfo::arguments(pid),
        }
    }
}

fn same_file(a: &std::path::Path, b: &std::path::Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// The CLI shipped beside this daemon: the only program a client may
/// launch as a supervised Codex terminal's controller.
pub(crate) fn companion_cli() -> Option<std::path::PathBuf> {
    let daemon = procinfo::executable_path().ok()?;
    Some(daemon.with_file_name(if cfg!(windows) {
        "agentdocker.exe"
    } else {
        "agentdocker"
    }))
}

/// Environment a client may not give a supervised Codex terminal's
/// controller: anything that loads code into it.
fn loads_code(key: &str) -> bool {
    key.starts_with("LD_") || key.starts_with("DYLD_")
}

/// A client's launch that asks to be a supervised Codex terminal must run
/// this daemon's own `agentdocker codex-input`: the controller is let send
/// what the person typed, so it is never a program of the client's choosing.
/// The command is resolved to the real file here, once, and launched as
/// that, so a link changed afterwards changes nothing; and nothing that
/// loads code into it may come with it.
pub(crate) fn admit_controller_launch(request: &mut Request) -> Option<Response> {
    admit_controller_launch_beside(request, companion_cli())
}

fn admit_controller_launch_beside(
    request: &mut Request,
    companion: Option<std::path::PathBuf>,
) -> Option<Response> {
    let (Request::Run { spec } | Request::RunContainer { spec, .. }) = request else {
        return None;
    };
    if !spec
        .env
        .get(agentdocker_host::provider_input::CODEX_INPUT_ENV)
        .is_some_and(|value| value == "1")
    {
        return None;
    }
    let refuse = |why: &str| {
        Some(Response::error(
            ErrorCode::Invalid,
            format!(
                "a supervised Codex terminal is started through this AgentDocker's own `agentdocker codex-input`, {why}"
            ),
        ))
    };
    if spec.env.keys().any(|key| loads_code(key)) {
        return refuse("with nothing that loads code into it");
    }
    let resolved = spec
        .command
        .first()
        .and_then(|program| std::fs::canonicalize(program).ok());
    let companion = companion.and_then(|cli| std::fs::canonicalize(cli).ok());
    match (resolved, companion) {
        (Some(resolved), Some(companion))
            if resolved == companion
                && spec.command.get(1).map(String::as_str) == Some("codex-input") =>
        {
            spec.command[0] = resolved.display().to_string();
            None
        }
        _ => refuse("and this is not it"),
    }
}

impl State {
    /// A terminal's own input path: the person typing into the terminal a
    /// supervised Codex runs in, sent by its controller to that agent. The
    /// controller is the agent's own registered process, exactly (never
    /// anything under it), running the program it was launched as — the
    /// daemon's companion `agentdocker codex-input`, checked at launch —
    /// as the kernel reports it.
    fn controller_typing(&self, peer: Peer, request: &Request, running: &Running) -> bool {
        let Request::Send { from, to, .. } = request else {
            return false;
        };
        if from != HUMAN {
            return false;
        }
        let Ok(id) = self.registry.resolve(to) else {
            return false;
        };
        self.registry.get(&id).is_some_and(|record| {
            record.status.is_live()
                && record.pid == Some(peer.0)
                && record.process_started_at.is_some()
                && record.process_started_at == peer.1
                && agentdocker_host::provider_input::is_codex_input(record)
                && record.spec.command.get(1).map(String::as_str) == Some("codex-input")
                && running
                    .argv
                    .as_ref()
                    .and_then(|argv| argv.get(1))
                    .map(String::as_str)
                    == Some("codex-input")
                && running.executable.as_ref().is_some_and(|exe| {
                    record
                        .spec
                        .command
                        .first()
                        .is_some_and(|program| same_file(exe, std::path::Path::new(program)))
                })
        })
    }
}

impl Daemon {
    /// [`admit_controller_launch`], for the socket.
    pub(crate) fn admit_launch(&self, request: &mut Request) -> Option<Response> {
        if !self.person_check {
            return None;
        }
        admit_controller_launch(request)
    }

    /// A refusal when `request` acts as the person and `peer`, the
    /// process on the socket, runs inside an agent's session. `None` lets
    /// it through.
    pub(crate) async fn person_refusal(
        self: &Arc<Self>,
        peer: Option<Peer>,
        request: &Request,
    ) -> Option<Response> {
        if !self.person_check
            || !(mutates(request)
                || matches!(request, Request::Subscribe { .. } | Request::Attach { .. }))
        {
            return None;
        }
        let names = acting(request);
        if names.is_empty() {
            return None;
        }
        // The daemon's own callers, in this process.
        if peer.is_some_and(|(pid, _)| pid == std::process::id()) {
            return None;
        }
        let agents: HashMap<u32, DateTime<Utc>> = {
            let state = lock(&self.state);
            let person = names.iter().any(|name| {
                name == HUMAN
                    || state
                        .registry
                        .resolve(name)
                        .ok()
                        .and_then(|id| state.registry.get(&id))
                        .is_some_and(humans::is_human)
            });
            if !person {
                return None;
            }
            state
                .registry
                .live()
                .filter(|a| !humans::is_human(a))
                .filter_map(|a| Some((a.pid?, a.process_started_at?)))
                .collect()
        };
        // The process that opened the connection, still: not another that
        // was given its pid. Checked before anything is let through.
        let Some((pid, Some(born))) = peer else {
            return Some(refusal("a process the daemon cannot identify"));
        };
        if procinfo::start_time(pid) != Some(born) {
            return Some(refusal("a connection whose process has ended"));
        }
        if matches!(request, Request::Send { from, .. } if from == HUMAN) {
            let running = Running::of(pid);
            if lock(&self.state).controller_typing((pid, Some(born)), request, &running) {
                return None;
            }
        }
        let key = (pid, born.timestamp_micros());
        let known = lock(&self.peers).found.get(&key).cloned();
        let found = match known {
            Some(Found::Inside(reason)) => Found::Inside(reason),
            // Believed a while, unless an agent now stands among its parents.
            Some(Found::Outside(at, chain))
                if at.elapsed() < OUTSIDE_FOR && !chain.iter().any(|p| agents.contains_key(p)) =>
            {
                Found::Outside(at, chain)
            }
            _ => {
                let table = match tokio::task::spawn_blocking(procinfo::processes).await {
                    Ok(Ok(table)) => table,
                    _ => return Some(refusal("a process whose ancestry could not be read")),
                };
                let found = match walk(pid, &table, &agents, procinfo::start_time, procinfo::unseen)
                {
                    Ok(Walk::Inside(reason)) => Found::Inside(reason),
                    Ok(Walk::Outside(chain)) => Found::Outside(Instant::now(), chain),
                    Err(reason) => return Some(refusal(&reason)),
                };
                if procinfo::start_time(pid) != Some(born) {
                    return Some(refusal(
                        "a connection whose process ended while it was traced",
                    ));
                }
                let mut peers = lock(&self.peers);
                if peers.found.len() >= REMEMBERED {
                    peers.found.clear();
                }
                peers.found.insert(key, found.clone());
                found
            }
        };
        match found {
            Found::Inside(reason) => {
                warn!(peer = pid, %reason, "refused a request acting as the person");
                Some(refusal(&reason))
            }
            Found::Outside(..) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::AgentSpec;
    use std::collections::BTreeMap;

    fn process(pid: u32, ppid: u32, argv: &[&str]) -> procinfo::Process {
        procinfo::Process {
            pid,
            ppid,
            argv: argv.iter().map(|a| (*a).to_owned()).collect(),
        }
    }

    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + seconds, 0).unwrap()
    }

    #[test]
    fn a_request_names_who_acts() {
        let named = |json: serde_json::Value| acting(&serde_json::from_value(json).unwrap());
        let person = |json: serde_json::Value| named(json).contains(&HUMAN.to_owned());
        assert_eq!(
            named(
                serde_json::json!({"op": "role_assign", "from": "user", "agent": "a1", "add": ["reviewer"]})
            ),
            ["user", "a1"]
        );
        assert!(
            person(serde_json::json!({"op": "answer", "message": "m1", "text": "Allow"})),
            "an unsigned answer"
        );
        assert!(
            person(serde_json::json!({"op": "merge_branch", "agent": "a1", "head": "abc"})),
            "a defaulted merge"
        );
        assert!(
            person(serde_json::json!({"op": "role", "agent": "a1", "role": "reviewer"})),
            "the old role request"
        );
        assert!(
            person(serde_json::json!({"op": "mark_read", "conversation": "c1", "through": 4})),
            "an unsigned read mark"
        );
        assert!(
            !person(
                serde_json::json!({"op": "mark_read", "conversation": "c1", "through": 4, "reader": "a1"})
            ),
            "an agent's own read mark"
        );
        assert!(
            person(
                serde_json::json!({"op": "journal", "project": "p", "digest": {"reader": "user", "max_entries": 5, "max_chars": 500, "advance": true}})
            ),
            "the person's digest cursor"
        );
        assert!(
            person(serde_json::json!({"op": "me", "workdir": "/repo"})),
            "moving the person"
        );
        assert!(
            !person(serde_json::json!({"op": "me"})),
            "looking the person up"
        );
        assert!(
            person(serde_json::json!({"op": "adopt", "pid": 7, "runtime": "human"})),
            "adopting a person"
        );
        assert!(
            !named(serde_json::json!({"op": "worktree_create", "agent": "a1", "path": "/x", "branch": "b", "from": "user"}))
                .contains(&"user".to_owned()),
            "a worktree's start point is not a sender"
        );
        let human = Request::Register {
            spec: AgentSpec {
                name: "me".into(),
                runtime: HUMAN_RUNTIME.into(),
                ..AgentSpec::default()
            },
            pid: None,
            session: None,
        };
        assert_eq!(acting(&human), ["user"], "registering a person");
    }

    #[test]
    fn a_walk_finds_a_session_or_a_proven_agent_and_stops_at_a_boundary() {
        let table = [
            process(10, 1, &["/usr/bin/login"]),
            process(11, 10, &["-zsh"]),
            process(12, 11, &["agentdocker", "role", "--as", "user"]),
            process(
                20,
                11,
                &[
                    "node",
                    "/usr/local/lib/node_modules/@anthropic-ai/claude-code/cli.js",
                ],
            ),
            process(21, 20, &["/bin/zsh", "-c", "agentdocker answer m1 Allow"]),
            process(22, 21, &["agentdocker", "answer", "m1", "Allow"]),
            process(30, 11, &["/opt/tools/some-agent"]),
            process(31, 30, &["agentdocker", "pause", "x"]),
            // Its "parent" 40 was born after it: that pid was reused.
            process(40, 11, &["/opt/tools/some-agent"]),
            process(41, 40, &["agentdocker", "pause", "x"]),
            // A Windows terminal under a service, and one whose parent ended.
            process(50, 900, &["WindowsTerminal.exe"]),
            process(51, 50, &["agentdocker.exe", "pause", "x"]),
            process(60, 901, &["explorer.exe"]),
            process(61, 60, &["agentdocker.exe", "pause", "x"]),
            process(70, 902, &["agentdocker", "pause", "x"]),
            // A parent whose birth cannot be read, still running; and one
            // whose birth cannot be read because it has just ended.
            process(80, 11, &["/opt/tools/custom-agent"]),
            process(81, 80, &["agentdocker", "pause", "x"]),
            process(90, 11, &["/opt/tools/gone"]),
            process(91, 90, &["agentdocker", "pause", "x"]),
        ];
        let born = |pid: u32| match pid {
            80 | 90 => None,
            40 => Some(at(100)),
            41 => Some(at(50)),
            _ => Some(at(i64::from(pid))),
        };
        let unseen = |pid: u32| match pid {
            900 => Unseen::OtherUser,
            901 | 90 => Unseen::Ended,
            _ => Unseen::Unreadable,
        };
        let none = HashMap::new();
        let go =
            |pid, agents: &HashMap<u32, DateTime<Utc>>| walk(pid, &table, agents, born, unseen);
        assert_eq!(
            go(12, &none),
            Ok(Walk::Outside(vec![12, 11, 10])),
            "the person's own terminal"
        );
        assert!(
            matches!(go(22, &none), Ok(Walk::Inside(reason)) if reason.contains("claude-code"))
        );
        assert!(matches!(go(31, &none), Ok(Walk::Outside(_))));
        // A live agent counts only by its pid and its birth.
        assert!(matches!(
            go(31, &HashMap::from([(30, at(30))])),
            Ok(Walk::Inside(_))
        ));
        assert!(
            matches!(
                go(31, &HashMap::from([(30, at(999))])),
                Ok(Walk::Outside(_))
            ),
            "a reused pid"
        );
        assert!(
            matches!(
                go(41, &HashMap::from([(40, at(100))])),
                Ok(Walk::Outside(_))
            ),
            "a parent born after its child"
        );
        assert!(
            matches!(go(51, &none), Ok(Walk::Outside(_))),
            "under another user's service"
        );
        assert!(
            matches!(go(61, &none), Ok(Walk::Outside(_))),
            "a parent that ended"
        );
        assert!(
            go(70, &none).is_err(),
            "this user's unreadable parent authorises nothing"
        );
        assert!(
            go(81, &none).is_err(),
            "a running parent of unreadable birth proves nothing"
        );
        assert!(
            matches!(go(91, &none), Ok(Walk::Outside(_))),
            "a parent that just ended"
        );
        assert!(
            go(99, &none).is_err(),
            "an untraceable peer authorises nothing"
        );
    }

    /// The controller exemption holds only for the agent's own process,
    /// born when recorded, running the very program it was launched as —
    /// the companion `agentdocker codex-input`.
    #[test]
    fn only_the_launched_controller_types_as_the_person() {
        let tmp = tempfile::tempdir().unwrap();
        let cli = tmp.path().join("agentdocker");
        let other = tmp.path().join("python3");
        std::fs::write(&cli, "x").unwrap();
        std::fs::write(&other, "x").unwrap();
        let daemon = Arc::new(Daemon::open(tmp.path().into(), tmp.path().join("s")).unwrap());
        let mut record = agentdocker_core::AgentRecord::new(
            AgentSpec {
                name: "codex-terminal".into(),
                runtime: "codex".into(),
                command: vec![
                    cli.display().to_string(),
                    "codex-input".into(),
                    "--".into(),
                    "codex".into(),
                ],
                env: [(
                    agentdocker_host::provider_input::CODEX_INPUT_ENV.to_owned(),
                    "1".to_owned(),
                )]
                .into(),
                ..AgentSpec::default()
            },
            true,
            Utc::now(),
        );
        record.status = agentdocker_core::AgentStatus::Running;
        record.pid = Some(4242);
        record.process_started_at = Some(at(1));
        let id = record.id.clone();
        lock(&daemon.state).registry.insert(record).unwrap();
        let send = Request::Send {
            from: HUMAN.into(),
            to: id.to_string(),
            kind: "chat".into(),
            payload: serde_json::json!({"text": "hello"}),
            reply_to: None,
            links: Vec::new(),
        };
        let running = |exe: &std::path::Path, sub: &str| Running {
            executable: Some(exe.to_owned()),
            argv: Some(vec![exe.display().to_string(), sub.to_owned()]),
        };
        let state = lock(&daemon.state);
        let peer = (4242, Some(at(1)));
        assert!(state.controller_typing(peer, &send, &running(&cli, "codex-input")));
        assert!(
            !state.controller_typing(peer, &send, &running(&other, "codex-input")),
            "another program"
        );
        assert!(
            !state.controller_typing(peer, &send, &running(&cli, "mcp")),
            "another subcommand"
        );
        assert!(
            !state.controller_typing((4242, Some(at(2))), &send, &running(&cli, "codex-input")),
            "another birth"
        );
        assert!(
            !state.controller_typing((4243, Some(at(1))), &send, &running(&cli, "codex-input")),
            "another process"
        );
    }

    /// A controller launch runs the companion CLI's real file, resolved
    /// once at admission, and carries nothing that loads code into it.
    #[cfg(unix)]
    #[test]
    fn a_controller_launch_is_the_real_companion_and_loads_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let cli = tmp.path().join("agentdocker");
        std::fs::write(&cli, "x").unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&cli, &link).unwrap();
        let launch = |program: &std::path::Path, extra: &[(&str, &str)]| {
            let mut env: BTreeMap<String, String> = [(
                agentdocker_host::provider_input::CODEX_INPUT_ENV.to_owned(),
                "1".to_owned(),
            )]
            .into();
            env.extend(
                extra
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned())),
            );
            Request::Run {
                spec: AgentSpec {
                    name: "codex".into(),
                    runtime: "codex".into(),
                    command: vec![
                        program.display().to_string(),
                        "codex-input".into(),
                        "--".into(),
                        "codex".into(),
                    ],
                    env,
                    ..AgentSpec::default()
                },
            }
        };
        // Through a link: admitted as the file it names, so a later change
        // to the link changes nothing.
        let mut request = launch(&link, &[]);
        assert!(admit_controller_launch_beside(&mut request, Some(cli.clone())).is_none());
        let Request::Run { spec } = &request else {
            unreachable!()
        };
        assert_eq!(
            std::path::Path::new(&spec.command[0]),
            std::fs::canonicalize(&cli).unwrap()
        );
        for (key, value) in [
            ("LD_PRELOAD", "/tmp/x.so"),
            ("DYLD_INSERT_LIBRARIES", "/tmp/x.dylib"),
        ] {
            let mut request = launch(&cli, &[(key, value)]);
            assert!(
                admit_controller_launch_beside(&mut request, Some(cli.clone())).is_some(),
                "{key}"
            );
        }
        let other = tmp.path().join("python3");
        std::fs::write(&other, "x").unwrap();
        assert!(
            admit_controller_launch_beside(&mut launch(&other, &[]), Some(cli.clone())).is_some()
        );
        // An ordinary launch is not this check's to judge.
        let mut ordinary = Request::Run {
            spec: AgentSpec {
                name: "x".into(),
                command: vec!["/bin/sh".into()],
                ..AgentSpec::default()
            },
        };
        assert!(admit_controller_launch_beside(&mut ordinary, Some(cli)).is_none());
    }

    /// On Windows, where the table holds only this user's readable
    /// processes, a test run's own ancestry reaches a boundary rather than
    /// a refusal.
    #[cfg(windows)]
    #[test]
    fn a_windows_process_s_own_ancestry_is_walked_to_a_boundary() {
        let table = procinfo::processes().unwrap();
        let walked = walk(
            std::process::id(),
            &table,
            &HashMap::new(),
            procinfo::start_time,
            procinfo::unseen,
        );
        assert!(walked.is_ok(), "{walked:?}");
    }

    /// Over the real socket: a process under a live agent that says it is
    /// the person is refused before anything is done; the same request from
    /// the daemon's own process is not; and a supervised Codex terminal's
    /// controller — the agent's own process, exactly — may queue what the
    /// person typed to that agent.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_process_under_an_agent_cannot_act_as_the_person() {
        use std::io::Write as _;
        let tmp = tempfile::tempdir().unwrap();
        let daemon = Arc::new(Daemon::open(tmp.path().into(), tmp.path().join("s")).unwrap());
        let listener = crate::server::bind(&daemon).await.unwrap();
        let serving = tokio::spawn(crate::server::serve(daemon.clone(), listener));
        // A shell that, once told, becomes a client sending `request`.
        let client = "import socket,sys\ns=socket.socket(socket.AF_UNIX)\ns.connect(sys.argv[1])\ns.sendall(sys.argv[2].encode()+b'\\n')\nprint(s.makefile().readline(),end='')\n";
        let spawn = |request: String| {
            std::process::Command::new("sh")
                .arg("-c")
                .arg("read go; exec python3 -c \"$0\" \"$1\" \"$2\"")
                .arg(client)
                .arg(&daemon.socket)
                .arg(request)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap()
        };
        let register = |name: &str, pid: u32| {
            let daemon = daemon.clone();
            let name = name.to_owned();
            let workdir = tmp.path().to_owned();
            async move {
                match daemon
                    .handle(Request::Register {
                        spec: AgentSpec {
                            name,
                            workdir: Some(workdir),
                            ..AgentSpec::default()
                        },
                        pid: Some(pid),
                        session: None,
                    })
                    .await
                {
                    Response::Agent { agent } => agent,
                    other => panic!("{other:?}"),
                }
            }
        };
        let finish = |mut child: std::process::Child| async move {
            writeln!(child.stdin.take().unwrap(), "go").unwrap();
            let output = tokio::task::spawn_blocking(move || child.wait_with_output())
                .await
                .unwrap()
                .unwrap();
            serde_json::from_slice::<Response>(&output.stdout).unwrap()
        };

        let agent = spawn(r#"{"op":"pause","from":"user","reason":"from an agent"}"#.into());
        register("an-agent", agent.id()).await;
        let reply = finish(agent).await;
        assert!(
            matches!(&reply, Response::Error { code: ErrorCode::Forbidden, message, .. }
                if message.contains("an agent's process")),
            "{reply:?}"
        );
        assert!(lock(&daemon.state).pauses.is_empty(), "nothing was paused");

        // A program that is not the daemon's companion controller, marked as a
        // supervised Codex terminal, typing "as the person" to itself: refused.
        let typed = spawn(String::new());
        let record = register("codex-terminal", typed.id()).await;
        {
            let mut state = lock(&daemon.state);
            let record = state.registry.get_mut(&record.id).unwrap();
            record.managed = true;
            record.spec.runtime = "codex".into();
            record.spec.command = vec!["python3".into(), "codex-input".into()];
            record.spec.env.insert(
                agentdocker_host::provider_input::CODEX_INPUT_ENV.into(),
                "1".into(),
            );
        }
        drop(typed);
        let typed = spawn(format!(
            r#"{{"op":"send","from":"user","to":"{}","kind":"chat","payload":{{"text":"hello"}}}}"#,
            record.id
        ));
        {
            let mut state = lock(&daemon.state);
            let record = state.registry.get_mut(&record.id).unwrap();
            record.pid = Some(typed.id());
            record.process_started_at = procinfo::start_time(typed.id());
        }
        let reply = finish(typed).await;
        assert!(
            matches!(
                &reply,
                Response::Error {
                    code: ErrorCode::Forbidden,
                    ..
                }
            ),
            "{reply:?}"
        );
        // Nor may a client launch such a program as one.
        let launch = spawn(
            r#"{"op":"run","spec":{"name":"fake","runtime":"codex","command":["python3","codex-input"],"env":{"AGENTDOCKER_CODEX_INPUT":"1"}}}"#
                .into(),
        );
        let reply = finish(launch).await;
        assert!(
            matches!(&reply, Response::Error { code: ErrorCode::Invalid, message, .. }
                if message.contains("codex-input")),
            "{reply:?}"
        );

        // The daemon's own process is trusted, whatever its ancestry.
        let mine = daemon
            .person_refusal(
                Some((std::process::id(), procinfo::start_time(std::process::id()))),
                &Request::Answer {
                    from: None,
                    message: agentdocker_core::MessageId::from("m".to_owned()),
                    text: "Allow".into(),
                },
            )
            .await;
        assert!(mine.is_none(), "{mine:?}");
        serving.abort();
    }
}
