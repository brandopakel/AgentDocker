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

use std::collections::{HashMap, HashSet};
use std::time::{Duration as StdDuration, Instant};

use super::*;
use agentdocker_core::{HUMAN, HUMAN_RUNTIME};
use agentdocker_host::procinfo;

/// Peers remembered at most; past it the memory starts again.
const REMEMBERED: usize = 256;
/// How long a peer found outside every agent's session is believed: an
/// agent that registers later can make a running process its own.
const OUTSIDE_FOR: StdDuration = StdDuration::from_secs(10);
/// How far up a peer's ancestry is walked.
const ANCESTRY: usize = 64;

/// What was found of each peer, by pid and birth: inside an agent's
/// session for good (the process cannot leave it), outside only for a while.
#[derive(Default)]
pub(crate) struct Peers {
    found: HashMap<(u32, i64), Found>,
}

#[derive(Clone)]
enum Found {
    Inside(String),
    Outside(Instant),
}

/// Who a request names as acting: its sender, the reader it acts for, the
/// agent it acts as; `user` for an answer that names nobody, which is the
/// person's; and `user` for a record registered as a person.
pub(crate) fn acting(request: &Request) -> Vec<String> {
    let Ok(serde_json::Value::Object(fields)) = serde_json::to_value(request) else {
        return Vec::new();
    };
    let op = fields.get("op").and_then(|op| op.as_str()).unwrap_or("");
    let mut names = Vec::new();
    for key in ["from", "agent", "reader"] {
        // A worktree's `from` is where it starts, not who asks.
        if key == "from" && op == "worktree_create" {
            continue;
        }
        match fields.get(key) {
            Some(serde_json::Value::String(name)) => names.push(name.clone()),
            None | Some(serde_json::Value::Null) if key == "from" && op == "answer" => {
                names.push(HUMAN.to_owned());
            }
            _ => {}
        }
    }
    if fields
        .get("spec")
        .and_then(|spec| spec.get("runtime"))
        .and_then(|runtime| runtime.as_str())
        == Some(HUMAN_RUNTIME)
    {
        names.push(HUMAN.to_owned());
    }
    names
}

/// Why `peer` is inside an agent's session, if it is: the first of its
/// ancestors (itself included) that is a provider's session process or a
/// live agent's.
fn inside(
    peer: u32,
    table: &[procinfo::Process],
    agents: &HashSet<u32>,
) -> Result<Option<String>, String> {
    let mut pid = peer;
    for _ in 0..ANCESTRY {
        if pid <= 1 {
            return Ok(None);
        }
        let Some(process) = table.iter().find(|p| p.pid == pid) else {
            return Err(format!(
                "process {pid}, which sent it or is among its parents, could not be traced"
            ));
        };
        if agents.contains(&pid) {
            return Ok(Some(format!("an agent's process ({pid})")));
        }
        if let Some(runtime) = procinfo::session_runtime_of(&process.argv) {
            return Ok(Some(format!("a {runtime} session ({pid})")));
        }
        if procinfo::is_codex_binary(&process.argv) {
            return Ok(Some(format!("a Codex server ({pid})")));
        }
        if process.ppid == pid {
            return Ok(None);
        }
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

impl Daemon {
    /// A refusal when `request` acts as the person and `peer`, the
    /// process on the socket, runs inside an agent's session. `None` lets
    /// it through.
    pub(crate) async fn person_refusal(
        self: &Arc<Self>,
        peer: Option<u32>,
        request: &Request,
    ) -> Option<Response> {
        if !self.person_check || !(mutates(request) || matches!(request, Request::Subscribe { .. }))
        {
            return None;
        }
        let names = acting(request);
        if names.is_empty() {
            return None;
        }
        let agents: HashSet<u32> = {
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
                .filter_map(|a| a.pid)
                .collect()
        };
        let Some(peer) = peer else {
            return Some(refusal("a process the daemon cannot identify"));
        };
        // The daemon's own callers, in this process.
        if peer == std::process::id() {
            return None;
        }
        let Some(born) = procinfo::start_time(peer) else {
            return Some(refusal("a process that could not be traced"));
        };
        let key = (peer, born.timestamp_micros());
        let known = lock(&self.peers).found.get(&key).cloned();
        let found = match known {
            Some(Found::Inside(reason)) => Found::Inside(reason),
            Some(Found::Outside(at)) if at.elapsed() < OUTSIDE_FOR => Found::Outside(at),
            _ => {
                let table = match tokio::task::spawn_blocking(procinfo::processes).await {
                    Ok(Ok(table)) => table,
                    _ => return Some(refusal("a process whose ancestry could not be read")),
                };
                let found = match inside(peer, &table, &agents) {
                    Ok(Some(reason)) => Found::Inside(reason),
                    Ok(None) => Found::Outside(Instant::now()),
                    Err(reason) => return Some(refusal(&reason)),
                };
                // The peer must still be the process that was traced.
                if procinfo::start_time(peer) != Some(born) {
                    return Some(refusal("a process that ended while it was traced"));
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
                warn!(peer, %reason, "refused a request acting as the person");
                Some(refusal(&reason))
            }
            Found::Outside(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::AgentSpec;

    fn process(pid: u32, ppid: u32, argv: &[&str]) -> procinfo::Process {
        procinfo::Process {
            pid,
            ppid,
            argv: argv.iter().map(|a| (*a).to_owned()).collect(),
        }
    }

    #[test]
    fn a_request_names_who_acts() {
        let named = |json: serde_json::Value| acting(&serde_json::from_value(json).unwrap());
        assert_eq!(
            named(
                serde_json::json!({"op": "role_assign", "from": "user", "agent": "a1", "add": ["reviewer"]})
            ),
            ["user", "a1"]
        );
        assert_eq!(
            named(serde_json::json!({"op": "answer", "message": "m1", "text": "Allow"})),
            ["user"],
            "an answer naming nobody is the person's"
        );
        assert_eq!(
            named(serde_json::json!({"op": "merge_branch", "agent": "a1", "head": "abc"})),
            ["user", "a1"],
            "a merge naming nobody is the person's"
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

    /// Over the real socket: a process under a live agent that says it is
    /// the person is refused before anything is done, and the same request
    /// from the daemon's own process is not.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_process_under_an_agent_cannot_act_as_the_person() {
        use std::io::Write as _;
        let tmp = tempfile::tempdir().unwrap();
        let daemon = Arc::new(Daemon::open(tmp.path().into(), tmp.path().join("s")).unwrap());
        let listener = crate::server::bind(&daemon).await.unwrap();
        let serving = tokio::spawn(crate::server::serve(daemon.clone(), listener));
        // The agent: a shell that, once told, runs a client asking as `user`.
        let client = "import socket,sys\ns=socket.socket(socket.AF_UNIX)\ns.connect(sys.argv[1])\ns.sendall(b'{\"op\":\"pause\",\"from\":\"user\",\"reason\":\"from an agent\"}\\n')\nprint(s.makefile().readline(),end='')\n";
        let mut agent = std::process::Command::new("sh")
            .arg("-c")
            .arg("read go; exec python3 -c \"$0\" \"$1\"")
            .arg(client)
            .arg(&daemon.socket)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let registered = daemon
            .handle(Request::Register {
                spec: AgentSpec {
                    name: "an-agent".into(),
                    workdir: Some(tmp.path().to_owned()),
                    ..AgentSpec::default()
                },
                pid: Some(agent.id()),
                session: None,
            })
            .await;
        assert!(
            matches!(registered, Response::Agent { .. }),
            "{registered:?}"
        );
        writeln!(agent.stdin.take().unwrap(), "go").unwrap();
        let output = tokio::task::spawn_blocking(move || agent.wait_with_output())
            .await
            .unwrap()
            .unwrap();
        let reply: Response = serde_json::from_slice(&output.stdout).unwrap();
        assert!(
            matches!(&reply, Response::Error { code: ErrorCode::Forbidden, message, .. }
                if message.contains("an agent's process")),
            "{reply:?}"
        );
        assert!(lock(&daemon.state).pauses.is_empty(), "nothing was paused");
        // The daemon's own process is trusted, whatever its ancestry.
        let mine = daemon
            .person_refusal(
                Some(std::process::id()),
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

    #[test]
    fn a_peer_under_a_provider_or_a_live_agent_is_inside_and_a_terminal_is_not() {
        let table = [
            process(1, 0, &["/sbin/launchd"]),
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
        ];
        let none = HashSet::new();
        assert_eq!(
            inside(12, &table, &none),
            Ok(None),
            "the person's own terminal"
        );
        assert!(
            matches!(inside(22, &table, &none), Ok(Some(reason)) if reason.contains("claude-code")),
            "{:?}",
            inside(22, &table, &none)
        );
        assert_eq!(inside(31, &table, &none), Ok(None));
        let agents = HashSet::from([30]);
        assert!(
            matches!(inside(31, &table, &agents), Ok(Some(_))),
            "under a live agent"
        );
        assert!(
            inside(99, &table, &none).is_err(),
            "untraceable authorises nothing"
        );
    }
}
