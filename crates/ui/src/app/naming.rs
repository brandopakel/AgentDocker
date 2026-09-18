//! What a session is called on screen.
//!
//! Adapters register sessions as `<runtime>-<pid or session id>`, which
//! nobody wants to read. A generated name is shown as the tool — *Claude
//! Code*, *Codex* — and, once the project holds more than one session of
//! that tool, with the first eight characters of the session's id: *Claude
//! Code · 0180d761*. The id is the session's for good, so the name does
//! not change while the session lives or after: a peer ending renames
//! nobody, a branch switch renames nothing — the branch is a fact about
//! the checkout, shown under the name where a row has room for it, never
//! part of the name — and pruning old records can only shorten a name
//! back to the tool, never give a session another's. A name somebody chose
//! is shown as chosen, whatever it looks like, and is the way to make a
//! session's name a word.
//!
//! Records that stand for one identity read as one agent: a record whose
//! id is a former id of another (the daemon's alias table says so, after a
//! reconciliation) shows under the id it stands for now and is `folded`,
//! so a list of agents shows that one and not its shadow. Nothing is folded
//! on the strength of a name alone: two records with the same generated
//! name and no alias between them are two records.

use std::collections::BTreeMap;

use agentdocker_core::AgentRecord;

use super::runtime_label;

/// How much of an id a name carries.
const ID_CHARS: usize = 8;

/// The names of one snapshot of records: built from the list the daemon
/// gave and its alias table, consulted for one name at a time.
pub(crate) struct Naming<'a> {
    agents: &'a [AgentRecord],
    /// Former id → the id it now stands for.
    aliases: &'a BTreeMap<String, String>,
}

impl<'a> Naming<'a> {
    pub fn new(agents: &'a [AgentRecord], aliases: &'a BTreeMap<String, String>) -> Self {
        Self { agents, aliases }
    }

    /// The id a record is known by now, following an alias.
    pub fn canonical(&self, id: &'a str) -> &'a str {
        self.aliases.get(id).map(String::as_str).unwrap_or(id)
    }

    /// The record that stands for an id: the canonical one, or, when only
    /// a former id is on the list, that.
    pub fn record(&self, id: &str) -> Option<&'a AgentRecord> {
        let canonical = self.canonical(id);
        self.agents
            .iter()
            .find(|a| a.id.as_str() == canonical)
            .or_else(|| self.agents.iter().find(|a| a.id.as_str() == id))
    }

    /// The name a person reads for a record.
    pub fn display(&self, agent: &AgentRecord) -> String {
        if !agent.name_is_generated() {
            return agent.spec.name.clone();
        }
        let tool = runtime_label(&agent.spec.runtime);
        if self.company(agent) {
            format!("{tool} · {}", self.short_id(agent))
        } else {
            tool
        }
    }

    /// The first characters of the id a record is known by now.
    pub fn short_id(&self, agent: &AgentRecord) -> String {
        self.canonical(agent.id.as_str())
            .chars()
            .take(ID_CHARS)
            .collect()
    }

    /// Whether the project holds another session of this record's tool,
    /// counting each identity once, so the tool's name alone would not
    /// say which.
    fn company(&self, agent: &AgentRecord) -> bool {
        let me = self.canonical(agent.id.as_str());
        let project = agent.project.as_ref().map(|p| p.id());
        self.agents.iter().any(|a| {
            a.name_is_generated()
                && a.spec.runtime == agent.spec.runtime
                && a.project.as_ref().map(|p| p.id()) == project
                && self.canonical(a.id.as_str()) != me
        })
    }

    /// Whether a record is a former id of another: shown under that one,
    /// not listed as an agent of its own.
    pub fn folded(&self, agent: &AgentRecord) -> bool {
        self.aliases.contains_key(agent.id.as_str())
    }

    /// The line under a name, when the checkout says where the session
    /// works: `on main`.
    pub fn context(&self, agent: &AgentRecord) -> Option<String> {
        agent
            .vcs
            .as_ref()
            .and_then(|v| v.branch.as_deref())
            .map(|branch| format!("on {branch}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::{AgentId, AgentStatus, VcsState};
    use chrono::{Duration, Utc};

    fn record(name: &str, runtime: &str, id: &str, project: &str) -> AgentRecord {
        let mut record = super::super::tests::record(name, runtime, None);
        record.id = AgentId::from(id.to_owned());
        record.created_at = Utc::now();
        record.spec.labels.insert(
            agentdocker_core::agent::NAME_LABEL.to_owned(),
            agentdocker_core::agent::GENERATED_NAME.to_owned(),
        );
        record.project = Some(agentdocker_core::ProjectRef {
            root: project.into(),
            worktree: None,
            fingerprint: None,
            source: agentdocker_core::ProjectSource::Directory,
        });
        record
    }

    fn on(record: &mut AgentRecord, branch: &str) {
        record.vcs = Some(VcsState {
            branch: Some(branch.into()),
            head: None,
            dirty: None,
            updated_at: Utc::now(),
        });
    }

    /// A name is the tool, with the session's own id once the project
    /// holds another session of that tool: nothing renames it — not a peer
    /// ending, not a branch switch — and a lone session is just the tool.
    #[test]
    fn names_are_the_tool_and_the_sessions_own_id_and_never_the_branch() {
        let aliases = BTreeMap::new();
        let mut first = record(
            "codex-5124",
            "codex",
            "0180d7615186449087095d7aa15ec0bb",
            "/p/a",
        );
        on(&mut first, "main");
        let mut second = record(
            "codex-6250",
            "codex",
            "ac5c138c2b3f4d8f90c5924988419055",
            "/p/a",
        );
        on(&mut second, "feature/x");
        let agents = vec![second.clone(), first.clone()];
        let names = Naming::new(&agents, &aliases);
        assert_eq!(names.display(&first), "Codex · 0180d761");
        assert_eq!(names.display(&second), "Codex · ac5c138c");
        assert_eq!(names.context(&second).as_deref(), Some("on feature/x"));
        // The second switches branch, the first ends: the same two names.
        on(&mut second, "main");
        first.status = AgentStatus::Exited { code: Some(0) };
        first.created_at += Duration::seconds(5);
        let agents = vec![second.clone(), first.clone()];
        let names = Naming::new(&agents, &aliases);
        assert_eq!(names.display(&first), "Codex · 0180d761");
        assert_eq!(names.display(&second), "Codex · ac5c138c");
        // Another tool, or another project, is company for nobody here.
        let claude = record(
            "claude-code-1",
            "claude-code",
            "c23201244b2b4a198029b2498d0a2590",
            "/p/a",
        );
        let elsewhere = record(
            "codex-8000",
            "codex",
            "cdadfe227a29483fa4b598a207395b6e",
            "/p/b",
        );
        let agents = vec![claude.clone(), elsewhere.clone(), first.clone()];
        let names = Naming::new(&agents, &aliases);
        assert_eq!(names.display(&claude), "Claude Code");
        assert_eq!(names.display(&elsewhere), "Codex");
        assert_eq!(names.display(&first), "Codex", "alone in its project again");
        // A record the list does not hold reads as its tool; a chosen name
        // as chosen.
        let stranger = record(
            "codex-9",
            "codex",
            "dea064bbfcfd4617a6280bf52465f1ce",
            "/p/a",
        );
        assert_eq!(
            names.display(&stranger),
            "Codex · dea064bb",
            "the list holds a peer"
        );
        let mut chosen = record(
            "reviewer",
            "codex",
            "f083e541ef194373a41542ce03576470",
            "/p/a",
        );
        chosen
            .spec
            .labels
            .remove(agentdocker_core::agent::NAME_LABEL);
        assert!(!chosen.name_is_generated());
        assert_eq!(names.display(&chosen), "reviewer");
    }

    /// A former id is the same session: it reads under the id it stands
    /// for now, counts once, and is folded; a record that merely shares a
    /// generated name is not, since a name is no evidence.
    #[test]
    fn a_former_id_folds_into_its_session_and_a_shared_name_does_not() {
        let mut former = record("claude-code-9", "claude-code", "id-former", "/p/a");
        former.status = AgentStatus::Exited { code: Some(0) };
        let current = record("claude-code-9", "claude-code", "id-current", "/p/a");
        let twin_a = record("claude-2c79ae10", "claude-code", "id-twin-a", "/p/a");
        let twin_b = record("claude-2c79ae10", "claude-code", "id-twin-b", "/p/a");
        let aliases = BTreeMap::from([("id-former".to_owned(), "id-current".to_owned())]);
        let agents = vec![
            former.clone(),
            current.clone(),
            twin_a.clone(),
            twin_b.clone(),
        ];
        let names = Naming::new(&agents, &aliases);
        assert_eq!(names.display(&former), "Claude Code · id-curre");
        assert_eq!(names.display(&current), "Claude Code · id-curre");
        assert!(names.folded(&former));
        assert!(!names.folded(&current));
        assert_eq!(
            names.record("id-former").map(|a| a.id.as_str()),
            Some("id-current")
        );
        assert_eq!(names.canonical("id-former"), "id-current");
        assert_eq!(names.display(&twin_a), "Claude Code · id-twin-");
        assert_eq!(names.display(&twin_b), "Claude Code · id-twin-");
        assert!(!names.folded(&twin_a) && !names.folded(&twin_b));
        // Only the former id and the current one on the list: one identity,
        // so the tool alone.
        let agents = vec![former.clone(), current.clone()];
        let names = Naming::new(&agents, &aliases);
        assert_eq!(names.display(&current), "Claude Code");
        assert_eq!(names.display(&former), "Claude Code");
    }
}
