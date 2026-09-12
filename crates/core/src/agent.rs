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
    /// How many times the daemon has started this agent again after an
    /// exit. Kept on the record because `on-failure` counts, and because
    /// a reader deserves to know an agent has died nine times.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub restarts: u32,
    /// Engine identity and intent for a managed container; never a host PID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container: Option<crate::container::ManagedContainer>,
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
}

impl AgentRecord {
    pub fn new(spec: AgentSpec, managed: bool, now: DateTime<Utc>) -> Self {
        Self {
            id: AgentId::generate(),
            spec,
            status: AgentStatus::Created,
            session: None,
            restarts: 0,
            host: "local".to_owned(),
            pid: None,
            process_started_at: None,
            process_group: None,
            managed,
            container: None,
            project: None,
            vcs: None,
            created_at: now,
            started_at: None,
            finished_at: None,
            last_seen: now,
            reported_activity: None,
            input_delivery: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_id_is_twelve_chars() {
        let id = AgentId::generate();
        assert_eq!(id.short().len(), 12);
        assert!(id.as_str().starts_with(id.short()));
        assert_eq!(AgentId::from("abc").short(), "abc");
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
}
