//! A project's crew: the sessions its team is made of.
//!
//! A person sees a *team*: a writer on one vendor, a reviewer on another,
//! started with one press. The code says *crew*, because `Team` is already
//! Claude Code's own agent teams ([`crate::team`]) and an Agentfile's
//! `team` label is a third thing.
//!
//! This module is the description only: who the members are, the default
//! the daemon offers when a project has none, and the rules an edit has to
//! keep. Starting a member, and what a member's session inherits, are not
//! here.
//!
//! A member has an id the daemon makes and never reuses, apart from the
//! name a person reads. Taking `writer` out and adding another `writer`
//! makes a new member: nothing of the old one follows the name.

pub mod earlier;
pub mod seat;

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ProjectId;

/// The most members a crew has.
pub const MEMBERS: usize = 8;
/// The longest a member's first words are.
pub const WORDS_CHARS: usize = 2000;

/// The runtimes a member can run on: the ones a session of which can be
/// told what to do while it sits idle. A member that could not be told its
/// first words would start and wait.
pub const SUPPORTED: [&str; 2] = ["claude-code", "codex"];

/// A member's identity within its project's crew.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MemberId(String);

impl MemberId {
    pub fn generate() -> Self {
        Self(uuid::Uuid::new_v4().simple().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Abbreviated, as an agent's id is.
    pub fn short(&self) -> &str {
        crate::agent::short(&self.0)
    }
}

impl From<&str> for MemberId {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

impl std::fmt::Display for MemberId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One member of a crew.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    pub id: MemberId,
    /// What a person calls it: `writer`, `reviewer`.
    pub name: String,
    pub runtime: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// Roles of the project's palette its session is given.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<String>,
    /// Its session works in a worktree of its own.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub own_worktree: bool,
    /// A plan its first words name, when the person has named one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    /// What its session is told first, in place of the default for its
    /// roles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_words: Option<String>,
}

/// A member as a person gives it. With an `id` it is a member the crew
/// already has, kept as that member whatever else changes; without one it
/// is new.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<MemberId>,
    pub name: String,
    pub runtime: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub own_worktree: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_words: Option<String>,
}

impl From<&Member> for MemberInput {
    fn from(member: &Member) -> Self {
        Self {
            id: Some(member.id.clone()),
            name: member.name.clone(),
            runtime: member.runtime.clone(),
            model: member.model.clone(),
            effort: member.effort.clone(),
            roles: member.roles.clone(),
            own_worktree: member.own_worktree,
            plan: member.plan.clone(),
            first_words: member.first_words.clone(),
        }
    }
}

/// A project's saved crew. `revision` counts its saves from one; an edit
/// names the revision it was made against.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Crew {
    pub project: ProjectId,
    pub revision: u64,
    pub members: Vec<Member>,
    pub updated_by: String,
    pub updated_at: DateTime<Utc>,
}

/// A runtime that is installed and can be a member's, with its vendor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Installed {
    pub runtime: String,
    pub vendor: String,
}

/// One member of a crew the daemon offers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proposed {
    pub name: String,
    pub runtime: String,
    pub vendor: String,
    pub roles: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub own_worktree: bool,
}

/// The crew the daemon offers a project that has saved none, from what is
/// installed. Nothing is saved until a person accepts it, by the key it
/// was shown under ([`Proposal::key_for`]): so what is saved is what they
/// were shown, where they were shown it. A runtime installed or lost in
/// between is a different proposal, and so is another project's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proposal {
    /// The members' part of the key: who, on what, holding what.
    pub key: String,
    pub members: Vec<Proposed>,
    /// Both members run on one vendor's runtime. It says which runtime,
    /// not which model: a runtime can be pointed at another's model.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub same_vendor: bool,
}

/// Why no crew can be proposed.
pub const NOTHING_TO_PROPOSE: &str =
    "no runtime that can be told what to do while idle is installed; install Claude Code or Codex";

/// The default crew for what is installed: a writer holding `implementer`
/// on one vendor and a reviewer holding `reviewer` on another. Claude Code
/// writes where it is installed. With one usable runtime both are on it,
/// and the proposal says so. `installed` is every installed runtime that
/// is one of [`SUPPORTED`], in the inventory's order; a runtime that is
/// neither is not the caller's to pass, and never vetoes these.
pub fn propose(installed: &[Installed]) -> Result<Proposal, &'static str> {
    let usable: Vec<&Installed> = installed
        .iter()
        .filter(|i| SUPPORTED.contains(&i.runtime.as_str()))
        .collect();
    let writer = usable
        .iter()
        .find(|i| i.runtime == "claude-code")
        .or_else(|| usable.first())
        .copied()
        .ok_or(NOTHING_TO_PROPOSE)?;
    let reviewer = usable
        .iter()
        .find(|i| i.vendor != writer.vendor)
        .copied()
        .unwrap_or(writer);
    let members = vec![
        Proposed {
            name: "writer".into(),
            runtime: writer.runtime.clone(),
            vendor: writer.vendor.clone(),
            roles: vec!["implementer".into()],
            own_worktree: true,
        },
        Proposed {
            name: "reviewer".into(),
            runtime: reviewer.runtime.clone(),
            vendor: reviewer.vendor.clone(),
            roles: vec!["reviewer".into()],
            own_worktree: false,
        },
    ];
    Ok(Proposal {
        key: proposal_key(&members),
        same_vendor: writer.vendor == reviewer.vendor,
        members,
    })
}

/// A proposal's name: its members, each with its runtime and roles, in
/// words a person can read back.
fn proposal_key(members: &[Proposed]) -> String {
    members
        .iter()
        .map(|m| format!("{}={}:{}", m.name, m.runtime, m.roles.join("+")))
        .collect::<Vec<_>>()
        .join(",")
}

impl Proposal {
    /// The key this proposal is shown under for `project`: its members,
    /// then the project it is for. Two projects on one machine are
    /// proposed the same members, and a key that named the members alone
    /// would accept one project's proposal as another's.
    pub fn key_for(&self, project: &ProjectId) -> String {
        format!("{}@{}", self.key, project.as_str())
    }

    /// The proposal as the members a save takes.
    pub fn inputs(&self) -> Vec<MemberInput> {
        self.members
            .iter()
            .map(|m| MemberInput {
                id: None,
                name: m.name.clone(),
                runtime: m.runtime.clone(),
                roles: m.roles.clone(),
                own_worktree: m.own_worktree,
                ..MemberInput::default()
            })
            .collect()
    }
}

/// Why `given` is not the key of the proposal a project has `now`, in
/// words: it was shown for another project, or what is installed has
/// changed since it was shown.
pub fn not_the_proposal(now: &str, given: &str) -> String {
    let project = |key: &str| key.rsplit_once('@').map(|(_, project)| project.to_owned());
    match (project(now), project(given)) {
        (Some(this), Some(other)) if this != other => format!(
            "that team was proposed for another project (`{other}`), not this one (`{this}`): nothing is saved here; read this project's team and accept the one it shows"
        ),
        _ => format!(
            "what is installed has changed since that team was proposed: the proposal is now `{now}`; read it again"
        ),
    }
}

/// Why a crew was not saved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The edit was made against another revision than the one saved now.
    /// `saved` is the revision there is, zero when none is saved.
    Stale { expected: u64, saved: u64 },
    /// The members are not ones a crew can have, in words for the person.
    Invalid(String),
}

impl Refused {
    pub fn words(&self) -> String {
        match self {
            Refused::Stale { expected, saved: 0 } => format!(
                "the team was edited against revision {expected}, and none is saved; read it again"
            ),
            Refused::Stale { expected, saved } => format!(
                "the team was edited against revision {expected}, and revision {saved} is saved now; read it again"
            ),
            Refused::Invalid(why) => why.clone(),
        }
    }
}

fn invalid(why: impl Into<String>) -> Refused {
    Refused::Invalid(why.into())
}

/// Whether a name is one a member can have: what a role's address is, so
/// that it can be typed in a command without quoting.
fn check_name(name: &str) -> Result<(), Refused> {
    if crate::role::slug_from(name).as_deref() == Some(name) {
        Ok(())
    } else {
        Err(invalid(format!(
            "a member's name is lower-case letters, digits and hyphens, beginning with a letter or a digit: `{name}` is not"
        )))
    }
}

/// Who saves a crew, and when.
#[derive(Clone, Copy, Debug)]
pub struct Saving<'a> {
    pub by: &'a str,
    pub at: DateTime<Utc>,
}

/// The crew that saving `inputs` makes, or why it is not saved.
///
/// `expected` is the revision the edit was made against, zero when none
/// was saved: an edit against another revision is refused, so a stale
/// editor cannot bring back a member someone took out. A member given
/// with an id is the member the saved crew has under it; one without is
/// new, with the id `new_id` makes. `palette` is the project's roles as
/// they are now: a role that is not among them is not saved.
pub fn save(
    project: &ProjectId,
    current: Option<&Crew>,
    expected: u64,
    inputs: Vec<MemberInput>,
    palette: &BTreeSet<String>,
    saving: Saving<'_>,
    mut new_id: impl FnMut() -> MemberId,
) -> Result<Crew, Refused> {
    let saved = current.map_or(0, |crew| crew.revision);
    if expected != saved {
        return Err(Refused::Stale { expected, saved });
    }
    if inputs.len() > MEMBERS {
        return Err(invalid(format!("a team has at most {MEMBERS} members")));
    }
    let (mut names, mut ids) = (BTreeSet::new(), BTreeSet::new());
    let mut members = Vec::with_capacity(inputs.len());
    for input in inputs {
        check_name(&input.name)?;
        if !names.insert(input.name.clone()) {
            return Err(invalid(format!("two members are called `{}`", input.name)));
        }
        if !SUPPORTED.contains(&input.runtime.as_str()) {
            return Err(invalid(format!(
                "`{}` runs on `{}`, which cannot be told what to do while idle; a member runs on {}",
                input.name,
                input.runtime,
                SUPPORTED.join(" or ")
            )));
        }
        let mut roles = BTreeSet::new();
        for role in &input.roles {
            if !palette.contains(role) {
                return Err(invalid(format!(
                    "the project has no role called `{role}`; its roles are {}",
                    palette.iter().cloned().collect::<Vec<_>>().join(", ")
                )));
            }
            roles.insert(role.clone());
        }
        for (what, value) in [
            ("model", &input.model),
            ("effort", &input.effort),
            ("plan", &input.plan),
        ] {
            if value.as_deref().is_some_and(|v| {
                v.trim().is_empty() || v.chars().count() > 128 || v.chars().any(char::is_control)
            }) {
                return Err(invalid(format!(
                    "`{}`: its {what} is one line of at most 128 characters",
                    input.name
                )));
            }
        }
        if let Some(words) = &input.first_words {
            if words.trim().is_empty()
                || words.chars().count() > WORDS_CHARS
                || words.chars().any(|c| c.is_control() && c != '\n')
            {
                return Err(invalid(format!(
                    "`{}`: its first words are text of at most {WORDS_CHARS} characters",
                    input.name
                )));
            }
        }
        let id = match input.id {
            Some(id) => {
                if !current.is_some_and(|crew| crew.members.iter().any(|m| m.id == id)) {
                    return Err(invalid(format!(
                        "the team has no member with the id {}",
                        id.short()
                    )));
                }
                id
            }
            None => new_id(),
        };
        if !ids.insert(id.clone()) {
            return Err(invalid(format!("the member {} is given twice", id.short())));
        }
        members.push(Member {
            id,
            name: input.name,
            runtime: input.runtime,
            model: input.model,
            effort: input.effort,
            roles: roles.into_iter().collect(),
            own_worktree: input.own_worktree,
            plan: input.plan,
            first_words: input.first_words,
        });
    }
    Ok(Crew {
        project: project.clone(),
        revision: saved + 1,
        members,
        updated_by: saving.by.to_owned(),
        updated_at: saving.at,
    })
}

/// Where a member's session stands, as one word a reader matches on
/// ([`seat::standing`] works it out from the member's seat).
///
/// A standing a reader does not know, from a newer daemon, or one that is
/// missing, is `Unknown`: it is shown as unknown, never as not started.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Standing {
    NotStarted,
    /// A start is under way: reserved, or anywhere short of complete.
    Starting,
    /// Running as its team's member, with nothing owed.
    Running,
    /// Running and told its first words, with something of an earlier
    /// session's still owed that the person has to see to.
    NeedsAttention,
    Stopping,
    /// Its last start failed, at the stage the read says.
    Failed,
    /// A start is asked for and waits for the one before it to settle.
    Waiting,
    /// Whose run its session is could not be established.
    Uncertain,
    /// The daemon is taking over from another and may not write yet.
    Recovering,
    #[default]
    #[serde(other)]
    Unknown,
}

impl Standing {
    pub fn word(self) -> &'static str {
        match self {
            Standing::NotStarted => "not started",
            Standing::Starting => "starting",
            Standing::Running => "running",
            Standing::NeedsAttention => "needs attention",
            Standing::Stopping => "stopping",
            Standing::Failed => "failed",
            Standing::Waiting => "waiting on the start before it",
            Standing::Uncertain => "not confirmed",
            Standing::Recovering => "recovering",
            Standing::Unknown => "unknown",
        }
    }
}

/// A member as a reader is shown it: what it is, whether its runtime is
/// installed here, and where its session stands.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberView {
    /// Absent for a member of a proposal: it has none until it is saved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<MemberId>,
    pub name: String,
    pub runtime: String,
    /// The runtime's vendor, when the inventory knows the runtime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor: Option<String>,
    /// The runtime's command-line tool was found on this machine.
    pub installed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub own_worktree: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_words: Option<String>,
    #[serde(default)]
    pub standing: Standing,
}

impl MemberView {
    /// The member as an edit gives it back: with its id, so that it stays
    /// the member it is, and with everything it was saved with.
    pub fn input(&self) -> MemberInput {
        MemberInput {
            id: self.id.clone(),
            name: self.name.clone(),
            runtime: self.runtime.clone(),
            model: self.model.clone(),
            effort: self.effort.clone(),
            roles: self.roles.clone(),
            own_worktree: self.own_worktree,
            plan: self.plan.clone(),
            first_words: self.first_words.clone(),
        }
    }
}

/// A project's team as a reader is shown it: the saved crew, or the
/// proposal when none is saved, or why none can be proposed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrewView {
    pub project: ProjectId,
    /// The saved crew's revision; zero when none is saved.
    pub revision: u64,
    /// The proposal's key, when what is shown is a proposal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal: Option<String>,
    /// Why there is nothing to show: no crew is saved and none can be
    /// proposed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
    pub members: Vec<MemberView>,
    /// Two members or more, and every one on the same vendor's runtime.
    /// It says which runtime each runs on, not which vendor's model did
    /// any piece of work.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub same_vendor: bool,
}

/// What the inventory says of one runtime.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Known {
    pub runtime: String,
    pub vendor: String,
    /// Its command-line tool was found on this machine.
    pub installed: bool,
}

/// A project's team as a reader is shown it: the saved crew when there is
/// one, and otherwise the proposal for what is installed, or why there is
/// none. A saved crew is shown as saved, whatever is installed now: a
/// member whose runtime has gone is shown not installed, and is not
/// swapped for another.
pub fn view(project: &ProjectId, saved: Option<&Crew>, known: &[Known]) -> CrewView {
    let of = |runtime: &str| known.iter().find(|k| k.runtime == runtime);
    let Some(crew) = saved else {
        let installed: Vec<Installed> = known
            .iter()
            .filter(|k| k.installed)
            .map(|k| Installed {
                runtime: k.runtime.clone(),
                vendor: k.vendor.clone(),
            })
            .collect();
        return match propose(&installed) {
            Ok(proposal) => CrewView {
                project: project.clone(),
                revision: 0,
                proposal: Some(proposal.key_for(project)),
                unavailable: None,
                same_vendor: proposal.same_vendor,
                members: proposal
                    .members
                    .into_iter()
                    .map(|m| MemberView {
                        id: None,
                        name: m.name,
                        runtime: m.runtime,
                        vendor: Some(m.vendor),
                        installed: true,
                        model: None,
                        effort: None,
                        roles: m.roles,
                        own_worktree: m.own_worktree,
                        plan: None,
                        first_words: None,
                        standing: Standing::NotStarted,
                    })
                    .collect(),
            },
            Err(why) => CrewView {
                project: project.clone(),
                revision: 0,
                proposal: None,
                unavailable: Some(why.to_owned()),
                same_vendor: false,
                members: Vec::new(),
            },
        };
    };
    let members: Vec<MemberView> = crew
        .members
        .iter()
        .map(|m| MemberView {
            id: Some(m.id.clone()),
            name: m.name.clone(),
            runtime: m.runtime.clone(),
            vendor: of(&m.runtime).map(|k| k.vendor.clone()),
            installed: of(&m.runtime).is_some_and(|k| k.installed),
            model: m.model.clone(),
            effort: m.effort.clone(),
            roles: m.roles.clone(),
            own_worktree: m.own_worktree,
            plan: m.plan.clone(),
            first_words: m.first_words.clone(),
            standing: Standing::NotStarted,
        })
        .collect();
    CrewView {
        project: project.clone(),
        revision: crew.revision,
        proposal: None,
        unavailable: None,
        same_vendor: same_vendor(members.iter().map(|m| m.vendor.as_deref())),
        members,
    }
}

/// Whether a crew of these vendors is a same-vendor team: two members or
/// more, each with a known vendor, and all of them the same. A member
/// whose vendor is not known is not claimed either way.
pub fn same_vendor<'a>(vendors: impl IntoIterator<Item = Option<&'a str>>) -> bool {
    let vendors: Vec<Option<&str>> = vendors.into_iter().collect();
    vendors.len() >= 2
        && vendors.iter().all(Option::is_some)
        && vendors.iter().collect::<BTreeSet<_>>().len() == 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn installed(pairs: &[(&str, &str)]) -> Vec<Installed> {
        pairs
            .iter()
            .map(|(runtime, vendor)| Installed {
                runtime: (*runtime).into(),
                vendor: (*vendor).into(),
            })
            .collect()
    }

    fn palette() -> BTreeSet<String> {
        ["lead", "implementer", "reviewer", "explorer", "tester"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    }

    fn member(name: &str, runtime: &str, roles: &[&str]) -> MemberInput {
        MemberInput {
            name: name.into(),
            runtime: runtime.into(),
            roles: roles.iter().map(|r| (*r).to_owned()).collect(),
            ..MemberInput::default()
        }
    }

    fn by() -> Saving<'static> {
        Saving {
            by: "user",
            at: "2026-01-02T03:04:05Z".parse().unwrap(),
        }
    }

    fn ids() -> impl FnMut() -> MemberId {
        let mut next = 0;
        move || {
            next += 1;
            MemberId::from(format!("member-{next}").as_str())
        }
    }

    /// The default crew is a writer on one vendor and a reviewer on
    /// another where two are installed, both on the one where one is, and
    /// nothing where none is. A runtime that is not one a member can run
    /// on changes none of it.
    #[test]
    fn the_proposal_follows_what_is_installed() {
        let both = propose(&installed(&[
            ("codex", "openai"),
            ("claude-code", "anthropic"),
            ("gemini-cli", "google"),
        ]))
        .unwrap();
        assert_eq!(
            both.members
                .iter()
                .map(|m| (m.name.as_str(), m.runtime.as_str(), m.roles.join("+")))
                .collect::<Vec<_>>(),
            [
                ("writer", "claude-code", "implementer".to_owned()),
                ("reviewer", "codex", "reviewer".to_owned()),
            ]
        );
        assert!(!both.same_vendor);
        assert!(both.members[0].own_worktree && !both.members[1].own_worktree);
        assert_eq!(
            both.key,
            "writer=claude-code:implementer,reviewer=codex:reviewer"
        );

        for (only, vendor) in [("claude-code", "anthropic"), ("codex", "openai")] {
            let one = propose(&installed(&[(only, vendor), ("cursor", "anysphere")])).unwrap();
            assert!(one.same_vendor, "{only}");
            assert!(one.members.iter().all(|m| m.runtime == only));
            assert_ne!(
                one.key, both.key,
                "a different proposal has a different key"
            );
        }

        for none in [
            &[][..],
            &[("gemini-cli", "google"), ("cursor", "anysphere")][..],
        ] {
            assert_eq!(propose(&installed(none)), Err(NOTHING_TO_PROPOSE));
        }
    }

    /// A first save makes revision one with ids of the daemon's making;
    /// an edit keeps the members it names by id, makes the others anew and
    /// counts the revision on.
    #[test]
    fn a_save_keeps_a_member_by_its_id_and_never_by_its_name() {
        let project = ProjectId::from("project");
        let proposal = propose(&installed(&[
            ("claude-code", "anthropic"),
            ("codex", "openai"),
        ]))
        .unwrap();
        let mut make = ids();
        let first = save(
            &project,
            None,
            0,
            proposal.inputs(),
            &palette(),
            by(),
            &mut make,
        )
        .unwrap();
        assert_eq!(first.revision, 1);
        assert_eq!(first.members[0].id, MemberId::from("member-1"));
        assert_eq!(first.members[1].id, MemberId::from("member-2"));
        assert_eq!(first.members[0].roles, ["implementer"]);

        // The reviewer is kept by its id under another name; the writer
        // is taken out, and another member called `writer` is added.
        let mut kept = MemberInput::from(&first.members[1]);
        kept.name = "checker".into();
        let second = save(
            &project,
            Some(&first),
            1,
            vec![kept, member("writer", "codex", &["implementer", "tester"])],
            &palette(),
            by(),
            &mut make,
        )
        .unwrap();
        assert_eq!(second.revision, 2);
        assert_eq!(second.members[0].id, first.members[1].id);
        assert_eq!(second.members[0].name, "checker");
        assert_eq!(second.members[1].name, "writer");
        assert_eq!(second.members[1].id, MemberId::from("member-3"));
        assert_ne!(
            second.members[1].id, first.members[0].id,
            "the name does not carry the old member"
        );
        assert_eq!(second.members[1].roles, ["implementer", "tester"]);
    }

    /// Each thing a save refuses, in the words the person is given.
    #[test]
    fn a_save_refuses_what_a_crew_cannot_be() {
        let project = ProjectId::from("project");
        let saved = save(
            &project,
            None,
            0,
            vec![member("writer", "claude-code", &["implementer"])],
            &palette(),
            by(),
            ids(),
        )
        .unwrap();
        let refused = |current: Option<&Crew>, expected: u64, inputs: Vec<MemberInput>| {
            save(&project, current, expected, inputs, &palette(), by(), ids()).unwrap_err()
        };

        // An edit against another revision, in both directions.
        assert_eq!(
            refused(Some(&saved), 0, vec![]),
            Refused::Stale {
                expected: 0,
                saved: 1
            }
        );
        assert_eq!(
            refused(None, 3, vec![]),
            Refused::Stale {
                expected: 3,
                saved: 0
            }
        );
        assert!(refused(None, 3, vec![]).words().contains("none is saved"));

        let words = |inputs: Vec<MemberInput>| refused(Some(&saved), 1, inputs).words();
        assert!(words(vec![member("Writer One", "codex", &[])]).contains("lower-case letters"));
        assert!(
            words(vec![
                member("writer", "codex", &[]),
                member("writer", "claude-code", &[])
            ])
            .contains("two members are called `writer`")
        );
        assert!(
            words(vec![member("writer", "gemini-cli", &[])])
                .contains("cannot be told what to do while idle")
        );
        assert!(
            words(vec![member("writer", "codex", &["review"])])
                .contains("the project has no role called `review`")
        );
        assert!(
            words(
                (0..=MEMBERS)
                    .map(|n| member(&format!("m{n}"), "codex", &[]))
                    .collect()
            )
            .contains("at most 8 members")
        );
        // An id the saved crew does not have, and one given twice.
        let mut stranger = member("writer", "codex", &[]);
        stranger.id = Some(MemberId::from("nobody"));
        assert!(words(vec![stranger]).contains("no member with the id"));
        let twice = MemberInput::from(&saved.members[0]);
        let mut again = twice.clone();
        again.name = "other".into();
        assert!(words(vec![twice, again]).contains("is given twice"));
        // First words that are not text.
        let mut wordy = member("writer", "codex", &[]);
        wordy.first_words = Some("  ".into());
        assert!(words(vec![wordy]).contains("first words"));

        // A crew may be emptied; what is refused is never half saved.
        let emptied = save(&project, Some(&saved), 1, vec![], &palette(), by(), ids()).unwrap();
        assert_eq!((emptied.revision, emptied.members.len()), (2, 0));
    }

    /// A same-vendor team is two members or more, all of one known
    /// vendor; a vendor that is not known claims nothing.
    #[test]
    fn a_same_vendor_team_is_said_only_when_it_is_known() {
        assert!(same_vendor([Some("anthropic"), Some("anthropic")]));
        assert!(!same_vendor([Some("anthropic"), Some("openai")]));
        assert!(!same_vendor([Some("anthropic")]));
        assert!(!same_vendor([Some("anthropic"), None]));
        assert!(!same_vendor(std::iter::empty()));
    }

    /// The stored and wire form: optional things absent when unset, and a
    /// member read back as itself.
    #[test]
    fn a_crew_round_trips_and_leaves_out_what_is_unset() {
        let crew = save(
            &ProjectId::from("project"),
            None,
            0,
            vec![member("writer", "claude-code", &["implementer"])],
            &palette(),
            by(),
            ids(),
        )
        .unwrap();
        let wire = serde_json::to_value(&crew).unwrap();
        let member = wire["members"][0].as_object().unwrap();
        let mut keys: Vec<_> = member.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["id", "name", "roles", "runtime"]);
        assert_eq!(serde_json::from_value::<Crew>(wire).unwrap(), crew);
        assert_eq!(
            serde_json::to_value(Standing::NotStarted).unwrap(),
            "not_started"
        );
        // A standing this build does not know, and one that is missing,
        // are unknown: never read as not started.
        assert_eq!(
            serde_json::from_value::<Standing>(serde_json::json!("a_later_builds_word")).unwrap(),
            Standing::Unknown
        );
        let mut member =
            serde_json::to_value(&view(&ProjectId::from("project"), Some(&crew), &[]).members[0])
                .unwrap();
        assert_eq!(member["standing"], "not_started");
        member.as_object_mut().unwrap().remove("standing");
        assert_eq!(
            serde_json::from_value::<MemberView>(member)
                .unwrap()
                .standing,
            Standing::Unknown
        );
        assert_eq!(Standing::Unknown.word(), "unknown");
    }

    fn known(triples: &[(&str, &str, bool)]) -> Vec<Known> {
        triples
            .iter()
            .map(|(runtime, vendor, installed)| Known {
                runtime: (*runtime).into(),
                vendor: (*vendor).into(),
                installed: *installed,
            })
            .collect()
    }

    /// What a reader is shown: a proposal while nothing is saved, the
    /// reason when nothing can be proposed, and a saved crew as it was
    /// saved whatever is installed now.
    #[test]
    fn a_reader_is_shown_the_saved_crew_or_the_proposal_or_why_neither() {
        let project = ProjectId::from("project");
        let both = known(&[
            ("claude-code", "anthropic", true),
            ("codex", "openai", true),
            ("gemini-cli", "google", true),
        ]);
        let proposed = view(&project, None, &both);
        assert_eq!(proposed.revision, 0);
        assert_eq!(
            proposed.proposal.as_deref(),
            Some("writer=claude-code:implementer,reviewer=codex:reviewer@project")
        );
        // The same members proposed to another project are another key:
        // what one project was shown is not what the other accepts.
        let elsewhere = view(&ProjectId::from("another"), None, &both);
        assert_eq!(
            elsewhere.proposal.as_deref(),
            Some("writer=claude-code:implementer,reviewer=codex:reviewer@another")
        );
        assert_ne!(proposed.proposal, elsewhere.proposal);
        let here = proposed.proposal.as_deref().unwrap();
        let there = elsewhere.proposal.as_deref().unwrap();
        let other_project = not_the_proposal(here, there);
        assert!(
            other_project
                .contains("proposed for another project (`another`), not this one (`project`)"),
            "{other_project}"
        );
        let changed = not_the_proposal(
            here,
            "writer=codex:implementer,reviewer=codex:reviewer@project",
        );
        assert!(
            changed.contains("what is installed has changed") && changed.contains(here),
            "{changed}"
        );
        // A key with no project in it, as an earlier build printed it.
        assert!(
            not_the_proposal(
                here,
                "writer=claude-code:implementer,reviewer=codex:reviewer"
            )
            .contains("what is installed has changed")
        );
        assert!(!proposed.same_vendor && proposed.unavailable.is_none());
        assert!(
            proposed
                .members
                .iter()
                .all(|m| m.id.is_none() && m.installed && m.standing == Standing::NotStarted)
        );

        // Known to the inventory and not installed is not installed.
        let none = view(
            &project,
            None,
            &known(&[
                ("claude-code", "anthropic", false),
                ("codex", "openai", false),
            ]),
        );
        assert_eq!(none.unavailable.as_deref(), Some(NOTHING_TO_PROPOSE));
        assert!(none.members.is_empty() && none.proposal.is_none());

        // Saved with two vendors; Codex is then uninstalled. The crew is
        // shown as saved: its reviewer is not installed, and nobody is
        // put in its place.
        let crew = save(
            &project,
            None,
            0,
            propose(&installed(&[
                ("claude-code", "anthropic"),
                ("codex", "openai"),
            ]))
            .unwrap()
            .inputs(),
            &palette(),
            by(),
            ids(),
        )
        .unwrap();
        let later = view(
            &project,
            Some(&crew),
            &known(&[
                ("claude-code", "anthropic", true),
                ("codex", "openai", false),
            ]),
        );
        assert_eq!((later.revision, later.proposal), (1, None));
        assert_eq!(
            later
                .members
                .iter()
                .map(|m| (m.name.as_str(), m.runtime.as_str(), m.installed))
                .collect::<Vec<_>>(),
            [
                ("writer", "claude-code", true),
                ("reviewer", "codex", false)
            ]
        );
        assert!(later.members.iter().all(|m| m.id.is_some()));
        assert!(!later.same_vendor);
        // An edit made from what a reader was shown keeps each member as
        // it was saved: its id and everything set on it.
        let mut detailed = MemberInput::from(&crew.members[0]);
        detailed.model = Some("a-model".into());
        detailed.effort = Some("high".into());
        detailed.plan = Some("plan-1".into());
        detailed.first_words = Some("Read the plan first.".into());
        let edited = save(
            &project,
            Some(&crew),
            1,
            vec![detailed, MemberInput::from(&crew.members[1])],
            &palette(),
            by(),
            ids(),
        )
        .unwrap();
        let shown = view(&project, Some(&edited), &both);
        let back: Vec<MemberInput> = shown.members.iter().map(MemberView::input).collect();
        assert_eq!(
            back,
            edited
                .members
                .iter()
                .map(MemberInput::from)
                .collect::<Vec<_>>()
        );
        // A runtime the inventory does not know has no vendor to claim.
        let unknown = view(&project, Some(&crew), &known(&[]));
        assert!(
            unknown
                .members
                .iter()
                .all(|m| m.vendor.is_none() && !m.installed)
        );
        assert!(!unknown.same_vendor);
    }
}
