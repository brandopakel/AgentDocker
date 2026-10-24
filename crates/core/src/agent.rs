//! Agents: the specs that describe them and the records that track them.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ProjectRef;

/// Unique identifier of an agent instance. Like a Docker container ID: a
/// random hex string that can be abbreviated to any unique prefix.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AgentId(String);

impl AgentId {
    pub fn generate() -> Self {
        Self(uuid::Uuid::new_v4().simple().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Abbreviated form for tables, like the 12-character IDs in `docker ps`.
    pub fn short(&self) -> &str {
        let end = self
            .0
            .char_indices()
            .nth(12)
            .map_or(self.0.len(), |(i, _)| i);
        &self.0[..end]
    }
}

impl From<String> for AgentId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for AgentId {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Immutable description of how an agent is created — the "image".
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSpec {
    /// Human-friendly name, unique among live agents.
    pub name: String,
    /// Runtime hosting the agent: `claude-code`, `codex`, `gemini-cli`,
    /// `cursor`, `custom`, ... Free-form so new runtimes need no code change.
    #[serde(default)]
    pub runtime: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Command line that launches the agent. Empty for externally managed
    /// agents that only register themselves.
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workdir: Option<PathBuf>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    /// `run` gives the agent its own linked worktree and branch instead of
    /// the checkout it was pointed at, so its edits are a layer of their own.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub isolate: bool,
    /// `run` gives the agent a terminal rather than pipes, so an
    /// interactive runtime works under supervision and `attach` can reach
    /// it. Output still lands in the log.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub tty: bool,
    /// A restarted daemon relaunches this agent under the same identity,
    /// so the read set, journal cursor, checkpoints and leases it already
    /// has still describe it. Opt-in: starting `agentd` should not spawn
    /// processes nobody asked it to.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub restore: bool,
    /// `run` puts the agent in a `tmux` pane instead of running it
    /// itself, so a person can reach it with `tmux attach`. tmux owns
    /// the process, so the agent is registered rather than supervised:
    /// no captured log, and it ends when its command does.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub in_pane: bool,
    /// When to start it again after it exits. Only a managed agent can
    /// have one: the daemon has to own the process to restart it.
    #[serde(default, skip_serializing_if = "RestartPolicy::is_no")]
    pub restart: RestartPolicy,
    /// Names that must be running before this one starts. Ordering for
    /// `agentdocker up`, where a set of agents is started together and
    /// one of them is a server the others talk to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
}

/// When a managed agent that has exited should be started again.
///
/// The daemon watches its own children, so this is the one place that
/// can act on an exit the moment it happens. The default is `No`:
/// restarting is a decision, and a supervisor that restarts by default
/// turns a command that fails immediately into a loop.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "policy", rename_all = "kebab-case")]
pub enum RestartPolicy {
    /// Never. What exited stays exited.
    #[default]
    No,
    /// Only when it failed, and only so many times. A command that is
    /// broken rather than flaky should stop being retried.
    OnFailure { max: u32 },
    /// Whatever the exit code — a service that is meant to be up.
    Always,
}

/// How many times `on-failure` retries when no number is given. Enough
/// to ride out a flake, few enough that a genuinely broken command stops
/// quickly and says so.
pub const DEFAULT_RESTART_LIMIT: u32 = 3;

impl RestartPolicy {
    /// `no`, `always`, `on-failure`, `on-failure:5`.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        match text {
            "no" | "never" => Some(Self::No),
            "always" => Some(Self::Always),
            "on-failure" => Some(Self::OnFailure {
                max: DEFAULT_RESTART_LIMIT,
            }),
            _ => {
                let max = text.strip_prefix("on-failure:")?;
                Some(Self::OnFailure {
                    max: max.parse().ok()?,
                })
            }
        }
    }

    /// Whether an agent that ended like this, having already been
    /// restarted this many times, should be started again.
    ///
    /// `Failed` counts as a failure — it is how a spawn that never got
    /// off the ground is recorded — and so does an exit with no code,
    /// which is what a signal leaves behind.
    pub fn restarts(&self, status: &AgentStatus, already: u32) -> bool {
        match self {
            Self::No => false,
            Self::Always => true,
            Self::OnFailure { max } => already < *max && failed(status),
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Self::No => "no".to_owned(),
            Self::Always => "always".to_owned(),
            Self::OnFailure { max } => format!("on-failure:{max}"),
        }
    }

    pub fn is_no(&self) -> bool {
        matches!(self, Self::No)
    }
}

/// Whether an ending was a failure. A clean zero is the only success.
fn failed(status: &AgentStatus) -> bool {
    !matches!(status, AgentStatus::Exited { code: Some(0) })
}

/// How long to wait before the nth restart.
///
/// Doubling from a fifth of a second and capped at half a minute. The
/// first restart is nearly immediate, because the common case is a
/// process that died for a reason that has passed; the cap is there
/// because the other case is a command that will fail every time, and
/// the daemon should not spend a core discovering that.
pub fn restart_delay(already: u32) -> std::time::Duration {
    let millis = 200_u64.saturating_mul(1_u64 << already.min(8));
    std::time::Duration::from_millis(millis.min(30_000))
}

/// Which branch and commit an agent's checkout is on, as last observed —
/// by the daemon reading `.git` on a timer, or reported by a hook the
/// moment it sees a tool run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VcsState {
    /// `None` when HEAD is detached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// The commit HEAD points at; `None` on an unborn branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    /// Uncommitted changes, when something cheap can tell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dirty: Option<bool>,
    pub updated_at: DateTime<Utc>,
}

impl VcsState {
    /// Same branch, commit, and dirtiness — the timestamp does not count.
    pub fn same_as(&self, other: &VcsState) -> bool {
        self.branch == other.branch && self.head == other.head && self.dirty == other.dirty
    }

    /// The first seven characters of the commit, as git prints it.
    pub fn short_head(&self) -> Option<&str> {
        self.head.as_deref().map(|head| {
            let end = head.char_indices().nth(7).map_or(head.len(), |(i, _)| i);
            &head[..end]
        })
    }

    /// `main@3f9c1e0`, `(detached)@3f9c1e0`, or `main (unborn)`.
    pub fn describe(&self) -> String {
        match (&self.branch, self.short_head()) {
            (Some(branch), Some(head)) => format!("{branch}@{head}"),
            (None, Some(head)) => format!("(detached)@{head}"),
            (Some(branch), None) => format!("{branch} (unborn)"),
            (None, None) => "-".to_owned(),
        }
    }
}

/// A running agent process nobody has registered, as `discover` reports it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredProcess {
    pub pid: u32,
    pub ppid: u32,
    /// From the known-runtime table: `claude-code`, `codex`, ...
    pub runtime: String,
    /// The command line, as `ps` shows it.
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    /// The project containing `cwd`, without a fingerprint — the id is
    /// assigned when the process is adopted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    /// The multiplexer this process lives in, where it lives in one:
    /// `tmux`, `screen`, `zellij`, herdr. A person can attach to it with
    /// the tool that already owns its terminal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<crate::multiplexer::Session>,
}

impl DiscoveredProcess {
    /// The name `adopt` gives the agent unless told otherwise.
    pub fn default_name(&self) -> String {
        format!("{}-{}", self.runtime, self.pid)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum AgentStatus {
    Created,
    Running,
    /// A stop signal was sent; exit has not yet been observed.
    Stopping,
    Exited {
        code: Option<i32>,
    },
    Failed {
        reason: String,
    },
}

impl AgentStatus {
    /// Created or running: the agent still counts as present.
    pub fn is_live(&self) -> bool {
        matches!(self, Self::Created | Self::Running | Self::Stopping)
    }
}

impl fmt::Display for AgentStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Created => f.write_str("created"),
            Self::Running => f.write_str("running"),
            Self::Stopping => f.write_str("stopping"),
            Self::Exited { code: Some(code) } => write!(f, "exited ({code})"),
            Self::Exited { code: None } => f.write_str("exited (signal)"),
            Self::Failed { reason } => write!(f, "failed: {reason}"),
        }
    }
}

/// Everything the daemon knows about one agent instance — the "container".
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRecord {
    pub id: AgentId,
    pub spec: AgentSpec,
    pub status: AgentStatus,
    /// Host that supervises the agent. Always `local` until federation lands.
    pub host: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// When the process behind `pid` started, so a recycled pid is not
    /// mistaken for the agent. `None` when the platform can't tell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_started_at: Option<DateTime<Utc>>,
    /// Dedicated process group created by agentd for a managed command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_group: Option<u32>,
    /// `true` when agentd spawned the process, `false` when an external
    /// process registered itself.
    pub managed: bool,
    /// The multiplexer this agent lives in, where it lives in one. Set
    /// when it registers or is adopted; `None` for a managed agent,
    /// which lives in the daemon's own terminal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<crate::multiplexer::Session>,
    /// The application its terminal is in (Terminal, iTerm, an editor)
    /// and its terminal device, read from its ancestry when it registers
    /// or is adopted. What the app names it by and brings forward to reach
    /// it. `None` for a managed agent, for one inside a multiplexer whose
    /// server has no such ancestor, and where nothing could be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal: Option<crate::terminal::Host>,
    /// How many times the daemon has started this agent again after an
    /// exit. Kept on the record because `on-failure` counts, and because
    /// a reader deserves to know an agent has died nine times.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub restarts: u32,
    /// Whether a stop was asked of this agent's current run, so that once
    /// it has ended its record says whether that ending followed one: the
    /// exit a stop ends in may be 0, and its status alone would read as
    /// success. `Some(true)` from the transition that publishes
    /// `agent_stopping` ([`AgentRecord::mark_stopping`]), kept once the
    /// agent has exited, through a daemon restart and for as long as the
    /// record is retained. `Some(false)` when a run begins under this id
    /// ([`AgentRecord::begin_run`]: a new record, a restart by its policy,
    /// a restore, a relaunch, a process joining the record) and no stop
    /// has been asked of it since; where a fresh process's record is
    /// folded into this one (a session resumed, input resumed), the fresh
    /// process's own mark comes with its run. `None` on a record no daemon
    /// that keeps
    /// this has written since its run began: unknown, and never taken as
    /// `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_requested: Option<bool>,
    /// Engine identity and intent for a managed container; never a host PID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container: Option<crate::container::ManagedContainer>,
    /// The session owner holding this managed agent's child, when one
    /// does; absent for adopted, registered and older records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<crate::session::SessionOwner>,
    /// The project derived from `spec.workdir` when the agent was created;
    /// `None` when there was no working directory to derive it from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectRef>,
    /// Branch and head of the checkout, when the agent has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vcs: Option<VcsState>,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<Utc>>,
    pub last_seen: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reported_activity: Option<crate::ActivityObservation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_delivery: Option<crate::InputDelivery>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_availability: Option<crate::ProviderAvailability>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub adapter_contacts: std::collections::BTreeMap<crate::AdapterKind, crate::AdapterContact>,
    /// When this process last read its own queue (`inbox`, `ack_inbox`): a
    /// session with no adapter that takes its messages itself is reachable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_read: Option<crate::AdapterContact>,
    /// The last contact of a session known by its key alone (no process
    /// here, nothing supervising it): what a verified provider host sent
    /// for it, and when. Its presence is reckoned from this
    /// ([`SESSION_PRESENCE`]); its lifecycle never is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_contact: Option<SessionContact>,
    /// The external controller that consumes this agent's queued input,
    /// while one is bound. See [`crate::InputBinding`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_binding: Option<crate::InputBinding>,
    /// When each queued message was first offered to a legacy reader (a
    /// hook's delivery queue, an inbox read, a subscription backlog), kept
    /// so a controller that binds later knows which messages may already
    /// have been injected. Cleared as messages leave the queue.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub legacy_offers: std::collections::BTreeMap<crate::MessageId, DateTime<Utc>>,
    /// The opt-in judge read its quiet screen as waiting on the person:
    /// what for, how sure, and which screen of which process. It raises
    /// the agent's activity while it stands and never lowers it; a change
    /// of screen, a report from the agent's own adapter or another
    /// process ends it (see [`crate::judgment`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judged: Option<crate::judgment::ScreenJudgment>,
}

/// The label an agent's roles are kept in (`role=reviewer`, or
/// `role=reviewer,tester` for several, sorted), so a hand-off or a message
/// can name an agent by what it does: `role:reviewer`.
pub const ROLE_LABEL: &str = "role";

/// Roles as the label keeps them: sorted, each once, comma-joined; `None`
/// for none.
pub fn join_roles<'a>(roles: impl IntoIterator<Item = &'a str>) -> Option<String> {
    let mut roles: Vec<&str> = roles
        .into_iter()
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .collect();
    roles.sort_unstable();
    roles.dedup();
    (!roles.is_empty()).then(|| roles.join(","))
}
/// The prefix that names an agent by its role in a reference.
pub const ROLE_PREFIX: &str = "role:";
/// A role's shape: a short lowercase word of letters, digits and hyphens.
pub const ROLE_CHARS: usize = 40;

/// Whether a role can be set: one to forty characters of `a-z`, `0-9`
/// and `-`, neither starting nor ending with a hyphen.
pub fn check_role(role: &str) -> Result<(), &'static str> {
    if role.is_empty() || role.len() > ROLE_CHARS {
        return Err("a role is one to forty characters");
    }
    if !role
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err("a role is lowercase letters, digits and hyphens");
    }
    if role.starts_with('-') || role.ends_with('-') {
        return Err("a role neither starts nor ends with a hyphen");
    }
    Ok(())
}

/// The longest name a person can give an agent, in characters.
pub const NAME_CHARS: usize = 64;

/// Whether `name` can be given to an agent by `rename`: one to
/// [`NAME_CHARS`] characters, no control characters or surrounding space,
/// and none of the spellings that address something other than an agent
/// (`project`, `all`, `project:…`, `topic:…`, `role:…`).
pub fn check_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() || name.chars().count() > NAME_CHARS {
        return Err("a name is one to sixty-four characters");
    }
    if name.trim() != name {
        return Err("a name neither starts nor ends with a space");
    }
    if name.chars().any(char::is_control) {
        return Err("a name has no control characters");
    }
    if matches!(name, "project" | "all" | "user")
        || ["project:", "topic:", ROLE_PREFIX]
            .iter()
            .any(|prefix| name.starts_with(prefix))
    {
        return Err("that name is how something else is addressed; pick another");
    }
    Ok(())
}

/// Label an adapter sets when it made the name up from a runtime and an
/// identifier rather than a person choosing it.
pub const NAME_LABEL: &str = "name";
/// The value of [`NAME_LABEL`] for a made-up name.
pub const GENERATED_NAME: &str = "generated";
/// The value of [`NAME_LABEL`] for a name a person chose with `rename`. It
/// overrides every inference from the name's shape: `codex-4242` chosen for
/// the process 4242 is a chosen name, not the adapter's.
pub const CHOSEN_NAME: &str = "chosen";

/// The label that says who started an agent. `user` (the person): a `run`,
/// `run_container` or `resume_session` sent by a process the daemon found
/// outside every agent's session (the app, the person's own terminal); an
/// agent started on a card by the person or by one of their board's rules;
/// or a session that registered itself (hooks, MCP, `adopt`) whose
/// process's parents reach the top through one of the person's session
/// roots (a terminal or editor application root installed, `login`,
/// `sshd`) with no agent's session anywhere among them.
/// [`LAUNCHED_BY_AGENT`] or [`LAUNCHED_BY_UNKNOWN`] for such a session
/// whose parents say otherwise. Only the daemon sets it, and once set it
/// stays: a client's spec never brings it in, and a record without it (a
/// launch from inside an agent's session, a connector agent, a session
/// known by its key alone, a record from before) is nobody's in particular.
///
/// It is attribution, for showing and filtering (`mine`), never
/// authorization: a terminal window an agent opens through the system's
/// app launcher (`open`, `osascript`) has the same parents as the person's
/// own and is not told apart from it, so nothing may grant rights from it.
pub const LAUNCHED_BY_LABEL: &str = "launched_by";
/// [`LAUNCHED_BY_LABEL`] for a registered session an agent's session stands
/// above.
pub const LAUNCHED_BY_AGENT: &str = "agent";
/// [`LAUNCHED_BY_LABEL`] for a registered session whose parents could no
/// longer say who started it (orphaned, a parent ended or its pid reused).
pub const LAUNCHED_BY_UNKNOWN: &str = "unknown";

/// Label the remote connector puts on the agent it registers
/// (`connector=true`): an external agent with no process on this machine.
/// Its runtime (`claude-browser`, `chatgpt-browser`) names the vendor
/// account it came through, not a browser: every client of that account
/// presents the same grant.
pub const CONNECTOR_LABEL: &str = "connector";

/// Label the remote connector puts on the agent it registers, saying what
/// is known of the surface that will use it: [`UNVERIFIED_SURFACE`], since
/// the browser side panel, the web and desktop apps and a terminal reaching
/// the account's connectors are indistinguishable at consent.
pub const SURFACE_LABEL: &str = "surface";
/// The value of [`SURFACE_LABEL`] when nothing proves which surface it is.
pub const UNVERIFIED_SURFACE: &str = "unverified";

/// How long a browser agent reached through the remote connector counts as
/// connected after its last tool call: one access token's lifetime. Such an
/// agent has no process here whose end could retire it, and its record stays
/// registered until its grant is revoked, so its last tool call is the only
/// evidence that anyone is still on the other side (the connector does not
/// count a client initializing or listing tools). Past this, the vendor
/// has to come back with a refresh before it can say anything at all.
pub const CONNECTOR_PRESENCE: chrono::Duration = chrono::Duration::hours(1);

/// The provider profile a session belongs to, as its verified host names it
/// (for Codex, its canonical `CODEX_HOME`). With the runtime and the
/// provider's session id it is the session's key: the same thread id under
/// another profile is another session.
pub const PROFILE_LABEL: &str = "provider_profile";

/// How long a session known by its key alone counts as present after its
/// last contact. An open conversation can sit idle for hours, so passing
/// this only stops counting it as somebody working now: its record, queue,
/// leases and channels stay until it ends in fact or is retired.
pub const SESSION_PRESENCE: chrono::Duration = chrono::Duration::hours(8);

/// A provider session's key: its runtime, the profile it belongs to, and
/// the provider's own id for it (Claude's session, Codex's root thread).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionKey {
    pub runtime: String,
    pub profile: Option<String>,
    pub session: String,
}

/// The last contact of a session known by its key alone: which adapter
/// made it, and when.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionContact {
    pub adapter: crate::AdapterKind,
    pub observed_at: DateTime<Utc>,
}

impl AgentRecord {
    /// A browser agent the remote connector registered: labelled
    /// `connector=true`, external and without a process here.
    pub fn via_connector(&self) -> bool {
        !self.managed
            && self.pid.is_none()
            && self
                .spec
                .labels
                .get(CONNECTOR_LABEL)
                .is_some_and(|value| value == "true")
    }

    /// A live connector agent nothing has been heard from for longer than
    /// [`CONNECTOR_PRESENCE`]: still registered (its grant stands, messages
    /// wait for it, and its next request makes it connected again), but not
    /// a session anybody is working in now. `now` is the caller's clock.
    pub fn connector_absent(&self, now: DateTime<Utc>) -> bool {
        self.status.is_live()
            && self.via_connector()
            && now.signed_duration_since(self.last_seen) > CONNECTOR_PRESENCE
    }

    /// Whether the agent counts as present now: live, and, for a connector
    /// agent, heard from within [`CONNECTOR_PRESENCE`], and for a session
    /// known by its key alone within [`SESSION_PRESENCE`]. Counts of live
    /// agents count these; [`AgentStatus::is_live`] still says whether the
    /// record has ended.
    pub fn is_present(&self, now: DateTime<Utc>) -> bool {
        self.status.is_live() && !self.connector_absent(now) && !self.session_absent(now)
    }

    /// This session's key, when it names its provider session.
    pub fn session_key(&self) -> Option<SessionKey> {
        let session = self
            .spec
            .labels
            .get("session_id")
            .filter(|id| !id.is_empty())?;
        Some(SessionKey {
            runtime: self.spec.runtime.clone(),
            profile: self.spec.labels.get(PROFILE_LABEL).cloned(),
            session: session.clone(),
        })
    }

    /// A session known by its key alone: no process here and nothing
    /// supervising it, kept by its contact. Not a connector agent.
    pub fn keyed_only(&self) -> bool {
        !self.managed
            && self.pid.is_none()
            && !self.via_connector()
            && self.session_contact.is_some()
            && self.session_key().is_some()
    }

    /// A live session known by its key alone that nothing has been heard
    /// from for longer than [`SESSION_PRESENCE`]: still registered, its
    /// messages waiting, but nobody is working in it now.
    pub fn session_absent(&self, now: DateTime<Utc>) -> bool {
        self.status.is_live()
            && self.keyed_only()
            && self.session_contact.as_ref().is_some_and(|contact| {
                now.signed_duration_since(contact.observed_at) > SESSION_PRESENCE
            })
    }

    /// The role this agent holds, when it has one.
    /// The roles this agent holds, as its label lists them.
    pub fn roles(&self) -> Vec<&str> {
        self.spec
            .labels
            .get(ROLE_LABEL)
            .map(|label| {
                label
                    .split(',')
                    .map(str::trim)
                    .filter(|r| !r.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn holds_role(&self, role: &str) -> bool {
        self.roles().contains(&role)
    }

    /// Whether the person launched it through AgentDocker, as its
    /// [`LAUNCHED_BY_LABEL`] says.
    pub fn launched_by_person(&self) -> bool {
        self.spec
            .labels
            .get(LAUNCHED_BY_LABEL)
            .is_some_and(|by| by == crate::HUMAN)
    }

    /// The person's: their own record, or an agent they launched. A card
    /// held by one of these is in the person's `mine`.
    pub fn persons(&self) -> bool {
        self.spec.runtime == crate::HUMAN_RUNTIME || self.launched_by_person()
    }

    /// A session that registered itself whose starter the daemon may still
    /// find from its process's parents: live, not the person, not launched
    /// by the daemon (managed, in a pane it opened, in a container), not a
    /// connector agent, with a process whose birth was recorded, and nothing
    /// said yet of who launched it — what is said once stays.
    pub fn starter_traceable(&self) -> bool {
        self.status.is_live()
            && self.spec.runtime != crate::HUMAN_RUNTIME
            && !self.managed
            && !self.spec.in_pane
            && self.container.is_none()
            && !self.via_connector()
            && self.pid.is_some()
            && self.process_started_at.is_some()
            && !self.spec.labels.contains_key(LAUNCHED_BY_LABEL)
    }

    /// Whether this record's name was generated by an adapter rather than
    /// chosen. Newer registrations say so with a label; older ones are
    /// recognised only by the exact form an adapter produces from this
    /// record's own pid or session id, never by how the name looks.
    pub fn name_is_generated(&self) -> bool {
        match self.spec.labels.get(NAME_LABEL).map(String::as_str) {
            Some(GENERATED_NAME) => return true,
            Some(CHOSEN_NAME) => return false,
            _ => {}
        }
        let name = self.spec.name.as_str();
        if let Some(pid) = self.pid
            && name == format!("{}-{pid}", self.spec.runtime)
        {
            return true;
        }
        if self.spec.runtime == "claude-code"
            && let Some(session) = self.spec.labels.get("session_id")
        {
            let end = session
                .char_indices()
                .nth(8)
                .map_or(session.len(), |(i, _)| i);
            if name == format!("claude-{}", &session[..end]) {
                return true;
            }
        }
        name == format!(
            "agent-{}",
            self.id.as_str().chars().take(6).collect::<String>()
        )
    }

    /// A stop is under way for its current run, as `agent_stopping` says:
    /// the record is `stopping`, and goes on saying a stop was asked once
    /// the agent has exited, whatever it exited with.
    pub fn mark_stopping(&mut self) {
        self.status = AgentStatus::Stopping;
        self.stop_requested = Some(true);
    }

    /// A run begins under this id. Whatever stop was asked of the run
    /// before belongs to that run, not to this one.
    pub fn begin_run(&mut self) {
        self.stop_requested = Some(false);
    }

    /// Whether a stop is known to have been asked of its current run —
    /// once it has ended, whether that ending followed a stop request.
    pub fn stopped_on_request(&self) -> bool {
        self.stop_requested == Some(true)
    }

    pub fn new(spec: AgentSpec, managed: bool, now: DateTime<Utc>) -> Self {
        Self {
            id: AgentId::generate(),
            spec,
            status: AgentStatus::Created,
            session: None,
            terminal: None,
            restarts: 0,
            stop_requested: Some(false),
            host: "local".to_owned(),
            pid: None,
            process_started_at: None,
            process_group: None,
            managed,
            container: None,
            owner: None,
            project: None,
            vcs: None,
            created_at: now,
            started_at: None,
            finished_at: None,
            last_seen: now,
            reported_activity: None,
            input_delivery: None,
            provider_availability: None,
            adapter_contacts: Default::default(),
            queue_read: None,
            session_contact: None,
            input_binding: None,
            legacy_offers: Default::default(),
            judged: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stop is marked on the run it was asked of, outlives the exit,
    /// and is not the next run's; a record written without the mark reads
    /// as unknown, never as "no stop".
    #[test]
    fn a_stop_is_marked_on_its_run_and_an_older_record_is_unknown() {
        let mut record = AgentRecord::new(AgentSpec::default(), true, chrono::Utc::now());
        assert_eq!(record.stop_requested, Some(false), "a new run");
        record.status = AgentStatus::Running;
        record.mark_stopping();
        assert_eq!(record.status, AgentStatus::Stopping);
        record.status = AgentStatus::Exited { code: Some(0) };
        assert!(record.stopped_on_request(), "kept once it has exited");

        // Through storage, as a JSON blob.
        let stored = serde_json::to_value(&record).unwrap();
        assert_eq!(stored["stop_requested"], true);
        let back: AgentRecord = serde_json::from_value(stored.clone()).unwrap();
        assert_eq!(back, record);

        // A run that begins under the same id carries no stop of the last.
        record.begin_run();
        assert_eq!(record.stop_requested, Some(false));
        assert!(!record.stopped_on_request());
        assert_eq!(
            serde_json::to_value(&record).unwrap()["stop_requested"],
            false
        );

        // A record from a daemon that did not keep the mark.
        let mut older = stored;
        older.as_object_mut().unwrap().remove("stop_requested");
        let older: AgentRecord = serde_json::from_value(older).unwrap();
        assert_eq!(older.stop_requested, None);
        assert!(!older.stopped_on_request());
        assert!(
            serde_json::to_value(&older)
                .unwrap()
                .get("stop_requested")
                .is_none(),
            "and unknown stays unknown when it is written again"
        );
    }

    #[test]
    fn an_agents_roles_are_kept_sorted_once_each_in_its_label() {
        assert_eq!(
            join_roles(["tester", "reviewer", " tester "]).as_deref(),
            Some("reviewer,tester")
        );
        assert_eq!(join_roles([""; 0]), None);
        let mut record = AgentRecord::new(AgentSpec::default(), false, chrono::Utc::now());
        assert!(record.roles().is_empty());
        record
            .spec
            .labels
            .insert(ROLE_LABEL.into(), "reviewer, tester".into());
        assert_eq!(record.roles(), ["reviewer", "tester"]);
        assert!(record.holds_role("tester") && !record.holds_role("test"));
    }

    /// The person's are their own record and the agents the label says they
    /// launched; nothing else is guessed at.
    #[test]
    fn the_persons_are_their_record_and_the_agents_they_launched() {
        let record = |runtime: &str, launcher: Option<&str>| {
            let mut record = AgentRecord::new(
                AgentSpec {
                    runtime: runtime.into(),
                    ..AgentSpec::default()
                },
                true,
                chrono::Utc::now(),
            );
            if let Some(by) = launcher {
                record
                    .spec
                    .labels
                    .insert(LAUNCHED_BY_LABEL.into(), by.into());
            }
            record
        };
        assert!(record(crate::HUMAN_RUNTIME, None).persons(), "the person");
        assert!(record("codex", Some(crate::HUMAN)).launched_by_person());
        assert!(record("codex", Some(crate::HUMAN)).persons(), "launched");
        assert!(!record("codex", None).persons(), "nobody said");
        assert!(
            !record("codex", Some("a1b2c3")).persons(),
            "launched by somebody else"
        );
    }

    /// Only a live, self-registered session with a process of known birth,
    /// whose starter nothing has said, is traced: never a pidless or
    /// connector record, a daemon launch, the person, or one already said.
    #[test]
    fn a_starter_is_traced_only_for_a_registered_session_with_a_known_process() {
        let session = || {
            let mut record = AgentRecord::new(
                AgentSpec {
                    runtime: "claude-code".into(),
                    ..AgentSpec::default()
                },
                false,
                chrono::Utc::now(),
            );
            record.status = AgentStatus::Running;
            record.pid = Some(4242);
            record.process_started_at = Some(chrono::Utc::now());
            record
        };
        assert!(session().starter_traceable());
        let mut pidless = session();
        pidless.pid = None;
        let mut unborn = session();
        unborn.process_started_at = None;
        let mut connector = session();
        connector.pid = None;
        connector
            .spec
            .labels
            .insert(CONNECTOR_LABEL.into(), "true".into());
        let mut managed = session();
        managed.managed = true;
        let mut paned = session();
        paned.spec.in_pane = true;
        let mut person = session();
        person.spec.runtime = crate::HUMAN_RUNTIME.into();
        let mut said = session();
        said.spec
            .labels
            .insert(LAUNCHED_BY_LABEL.into(), crate::HUMAN.into());
        let mut by_agent = session();
        by_agent
            .spec
            .labels
            .insert(LAUNCHED_BY_LABEL.into(), LAUNCHED_BY_AGENT.into());
        let mut unknown = session();
        unknown
            .spec
            .labels
            .insert(LAUNCHED_BY_LABEL.into(), LAUNCHED_BY_UNKNOWN.into());
        let mut ended = session();
        ended.status = AgentStatus::Exited { code: Some(0) };
        for (record, why) in [
            (pidless, "no process"),
            (unborn, "no recorded birth"),
            (connector, "a connector agent"),
            (managed, "the daemon's launch"),
            (paned, "a pane the daemon opened"),
            (person, "the person"),
            (said, "already said"),
            (by_agent, "said to be an agent's"),
            (unknown, "said to be unknowable"),
            (ended, "ended"),
        ] {
            assert!(!record.starter_traceable(), "{why}");
        }
    }

    #[test]
    fn legacy_claude_names_do_not_replace_other_runtimes_chosen_names() {
        let now = chrono::DateTime::from_timestamp(1000, 0).unwrap();
        let mut record = AgentRecord::new(
            AgentSpec {
                name: "claude-12345678".into(),
                runtime: "custom-runtime".into(),
                ..Default::default()
            },
            false,
            now,
        );
        record
            .spec
            .labels
            .insert("session_id".into(), "12345678-rest".into());
        assert!(!record.name_is_generated());
        record.spec.runtime = "claude-code".into();
        assert!(record.name_is_generated());
        record.id = AgentId::from("ééééééé");
        record.spec.name = "chosen name".into();
        assert!(
            !record.name_is_generated(),
            "non-ASCII retained ids must not panic"
        );
    }

    #[test]
    fn short_id_is_twelve_chars() {
        let id = AgentId::generate();
        assert_eq!(id.short().len(), 12);
        assert!(id.as_str().starts_with(id.short()));
        assert_eq!(AgentId::from("abc").short(), "abc");
    }

    /// A session known by its key alone: its key is runtime, profile and
    /// the provider's id; it is present while heard from within
    /// SESSION_PRESENCE and, past it, absent but never ended.
    #[test]
    fn a_session_known_by_its_key_is_present_by_contact_and_never_ended_by_silence() {
        let at = chrono::DateTime::from_timestamp(1_000_000, 0).unwrap();
        let mut session = AgentRecord::new(
            AgentSpec {
                name: "codex-thread".into(),
                runtime: "codex".into(),
                labels: [
                    ("session_id".to_owned(), "thread-1".to_owned()),
                    (PROFILE_LABEL.to_owned(), "/home/p/.codex".to_owned()),
                ]
                .into(),
                ..Default::default()
            },
            false,
            at,
        );
        session.status = AgentStatus::Running;
        assert_eq!(
            session.session_key(),
            Some(SessionKey {
                runtime: "codex".into(),
                profile: Some("/home/p/.codex".into()),
                session: "thread-1".into(),
            })
        );
        assert!(
            !session.keyed_only(),
            "no contact yet: not a keyed-only session"
        );
        session.session_contact = Some(SessionContact {
            adapter: crate::AdapterKind::Mcp,
            observed_at: at,
        });
        assert!(session.keyed_only());
        assert!(session.is_present(at + SESSION_PRESENCE));
        let later = at + SESSION_PRESENCE + chrono::Duration::seconds(1);
        assert!(session.session_absent(later) && !session.is_present(later));
        assert!(session.status.is_live(), "silence is not an end");
        // A process here, or a supervisor, is reckoned by that instead.
        let mut with_process = session.clone();
        with_process.pid = Some(42);
        assert!(!with_process.keyed_only() && with_process.is_present(later));
        // Stored without the field until there is a contact, and read back.
        let mut fresh = session.clone();
        fresh.session_contact = None;
        let stored = serde_json::to_value(&fresh).unwrap();
        assert!(stored.get("session_contact").is_none());
        assert_eq!(
            serde_json::from_value::<AgentRecord>(stored).unwrap(),
            fresh
        );
    }
    /// A browser agent the connector registered has no process to end it, so
    /// before this it counted as a live session for as long as its grant
    /// stood: weeks after anybody last used it. Its last tool call is the
    /// evidence; a call after the window makes it present again.
    #[test]
    fn a_connector_agent_is_present_only_while_it_is_heard_from() {
        let registered = chrono::DateTime::from_timestamp(1_000_000, 0).unwrap();
        let mut browser = AgentRecord::new(
            AgentSpec {
                name: "chatgpt-browser-ab12".into(),
                runtime: "chatgpt-browser".into(),
                labels: BTreeMap::from([
                    (CONNECTOR_LABEL.into(), "true".into()),
                    ("vendor".into(), "ChatGPT".into()),
                ]),
                ..Default::default()
            },
            false,
            registered,
        );
        browser.status = AgentStatus::Running;
        assert!(browser.via_connector());
        assert!(browser.is_present(registered));
        assert!(browser.is_present(registered + CONNECTOR_PRESENCE));
        let later = registered + CONNECTOR_PRESENCE + chrono::Duration::seconds(1);
        assert!(browser.connector_absent(later));
        assert!(!browser.is_present(later));
        assert!(
            browser.status.is_live(),
            "absent is not ended: the grant stands"
        );
        // Heard from again: present again.
        browser.last_seen = later;
        assert!(browser.is_present(later));
        // Only connector agents go quiet this way. A terminal session's
        // liveness is its process, however long it has been silent, and an
        // ended record is simply not present.
        let mut terminal = browser.clone();
        terminal.spec.labels.remove(CONNECTOR_LABEL);
        terminal.last_seen = registered;
        assert!(!terminal.connector_absent(later) && terminal.is_present(later));
        let mut with_process = browser.clone();
        with_process.last_seen = registered;
        with_process.pid = Some(42);
        assert!(with_process.is_present(later));
        let mut ended = browser.clone();
        ended.status = AgentStatus::Exited { code: None };
        assert!(!ended.is_present(registered) && !ended.connector_absent(later));
    }

    #[test]
    fn status_liveness() {
        assert!(AgentStatus::Created.is_live());
        assert!(AgentStatus::Running.is_live());
        assert!(!AgentStatus::Exited { code: Some(0) }.is_live());
        assert!(!AgentStatus::Failed { reason: "x".into() }.is_live());
    }

    #[test]
    fn status_serialises_with_state_tag() {
        let json = serde_json::to_string(&AgentStatus::Exited { code: Some(1) }).unwrap();
        assert_eq!(json, r#"{"state":"exited","code":1}"#);
    }

    #[test]
    fn legacy_agent_records_omit_container_identity() {
        let record = AgentRecord::new(AgentSpec::default(), true, Utc::now());
        let json = serde_json::to_value(&record).unwrap();
        assert!(json.get("container").is_none());
        let restored: AgentRecord = serde_json::from_value(json).unwrap();
        assert_eq!(restored, record);
        assert!(restored.container.is_none());
    }
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

#[cfg(test)]
mod restart_tests {
    use super::*;

    fn exited(code: i32) -> AgentStatus {
        AgentStatus::Exited { code: Some(code) }
    }

    #[test]
    fn policies_are_parsed_the_way_they_are_written() {
        assert_eq!(RestartPolicy::parse("no"), Some(RestartPolicy::No));
        assert_eq!(RestartPolicy::parse("never"), Some(RestartPolicy::No));
        assert_eq!(RestartPolicy::parse("always"), Some(RestartPolicy::Always));
        assert_eq!(
            RestartPolicy::parse("on-failure"),
            Some(RestartPolicy::OnFailure {
                max: DEFAULT_RESTART_LIMIT
            })
        );
        assert_eq!(
            RestartPolicy::parse(" on-failure:5 "),
            Some(RestartPolicy::OnFailure { max: 5 })
        );
        assert_eq!(RestartPolicy::parse("on-failure:x"), None);
        assert_eq!(RestartPolicy::parse("sometimes"), None);
        // And a policy round-trips through the text it prints.
        for text in ["no", "always", "on-failure:3", "on-failure:0"] {
            let parsed = RestartPolicy::parse(text).unwrap();
            assert_eq!(parsed.describe(), text);
        }
    }

    #[test]
    fn no_never_restarts_and_always_always_does() {
        let never = RestartPolicy::No;
        assert!(!never.restarts(&exited(0), 0));
        assert!(!never.restarts(&exited(1), 0));

        let always = RestartPolicy::Always;
        assert!(always.restarts(&exited(0), 0), "a clean exit too");
        assert!(always.restarts(&exited(1), 99), "and however many times");
    }

    #[test]
    fn on_failure_counts_and_only_counts_failures() {
        let policy = RestartPolicy::OnFailure { max: 2 };
        // A clean exit is the end of it, whatever the count.
        assert!(!policy.restarts(&exited(0), 0));
        // A failure restarts until the limit, then stops.
        assert!(policy.restarts(&exited(1), 0));
        assert!(policy.restarts(&exited(1), 1));
        assert!(!policy.restarts(&exited(1), 2), "the limit is a limit");
        // A signal leaves no code, and a spawn that never started is a
        // failure too — both are things worth retrying.
        assert!(policy.restarts(&AgentStatus::Exited { code: None }, 0));
        assert!(policy.restarts(
            &AgentStatus::Failed {
                reason: "no such file".into()
            },
            0
        ));
        // Zero retries means the policy is on but spent immediately.
        assert!(!RestartPolicy::OnFailure { max: 0 }.restarts(&exited(1), 0));
    }

    #[test]
    fn the_delay_backs_off_and_then_stops_growing() {
        assert_eq!(restart_delay(0), std::time::Duration::from_millis(200));
        assert_eq!(restart_delay(1), std::time::Duration::from_millis(400));
        assert_eq!(restart_delay(3), std::time::Duration::from_millis(1600));
        // Capped, so a command that always fails costs a restart every
        // half minute rather than a core.
        assert_eq!(restart_delay(20), std::time::Duration::from_secs(30));
        // And it never goes backwards.
        let mut last = std::time::Duration::ZERO;
        for n in 0..40 {
            let delay = restart_delay(n);
            assert!(delay >= last, "delay went backwards at {n}");
            last = delay;
        }
    }

    #[test]
    fn a_policy_that_is_off_is_left_out_of_the_wire() {
        let spec = AgentSpec::default();
        let json = serde_json::to_string(&spec).unwrap();
        assert!(!json.contains("restart"), "{json}");
        let with = AgentSpec {
            restart: RestartPolicy::Always,
            ..AgentSpec::default()
        };
        let json = serde_json::to_string(&with).unwrap();
        assert!(json.contains(r#""policy":"always""#), "{json}");
        assert_eq!(
            serde_json::from_str::<AgentSpec>(&json).unwrap().restart,
            RestartPolicy::Always
        );
    }

    #[test]
    fn a_chosen_name_is_short_printable_and_never_another_address() {
        for good in ["Release helper", "reviewer-2", "Codex · Otter", "é"] {
            assert!(check_name(good).is_ok(), "{good}");
        }
        for bad in [
            "",
            " lead",
            "trail ",
            "a\nb",
            "project",
            "all",
            "user",
            "project:x",
            "topic:y",
            "role:reviewer",
        ] {
            assert!(check_name(bad).is_err(), "{bad:?}");
        }
        assert!(check_name(&"x".repeat(NAME_CHARS)).is_ok());
        assert!(check_name(&"x".repeat(NAME_CHARS + 1)).is_err());
    }
}
