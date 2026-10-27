//! Claude Code agent teams on the records: which live Claude Code session
//! leads a team and which are its teammates, read from each process's
//! command line and Claude's own team files
//! ([`agentdocker_core::team`], [`agentdocker_host::teams`]).
//!
//! The reading happens off the state lock, on the five-second tick that
//! also sweeps checkouts and again right after a Claude Code session
//! registers, so a teammate is named within a second of its first hook.
//! A command line is read once per process (by pid and birth) and kept;
//! the team files are small and read each time. What changed is written
//! and announced as `agent_team_changed`; nothing is written under
//! `.claude/` or Claude's configuration directory.

use super::*;
use agentdocker_core::team::{self, SessionFacts, Team, TeamMembership, TeammateArgs};

/// What the refresh needs of one live Claude Code record.
struct Target {
    id: AgentId,
    pid: Option<u32>,
    born: Option<DateTime<Utc>>,
    session: Option<String>,
    workdir: Option<PathBuf>,
}

/// A process's command line as last read, with the birth it was read
/// under: a pid reused by another process is read again.
pub(super) type ArgvCache = HashMap<u32, (Option<DateTime<Utc>>, Option<Vec<String>>)>;

/// A target's command line, where it was read.
fn line<'a>(argv: &'a ArgvCache, target: &Target) -> Option<&'a [String]> {
    argv.get(&target.pid?).and_then(|(_, a)| a.as_deref())
}

impl Daemon {
    /// Read every live Claude Code session's place in a team and record
    /// what changed.
    pub async fn refresh_teams(&self) {
        let (targets, config_dir, cached) = {
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
                })
                .collect();
            (
                targets,
                state.claude_config_dir.clone(),
                state.teammate_argv.clone(),
            )
        };
        if targets.is_empty() {
            if !cached.is_empty() {
                lock(&self.state).teammate_argv.clear();
            }
            return;
        }
        let Ok((argv, memberships)) = tokio::task::spawn_blocking(move || {
            let mut argv: ArgvCache = HashMap::new();
            for target in &targets {
                if let Some(pid) = target.pid {
                    let known = cached
                        .get(&pid)
                        .filter(|(born, _)| *born == target.born)
                        .cloned();
                    argv.insert(
                        pid,
                        known.unwrap_or_else(|| (target.born, procinfo::arguments(pid))),
                    );
                }
            }
            let plain = |target: &Target| {
                line(&argv, target)
                    .and_then(TeammateArgs::from_argv)
                    .is_none()
            };
            let mut teams: HashMap<Option<PathBuf>, Vec<Team>> = HashMap::new();
            for target in &targets {
                teams.entry(target.workdir.clone()).or_insert_with(|| {
                    match (&config_dir, &target.workdir) {
                        (Some(config), Some(workdir)) => {
                            agentdocker_host::teams::teams_for_checkout(config, workdir)
                        }
                        (Some(config), None) => agentdocker_host::teams::read_teams(config),
                        (None, _) => Vec::new(),
                    }
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
                        argv: line(&argv, target),
                        session_id: target.session.as_deref(),
                        workdir: target.workdir.as_deref(),
                        sole_plain_session: plain_here == 1,
                    };
                    let known = teams.get(&target.workdir).map_or(&[][..], Vec::as_slice);
                    (target.id.clone(), team::membership(&facts, known))
                })
                .collect();
            (argv, memberships)
        })
        .await
        else {
            return;
        };
        let mut state = lock(&self.state);
        state.teammate_argv = argv;
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
