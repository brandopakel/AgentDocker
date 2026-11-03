//! Claude Code agent teams on the records: which live Claude Code session
//! leads a team and which are its teammates, read from each process's
//! command line and Claude's own team files
//! ([`agentdocker_core::team`], [`agentdocker_host::teams`]).
//!
//! The reading happens off the state lock, on the five-second tick that
//! also sweeps checkouts and again right after a Claude Code session
//! registers, so a teammate is named within a second of its first hook.
//! A process's command line and environment are read once per process
//! (by pid and birth) and kept; the team files are small and read each
//! time, from the configuration directory each session uses. That is
//! taken from the strongest evidence there is: the session process's own
//! environment where the platform lets the daemon read another process's
//! (Linux), else the directory the session's hooks and MCP server
//! resolved in theirs and put on its record (`claude_config_dir`), else
//! the daemon's own. What changed is written and announced as
//! `agent_team_changed`; nothing is written under `.claude/` or Claude's
//! configuration directory.

use super::*;
use agentdocker_core::team::{self, SessionFacts, Team, TeamMembership, TeammateArgs};

/// What the refresh needs of one live Claude Code record.
struct Target {
    id: AgentId,
    pid: Option<u32>,
    born: Option<DateTime<Utc>>,
    session: Option<String>,
    workdir: Option<PathBuf>,
    /// The configuration directory its adapters reported, where they did.
    reported_config: Option<PathBuf>,
}

/// What was read of one live process, with the birth it was read under:
/// a pid reused by another process is read again.
#[derive(Clone, Debug)]
pub(super) struct Seen {
    born: Option<DateTime<Utc>>,
    argv: Option<Vec<String>>,
    /// The configuration directory its own environment says it uses,
    /// where the daemon could read that environment.
    config_dir: Option<PathBuf>,
}

/// Each live Claude Code process as last read, by pid.
pub(super) type ProcessCache = HashMap<u32, Seen>;

impl Seen {
    fn read(pid: u32, born: Option<DateTime<Utc>>) -> Self {
        // The daemon's own environment is nobody's session: a record that
        // names this process is a fixture's, never Claude's.
        let environment = (pid != std::process::id())
            .then(|| multiplexer::environment(pid))
            .flatten();
        Self {
            born,
            argv: procinfo::arguments(pid),
            config_dir: environment.and_then(|env| {
                team::config_dir(
                    env.get("CLAUDE_CONFIG_DIR").map(String::as_str),
                    env.get("HOME").map(Path::new),
                )
            }),
        }
    }
}

impl Target {
    fn seen<'a>(&self, processes: &'a ProcessCache) -> Option<&'a Seen> {
        processes.get(&self.pid?)
    }

    fn argv<'a>(&self, processes: &'a ProcessCache) -> Option<&'a [String]> {
        self.seen(processes).and_then(|seen| seen.argv.as_deref())
    }

    /// The configuration directory this session uses: first-hand, else
    /// as reported, else the daemon's own.
    fn config_dir(&self, processes: &ProcessCache, default: Option<&Path>) -> Option<PathBuf> {
        self.seen(processes)
            .and_then(|seen| seen.config_dir.clone())
            .or_else(|| self.reported_config.clone())
            .or_else(|| default.map(Path::to_path_buf))
    }
}

impl Daemon {
    /// Read every live Claude Code session's place in a team and record
    /// what changed.
    pub async fn refresh_teams(&self) {
        let (targets, cached, default_config) = {
            let state = lock(&self.state);
            let targets: Vec<Target> = state
                .registry
                .live()
                .filter(|a| a.spec.runtime == "claude-code")
                .map(|a| Target {
                    id: a.id.clone(),
                    pid: a.pid,
                    born: a.process_started_at,
                    session: a.spec.labels.get("session_id").cloned(),
                    workdir: a.spec.workdir.clone(),
                    reported_config: a
                        .spec
                        .labels
                        .get(team::CONFIG_DIR_LABEL)
                        .filter(|dir| !dir.is_empty())
                        .map(PathBuf::from),
                })
                .collect();
            (
                targets,
                state.team_processes.clone(),
                state.claude_config_dir.clone(),
            )
        };
        if targets.is_empty() {
            let mut state = lock(&self.state);
            state.team_processes.clear();
            state.teams.clear();
            return;
        }
        let Ok((processes, memberships, teams_read)) = tokio::task::spawn_blocking(move || {
            let mut processes: ProcessCache = HashMap::new();
            for target in &targets {
                if let Some(pid) = target.pid {
                    let seen = cached
                        .get(&pid)
                        .filter(|seen| seen.born == target.born)
                        .cloned()
                        .unwrap_or_else(|| Seen::read(pid, target.born));
                    processes.insert(pid, seen);
                }
            }
            let plain = |target: &Target| {
                target
                    .argv(&processes)
                    .and_then(TeammateArgs::from_argv)
                    .is_none()
            };
            let config_of =
                |target: &Target| target.config_dir(&processes, default_config.as_deref());
            // The teams each session can see: those under the directory it
            // uses, with what its checkout's own `.claude/` adds.
            let mut teams: HashMap<(Option<PathBuf>, Option<PathBuf>), Vec<Team>> = HashMap::new();
            for target in &targets {
                let config = config_of(target);
                teams
                    .entry((config.clone(), target.workdir.clone()))
                    .or_insert_with(|| match (&config, &target.workdir) {
                        (Some(config), Some(workdir)) => {
                            agentdocker_host::teams::teams_for_checkout(config, workdir)
                        }
                        (Some(config), None) => agentdocker_host::teams::read_teams(config),
                        (None, _) => Vec::new(),
                    });
            }
            let memberships: Vec<(AgentId, Option<TeamMembership>)> = targets
                .iter()
                .map(|target| {
                    let plain_here = targets
                        .iter()
                        .filter(|other| other.workdir == target.workdir && plain(other))
                        .count();
                    let facts = SessionFacts {
                        argv: target.argv(&processes),
                        session_id: target.session.as_deref(),
                        workdir: target.workdir.as_deref(),
                        sole_plain_session: plain_here == 1,
                    };
                    let known = teams
                        .get(&(config_of(target), target.workdir.clone()))
                        .map_or(&[][..], Vec::as_slice);
                    (target.id.clone(), team::membership(&facts, known))
                })
                .collect();
            // Every team read, by name, for naming a lease's actor.
            let teams_read: HashMap<String, Team> = teams
                .into_values()
                .flatten()
                .map(|team| (team.name.clone(), team))
                .collect();
            (processes, memberships, teams_read)
        })
        .await
        else {
            return;
        };
        let mut state = lock(&self.state);
        state.team_processes = processes;
        state.teams = teams_read;
        for (id, membership) in memberships {
            state.apply_team(&id, membership);
        }
    }

    /// After a registration: a Claude Code session is placed in its team
    /// at once rather than at the next tick.
    pub(super) async fn note_team(&self, response: &Response) {
        if let Response::Agent { agent } = response
            && agent.spec.runtime == "claude-code"
            && agent.status.is_live()
        {
            self.refresh_teams().await;
        }
    }
}

impl State {
    /// Record an agent's place in a team. Disk first, memory on commit,
    /// as a checkout move is: a change that did not commit is noticed
    /// again at the next refresh rather than lost.
    pub(super) fn apply_team(&mut self, id: &AgentId, team: Option<TeamMembership>) {
        let Some(current) = self
            .registry
            .get(id)
            .filter(|a| a.status.is_live())
            .cloned()
        else {
            return;
        };
        if current.team == team {
            return;
        }
        let mut record = current;
        record.team = team.clone();
        info!(
            agent = %id.short(),
            team = %team.as_ref().map_or_else(|| "none".to_owned(), TeamMembership::describe),
            "team membership read"
        );
        if self.persist("agent", |store| store.upsert_agent(&record)) == Persisted::Committed {
            if let Some(live) = self.registry.get_mut(id) {
                live.team = team.clone();
            }
            self.emit(EventKind::AgentTeamChanged {
                agent: id.clone(),
                team,
            });
        }
    }
}
