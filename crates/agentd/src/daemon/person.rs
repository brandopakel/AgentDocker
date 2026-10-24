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

/// Where the process on a connection was found to run.
pub(super) enum Traced {
    /// The person's: outside every agent's session, or the daemon's own.
    Person,
    /// Inside an agent's session, as found.
    Inside(String),
    /// Somewhere that cannot be told, for this reason; it authorises
    /// nothing.
    Unknown(String),
}

/// Walk up from `peer`, itself included. Inside an agent's session when a
/// process on the way is one ([`agent_session`]). Outside when the chain
/// reaches the first process, a process that has ended, another user's, or
/// a "parent" born after its child (its pid was reused). This user's process
/// that still runs and cannot be read authorises nothing.
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
        if child.is_some_and(|child| birth > child) {
            return Ok(Walk::Outside(chain));
        }
        if let Some(reason) = agent_session(process, birth, agents) {
            return Ok(Walk::Inside(reason));
        }
        chain.push(pid);
        if process.ppid == pid {
            return Ok(Walk::Outside(chain));
        }
        child = Some(birth);
        pid = process.ppid;
    }
    Err("its process ancestry is deeper than can be traced".to_owned())
}

/// Whether `process`, born `birth`, is an agent's session, and which: a live
/// agent's own process — by its pid *and* its birth, so a reused pid is not
/// taken for the agent — a provider's session process, or a Codex server.
fn agent_session(
    process: &procinfo::Process,
    birth: DateTime<Utc>,
    agents: &HashMap<u32, DateTime<Utc>>,
) -> Option<String> {
    let pid = process.pid;
    if agents.get(&pid) == Some(&birth) {
        return Some(format!("an agent's process ({pid})"));
    }
    if let Some(runtime) = procinfo::session_runtime_of(&process.argv) {
        return Some(format!("a {runtime} session ({pid})"));
    }
    if procinfo::is_codex_binary(&process.argv) {
        return Some(format!("a Codex server ({pid})"));
    }
    None
}

/// Who started a session that registered itself, as its parents say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Starter {
    /// Its parents reach the top — the first process, or another user's —
    /// through one of the person's session roots ([`session_root`]) and
    /// with no agent's session anywhere among them.
    Person,
    /// An agent's session ([`agent_session`]) stands among its parents,
    /// below a root or above one.
    Agent,
    /// Its parents cannot say: they reach the top through no root (an
    /// orphan, a shell whose agent exited, a multiplexer server), or the
    /// chain breaks first, at a parent that has ended or a "parent" born
    /// after its child (a reused pid).
    Unknown,
}

impl Starter {
    /// The value its record's `launched_by` label takes.
    fn label(self) -> &'static str {
        match self {
            Starter::Person => HUMAN,
            Starter::Agent => agentdocker_core::agent::LAUNCHED_BY_AGENT,
            Starter::Unknown => agentdocker_core::agent::LAUNCHED_BY_UNKNOWN,
        }
    }
}

/// Who started the session whose process is `pid`, born `birth`, walking
/// every parent from its own up to the top: an agent's session anywhere on
/// the way makes it [`Starter::Agent`]; a root on the way (`root`) is
/// evidence for the person and never the end of the walk, so an agent or
/// the daemon above a terminal still counts; the top reached with a root
/// below it is [`Starter::Person`], without one or past a break in the chain
/// [`Starter::Unknown`]. `None` when it cannot be read now (the process is
/// not that one any more, a row this user owns is unreadable, or with no
/// root found a root check could not say, as a signature check that timed
/// out) and when the daemon's own process is the session or among its
/// parents, whose launches are said at launch: nothing is decided, and the
/// next registration traces again.
#[allow(clippy::too_many_arguments)]
fn starter(
    pid: u32,
    birth: DateTime<Utc>,
    own: u32,
    table: &[procinfo::Process],
    agents: &HashMap<u32, DateTime<Utc>>,
    born: impl Fn(u32) -> Option<DateTime<Utc>>,
    unseen: impl Fn(u32) -> Unseen,
    root: impl Fn(&procinfo::Process) -> Option<bool>,
) -> Option<Starter> {
    if pid == own || born(pid) != Some(birth) {
        return None;
    }
    let session = table.iter().find(|p| p.pid == pid)?;
    // A root found, and a process whose check could not say whether it is
    // one: with no root found, that leaves the top undecided.
    let mut rooted = false;
    let mut undecided = false;
    let top = |rooted: bool, undecided: bool| {
        if rooted {
            Some(Starter::Person)
        } else if undecided {
            None
        } else {
            Some(Starter::Unknown)
        }
    };
    let mut child = birth;
    let mut current = session.ppid;
    for _ in 0..ANCESTRY {
        if current == own {
            return None;
        }
        if current <= 1 {
            return top(rooted, undecided);
        }
        let missing = |pid: u32| match unseen(pid) {
            Unseen::Ended => Some(Starter::Unknown),
            Unseen::OtherUser => top(rooted, undecided),
            Unseen::Unreadable => None,
        };
        let Some(process) = table.iter().find(|p| p.pid == current) else {
            return missing(current);
        };
        let Some(birth) = born(current) else {
            return missing(current);
        };
        if birth > child {
            return Some(Starter::Unknown);
        }
        if agent_session(process, birth, agents).is_some() {
            return Some(Starter::Agent);
        }
        match root(process) {
            Some(true) => rooted = true,
            Some(false) => {}
            None => undecided = true,
        }
        if process.ppid == current {
            return top(rooted, undecided);
        }
        child = birth;
        current = process.ppid;
    }
    Some(Starter::Unknown)
}

/// `login` and the SSH session leaders at the system's own paths.
const ROOT_PROGRAMS: &[&str] = &[
    "/usr/bin/login",
    "/bin/login",
    "/usr/sbin/sshd",
    "/usr/sbin/sshd-session",
    "/usr/libexec/sshd-session",
    "/usr/lib/openssh/sshd-session",
    "/usr/libexec/openssh/sshd-session",
];

/// Where a person's terminal and editor applications are installed on
/// macOS, root's: the sealed system's own folders and the shared
/// Applications folder.
const APPLICATION_FOLDERS: &[&str] = &[
    "/System/Applications",
    "/System/Applications/Utilities",
    "/Applications",
    "/Applications/Utilities",
];

/// Where the person's own installs of them are, on macOS: the shared
/// Applications folder and their own `~/Applications` (`HOME` taken in
/// [`root_program`]). A bundle here that root does not own counts only
/// when its code signature verifies ([`codesign_verifies`]).
const OWN_APPLICATION_FOLDERS: &[&str] = &["/Applications", "/Applications/Utilities"];

/// How long a signature check may take: a large application's sealed
/// resources take a second or two to hash.
const SIGNATURE_CHECK: StdDuration = StdDuration::from_secs(5);

/// The signature check's requirement: a certificate chain to Apple's root
/// (an Apple, App Store or Developer ID signature), which a process of the
/// person's cannot make. A signature it makes for itself (ad hoc, or its
/// own certificate) verifies without it.
const SIGNATURE_REQUIREMENT: &str = "anchor apple generic";

/// One of the person's session roots, by the executable the kernel names
/// for `process` ([`root_program`]); not one when it cannot be named.
fn session_root(process: &procinfo::Process) -> Option<bool> {
    match procinfo::executable_path_of(process.pid) {
        Ok(executable) => root_program(&executable),
        Err(_) => Some(false),
    }
}

/// Whether `executable` is a program the person's sessions start under,
/// where nothing an agent can write could stand in for it: `login`, `sshd`
/// or `sshd-session` at the system's own paths ([`ROOT_PROGRAMS`]); on
/// macOS, a known terminal or editor application (by its bundle's name,
/// never any bundle) installed directly in a system or the shared
/// Applications folder; elsewhere on Unix, a known terminal emulator under
/// `/usr`, `/bin` or `/sbin`. The file, and every folder from its bundle
/// (or its own folder) down to it, is root's and nobody else may write it
/// ([`root_owned`]), as the system's own programs and an App Store or
/// administrator's install are. On macOS a known application of the
/// person's own — in the shared or their own Applications folder, not
/// root's — counts too when the program running is the bundle's own (in
/// its `Contents/MacOS`) and the bundle's signature verifies now
/// ([`codesign_verifies`]); `None` when that check could not say. A copy,
/// an unsigned or self-signed bundle, or one with anything added inside is
/// no root.
fn root_program(executable: &std::path::Path) -> Option<bool> {
    let Ok(executable) = std::fs::canonicalize(executable) else {
        return Some(false);
    };
    let home = std::env::home_dir().and_then(|home| std::fs::canonicalize(home).ok());
    root_at(&executable, home.as_deref(), root_owned, codesign_verifies)
}

/// [`root_program`]'s rule for a resolved `executable`, with `home` the
/// person's home folder, `owned` saying whether root alone owns and may
/// write a file or folder, and `verifies` whether a bundle's signature
/// verifies (`None`: it could not say).
fn root_at(
    executable: &std::path::Path,
    home: Option<&std::path::Path>,
    owned: impl Fn(&std::path::Path) -> bool,
    verifies: impl Fn(&std::path::Path) -> Option<bool>,
) -> Option<bool> {
    let path = std::path::Path::new;
    let system = ROOT_PROGRAMS
        .iter()
        .any(|program| executable == path(program));
    let host = agentdocker_core::terminal::classify(executable).filter(|host| host.kind != "app");
    // The file, and each folder from `top` down to it.
    let owned_from = |top: &std::path::Path| {
        executable.starts_with(top)
            && executable
                .ancestors()
                .take_while(|folder| folder.starts_with(top))
                .all(&owned)
    };
    let Some(folder) = executable.parent() else {
        return Some(false);
    };
    if system {
        return Some(owned_from(folder));
    }
    if cfg!(target_os = "macos") {
        let Some(bundle) = host.and_then(|host| host.app) else {
            return Some(false);
        };
        let Some(installed) = bundle.parent() else {
            return Some(false);
        };
        let in_folder = |folders: &[&str]| {
            folders
                .iter()
                .any(|known| installed == std::path::Path::new(known))
        };
        if in_folder(APPLICATION_FOLDERS) && owned_from(&bundle) {
            return Some(true);
        }
        let own_folder = in_folder(OWN_APPLICATION_FOLDERS)
            || home.is_some_and(|home| installed == home.join("Applications"));
        if !own_folder || folder != bundle.join("Contents").join("MacOS") {
            return Some(false);
        }
        verifies(&bundle)
    } else if cfg!(unix) {
        Some(
            host.is_some_and(|host| host.app.is_none())
                && ["/usr/", "/bin/", "/sbin/"]
                    .iter()
                    .any(|dir| executable.starts_with(dir))
                && owned_from(folder),
        )
    } else {
        Some(false)
    }
}

/// `codesign --verify --strict` of `bundle` against
/// [`SIGNATURE_REQUIREMENT`], within [`SIGNATURE_CHECK`], each time it is
/// asked — a verdict kept would outlive a change to any sealed file:
/// whether it verifies — its code and every sealed resource intact, so a
/// program added inside breaks it — or `None` when the check could not say
/// (it timed out or could not run).
fn codesign_verifies(bundle: &std::path::Path) -> Option<bool> {
    let argv = [
        "/usr/bin/codesign".to_owned(),
        "--verify".to_owned(),
        "--strict".to_owned(),
        format!("-R={SIGNATURE_REQUIREMENT}"),
        bundle.to_string_lossy().into_owned(),
    ];
    agentdocker_host::command::run(std::path::Path::new("/"), &argv, SIGNATURE_CHECK)
        .ok()
        .map(|output| output.success)
}

/// Whether root owns `path` and nobody else may write it: what a process
/// of the person's cannot make or change.
#[cfg(unix)]
fn root_owned(path: &std::path::Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.uid() == 0 && meta.mode() & 0o022 == 0)
}

#[cfg(not(unix))]
fn root_owned(_: &std::path::Path) -> bool {
    false
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

/// Whether `a` and `b` are one file, by its identity rather than a name:
/// `agentd` and `agentdocker` are one file, and macOS may name a process
/// running it by either.
fn same_file(a: &std::path::Path, b: &std::path::Path) -> bool {
    agentdocker_host::program::same_file(a, b)
}

/// The CLI shipped beside this daemon: the only program a client may
/// launch as a supervised Codex terminal's controller. The daemon is the
/// CLI's own file under the name `agentd` (or the CLI itself, run as
/// `agentdocker daemon run`), so this is the `agentdocker` name in the
/// daemon's directory: a path, not just a file, because a process started
/// by it must start as the CLI, and the name it is started by decides that.
pub(crate) fn companion_cli() -> Option<std::path::PathBuf> {
    let daemon = procinfo::executable_path().ok()?;
    Some(daemon.with_file_name(agentdocker_host::program::Role::Cli.file_name()))
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
/// loads code into it may come with it. It is compared by resolved path,
/// not by file: `agentd` is the same file, and launched by that name it
/// would be the daemon, not the CLI.
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
    /// as the kernel reports it: that file, invoked as the CLI.
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
                && running
                    .argv
                    .as_ref()
                    .and_then(|argv| argv.first())
                    .is_some_and(|argv0| {
                        agentdocker_host::program::Role::invoked_as(std::ffi::OsStr::new(argv0))
                            == agentdocker_host::program::Role::Cli
                    })
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

impl State {
    /// Say on a registered session who started it, once: `launched_by` on
    /// its record and `agent_launched_by`, in one commit. Only while the
    /// record is still that process (`pid`, `birth`) and still
    /// [`AgentRecord::starter_traceable`] — what is said is never said
    /// again, so a later loss of its ancestry cannot change it; `None`
    /// otherwise, or when the write failed.
    fn write_starter(
        &mut self,
        id: &AgentId,
        pid: u32,
        birth: DateTime<Utc>,
        starter: Starter,
        now: DateTime<Utc>,
    ) -> Option<AgentRecord> {
        let mut record = self.registry.get(id)?.clone();
        if !record.starter_traceable()
            || record.pid != Some(pid)
            || record.process_started_at != Some(birth)
        {
            return None;
        }
        let by = starter.label().to_owned();
        record.spec.labels.insert(
            agentdocker_core::agent::LAUNCHED_BY_LABEL.to_owned(),
            by.clone(),
        );
        let event = Event::new(
            EventKind::AgentLaunchedBy {
                agent: id.clone(),
                project: record.project.as_ref().map(ProjectRef::id),
                by,
            },
            now,
        );
        let _ = self.transition(
            "launcher",
            event,
            |store, event| store.agent_transition(&record, event),
            |state| {
                if let Some(current) = state.registry.get_mut(id) {
                    *current = record.clone();
                }
            },
        );
        if self.write_failure().is_some() {
            return None;
        }
        Some(record)
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
        {
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
        }
        match self.trace(peer, request).await {
            Traced::Person => None,
            Traced::Inside(reason) => {
                let pid = peer.map_or(0, |(pid, _)| pid);
                warn!(peer = pid, %reason, "refused a request acting as the person");
                Some(refusal(&reason))
            }
            Traced::Unknown(reason) => Some(refusal(&reason)),
        }
    }

    /// A client's launch is the person's when the process that sent it is
    /// theirs ([`Traced::Person`]: the app, their own terminal), and then
    /// it carries `launched_by=user`. Whatever a client wrote there was
    /// dropped at admission, so a launch from inside an agent's session, or
    /// from a process that cannot be traced, carries none. Traced whether or
    /// not the person check is on: turning authorisation off lets a launch
    /// through, and says nothing of whose it is.
    pub(crate) async fn mark_launcher(self: &Arc<Self>, peer: Option<Peer>, request: &mut Request) {
        if !matches!(
            request,
            Request::Run { .. } | Request::RunContainer { .. } | Request::ResumeSession { .. }
        ) {
            return;
        }
        let persons = matches!(self.trace(peer, request).await, Traced::Person);
        if let Request::Run { spec }
        | Request::RunContainer { spec, .. }
        | Request::ResumeSession { spec, .. } = request
            && persons
        {
            spec.labels.insert(
                agentdocker_core::agent::LAUNCHED_BY_LABEL.to_owned(),
                HUMAN.to_owned(),
            );
        }
    }

    /// After a registration (`register`, `adopt`, `session_attach`) answers
    /// with a record whose starter nothing has said yet
    /// ([`AgentRecord::starter_traceable`]): its process's parents say who
    /// started it ([`starter`]) and the record says so from here on — the
    /// person (`launched_by=user`), an agent, or that it cannot be known.
    /// Traced on its own task once the answer is on its way, so the reply
    /// waits for neither the process table nor a signature check, and no
    /// lock is held while either runs; the label goes in its own commit,
    /// only while the record is still that process. A client's word plays
    /// no part; what cannot be read now decides nothing.
    pub(super) fn trace_starter(self: &Arc<Self>, response: &Response) {
        let Response::Agent { agent } = response else {
            return;
        };
        let (Some(pid), Some(birth)) = (agent.pid, agent.process_started_at) else {
            return;
        };
        if !agent.starter_traceable() {
            return;
        }
        let id = agent.id.clone();
        let daemon = self.clone();
        tokio::spawn(async move {
            daemon.write_traced_starter(id, pid, birth).await;
        });
    }

    /// [`Daemon::trace_starter`]'s work: the trace off the lock, then the
    /// label, admitted like any background write.
    async fn write_traced_starter(
        self: Arc<Self>,
        id: AgentId,
        pid: u32,
        birth: DateTime<Utc>,
    ) -> Option<AgentRecord> {
        let agents: HashMap<u32, DateTime<Utc>> = lock(&self.state)
            .registry
            .live()
            .filter(|a| !humans::is_human(a) && a.pid != Some(pid))
            .filter_map(|a| Some((a.pid?, a.process_started_at?)))
            .collect();
        let found = tokio::task::spawn_blocking(move || {
            let table = procinfo::processes().ok()?;
            starter(
                pid,
                birth,
                std::process::id(),
                &table,
                &agents,
                procinfo::start_time,
                procinfo::unseen,
                session_root,
            )
        })
        .await
        .ok()
        .flatten()?;
        let _admitted = self.admit_background().ok()?;
        lock(&self.state).write_starter(&id, pid, birth, found, Utc::now())
    }

    /// Where the process on a connection runs: the person's, inside an
    /// agent's session, or somewhere that cannot be told. The process must
    /// still be the one that connected; what was found is remembered by
    /// its pid and birth ([`Peers`]).
    pub(super) async fn trace(self: &Arc<Self>, peer: Option<Peer>, request: &Request) -> Traced {
        // The daemon's own callers, in this process.
        if peer.is_some_and(|(pid, _)| pid == std::process::id()) {
            return Traced::Person;
        }
        let agents: HashMap<u32, DateTime<Utc>> = lock(&self.state)
            .registry
            .live()
            .filter(|a| !humans::is_human(a))
            .filter_map(|a| Some((a.pid?, a.process_started_at?)))
            .collect();
        let unknown = |reason: &str| Traced::Unknown(reason.to_owned());
        // The process that opened the connection, still: not another that
        // was given its pid. Checked before anything is let through.
        let Some((pid, Some(born))) = peer else {
            return unknown("a process the daemon cannot identify");
        };
        if procinfo::start_time(pid) != Some(born) {
            return unknown("a connection whose process has ended");
        }
        if matches!(request, Request::Send { from, .. } if from == HUMAN) {
            let running = Running::of(pid);
            if lock(&self.state).controller_typing((pid, Some(born)), request, &running) {
                return Traced::Person;
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
                    _ => return unknown("a process whose ancestry could not be read"),
                };
                // Another user's parent (root's `login` above a terminal) is
                // read too, so the walk goes on past it to the top.
                let found = match walk(
                    pid,
                    &table,
                    &agents,
                    procinfo::start_time_of_any,
                    procinfo::unseen,
                ) {
                    Ok(Walk::Inside(reason)) => Found::Inside(reason),
                    Ok(Walk::Outside(chain)) => Found::Outside(Instant::now(), chain),
                    Err(reason) => return Traced::Unknown(reason),
                };
                if procinfo::start_time(pid) != Some(born) {
                    return unknown("a connection whose process ended while it was traced");
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
            Found::Inside(reason) => Traced::Inside(reason),
            Found::Outside(..) => Traced::Person,
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

    /// Who started a registered session, from all of its parents: the
    /// person only when they reach the top through one of the person's
    /// roots with no agent's session anywhere on the way, a terminal an
    /// agent or the daemon started included; an agent when one stands among
    /// them; unknown when no root is passed or the chain breaks — an orphan,
    /// a shell orphaned when its agent exited, an ended or reused
    /// grandparent, a multiplexer server, an ended parent above a root —
    /// and nothing at all when a row cannot be read now, the process is not
    /// the one recorded, or the daemon is it or among its parents.
    #[test]
    fn a_session_is_the_persons_only_when_its_parents_reach_the_persons_root() {
        let table = [
            process(
                5,
                1,
                &["/System/Applications/Utilities/Terminal.app/Contents/MacOS/Terminal"],
            ),
            process(10, 5, &["login", "-pf", "someone"]),
            process(11, 10, &["-zsh"]),
            // In the person's terminal.
            process(12, 11, &["claude"]),
            // From a provider session's tool.
            process(
                20,
                11,
                &[
                    "node",
                    "/usr/local/lib/node_modules/@anthropic-ai/claude-code/cli.js",
                ],
            ),
            process(21, 20, &["/bin/zsh", "-c", "claude -p go"]),
            process(22, 21, &["claude", "-p", "go"]),
            // In a terminal that session opened.
            process(23, 21, &["xterm"]),
            process(24, 23, &["/bin/zsh"]),
            process(25, 24, &["claude"]),
            // Under a program that is an agent only by its record, directly
            // and through a terminal it opened.
            process(30, 11, &["/opt/tools/some-agent"]),
            process(31, 30, &["codex"]),
            process(32, 30, &["xterm"]),
            process(33, 32, &["/bin/zsh"]),
            process(34, 33, &["claude"]),
            // Orphaned.
            process(40, 1, &["claude"]),
            // The agent that started its shell exited: the shell is now the
            // first process's.
            process(70, 1, &["/bin/zsh"]),
            process(71, 70, &["claude"]),
            // Its grandparent's pid was reused: the "grandparent" is younger.
            process(80, 11, &["/opt/tools/other"]),
            process(81, 80, &["/bin/zsh"]),
            process(82, 81, &["claude"]),
            // Its grandparent has ended (90), cannot be read (100), or is
            // another user's (110); another user's above a terminal (140).
            process(91, 90, &["/bin/zsh"]),
            process(92, 91, &["claude"]),
            process(101, 100, &["/bin/zsh"]),
            process(102, 101, &["claude"]),
            process(111, 110, &["/bin/zsh"]),
            process(112, 111, &["claude"]),
            process(141, 140, &["xterm"]),
            process(142, 141, &["/bin/zsh"]),
            process(143, 142, &["claude"]),
            // A terminal whose own parent has ended: what was above it
            // cannot be seen.
            process(131, 130, &["xterm"]),
            process(132, 131, &["/bin/zsh"]),
            process(133, 132, &["claude"]),
            // A multiplexer server: the first process's, no root above.
            process(120, 1, &["tmux"]),
            process(121, 120, &["/bin/zsh"]),
            process(122, 121, &["claude"]),
            // Its parent's pid was reused.
            process(60, 11, &["/opt/tools/other"]),
            process(61, 60, &["claude"]),
            // The daemon, a process it started, and a terminal it opened.
            process(50, 11, &["agentd"]),
            process(51, 50, &["claude"]),
            process(52, 50, &["xterm"]),
            process(53, 52, &["/bin/zsh"]),
            process(54, 53, &["claude"]),
        ];
        let born = |pid: u32| match pid {
            61 => Some(at(5)),
            80 => Some(at(500)),
            _ => Some(at(i64::from(pid))),
        };
        let unseen = |pid: u32| match pid {
            100 => Unseen::Unreadable,
            110 | 140 => Unseen::OtherUser,
            _ => Unseen::Ended,
        };
        let root = |process: &procinfo::Process| {
            Some(process.argv.first().is_some_and(|program| {
                program.contains(".app/") || program == "login" || program == "xterm"
            }))
        };
        let started = |pid: u32, agents: &HashMap<u32, DateTime<Utc>>| {
            let birth = if pid == 61 { at(5) } else { at(i64::from(pid)) };
            starter(pid, birth, 50, &table, agents, born, unseen, root)
        };
        let none = HashMap::new();
        let agent = HashMap::from([(30, at(30))]);
        use Starter::{Agent, Person, Unknown};
        for (pid, agents, expected, why) in [
            (12, &none, Some(Person), "the person's terminal"),
            (12, &agent, Some(Person), "an agent elsewhere"),
            (22, &none, Some(Agent), "a provider session's tool"),
            (
                25,
                &none,
                Some(Agent),
                "a terminal a provider session opened",
            ),
            (31, &agent, Some(Agent), "a live agent's process"),
            (34, &agent, Some(Agent), "a terminal a live agent opened"),
            (40, &none, Some(Unknown), "an orphan"),
            (
                71,
                &none,
                Some(Unknown),
                "its shell orphaned when its agent exited",
            ),
            (82, &none, Some(Unknown), "a reused grandparent"),
            (92, &none, Some(Unknown), "an ended grandparent"),
            (102, &none, None, "an unreadable grandparent"),
            (
                112,
                &none,
                Some(Unknown),
                "another user's, with no root below",
            ),
            (143, &none, Some(Person), "another user's, above a terminal"),
            (
                133,
                &none,
                Some(Unknown),
                "an ended parent above a terminal",
            ),
            (
                122,
                &none,
                Some(Unknown),
                "no root before the first process",
            ),
            (61, &none, Some(Unknown), "a reused parent"),
            (51, &none, None, "the daemon's own launch"),
            (54, &none, None, "a terminal the daemon opened"),
            (50, &none, None, "the daemon itself"),
        ] {
            assert_eq!(started(pid, agents), expected, "{why}");
        }
        assert_eq!(
            started(31, &HashMap::from([(30, at(999))])),
            Some(Person),
            "a reused pid is not the agent"
        );
        assert_eq!(
            starter(12, at(99), 50, &table, &none, born, unseen, root),
            None,
            "another process now"
        );
    }

    /// A root check that cannot say (a signature check that timed out)
    /// decides nothing when no other root is found, so the session is
    /// traced again at its next registration and found then; an agent's
    /// session among its parents, or a root found elsewhere, still decides.
    #[test]
    fn a_root_check_that_cannot_say_leaves_the_starter_undecided() {
        let table = [
            process(
                300,
                1,
                &["/Applications/Ghostty.app/Contents/MacOS/ghostty"],
            ),
            process(301, 300, &["/bin/zsh"]),
            process(302, 301, &["claude"]),
            // A root that can be told, beneath one that cannot.
            process(305, 300, &["login"]),
            process(306, 305, &["/bin/zsh"]),
            process(307, 306, &["claude"]),
            // An agent above the one that cannot be told.
            process(
                310,
                1,
                &[
                    "node",
                    "/usr/local/lib/node_modules/@anthropic-ai/claude-code/cli.js",
                ],
            ),
            process(
                311,
                310,
                &["/Applications/Ghostty.app/Contents/MacOS/ghostty"],
            ),
            process(312, 311, &["/bin/zsh"]),
            process(313, 312, &["claude"]),
        ];
        let born = |pid: u32| Some(at(i64::from(pid)));
        let unseen = |_| Unseen::Ended;
        let none = HashMap::new();
        // The signature check times out the first time, verifies the next.
        let checks = std::cell::Cell::new(0);
        let root = |process: &procinfo::Process| {
            let program = process.argv.first().map(String::as_str);
            if program == Some("login") {
                return Some(true);
            }
            if !program.is_some_and(|program| program.contains(".app/")) {
                return Some(false);
            }
            checks.set(checks.get() + 1);
            (checks.get() > 1).then_some(true)
        };
        let started = |pid: u32| {
            starter(
                pid,
                at(i64::from(pid)),
                50,
                &table,
                &none,
                born,
                unseen,
                root,
            )
        };
        assert_eq!(started(302), None, "the check timed out: nothing decided");
        assert_eq!(started(302), Some(Starter::Person), "the next trace says");
        checks.set(0);
        assert_eq!(
            started(307),
            Some(Starter::Person),
            "a root that can be told decides"
        );
        checks.set(0);
        assert_eq!(started(313), Some(Starter::Agent), "an agent above decides");
    }

    /// A root is a known program where no process of the person's could
    /// have put or changed it: never any bundle, a known terminal's bundle
    /// or a copy of `login` in a folder of theirs, or a bundle the person
    /// owns, down to any folder of it, whose signature does not verify for
    /// the program running from its own `Contents/MacOS`.
    #[cfg(unix)]
    #[test]
    fn a_root_is_a_known_program_no_process_of_the_persons_could_change() {
        let tmp = tempfile::tempdir().unwrap();
        let file = |relative: &str| {
            let path = tmp.path().join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "x").unwrap();
            path
        };
        let helper = file("Helper.app/Contents/MacOS/helper");
        let terminal = file("Terminal.app/Contents/MacOS/Terminal");
        let applications = file("Applications/Ghostty.app/Contents/MacOS/ghostty");
        let copied = tmp.path().join("login");
        if std::fs::copy("/usr/bin/login", &copied).is_err() {
            std::fs::write(&copied, "x").unwrap();
        }
        let xterm = file("usr/bin/xterm");
        // Run as root, every file here is root's: only the places refuse.
        let as_root = {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata(tmp.path()).unwrap().uid() == 0
        };
        for path in [&helper, &terminal, &applications, &copied, &xterm] {
            assert_eq!(root_program(path), Some(false), "{}", path.display());
            assert_eq!(root_owned(path), as_root, "the person's own file");
        }
        let path = std::path::Path::new;
        let never = |_: &std::path::Path| Some(false);
        let verifies = |_: &std::path::Path| Some(true);
        let cannot_say = |_: &std::path::Path| None;
        let macos = cfg!(target_os = "macos");
        // On macOS what the signature check says; elsewhere never a root.
        let on_macos = |says: Option<bool>| if macos { says } else { Some(false) };
        let installed = path("/Applications/Ghostty.app/Contents/MacOS/ghostty");
        assert_eq!(
            root_at(installed, None, |_| false, never),
            Some(false),
            "a bundle the person owns, unsigned"
        );
        assert_eq!(
            root_at(
                installed,
                None,
                |p| p != path("/Applications/Ghostty.app"),
                never
            ),
            Some(false),
            "root's file in a bundle folder the person owns, unsigned"
        );
        assert_eq!(
            root_at(installed, None, |_| true, never),
            Some(macos),
            "an administrator's install, on macOS"
        );
        assert_eq!(
            root_at(installed, None, |_| false, verifies),
            Some(macos),
            "the person's own install, signed"
        );
        assert_eq!(
            root_at(installed, None, |_| false, cannot_say),
            on_macos(None),
            "a signature check that could not say"
        );
        assert_eq!(
            root_at(
                path("/Applications/Ghostty.app/Contents/Resources/helper"),
                None,
                |_| false,
                verifies
            ),
            Some(false),
            "a program of the bundle's that is not its own"
        );
        // In the person's own Applications folder only a signed install
        // counts, whoever the file says owns it.
        if let Some(home) = std::env::home_dir() {
            let mine = home.join("Applications/Ghostty.app/Contents/MacOS/ghostty");
            assert_eq!(root_at(&mine, Some(&home), |_| true, never), Some(false));
            assert_eq!(
                root_at(&mine, Some(&home), |_| false, verifies),
                Some(macos)
            );
            assert_eq!(
                root_at(&mine, None, |_| true, verifies),
                Some(false),
                "with no home, not there"
            );
        }
        assert_eq!(
            root_at(
                path("/Applications/Claude.app/Contents/MacOS/Claude"),
                None,
                |_| true,
                verifies
            ),
            Some(false),
            "an application that is no terminal or editor"
        );
        assert_eq!(
            root_at(&copied, None, |_| true, verifies),
            Some(false),
            "a copy of login"
        );
        let login = path("/usr/bin/login");
        assert_eq!(
            root_at(login, None, |_| false, verifies),
            Some(false),
            "login the person could change"
        );
        assert_eq!(root_at(login, None, |_| true, never), Some(true));
        if macos {
            for system in [
                "/System/Applications/Utilities/Terminal.app/Contents/MacOS/Terminal",
                "/usr/bin/login",
            ] {
                assert_eq!(root_program(path(system)), Some(true), "{system}");
            }
        }
    }

    /// The real check, made afresh each time: an unsigned bundle and one
    /// signed by nobody Apple vouches for (ad hoc) do not verify, even in
    /// the person's own Applications folder, and the system's own Terminal
    /// does. A user-owned signed terminal in `/Applications` is checked
    /// only when `AGENTDOCKER_TEST_INSTALLED_APPS=1` asks for it: a local or
    /// ad hoc build there rightly fails.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_bundle_counts_only_when_its_signature_verifies() {
        let tmp = tempfile::tempdir().unwrap();
        let home = std::fs::canonicalize(tmp.path()).unwrap();
        let bundle = home.join("Applications/Ghostty.app");
        let running = bundle.join("Contents/MacOS/ghostty");
        std::fs::create_dir_all(running.parent().unwrap()).unwrap();
        std::fs::write(&running, "#!/bin/sh\nexit 0\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&running, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(
            bundle.join("Contents/Info.plist"),
            r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict><key>CFBundleExecutable</key><string>ghostty</string><key>CFBundleIdentifier</key><string>test.not.ghostty</string></dict></plist>
"#,
        )
        .unwrap();
        let real = || root_at(&running, Some(&home), |_| false, codesign_verifies);
        assert_eq!(codesign_verifies(&bundle), Some(false), "unsigned");
        assert_eq!(real(), Some(false));
        let adhoc = std::process::Command::new("/usr/bin/codesign")
            .args(["-s", "-", "--force"])
            .arg(&bundle)
            .output()
            .unwrap();
        assert!(adhoc.status.success(), "{adhoc:?}");
        assert_eq!(codesign_verifies(&bundle), Some(false), "ad hoc");
        assert_eq!(real(), Some(false));
        assert_eq!(
            codesign_verifies(std::path::Path::new(
                "/System/Applications/Utilities/Terminal.app"
            )),
            Some(true)
        );
        if std::env::var_os("AGENTDOCKER_TEST_INSTALLED_APPS").is_some_and(|v| v == "1") {
            for (bundle, program) in [
                ("/Applications/Ghostty.app", "Contents/MacOS/ghostty"),
                ("/Applications/iTerm.app", "Contents/MacOS/iTerm2"),
            ] {
                let program = std::path::Path::new(bundle).join(program);
                if program.exists() && !root_owned(&program) {
                    assert_eq!(root_program(&program), Some(true), "{}", program.display());
                }
            }
        }
    }

    /// What is said of a session's starter is the daemon's, written once
    /// with its event and kept across a restart: a later trace cannot turn
    /// an agent's session into the person's. A registration from under the
    /// daemon (this test's own children) decides nothing, nor does a record
    /// that is no longer the process traced or has no process.
    #[cfg(unix)]
    #[tokio::test]
    async fn what_is_said_of_a_starter_is_said_once_and_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let open =
            || Arc::new(Daemon::open(tmp.path().into(), tmp.path().join("host.sock")).unwrap());
        let daemon = open();
        let mut children = Vec::new();
        let mut register = async |name: &str| {
            let child = std::process::Command::new("sleep")
                .arg("30")
                .spawn()
                .unwrap();
            let pid = child.id();
            children.push(child);
            let Response::Agent { agent } = daemon
                .handle(Request::Register {
                    spec: AgentSpec {
                        name: name.into(),
                        runtime: "claude-code".into(),
                        workdir: Some(tmp.path().to_owned()),
                        ..AgentSpec::default()
                    },
                    pid: Some(pid),
                    session: None,
                })
                .await
            else {
                panic!("registers")
            };
            agent
        };
        let person = register("terminal-session").await;
        let agents = register("agent-session").await;
        for record in [&person, &agents] {
            assert!(
                record.starter_traceable(),
                "under the daemon's own process nothing is decided: {record:?}"
            );
        }
        let label = |record: &AgentRecord| {
            record
                .spec
                .labels
                .get(agentdocker_core::agent::LAUNCHED_BY_LABEL)
                .cloned()
        };
        let mut events = daemon.subscribe_events();
        {
            let mut state = lock(&daemon.state);
            let now = Utc::now();
            let (pid, birth) = (person.pid.unwrap(), person.process_started_at.unwrap());
            assert!(
                state
                    .write_starter(&person.id, pid, at(1), Starter::Person, now)
                    .is_none(),
                "another process"
            );
            let marked = state
                .write_starter(&person.id, pid, birth, Starter::Person, now)
                .unwrap();
            assert!(marked.launched_by_person());
            for again in [Starter::Person, Starter::Agent, Starter::Unknown] {
                assert!(
                    state
                        .write_starter(&person.id, pid, birth, again, now)
                        .is_none(),
                    "said once"
                );
            }
            let (pid, birth) = (agents.pid.unwrap(), agents.process_started_at.unwrap());
            let marked = state
                .write_starter(&agents.id, pid, birth, Starter::Agent, now)
                .unwrap();
            assert_eq!(label(&marked).as_deref(), Some("agent"));
            assert!(!marked.persons());
            assert!(
                state
                    .write_starter(&agents.id, pid, birth, Starter::Person, now)
                    .is_none(),
                "an agent's session is never promoted"
            );
        }
        let said: Vec<(AgentId, String)> = std::iter::from_fn(|| events.try_recv().ok())
            .filter_map(|event| match event.kind {
                EventKind::AgentLaunchedBy { agent, by, .. } => Some((agent, by)),
                _ => None,
            })
            .collect();
        assert_eq!(
            said,
            [
                (person.id.clone(), HUMAN.to_owned()),
                (agents.id.clone(), "agent".to_owned())
            ]
        );
        // A record with no process of its own (a connector agent, a session
        // known by its key alone) is never traced or marked.
        let Response::Agent { agent: pidless } = daemon
            .handle(Request::Register {
                spec: AgentSpec {
                    name: "no-process".into(),
                    runtime: "claude-browser".into(),
                    labels: [(
                        agentdocker_core::agent::CONNECTOR_LABEL.to_owned(),
                        "true".to_owned(),
                    )]
                    .into(),
                    ..AgentSpec::default()
                },
                pid: None,
                session: None,
            })
            .await
        else {
            panic!("registers")
        };
        assert_eq!(label(&pidless), None);
        let (pid, birth) = (person.pid.unwrap(), person.process_started_at.unwrap());
        assert!(
            lock(&daemon.state)
                .write_starter(&pidless.id, pid, birth, Starter::Person, Utc::now())
                .is_none()
        );
        while let Ok(event) = events.try_recv() {
            assert!(
                !matches!(event.kind, EventKind::AgentLaunchedBy { .. }),
                "nothing said of it: {event:?}"
            );
        }
        drop(events);
        drop(daemon);
        let daemon = open();
        for (record, by) in [(&person, HUMAN), (&agents, "agent")] {
            let Response::Agent { agent: kept } = daemon
                .handle(Request::Inspect {
                    agent: record.id.to_string(),
                })
                .await
            else {
                panic!("kept")
            };
            assert_eq!(label(&kept).as_deref(), Some(by));
        }
        for mut child in children {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// Turning the person check off lets launches through and says nothing
    /// of whose they are: a launch is marked the person's only when its
    /// process is traced to them, never from inside an agent's process or
    /// from one that cannot be identified.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_launch_is_traced_to_its_starter_with_the_person_check_off() {
        let tmp = tempfile::tempdir().unwrap();
        let mut daemon = Daemon::open(tmp.path().into(), tmp.path().join("host.sock")).unwrap();
        daemon.person_check = false;
        let daemon = Arc::new(daemon);
        let launch = || -> Request {
            serde_json::from_str(r#"{"op":"run","spec":{"name":"x","command":["true"]}}"#).unwrap()
        };
        let marked = |request: &Request| {
            matches!(request, Request::Run { spec }
                if spec.labels.contains_key(agentdocker_core::agent::LAUNCHED_BY_LABEL))
        };
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let Response::Agent { .. } = daemon
            .handle(Request::Register {
                spec: AgentSpec {
                    name: "an-agent".into(),
                    workdir: Some(tmp.path().to_owned()),
                    ..AgentSpec::default()
                },
                pid: Some(child.id()),
                session: None,
            })
            .await
        else {
            panic!("registers")
        };
        let mut inside = launch();
        daemon
            .mark_launcher(
                Some((child.id(), procinfo::start_time(child.id()))),
                &mut inside,
            )
            .await;
        let mut unknown = launch();
        daemon.mark_launcher(None, &mut unknown).await;
        let mut own = launch();
        daemon
            .mark_launcher(
                Some((std::process::id(), procinfo::start_time(std::process::id()))),
                &mut own,
            )
            .await;
        let _ = child.kill();
        let _ = child.wait();
        assert!(!marked(&inside), "from an agent's own process");
        assert!(!marked(&unknown), "from a process nobody can identify");
        assert!(marked(&own), "from the person's (here the daemon's own)");
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
        // The daemon's link is the CLI's own file, and the kernel may name a
        // process running it by either name: the file decides, as long as
        // it was invoked as the CLI.
        let link = tmp.path().join("agentd");
        std::fs::hard_link(&cli, &link).unwrap();
        let named = |exe: &std::path::Path, argv0: &std::path::Path| Running {
            executable: Some(exe.to_owned()),
            argv: Some(vec![argv0.display().to_string(), "codex-input".into()]),
        };
        assert!(state.controller_typing(peer, &send, &named(&link, &cli)));
        assert!(
            !state.controller_typing(peer, &send, &named(&cli, &link)),
            "invoked as the daemon"
        );
        // A copy of the same bytes is another file.
        let copy = tmp.path().join("copy").join("agentdocker");
        std::fs::create_dir(copy.parent().unwrap()).unwrap();
        std::fs::copy(&cli, &copy).unwrap();
        assert!(
            !state.controller_typing(peer, &send, &running(&copy, "codex-input")),
            "an impostor of the same name and bytes"
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
        // The daemon's hard link is the CLI's own file, but started by that
        // name it would be the daemon: only the CLI's name is admitted. A
        // symbolic `agentd` resolves to the CLI's file and is launched as
        // that, as the CLI.
        let daemon = tmp.path().join("agentd");
        std::fs::hard_link(&cli, &daemon).unwrap();
        assert!(
            admit_controller_launch_beside(&mut launch(&daemon, &[]), Some(cli.clone())).is_some()
        );
        let symbolic = tmp.path().join("bin").join("agentd");
        std::fs::create_dir(symbolic.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&cli, &symbolic).unwrap();
        let mut request = launch(&symbolic, &[]);
        assert!(admit_controller_launch_beside(&mut request, Some(cli.clone())).is_none());
        let Request::Run { spec } = &request else {
            unreachable!()
        };
        assert_eq!(
            std::path::Path::new(&spec.command[0]),
            std::fs::canonicalize(&cli).unwrap()
        );
        // A program named `agentdocker` that is not the companion's file is
        // an impostor, its bytes the same or not.
        let impostor = tmp.path().join("copy").join("agentdocker");
        std::fs::create_dir(impostor.parent().unwrap()).unwrap();
        std::fs::copy(&cli, &impostor).unwrap();
        assert!(
            admit_controller_launch_beside(&mut launch(&impostor, &[]), Some(cli.clone()))
                .is_some()
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

    /// A handover's successor reaches session owners by its pid and birth,
    /// and not at all once the attempt is given up.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_successor_is_named_with_its_birth_and_dropped_on_abort() {
        let tmp = tempfile::tempdir().unwrap();
        let daemon = Arc::new(Daemon::open(tmp.path().into(), tmp.path().join("s")).unwrap());
        let mut successor = std::process::Command::new("sleep")
            .arg("5")
            .spawn()
            .unwrap();
        let transfer = daemon.offer_transfer(std::process::id()).unwrap();
        assert!(lock(&daemon.state).readdress_offer(&transfer.id, successor.id()));
        let Response::DaemonProcess {
            successor: named,
            successor_started_at,
            ..
        } = daemon.handle(Request::DaemonProcess).await
        else {
            panic!("no answer");
        };
        assert_eq!(named, Some(successor.id()));
        assert_eq!(successor_started_at, procinfo::start_time(successor.id()));
        daemon.abort_transfer("the successor failed");
        let Response::DaemonProcess {
            successor: named, ..
        } = daemon.handle(Request::DaemonProcess).await
        else {
            panic!("no answer");
        };
        assert_eq!(named, None);
        let _ = successor.kill();
        let _ = successor.wait();
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
    /// person typed to that agent; nor is its launch the person's.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_process_under_an_agent_cannot_act_as_the_person() {
        use std::io::Write as _;
        let tmp = tempfile::tempdir().unwrap();
        // Not `s`: that is where session owners live, and this test
        // launches one.
        let daemon =
            Arc::new(Daemon::open(tmp.path().into(), tmp.path().join("host.sock")).unwrap());
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

        // A launch from under an agent is not the person's, whatever its
        // spec says; the same launch from the daemon's own process is.
        let inside = spawn(
            r#"{"op":"run","spec":{"name":"launched-inside","command":["/bin/sh","-c","exit 0"],"labels":{"launched_by":"user"}}}"#
                .into(),
        );
        register("another-agent", inside.id()).await;
        let reply = finish(inside).await;
        let Response::Agent { agent: launched } = &reply else {
            panic!("{reply:?}")
        };
        assert!(!launched.launched_by_person(), "{launched:?}");
        let mut launch: Request = serde_json::from_str(
            r#"{"op":"run","spec":{"name":"launched-here","command":["/bin/sh","-c","exit 0"]}}"#,
        )
        .unwrap();
        daemon
            .mark_launcher(
                Some((std::process::id(), procinfo::start_time(std::process::id()))),
                &mut launch,
            )
            .await;
        let Request::Run { spec } = &launch else {
            unreachable!()
        };
        assert_eq!(
            spec.labels
                .get(agentdocker_core::agent::LAUNCHED_BY_LABEL)
                .map(String::as_str),
            Some(HUMAN)
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
