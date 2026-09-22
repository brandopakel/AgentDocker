//! What a session is called on screen.
//!
//! Adapters register sessions as `<runtime>-<pid or session id>`, which
//! nobody wants to read, and eight hex digits of an id told three Claude
//! sessions apart only to a machine. A generated name is shown as the tool
//! and a word the session's id picks from a fixed list — *Claude Code ·
//! Otter*, *Codex · Heron* — with the id's first four characters after the
//! word only when another session of that tool in the project drew the same
//! word. The id is the session's for good, so the name does not change
//! while the session lives or after: a peer ending renames
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

/// How much of an id tells two sessions with one word apart.
const TIE_CHARS: usize = 4;

/// The words a generated name draws from: short, distinct when spoken,
/// none a word that already means something here (no "agent", "task").
const WORDS: [&str; 96] = [
    "Otter", "Heron", "Falcon", "Badger", "Lynx", "Marten", "Puffin", "Ibis", "Wren", "Finch",
    "Robin", "Swift", "Kestrel", "Osprey", "Egret", "Crane", "Plover", "Tern", "Gannet", "Petrel",
    "Raven", "Magpie", "Jay", "Lark", "Starling", "Sparrow", "Owl", "Kite", "Hawk", "Eagle",
    "Condor", "Pelican", "Stork", "Toucan", "Parrot", "Macaw", "Dove", "Quail", "Grouse",
    "Pheasant", "Fox", "Wolf", "Bear", "Moose", "Elk", "Bison", "Yak", "Ibex", "Gazelle", "Impala",
    "Zebra", "Giraffe", "Okapi", "Tapir", "Panda", "Koala", "Wombat", "Lemur", "Gibbon", "Tamarin",
    "Beaver", "Hare", "Rabbit", "Squirrel", "Chipmunk", "Hedgehog", "Mole", "Shrew", "Vole",
    "Ferret", "Stoat", "Weasel", "Mink", "Seal", "Walrus", "Dolphin", "Orca", "Narwhal", "Beluga",
    "Manatee", "Turtle", "Tortoise", "Gecko", "Iguana", "Newt", "Salmon", "Trout", "Pike",
    "Marlin", "Tuna", "Octopus", "Squid", "Crab", "Lobster", "Starfish", "Coral",
];

/// The word an id draws: FNV-1a over the id's bytes, so it is the same on
/// every machine, in every build, for as long as the list is.
pub(crate) fn word_for(id: &str) -> &'static str {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in id.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    WORDS[(hash % WORDS.len() as u64) as usize]
}

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
        let me = self.canonical(agent.id.as_str());
        let word = word_for(me);
        let rivals = self.same_word(agent, word);
        if rivals.is_empty() {
            return format!("{tool} · {word}");
        }
        // The shortest prefix of the id, from four characters, that no
        // other session with this word shares.
        let length = (TIE_CHARS..=me.chars().count())
            .find(|&n| {
                let mine: String = me.chars().take(n).collect();
                rivals
                    .iter()
                    .all(|theirs| theirs.chars().take(n).collect::<String>() != mine)
            })
            .unwrap_or(me.chars().count());
        let tie: String = me.chars().take(length).collect();
        format!("{tool} · {word} {tie}")
    }

    /// The ids of the other sessions of this tool in the project that drew
    /// the same word, each identity once.
    fn same_word(&self, agent: &AgentRecord, word: &str) -> Vec<&'a str> {
        let me = self.canonical(agent.id.as_str());
        let project = agent.project.as_ref().map(|p| p.id());
        let mut ids: Vec<&'a str> = self
            .agents
            .iter()
            .filter(|a| {
                a.name_is_generated()
                    && a.spec.runtime == agent.spec.runtime
                    && a.project.as_ref().map(|p| p.id()) == project
            })
            .map(|a| self.canonical(a.id.as_str()))
            .filter(|theirs| *theirs != me && word_for(theirs) == word)
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
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
    #[test]
    fn a_generated_word_is_stable_and_a_shared_word_is_told_apart() {
        assert_eq!(
            word_for("0180d7615186449087095d7aa15ec0bb"),
            word_for("0180d7615186449087095d7aa15ec0bb")
        );
        // Find two ids that draw the same word and check the tie-break.
        let base = word_for("tie-0");
        let other = (1..10_000)
            .map(|n| format!("tie-{n}"))
            .find(|id| word_for(id) == base)
            .expect("a collision within ten thousand ids");
        let a = record("codex-1", "codex", "tie-0", "/p/a");
        let b = record("codex-2", "codex", &other, "/p/a");
        let aliases = BTreeMap::new();
        let agents = vec![a.clone(), b.clone()];
        let names = Naming::new(&agents, &aliases);
        assert!(
            names
                .display(&a)
                .starts_with(&format!("Codex · {base} tie-"))
        );
        assert_ne!(names.display(&a), names.display(&b));
    }

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
        assert_eq!(
            names.display(&first),
            format!("Codex · {}", word_for(first.id.as_str()))
        );
        assert_eq!(
            names.display(&second),
            format!("Codex · {}", word_for(second.id.as_str()))
        );
        assert_eq!(names.context(&second).as_deref(), Some("on feature/x"));
        // The second switches branch, the first ends: the same two names.
        on(&mut second, "main");
        first.status = AgentStatus::Exited { code: Some(0) };
        first.created_at += Duration::seconds(5);
        let agents = vec![second.clone(), first.clone()];
        let names = Naming::new(&agents, &aliases);
        assert_eq!(
            names.display(&first),
            format!("Codex · {}", word_for(first.id.as_str()))
        );
        assert_eq!(
            names.display(&second),
            format!("Codex · {}", word_for(second.id.as_str()))
        );
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
        assert_eq!(
            names.display(&claude),
            format!("Claude Code · {}", word_for(claude.id.as_str()))
        );
        assert_eq!(
            names.display(&elsewhere),
            format!("Codex · {}", word_for(elsewhere.id.as_str()))
        );
        assert_eq!(
            names.display(&first),
            format!("Codex · {}", word_for(first.id.as_str())),
            "alone in its project again, and the same name"
        );
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
            format!("Codex · {}", word_for("dea064bbfcfd4617a6280bf52465f1ce")),
            "a record the list does not hold still has its word"
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
        let current_name = format!("Claude Code · {}", word_for("id-current"));
        assert_eq!(
            names.display(&former),
            current_name,
            "a former id shows as its current one"
        );
        assert_eq!(names.display(&current), current_name);
        assert!(names.folded(&former));
        assert!(!names.folded(&current));
        assert_eq!(
            names.record("id-former").map(|a| a.id.as_str()),
            Some("id-current")
        );
        assert_eq!(names.canonical("id-former"), "id-current");
        // Two records, one generated name and no alias: two names, each
        // its own id's word (with a tie-break if they drew the same one).
        assert_ne!(names.display(&twin_a), names.display(&twin_b));
        assert!(names.display(&twin_a).starts_with("Claude Code · "));
        assert!(!names.folded(&twin_a) && !names.folded(&twin_b));
        // Only the former id and the current one on the list: one identity,
        // so one name.
        let agents = vec![former.clone(), current.clone()];
        let names = Naming::new(&agents, &aliases);
        assert!(names.display(&current).starts_with("Claude Code · "));
        assert_eq!(names.display(&former), names.display(&current));
    }
}
