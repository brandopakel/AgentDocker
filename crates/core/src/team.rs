//! Claude Code agent teams, as their files and command lines say them.
//!
//! Claude Code can run a session as a *lead* with named *teammates*: more
//! Claude Code processes in the same checkout, each with a name and a
//! colour the lead gave it, a JSON inbox the others write to, and a team
//! file naming them all. The team is Claude's own: it has no file leases,
//! so two teammates editing one file overwrite each other, and a Codex
//! agent in the same checkout knows nothing of it. AgentDocker reads the
//! team to show the relation and to name the teammate whose lease refuses
//! an edit. It writes nothing of its own there.
//!
//! What is read, and how sure each part is:
//!
//! - **The team file**, `teams/<team>/config.json` under Claude Code's
//!   configuration directory (`$CLAUDE_CONFIG_DIR`, else `~/.claude`). Its
//!   shape is observed from the code that writes it: `{name, createdAt,
//!   leadAgentId, leadSessionId, members: [{agentId, name, agentType,
//!   color?, joinedAt, tmuxPaneId, cwd, subscriptions, backendType,
//!   planModeRequired?}]}`; the lead is the member named `team-lead`, and a
//!   team made without a name is `session-<first eight of the lead's
//!   session id>`. `leadSessionId` is the lead's Claude Code session id,
//!   which its hooks already report to AgentDocker: that is what ties the
//!   lead's record to the team.
//! - **Inboxes**, `teams/<team>/inboxes/<name>.json`, one per agent of the
//!   team: observed as the path the mailbox code builds. Only their names
//!   are read here; their contents are the agents' own.
//! - **A teammate's command line**: Claude Code starts a teammate with
//!   hidden options `--agent-id <id> --agent-name <name> --team-name <team>
//!   --agent-color <colour> --parent-session-id <lead's session id>`,
//!   observed from the option table. That is what ties a teammate's
//!   process to its team, with no file read at all.
//! - **A project's own `.claude/agent-registry.json` and `.claude/mailbox/`**
//!   are named in Claude Code only as paths it keeps out of git; nothing
//!   observed says what the registry holds. The registry reader here is
//!   tolerant — a list of entries, or an object holding one, or a map of
//!   names to entries, each with whatever of a name, colour, agent id and
//!   team it carries under the spellings Claude uses elsewhere — and
//!   everything unknown is ignored, never invented.
//!
//! Everything here is pure: callers pass file contents and directory
//! listings as data.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The name every team's lead has in Claude Code.
pub const LEAD_NAME: &str = "team-lead";
/// The team a teammate belongs to when its command line names none.
pub const DEFAULT_TEAM: &str = "default";
/// Where the team files live under Claude Code's configuration directory.
pub const TEAMS_DIR: &str = "teams";
/// A team's file, inside its directory under [`TEAMS_DIR`].
pub const TEAM_FILE: &str = "config.json";
/// The inboxes directory inside a team's directory.
pub const INBOXES_DIR: &str = "inboxes";
/// A project's own registry of agents, under its `.claude/`.
pub const REGISTRY_FILE: &str = "agent-registry.json";
/// A project's own mailbox directory, under its `.claude/`.
pub const MAILBOX_DIR: &str = "mailbox";
/// The label a Claude Code session's adapters put on its record saying
/// which configuration directory that session uses, resolved by
/// [`config_dir`] in the environment Claude Code gave them — its hooks
/// and its MCP server run inside the session and inherit it. The daemon
/// reads that session's team files from there, so a session launched
/// under another directory than the daemon's own — or under the default
/// while the daemon has one set — is still placed.
pub const CONFIG_DIR_LABEL: &str = "claude_config_dir";

/// The configuration directory a Claude Code session uses, by Claude's
/// own rule as observed: `CLAUDE_CONFIG_DIR` with surrounding space
/// trimmed, a blank value being unset; else `.claude` in the home
/// directory. `None` with neither.
pub fn config_dir(claude_config_dir: Option<&str>, home: Option<&Path>) -> Option<PathBuf> {
    match claude_config_dir.map(str::trim).filter(|v| !v.is_empty()) {
        Some(dir) => Some(PathBuf::from(dir)),
        None => home.map(|home| home.join(".claude")),
    }
}

/// One agent of a team as the team names it: the lead or a teammate.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Teammate {
    pub name: String,
    /// The colour the lead gave it, as Claude spells it (`blue`); `None`
    /// where the file says none, as it does for the lead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colour: Option<String>,
    /// Claude's own id for it (`<name>@<team>` as observed), never an
    /// AgentDocker id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// The checkout it was started in, as the team file records it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    /// Its inbox file, where the listing had one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inbox_path: Option<PathBuf>,
}

/// A team: its lead and the teammates it has.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Team {
    pub name: String,
    /// The lead's own entry; its name is [`LEAD_NAME`] unless the file
    /// says otherwise.
    pub lead: Teammate,
    /// The lead's Claude Code session id, where the file carries it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lead_session_id: Option<String>,
    /// Everyone but the lead, in the file's order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<Teammate>,
}

/// Why a team file could not be read as one.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TeamParseError {
    #[error("not JSON: {0}")]
    NotJson(String),
    #[error("not a team file: {0}")]
    NotATeam(&'static str),
}

/// The team file's shape, as observed; every field optional so a file
/// from another version still names what it can. Unknown fields are
/// ignored.
#[derive(Deserialize, Default)]
#[serde(default)]
struct TeamFile {
    name: Option<String>,
    #[serde(alias = "teamName")]
    team_name: Option<String>,
    #[serde(alias = "leadAgentId")]
    lead_agent_id: Option<String>,
    #[serde(alias = "leadSessionId")]
    lead_session_id: Option<String>,
    members: Vec<MemberEntry>,
}

/// One member as the team file or a registry writes it. Every spelling
/// seen in Claude Code for each field is accepted.
#[derive(Deserialize, Default)]
#[serde(default)]
struct MemberEntry {
    #[serde(alias = "agentName", alias = "agent_name")]
    name: Option<String>,
    #[serde(alias = "colour", alias = "agentColor", alias = "agent_color")]
    color: Option<String>,
    #[serde(alias = "agentId", alias = "agent_id", alias = "id")]
    agent_id: Option<String>,
    cwd: Option<PathBuf>,
    #[serde(alias = "teamName", alias = "team_name")]
    team: Option<String>,
}

impl MemberEntry {
    fn teammate(self, fallback_name: Option<&str>) -> Option<Teammate> {
        let name = self
            .name
            .as_deref()
            .or(fallback_name)
            .map(str::trim)
            .filter(|n| !n.is_empty())?
            .to_owned();
        Some(Teammate {
            name,
            colour: self
                .color
                .map(|c| c.trim().to_lowercase())
                .filter(|c| !c.is_empty()),
            agent_id: self.agent_id.filter(|id| !id.trim().is_empty()),
            cwd: self.cwd,
            inbox_path: None,
        })
    }
}

impl Team {
    /// A team from its file. `directory` is the team's directory name,
    /// which names the team when the file does not.
    pub fn from_team_file(directory: &str, json: &str) -> Result<Self, TeamParseError> {
        let file: TeamFile =
            serde_json::from_str(json).map_err(|e| TeamParseError::NotJson(e.to_string()))?;
        let name = file
            .name
            .or(file.team_name)
            .map(|n| n.trim().to_owned())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| directory.trim().to_owned());
        if name.is_empty() {
            return Err(TeamParseError::NotATeam("it names no team"));
        }
        let mut lead = None;
        let mut members = Vec::new();
        for entry in file.members {
            let is_lead = (entry.agent_id.is_some() && entry.agent_id == file.lead_agent_id)
                || entry
                    .name
                    .as_deref()
                    .is_some_and(|n| same_name(n, LEAD_NAME));
            let Some(teammate) = entry.teammate(None) else {
                continue;
            };
            if is_lead && lead.is_none() {
                lead = Some(teammate);
            } else {
                members.push(teammate);
            }
        }
        let lead = lead.unwrap_or_else(|| Teammate {
            name: LEAD_NAME.to_owned(),
            agent_id: file.lead_agent_id.clone(),
            ..Teammate::default()
        });
        Ok(Self {
            name,
            lead,
            lead_session_id: file.lead_session_id.filter(|s| !s.trim().is_empty()),
            members,
        })
    }

    /// Attach the inboxes a directory listing shows: each `<name>.json`
    /// goes to the member of that name, and one that names nobody in the
    /// file adds a member known by name alone, so a teammate the file no
    /// longer lists but whose inbox remains is still named.
    pub fn with_inboxes(mut self, listing: &[PathBuf]) -> Self {
        for path in listing {
            let Some(stem) = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(".json"))
                .map(str::trim)
                .filter(|n| !n.is_empty())
            else {
                continue;
            };
            if same_name(stem, &self.lead.name) {
                self.lead.inbox_path = Some(path.clone());
            } else if let Some(member) = self.members.iter_mut().find(|m| same_name(&m.name, stem))
            {
                member.inbox_path = Some(path.clone());
            } else {
                self.members.push(Teammate {
                    name: stem.to_owned(),
                    inbox_path: Some(path.clone()),
                    ..Teammate::default()
                });
            }
        }
        self
    }

    /// The member of that name, the lead included.
    pub fn member(&self, name: &str) -> Option<&Teammate> {
        if same_name(&self.lead.name, name) {
            return Some(&self.lead);
        }
        self.members.iter().find(|m| same_name(&m.name, name))
    }

    /// The member an in-process teammate's id names. Claude's default
    /// teammates run inside the lead's process, so their hooks fire under
    /// the lead's session with the teammate's `agent_id`; it is matched to
    /// a member's `agentId` exactly, then, as that id is observed to be
    /// `<name>@<team>` for some members, by the name before the `@`, then
    /// as a bare name. `None` for an id that names nobody here.
    pub fn acting_member(&self, actor: &str) -> Option<&Teammate> {
        let actor = actor.trim();
        if actor.is_empty() {
            return None;
        }
        let all = std::iter::once(&self.lead).chain(self.members.iter());
        if let Some(exact) = all.clone().find(|m| m.agent_id.as_deref() == Some(actor)) {
            return Some(exact);
        }
        let name = actor.split_once('@').map_or(actor, |(name, _)| name);
        all.clone().find(|m| same_name(&m.name, name))
    }
}

/// Entries of a project's `.claude/agent-registry.json`, read tolerantly
/// (see the module notes): whatever names an agent is kept, under
/// whichever team it says. An unreadable or unrecognised file is empty,
/// never an error: the file is Claude's, and its shape is not promised.
pub fn registry_entries(json: &str) -> Vec<(Option<String>, Teammate)> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    let mut take = |value: serde_json::Value, fallback: Option<&str>| {
        if let Ok(entry) = serde_json::from_value::<MemberEntry>(value) {
            let team = entry.team.clone();
            if let Some(teammate) = entry.teammate(fallback) {
                entries.push((team, teammate));
            }
        }
    };
    match value {
        serde_json::Value::Array(items) => {
            for item in items {
                take(item, None);
            }
        }
        serde_json::Value::Object(map) => {
            let listed = ["agents", "members", "teammates", "registry"]
                .iter()
                .find_map(|key| map.get(*key).and_then(|v| v.as_array()).cloned());
            match listed {
                Some(items) => {
                    for item in items {
                        take(item, None);
                    }
                }
                None => {
                    for (name, item) in map {
                        if item.is_object() {
                            take(item, Some(&name));
                        }
                    }
                }
            }
        }
        _ => {}
    }
    entries
}

/// What a teammate's command line says about it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeammateArgs {
    pub team: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colour: Option<String>,
    /// The lead's session id, as `--parent-session-id` gives it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
}

impl TeammateArgs {
    /// The teammate a Claude Code command line starts, by its hidden
    /// options; `None` without `--agent-name`, which only a teammate
    /// has. Both `--flag value` and `--flag=value` are read. A team name
    /// missing is [`DEFAULT_TEAM`], as Claude's mailbox code takes it.
    pub fn from_argv(argv: &[String]) -> Option<Self> {
        let mut options: BTreeMap<&str, &str> = BTreeMap::new();
        let mut i = 0;
        while i < argv.len() {
            let arg = argv[i].as_str();
            if arg == "--" {
                break;
            }
            if let Some((flag, value)) = arg.split_once('=')
                && TEAMMATE_FLAGS.contains(&flag)
            {
                options.insert(flag, value);
            } else if TEAMMATE_FLAGS.contains(&arg) {
                if let Some(value) = argv.get(i + 1).filter(|v| !v.starts_with("--")) {
                    options.insert(arg, value);
                    i += 1;
                }
            }
            i += 1;
        }
        let name = options
            .get("--agent-name")
            .map(|n| n.trim())
            .filter(|n| !n.is_empty())?;
        let clean = |flag: &str| {
            options
                .get(flag)
                .map(|v| v.trim())
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
        };
        Some(Self {
            team: clean("--team-name").unwrap_or_else(|| DEFAULT_TEAM.to_owned()),
            name: name.to_owned(),
            agent_id: clean("--agent-id"),
            colour: clean("--agent-color").map(|c| c.to_lowercase()),
            parent_session_id: clean("--parent-session-id"),
        })
    }
}

const TEAMMATE_FLAGS: [&str; 5] = [
    "--agent-id",
    "--agent-name",
    "--team-name",
    "--agent-color",
    "--parent-session-id",
];

/// Which side of a team an agent is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamRole {
    Lead,
    Teammate,
}

/// An agent's place in a team, as its record carries it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamMembership {
    pub team: String,
    pub role: TeamRole,
    /// Its name in the team: the lead's is [`LEAD_NAME`].
    pub name: String,
    /// The name of the team's lead, for a teammate to say who it answers
    /// to; the lead's own name for the lead.
    pub lead_name: String,
    /// The colour the lead gave a teammate, as Claude spells it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colour: Option<String>,
    /// Claude's own id for the agent in the team, where known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
}

impl TeamMembership {
    /// `lead of <team>`, or `<name>, teammate of <lead> in <team>`.
    pub fn describe(&self) -> String {
        match self.role {
            TeamRole::Lead => format!("lead of {}", self.team),
            TeamRole::Teammate => format!(
                "{}, teammate of {} in {}",
                self.name, self.lead_name, self.team
            ),
        }
    }

    /// The short words for a chip: `Lead of <team>` or `Teammate of <lead>`.
    pub fn chip_words(&self) -> String {
        match self.role {
            TeamRole::Lead => format!("Lead of {}", self.team),
            TeamRole::Teammate => format!("Teammate of {}", self.lead_name),
        }
    }

    /// The colour as one of the eight Claude Code uses, where it is.
    pub fn role_color(&self) -> Option<crate::RoleColor> {
        self.colour.as_deref().and_then(crate::RoleColor::parse)
    }

    /// The owner an `actor` reported inside this agent's session is for a
    /// lease (see `Lease::actor`): another member of this agent's team, by
    /// its id there (its `agentId`, else its name), or `None` — the
    /// session itself. An actor that names nobody in `team`, or names
    /// this agent itself, is the session: Claude Code reports an
    /// `agent_id` for an ordinary subagent too, and a subagent is the
    /// session's own work, never an agent apart from it.
    pub fn owner_actor(&self, actor: Option<&str>, team: Option<&Team>) -> Option<String> {
        let member = team?.acting_member(actor?)?;
        if same_name(&member.name, &self.name) {
            return None;
        }
        Some(
            member
                .agent_id
                .clone()
                .unwrap_or_else(|| member.name.clone()),
        )
    }

    /// What a lease taken under this membership says of its holder when
    /// `actor` names who took it: the in-process teammate as the team
    /// names it (`pruner, teammate of team-lead in orchard`), the
    /// membership itself when no actor is named, and the membership with
    /// the actor's id when the team does not know the id.
    pub fn describe_acting(&self, actor: Option<&str>, team: Option<&Team>) -> String {
        match actor.map(str::trim).filter(|a| !a.is_empty()) {
            None => self.describe(),
            Some(actor) => match team.and_then(|t| t.acting_member(actor)) {
                Some(member) if same_name(&member.name, &self.lead_name) => self.describe(),
                Some(member) => format!(
                    "{}, teammate of {} in {}",
                    member.name, self.lead_name, self.team
                ),
                None => format!("{}, as its agent {actor}", self.describe()),
            },
        }
    }
}

/// What is known about one Claude Code session when its team is worked
/// out: its command line, its session id and where it works.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionFacts<'a> {
    pub argv: Option<&'a [String]>,
    pub session_id: Option<&'a str>,
    pub workdir: Option<&'a Path>,
    /// Whether this is the only Claude Code session in its checkout that
    /// is not a teammate by its command line. The caller knows the
    /// registry; this function does not.
    pub sole_plain_session: bool,
}

/// The team an agent belongs to, from the strongest evidence that holds:
///
/// 1. Its command line names it a teammate (`--agent-name`): a teammate of
///    that team, coloured as the command line says, else as the team file
///    does. No team file is needed.
/// 2. A team file's `leadSessionId` is its session id: that team's lead.
/// 3. With no session id of its own (an adopted session, no hooks), it is
///    the only plain Claude Code session in a checkout where exactly one
///    team's lead was started: that team's lead. A session whose id is
///    known and matches no team is never a lead by elimination.
///
/// `None` when nothing says it is in a team.
pub fn membership(facts: &SessionFacts<'_>, teams: &[Team]) -> Option<TeamMembership> {
    if let Some(args) = facts.argv.and_then(TeammateArgs::from_argv) {
        let team = teams.iter().find(|t| same_name(&t.name, &args.team));
        let filed = team.and_then(|t| t.member(&args.name));
        return Some(TeamMembership {
            team: team.map_or(args.team.clone(), |t| t.name.clone()),
            role: TeamRole::Teammate,
            name: args.name.clone(),
            lead_name: team.map_or(LEAD_NAME.to_owned(), |t| t.lead.name.clone()),
            colour: args.colour.or_else(|| filed.and_then(|m| m.colour.clone())),
            agent_id: args
                .agent_id
                .or_else(|| filed.and_then(|m| m.agent_id.clone())),
        });
    }
    let lead_of = |team: &Team| TeamMembership {
        team: team.name.clone(),
        role: TeamRole::Lead,
        name: team.lead.name.clone(),
        lead_name: team.lead.name.clone(),
        colour: team.lead.colour.clone(),
        agent_id: team.lead.agent_id.clone(),
    };
    if let Some(session) = facts.session_id.filter(|s| !s.is_empty()) {
        return teams
            .iter()
            .find(|t| t.lead_session_id.as_deref() == Some(session))
            .map(lead_of);
    }
    let workdir = facts.workdir?;
    if !facts.sole_plain_session {
        return None;
    }
    let mut here = teams
        .iter()
        .filter(|t| t.lead.cwd.as_deref().is_some_and(|cwd| cwd == workdir));
    match (here.next(), here.next()) {
        (Some(team), None) => Some(lead_of(team)),
        _ => None,
    }
}

/// Whether two names are one, as Claude compares them: case and
/// surrounding space do not count, and a space is a hyphen.
pub fn same_name(a: &str, b: &str) -> bool {
    normalize(a) == normalize(b)
}

fn normalize(name: &str) -> String {
    name.trim()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-")
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEAM_FILE_JSON: &str = r#"{
        "name": "orchard",
        "createdAt": 1700000000000,
        "leadAgentId": "team-lead@orchard",
        "leadSessionId": "0123456789abcdef0123",
        "members": [
            {"agentId": "team-lead@orchard", "name": "team-lead", "agentType": "team-lead",
             "joinedAt": 1700000000000, "tmuxPaneId": "leader", "cwd": "/work/orchard",
             "subscriptions": [], "backendType": "in-process"},
            {"agentId": "pruner@orchard", "name": "pruner", "color": "Blue", "agentType": "general-purpose",
             "planModeRequired": false, "joinedAt": 1700000001000, "tmuxPaneId": "%3",
             "cwd": "/work/orchard", "subscriptions": [], "backendType": "tmux", "unknownLater": 1},
            {"agentId": "grafter@orchard", "name": "grafter", "color": "green", "joinedAt": 1700000002000}
        ],
        "somethingNewer": {"ignored": true}
    }"#;

    fn argv(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn a_team_file_names_its_lead_and_teammates() {
        let team = Team::from_team_file("orchard", TEAM_FILE_JSON).unwrap();
        assert_eq!(team.name, "orchard");
        assert_eq!(team.lead.name, LEAD_NAME);
        assert_eq!(team.lead.agent_id.as_deref(), Some("team-lead@orchard"));
        assert_eq!(team.lead.cwd.as_deref(), Some(Path::new("/work/orchard")));
        assert_eq!(
            team.lead_session_id.as_deref(),
            Some("0123456789abcdef0123")
        );
        assert_eq!(
            team.members
                .iter()
                .map(|m| (m.name.as_str(), m.colour.as_deref()))
                .collect::<Vec<_>>(),
            [("pruner", Some("blue")), ("grafter", Some("green"))],
            "colours are lowered, unknown fields ignored"
        );
        assert_eq!(team.member("Pruner").unwrap().name, "pruner");
    }

    #[test]
    fn a_team_file_without_a_name_is_named_by_its_directory_and_tolerates_absence() {
        let team = Team::from_team_file("session-01234567", r#"{"members": []}"#).unwrap();
        assert_eq!(team.name, "session-01234567");
        assert_eq!(team.lead.name, LEAD_NAME, "a lead is assumed even unlisted");
        assert!(team.members.is_empty() && team.lead_session_id.is_none());
        assert!(matches!(
            Team::from_team_file("x", "{not json"),
            Err(TeamParseError::NotJson(_))
        ));
        assert!(matches!(
            Team::from_team_file("  ", "{}"),
            Err(TeamParseError::NotATeam(_))
        ));
        // A lead known by its id alone, under another name.
        let team = Team::from_team_file(
            "t",
            r#"{"leadAgentId":"boss@t","members":[{"agentId":"boss@t","name":"boss"},{"name":"hand"}]}"#,
        )
        .unwrap();
        assert_eq!(team.lead.name, "boss");
        assert_eq!(team.members.len(), 1);
    }

    #[test]
    fn inboxes_attach_by_name_and_name_the_unlisted() {
        let team = Team::from_team_file("orchard", TEAM_FILE_JSON)
            .unwrap()
            .with_inboxes(&[
                PathBuf::from("/c/teams/orchard/inboxes/team-lead.json"),
                PathBuf::from("/c/teams/orchard/inboxes/pruner.json"),
                PathBuf::from("/c/teams/orchard/inboxes/scout.json"),
                PathBuf::from("/c/teams/orchard/inboxes/notes.txt"),
            ]);
        assert_eq!(
            team.lead.inbox_path.as_deref(),
            Some(Path::new("/c/teams/orchard/inboxes/team-lead.json"))
        );
        assert_eq!(
            team.member("pruner").unwrap().inbox_path.as_deref(),
            Some(Path::new("/c/teams/orchard/inboxes/pruner.json"))
        );
        assert!(team.member("grafter").unwrap().inbox_path.is_none());
        let scout = team.member("scout").expect("an inbox alone names a member");
        assert!(scout.colour.is_none() && scout.inbox_path.is_some());
        assert!(
            team.member("notes").is_none(),
            "only .json files are inboxes"
        );
    }

    #[test]
    fn a_registry_is_read_in_whatever_shape_it_has() {
        let listed = registry_entries(
            r#"{"agents":[{"agentName":"pruner","agentColor":"blue","teamName":"orchard","agentId":"pruner@orchard"},{"nope":1}]}"#,
        );
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].0.as_deref(), Some("orchard"));
        assert_eq!(listed[0].1.name, "pruner");
        assert_eq!(listed[0].1.colour.as_deref(), Some("blue"));
        let mapped = registry_entries(r#"{"grafter":{"color":"green"},"count":3}"#);
        assert_eq!(mapped.len(), 1);
        assert_eq!(mapped[0].1.name, "grafter");
        assert_eq!(mapped[0].0, None);
        let array = registry_entries(r#"[{"name":"scout"},{"color":"red"}]"#);
        assert_eq!(array.len(), 1, "an entry without a name names nobody");
        assert!(registry_entries("nonsense").is_empty());
        assert!(registry_entries("42").is_empty());
    }

    #[test]
    fn a_teammate_is_known_by_its_command_line() {
        let args = TeammateArgs::from_argv(&argv(
            "claude --agent-id pruner@orchard --agent-name pruner --team-name orchard \
             --agent-color Blue --parent-session-id 0123456789abcdef0123 --model opus",
        ))
        .unwrap();
        assert_eq!(args.team, "orchard");
        assert_eq!(args.name, "pruner");
        assert_eq!(args.agent_id.as_deref(), Some("pruner@orchard"));
        assert_eq!(args.colour.as_deref(), Some("blue"));
        assert_eq!(
            args.parent_session_id.as_deref(),
            Some("0123456789abcdef0123")
        );
        let equals = TeammateArgs::from_argv(&argv(
            "node /x/@anthropic-ai/claude-code/cli.js --agent-name=scout --team-name=orchard",
        ))
        .unwrap();
        assert_eq!(
            (equals.team.as_str(), equals.name.as_str()),
            ("orchard", "scout")
        );
        assert_eq!(
            TeammateArgs::from_argv(&argv("claude --agent-name lone"))
                .unwrap()
                .team,
            DEFAULT_TEAM
        );
        assert!(TeammateArgs::from_argv(&argv("claude --team-name orchard")).is_none());
        assert!(TeammateArgs::from_argv(&argv("claude --agent-name --team-name t")).is_none());
        assert!(
            TeammateArgs::from_argv(&argv("claude -- --agent-name pruner")).is_none(),
            "after `--` it is the prompt"
        );
        assert!(TeammateArgs::from_argv(&argv("claude --resume abc")).is_none());
    }

    #[test]
    fn membership_follows_the_strongest_evidence() {
        let team = Team::from_team_file("orchard", TEAM_FILE_JSON).unwrap();
        let other = Team::from_team_file(
            "other",
            r#"{"name":"other","leadSessionId":"ffff","members":[{"name":"team-lead","cwd":"/work/other"}]}"#,
        )
        .unwrap();
        let teams = [team, other];
        // A teammate by its command line, coloured by the file when the
        // command line does not say.
        let line = argv("claude --agent-name grafter --team-name orchard");
        let teammate = membership(
            &SessionFacts {
                argv: Some(&line),
                session_id: Some("abcdef0123456789"),
                workdir: Some(Path::new("/work/orchard")),
                sole_plain_session: false,
            },
            &teams,
        )
        .unwrap();
        assert_eq!(teammate.role, TeamRole::Teammate);
        assert_eq!(teammate.name, "grafter");
        assert_eq!(teammate.lead_name, LEAD_NAME);
        assert_eq!(teammate.colour.as_deref(), Some("green"));
        assert_eq!(teammate.agent_id.as_deref(), Some("grafter@orchard"));
        assert_eq!(
            teammate.describe(),
            "grafter, teammate of team-lead in orchard"
        );
        assert_eq!(teammate.chip_words(), "Teammate of team-lead");
        assert_eq!(teammate.role_color(), Some(crate::RoleColor::Green));
        // A teammate of a team no file knows is still a teammate.
        let line = argv("claude --agent-name x --team-name unknown --agent-color pink");
        let unknown = membership(
            &SessionFacts {
                argv: Some(&line),
                ..Default::default()
            },
            &teams,
        )
        .unwrap();
        assert_eq!(
            (unknown.team.as_str(), unknown.colour.as_deref()),
            ("unknown", Some("pink"))
        );
        // The lead by its session id.
        let lead = membership(
            &SessionFacts {
                argv: Some(&argv("claude")),
                session_id: Some("0123456789abcdef0123"),
                workdir: Some(Path::new("/elsewhere")),
                sole_plain_session: true,
            },
            &teams,
        )
        .unwrap();
        assert_eq!(lead.role, TeamRole::Lead);
        assert_eq!(lead.team, "orchard");
        assert_eq!(lead.describe(), "lead of orchard");
        assert_eq!(lead.chip_words(), "Lead of orchard");
        // A known session that leads nothing is nothing, even alone in
        // a team's checkout.
        assert!(
            membership(
                &SessionFacts {
                    argv: None,
                    session_id: Some("unrelated"),
                    workdir: Some(Path::new("/work/orchard")),
                    sole_plain_session: true,
                },
                &teams,
            )
            .is_none()
        );
        // No session id: the lead by elimination, only when alone.
        let alone = SessionFacts {
            argv: None,
            session_id: None,
            workdir: Some(Path::new("/work/orchard")),
            sole_plain_session: true,
        };
        assert_eq!(membership(&alone, &teams).unwrap().team, "orchard");
        assert!(
            membership(
                &SessionFacts {
                    sole_plain_session: false,
                    ..alone.clone()
                },
                &teams
            )
            .is_none()
        );
        assert!(
            membership(
                &SessionFacts {
                    workdir: Some(Path::new("/work/nowhere")),
                    ..alone.clone()
                },
                &teams
            )
            .is_none()
        );
        // Two teams led from one checkout cannot be told apart.
        let twin = Team::from_team_file(
            "twin",
            r#"{"name":"twin","members":[{"name":"team-lead","cwd":"/work/orchard"}]}"#,
        )
        .unwrap();
        let mut three = teams.to_vec();
        three.push(twin);
        assert!(membership(&alone, &three).is_none());
        assert!(membership(&SessionFacts::default(), &teams).is_none());
    }

    #[test]
    fn an_in_process_teammate_is_named_by_its_agent_id() {
        let team = Team::from_team_file("orchard", TEAM_FILE_JSON).unwrap();
        assert_eq!(team.acting_member("pruner@orchard").unwrap().name, "pruner");
        assert_eq!(
            team.acting_member("grafter@elsewhere").unwrap().name,
            "grafter"
        );
        assert_eq!(team.acting_member("Pruner").unwrap().name, "pruner");
        assert_eq!(
            team.acting_member("team-lead@orchard").unwrap().name,
            LEAD_NAME
        );
        assert!(team.acting_member("a1b2c3").is_none());
        assert!(team.acting_member(" ").is_none());
        let lead = TeamMembership {
            team: "orchard".into(),
            role: TeamRole::Lead,
            name: LEAD_NAME.into(),
            lead_name: LEAD_NAME.into(),
            colour: None,
            agent_id: Some("team-lead@orchard".into()),
        };
        assert_eq!(
            lead.describe_acting(Some("pruner@orchard"), Some(&team)),
            "pruner, teammate of team-lead in orchard"
        );
        assert_eq!(lead.describe_acting(None, Some(&team)), "lead of orchard");
        assert_eq!(
            lead.describe_acting(Some("team-lead@orchard"), Some(&team)),
            "lead of orchard"
        );
        assert_eq!(
            lead.describe_acting(Some("a1b2c3"), Some(&team)),
            "lead of orchard, as its agent a1b2c3"
        );
        assert_eq!(
            lead.describe_acting(Some("pruner@orchard"), None),
            "lead of orchard, as its agent pruner@orchard"
        );
        let home = Path::new("/home/p");
        assert_eq!(
            config_dir(Some(" /x/claude-work "), Some(home)),
            Some(PathBuf::from("/x/claude-work")),
            "the variable wins, trimmed"
        );
        assert_eq!(
            config_dir(Some("  "), Some(home)),
            Some(PathBuf::from("/home/p/.claude")),
            "a blank variable is unset: the home's directory"
        );
        assert_eq!(
            config_dir(None, Some(home)),
            Some(PathBuf::from("/home/p/.claude"))
        );
        assert_eq!(config_dir(None, None), None);
    }

    /// Only another member of the agent's own team owns a lease apart
    /// from its session: a subagent's id, the agent itself, or any actor
    /// with no team read, is the session.
    #[test]
    fn only_another_member_of_the_team_owns_a_lease_apart_from_its_session() {
        let team = Team::from_team_file("orchard", TEAM_FILE_JSON).unwrap();
        let lead = membership(
            &SessionFacts {
                session_id: Some("0123456789abcdef0123"),
                ..Default::default()
            },
            std::slice::from_ref(&team),
        )
        .unwrap();
        assert_eq!(
            lead.owner_actor(Some("pruner@orchard"), Some(&team))
                .as_deref(),
            Some("pruner@orchard")
        );
        assert_eq!(
            lead.owner_actor(Some("Pruner"), Some(&team)).as_deref(),
            Some("pruner@orchard"),
            "keyed by the member's id however the actor spelt it"
        );
        assert_eq!(
            lead.owner_actor(Some("a1b2c3d4e5"), Some(&team)),
            None,
            "a subagent"
        );
        assert_eq!(
            lead.owner_actor(Some("team-lead@orchard"), Some(&team)),
            None,
            "itself"
        );
        assert_eq!(lead.owner_actor(None, Some(&team)), None);
        assert_eq!(
            lead.owner_actor(Some("pruner@orchard"), None),
            None,
            "no team read"
        );
        // A teammate in its own process reporting its own id is itself.
        let line: Vec<String> = ["claude", "--agent-name", "pruner", "--team-name", "orchard"]
            .map(str::to_owned)
            .to_vec();
        let pruner = membership(
            &SessionFacts {
                argv: Some(&line),
                ..Default::default()
            },
            std::slice::from_ref(&team),
        )
        .unwrap();
        assert_eq!(
            pruner.owner_actor(Some("pruner@orchard"), Some(&team)),
            None
        );
        assert_eq!(
            pruner
                .owner_actor(Some("grafter@orchard"), Some(&team))
                .as_deref(),
            Some("grafter@orchard")
        );
    }

    #[test]
    fn membership_round_trips_through_json_with_absent_fields() {
        let membership = TeamMembership {
            team: "orchard".into(),
            role: TeamRole::Teammate,
            name: "pruner".into(),
            lead_name: LEAD_NAME.into(),
            colour: None,
            agent_id: None,
        };
        let json = serde_json::to_value(&membership).unwrap();
        assert_eq!(json["role"], "teammate");
        assert!(json.get("colour").is_none());
        let back: TeamMembership = serde_json::from_value(json).unwrap();
        assert_eq!(back, membership);
    }
}
