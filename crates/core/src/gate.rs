//! A reviewer's word on a branch, bound to the commit it read, and the gate
//! that says whether a branch may land: only an approval of the branch's
//! head as it is now lets it through. A commit added after the approval,
//! a merge of the target branch included, makes it stale until the
//! reviewer has read that commit too; a request for changes blocks until
//! the same reviewer says otherwise — or, once that reviewer's record has
//! ended, until somebody who holds the role now, or the person, passes
//! that commit or a later one. A verdict so set aside is marked
//! superseded at that role's gate, and the person can set any verdict
//! aside at every gate.
//!
//! Whose word counts is the project's to say, by role: a verdict records
//! the roles its reviewer held when it was given, and the gate is asked
//! for one of them (`reviewer` by default). The person's word always
//! counts. Nothing here names a provider or a repository.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{AgentId, ProjectId, Verdict};

/// The role whose verdicts the gate counts when asked for none.
pub const GATE_ROLE: &str = "reviewer";
/// Verdicts a project keeps; the oldest go first.
pub const VERDICTS_PER_PROJECT: usize = 2000;
/// Characters a verdict's note holds at most.
pub const NOTE_CHARS: usize = 4000;

/// How many findings of each priority the review raised.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Findings {
    #[serde(default)]
    pub p1: u32,
    #[serde(default)]
    pub p2: u32,
    #[serde(default)]
    pub p3: u32,
}

/// Why a verdict was set aside.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupersedeReason {
    /// Its reviewer's record had ended, and somebody holding the role
    /// passed the commit it read or a later one.
    ReviewerEnded,
    /// The person lifted it.
    Person,
}

impl SupersedeReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReviewerEnded => "reviewer_ended",
            Self::Person => "person",
        }
    }
}

/// That a verdict no longer counts at a gate: who set it aside, why, and
/// with which verdict when a successor's pass did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Supersession {
    pub by: AgentId,
    pub by_name: String,
    /// The id of the pass that took its place; none when the person
    /// lifted it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub with: Option<String>,
    /// The role whose gate it no longer counts at: a successor's pass
    /// speaks for the role it was given in. None when the person lifted
    /// it, at every gate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub reason: SupersedeReason,
    /// What the person said when lifting it, if anything.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    pub at: DateTime<Utc>,
}

/// One reviewer's word on one branch at one commit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeVerdict {
    /// Sortable by when it was given.
    pub id: String,
    /// Its place in the project's verdicts, one more than the last: which
    /// word is a reviewer's latest is decided by this, never by a clock.
    #[serde(default)]
    pub seq: u64,
    pub project: ProjectId,
    pub branch: String,
    /// The full id of the commit the reviewer read.
    pub head: String,
    /// The commit an earlier review read, when this one read only what
    /// came after it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    pub by: AgentId,
    pub by_name: String,
    /// Whether the person gave it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub human: bool,
    /// The roles its reviewer held when it was given.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<String>,
    pub verdict: Verdict,
    #[serde(default)]
    pub findings: Findings,
    pub note: String,
    pub at: DateTime<Utc>,
    /// Where it was set aside, one mark per role (or one for every gate):
    /// it is nobody's standing word at a gate a mark covers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub superseded: Vec<Supersession>,
}

/// Whether `text` is a full commit id: 40 (SHA-1) or 64 (SHA-256)
/// lowercase hexadecimal digits.
pub fn full_commit(text: &str) -> bool {
    matches!(text.len(), 40 | 64)
        && text
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

/// Whether `branch` is a name a branch can have: what `git check-ref-format
/// --branch` accepts, in the cases a reviewer could mistype.
pub fn branch_name(branch: &str) -> bool {
    !branch.is_empty()
        && branch.len() <= 255
        && branch != "@"
        && !branch.starts_with(['-', '/'])
        && !branch.ends_with(['/', '.'])
        && !branch.contains("..")
        && !branch.contains("@{")
        && !branch
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || "~^:?*[\\".contains(c))
        // As git checks a ref, one `/` part at a time: none empty, none
        // starting with `.`, none ending with `.lock`.
        && branch
            .split('/')
            .all(|part| !part.is_empty() && !part.starts_with('.') && !part.ends_with(".lock"))
}

impl CodeVerdict {
    pub fn check(&self) -> Result<(), &'static str> {
        if !branch_name(&self.branch) {
            return Err("that is not a branch name");
        }
        if !full_commit(&self.head) || self.from.as_deref().is_some_and(|c| !full_commit(c)) {
            return Err("a verdict names full commit ids (40 or 64 lowercase hex digits)");
        }
        if self.from.as_deref() == Some(self.head.as_str()) {
            return Err("a review from a commit reads what came after it: name an earlier one");
        }
        if self.note.chars().count() > NOTE_CHARS {
            return Err("a verdict's note is at most 4000 characters");
        }
        if self.verdict == Verdict::Approve && self.findings.p1 > 0 {
            return Err("an approval cannot carry a P1 finding: ask for changes");
        }
        Ok(())
    }

    /// Whether its word counts at the gate for `role`.
    pub fn counts_for(&self, role: &str) -> bool {
        self.human || self.roles.iter().any(|r| r == role)
    }

    /// The mark that sets it aside at `role`'s gate, if one does.
    pub fn superseded_at(&self, role: &str) -> Option<&Supersession> {
        self.superseded
            .iter()
            .find(|mark| mark.role.as_deref().is_none_or(|r| r == role))
    }

    /// Whether `role`'s gate reads it: a pass or a request for changes
    /// that counts for the role and was not set aside there.
    fn stands_for(&self, role: &str) -> bool {
        self.verdict != Verdict::Comment
            && self.counts_for(role)
            && self.superseded_at(role).is_none()
    }

    /// Set it aside at every gate, as the person does. Refused once it is.
    pub fn supersede(&mut self, mark: Supersession) -> Result<(), &'static str> {
        if self.superseded.iter().any(|m| m.role.is_none()) {
            return Err("that verdict is already superseded");
        }
        self.superseded.push(Supersession { role: None, ..mark });
        Ok(())
    }
}

/// An id that sorts by the moment it was made, then by a random tail.
pub fn verdict_id(at: DateTime<Utc>) -> String {
    let nanos = at.timestamp_nanos_opt().unwrap_or_default().max(0);
    let tail = &uuid::Uuid::new_v4().simple().to_string()[..8];
    format!("{nanos:016x}{tail}")
}

impl CodeVerdict {
    /// The order verdicts were given in.
    fn order(&self) -> (u64, DateTime<Utc>, &str) {
        (self.seq, self.at, self.id.as_str())
    }
}

/// Each reviewer's standing word on each branch, for every role a gate may
/// be asked about: the latest approval or request for changes among the
/// verdicts that count for that role (a comment is never one). A word one
/// role's gate still reads stands, whatever a later word in another role
/// says.
pub fn standing(verdicts: &[CodeVerdict]) -> std::collections::BTreeSet<&str> {
    let mut roles: std::collections::BTreeSet<&str> = verdicts
        .iter()
        .flat_map(|v| v.roles.iter().map(String::as_str))
        .collect();
    roles.insert(GATE_ROLE);
    let mut kept = std::collections::BTreeSet::new();
    for role in roles {
        let mut latest: std::collections::BTreeMap<(&str, &AgentId), &CodeVerdict> =
            Default::default();
        for verdict in verdicts.iter().filter(|v| v.stands_for(role)) {
            let kept = latest
                .entry((verdict.branch.as_str(), &verdict.by))
                .or_insert(verdict);
            if verdict.order() > kept.order() {
                *kept = verdict;
            }
        }
        kept.extend(latest.values().map(|v| v.id.as_str()));
    }
    kept
}

/// Which of a project's verdicts to forget so that `given` fits under
/// `cap`: the oldest that stand for nobody — comments, and words a
/// reviewer has since replaced — and never a standing one. `None` when
/// only standing words would be left to forget: the new one is refused
/// rather than a reviewer's word lost.
pub fn to_forget(existing: &[CodeVerdict], given: &CodeVerdict, cap: usize) -> Option<Vec<String>> {
    let excess = (existing.len() + 1).saturating_sub(cap);
    if excess == 0 {
        return Some(Vec::new());
    }
    let mut all: Vec<CodeVerdict> = existing.to_vec();
    all.push(given.clone());
    let standing = standing(&all);
    let mut idle: Vec<&CodeVerdict> = existing
        .iter()
        .filter(|v| !standing.contains(v.id.as_str()))
        .collect();
    idle.sort_by(|a, b| a.order().cmp(&b.order()));
    (idle.len() >= excess).then(|| {
        idle.into_iter()
            .take(excess)
            .map(|v| v.id.clone())
            .collect()
    })
}

/// The full ref a branch lives under, `refs/heads/<name>`: what authorship
/// is kept and asked by, so that a tag of the same name never stands for
/// the branch.
pub fn branch_ref(branch: &str) -> String {
    format!("refs/heads/{branch}")
}

/// How the daemon came to know of a commit an agent made.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Witness {
    /// The daemon made it, asked by the agent (`commit`).
    Commit,
    /// The post-commit hook of a worktree the daemon created reported it,
    /// from inside the session of the agent it names.
    Hook,
}

impl Witness {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Commit => "commit",
            Self::Hook => "hook",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "commit" => Some(Self::Commit),
            "hook" => Some(Self::Hook),
            _ => None,
        }
    }
}

/// That an agent made a commit on a branch, as the daemon made or saw it.
/// Kept apart from the journal, which pruning and retention may empty, so
/// that an agent's word is not taken on a branch it wrote, wherever its
/// checkout stands later. One per project, commit and branch: a commit is
/// recorded once.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Authorship {
    pub project: ProjectId,
    pub agent: AgentId,
    /// The full ref the commit was made on, `refs/heads/<name>`.
    pub branch: String,
    /// The full id of the commit.
    pub commit: String,
    pub witness: Witness,
    pub at: DateTime<Utc>,
}

impl Authorship {
    /// Whether it can be kept: a branch's full ref and a full commit id.
    pub fn check(&self) -> Result<(), &'static str> {
        if !self
            .branch
            .strip_prefix("refs/heads/")
            .is_some_and(branch_name)
        {
            return Err("authorship names a branch by its full ref, refs/heads/<name>");
        }
        if !full_commit(&self.commit) {
            return Err("authorship names a full commit id");
        }
        Ok(())
    }
}

/// The file, in a witnessed worktree's own git directory, that its
/// post-commit hook reports with.
pub const WITNESS_FILE: &str = "agentdocker-witness";

/// What [`WITNESS_FILE`] holds, readable by this user alone: the token only
/// this worktree's reports carry, the socket of the daemon that made the
/// worktree, and the worktree itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WitnessFile {
    pub token: crate::secret::SecretText,
    pub socket: std::path::PathBuf,
    pub checkout: std::path::PathBuf,
}

/// What the gate says about a branch at a commit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Gate {
    /// A reviewer approved exactly this commit and nobody who counts
    /// asks for changes.
    Approved { verdict: Box<CodeVerdict> },
    /// A reviewer's latest word on the branch asks for changes.
    Changes { verdict: Box<CodeVerdict> },
    /// The latest approval read an earlier commit: what came after it
    /// (`verdict.head` to the asked head) has not been reviewed.
    Stale { verdict: Box<CodeVerdict> },
    /// Nobody who counts has approved or asked for changes.
    Missing,
}

impl Gate {
    pub fn approved(&self) -> bool {
        matches!(self, Self::Approved { .. })
    }
}

/// Each reviewer's latest standing word on `branch` for `role`.
fn latest_words<'a>(
    verdicts: &'a [CodeVerdict],
    branch: &str,
    role: &str,
) -> std::collections::BTreeMap<&'a AgentId, &'a CodeVerdict> {
    let mut latest: std::collections::BTreeMap<&AgentId, &CodeVerdict> = Default::default();
    for verdict in verdicts
        .iter()
        .filter(|v| v.branch == branch && v.stands_for(role))
    {
        let kept = latest.entry(&verdict.by).or_insert(verdict);
        if verdict.order() > kept.order() {
            *kept = verdict;
        }
    }
    latest
}

/// The gate for `branch` at `head`, counting the verdicts of `role`
/// holders and the person. Each reviewer's latest approval or request for
/// changes on the branch is their word (a comment is neither, and nor is
/// a superseded verdict); one request for changes blocks; otherwise an
/// approval of `head` passes it, and an approval of anything else is
/// stale.
pub fn gate(verdicts: &[CodeVerdict], branch: &str, head: &str, role: &str) -> Gate {
    let latest = latest_words(verdicts, branch, role);
    let newest = |verdict: Verdict| {
        latest
            .values()
            .filter(|v| v.verdict == verdict)
            .max_by_key(|v| v.order())
            .map(|v| Box::new((*v).clone()))
    };
    if let Some(verdict) = newest(Verdict::Changes) {
        return Gate::Changes { verdict };
    }
    if let Some(verdict) = latest
        .values()
        .filter(|v| v.verdict == Verdict::Approve && v.head == head)
        .max_by_key(|v| v.order())
    {
        return Gate::Approved {
            verdict: Box::new((*verdict).clone()),
        };
    }
    match newest(Verdict::Approve) {
        Some(verdict) => Gate::Stale { verdict },
        None => Gate::Missing,
    }
}

/// What the gate is told beyond the verdicts, to say whether a departed
/// reviewer's request for changes has a successor.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Succession {
    /// Reviewers whose records have ended: exited, failed or forgotten (a
    /// record that lives on under another id has not).
    pub ended: BTreeSet<AgentId>,
    /// Who holds the gate's role now and may speak for it. The person is
    /// not among them: their word stands on its own.
    pub holders: BTreeSet<AgentId>,
    /// `(earlier, later)` commits where `later` is known to be `earlier`
    /// or to come after it. A pair not here is not known to be in that
    /// order; a commit always is at or after itself.
    pub descends: BTreeSet<(String, String)>,
}

impl Succession {
    fn at_or_after(&self, earlier: &str, later: &str) -> bool {
        earlier == later
            || self
                .descends
                .contains(&(earlier.to_owned(), later.to_owned()))
    }
}

/// Each departed reviewer's standing request for changes, with the passes
/// that could take its place: each the latest word of somebody else who
/// holds the role now, or of the person, given after it.
fn candidates<'a>(
    latest: &std::collections::BTreeMap<&'a AgentId, &'a CodeVerdict>,
    succession: &Succession,
) -> Vec<(&'a CodeVerdict, Vec<&'a CodeVerdict>)> {
    latest
        .values()
        .filter(|v| v.verdict == Verdict::Changes && !v.human && succession.ended.contains(&v.by))
        .map(|changes| {
            let passes = latest
                .values()
                .filter(|v| {
                    v.verdict == Verdict::Approve
                        && v.by != changes.by
                        && v.order() > changes.order()
                        && (v.human || succession.holders.contains(&v.by))
                })
                .copied()
                .collect();
            (*changes, passes)
        })
        .collect()
}

/// The commit orders a lift on `branch` at `role`'s gate turns on: for
/// each departed reviewer's request for changes and each pass that could
/// take its place on another commit, `(the commit the request read, the
/// commit the pass read)`. The asker finds out which are in that order
/// ([`Succession::descends`]); one it cannot tell is not.
pub fn ancestry_questions(
    verdicts: &[CodeVerdict],
    branch: &str,
    role: &str,
    succession: &Succession,
) -> BTreeSet<(String, String)> {
    let latest = latest_words(verdicts, branch, role);
    candidates(&latest, succession)
        .into_iter()
        .flat_map(|(changes, passes)| {
            passes
                .into_iter()
                .filter(|pass| pass.head != changes.head)
                .map(|pass| (changes.head.clone(), pass.head.clone()))
        })
        .collect()
}

/// A request for changes a gate set aside, and how.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lifted {
    /// The request for changes, its new mark included.
    pub verdict: CodeVerdict,
    pub mark: Supersession,
    /// The pass that took its place.
    pub pass: CodeVerdict,
}

/// The requests for changes `role`'s gate on `branch` sets aside now: a
/// departed reviewer's standing request for changes, once somebody else
/// who holds the role now, or the person, passed the commit it read or a
/// later one after it was given. Only a pass lifts it, never a comment; a
/// live reviewer's request stands until that reviewer says otherwise; and
/// a pass of an earlier commit, or of one not known to come after it,
/// lifts nothing.
pub fn supersessions(
    verdicts: &[CodeVerdict],
    branch: &str,
    role: &str,
    succession: &Succession,
    at: DateTime<Utc>,
) -> Vec<Lifted> {
    let latest = latest_words(verdicts, branch, role);
    let mut lifted: Vec<Lifted> = candidates(&latest, succession)
        .into_iter()
        .filter_map(|(changes, passes)| {
            let pass = passes
                .into_iter()
                .filter(|pass| succession.at_or_after(&changes.head, &pass.head))
                .max_by_key(|pass| pass.order())?;
            let mark = Supersession {
                by: pass.by.clone(),
                by_name: pass.by_name.clone(),
                with: Some(pass.id.clone()),
                role: Some(role.to_owned()),
                reason: SupersedeReason::ReviewerEnded,
                note: String::new(),
                at,
            };
            let mut verdict = changes.clone();
            verdict.superseded.push(mark.clone());
            Some(Lifted {
                verdict,
                mark,
                pass: pass.clone(),
            })
        })
        .collect();
    lifted.sort_by(|a, b| a.verdict.order().cmp(&b.verdict.order()));
    lifted
}

/// A gate with the verdicts it set aside.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Gated {
    pub gate: Gate,
    /// The branch's passes and requests for changes that would count at
    /// this gate but are set aside there, the ones lifted now included,
    /// oldest first.
    pub superseded: Vec<CodeVerdict>,
    /// The ones set aside now, for the caller to keep.
    pub lifted: Vec<Lifted>,
}

/// The gate for `branch` at `head`, after setting aside what
/// [`supersessions`] says no longer blocks.
pub fn gated(
    verdicts: &[CodeVerdict],
    branch: &str,
    head: &str,
    role: &str,
    succession: &Succession,
    at: DateTime<Utc>,
) -> Gated {
    let lifted = supersessions(verdicts, branch, role, succession, at);
    let mut all = verdicts.to_vec();
    for verdict in &mut all {
        if let Some(lift) = lifted.iter().find(|l| l.verdict.id == verdict.id) {
            verdict.superseded.push(lift.mark.clone());
        }
    }
    let gate = gate(&all, branch, head, role);
    let mut superseded: Vec<CodeVerdict> = all
        .into_iter()
        .filter(|v| {
            v.branch == branch
                && v.verdict != Verdict::Comment
                && v.counts_for(role)
                && v.superseded_at(role).is_some()
        })
        .collect();
    superseded.sort_by(|a, b| a.order().cmp(&b.order()));
    Gated {
        gate,
        superseded,
        lifted,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn verdict(by: &str, roles: &[&str], head: &str, word: Verdict, minutes: i64) -> CodeVerdict {
        let at = DateTime::from_timestamp(1_700_000_000, 0).unwrap() + Duration::minutes(minutes);
        CodeVerdict {
            id: verdict_id(at),
            seq: u64::try_from(minutes).unwrap_or(0) + 1,
            project: ProjectId::from("p"),
            branch: "feature/x".into(),
            head: head.into(),
            from: None,
            by: AgentId::from(by),
            by_name: by.into(),
            human: by == "user",
            roles: roles.iter().map(|r| (*r).to_owned()).collect(),
            verdict: word,
            findings: Findings::default(),
            note: String::new(),
            at,
            superseded: Vec::new(),
        }
    }

    #[test]
    fn only_an_approval_of_the_head_as_it_is_now_passes() {
        let approved = [verdict("r", &["reviewer"], A, Verdict::Approve, 0)];
        assert!(gate(&approved, "feature/x", A, GATE_ROLE).approved());
        // A commit since, a merge of main included, makes it stale.
        assert!(matches!(
            gate(&approved, "feature/x", B, GATE_ROLE),
            Gate::Stale { verdict } if verdict.head == A
        ));
        // Another branch, or nothing said, is missing.
        assert_eq!(gate(&approved, "other", A, GATE_ROLE), Gate::Missing);
        assert_eq!(gate(&[], "feature/x", A, GATE_ROLE), Gate::Missing);
    }

    #[test]
    fn a_request_for_changes_blocks_until_the_same_reviewer_says_otherwise() {
        let mut said = vec![
            verdict("r", &["reviewer"], A, Verdict::Changes, 0),
            verdict("s", &["reviewer"], B, Verdict::Approve, 1),
        ];
        assert!(matches!(
            gate(&said, "feature/x", B, GATE_ROLE),
            Gate::Changes { verdict } if verdict.by.as_str() == "r"
        ));
        // A comment is not a change of mind.
        said.push(verdict("r", &["reviewer"], B, Verdict::Comment, 2));
        assert!(matches!(
            gate(&said, "feature/x", B, GATE_ROLE),
            Gate::Changes { .. }
        ));
        said.push(verdict("r", &["reviewer"], B, Verdict::Approve, 3));
        assert!(gate(&said, "feature/x", B, GATE_ROLE).approved());
    }

    const C: &str = "cccccccccccccccccccccccccccccccccccccccc";

    fn succession(ended: &[&str], holders: &[&str], descends: &[(&str, &str)]) -> Succession {
        Succession {
            ended: ended.iter().map(|id| AgentId::from(*id)).collect(),
            holders: holders.iter().map(|id| AgentId::from(*id)).collect(),
            descends: descends
                .iter()
                .map(|(e, l)| ((*e).to_owned(), (*l).to_owned()))
                .collect(),
        }
    }

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_009_000, 0).unwrap()
    }

    #[test]
    fn a_departed_reviewer_s_changes_are_lifted_by_a_successor_s_pass_of_a_later_commit() {
        // R asked for changes at A, holding the reviewer and tester roles;
        // S, a reviewer, passed B, which came after A.
        let said = vec![
            verdict("r", &["reviewer", "tester"], A, Verdict::Changes, 0),
            verdict("s", &["reviewer"], B, Verdict::Approve, 1),
        ];
        let b_after_a = [(A, B)];
        // While R lives, its word stands whatever S says.
        let live = gated(
            &said,
            "feature/x",
            B,
            GATE_ROLE,
            &succession(&[], &["r", "s"], &b_after_a),
            now(),
        );
        assert!(matches!(&live.gate, Gate::Changes { verdict } if verdict.by.as_str() == "r"));
        assert!(live.lifted.is_empty() && live.superseded.is_empty());
        // R gone, the lift turns on B coming after A: asked, and not
        // known, nothing is lifted.
        let ended = succession(&["r"], &["s"], &[]);
        assert_eq!(
            ancestry_questions(&said, "feature/x", GATE_ROLE, &ended),
            [(A.to_owned(), B.to_owned())].into_iter().collect()
        );
        assert!(matches!(
            gated(&said, "feature/x", B, GATE_ROLE, &ended, now()).gate,
            Gate::Changes { .. }
        ));
        // Known: S's pass lifts it, and the gate says so.
        let ended = succession(&["r"], &["s"], &b_after_a);
        let after = gated(&said, "feature/x", B, GATE_ROLE, &ended, now());
        assert!(after.gate.approved(), "{after:?}");
        assert_eq!(after.lifted.len(), 1);
        let lift = &after.lifted[0];
        assert_eq!(lift.verdict.id, said[0].id);
        assert_eq!(lift.pass.id, said[1].id);
        assert_eq!(lift.mark.by.as_str(), "s");
        assert_eq!(lift.mark.with.as_deref(), Some(said[1].id.as_str()));
        assert_eq!(lift.mark.role.as_deref(), Some(GATE_ROLE));
        assert_eq!(lift.mark.reason, SupersedeReason::ReviewerEnded);
        assert_eq!(after.superseded, std::slice::from_ref(&lift.verdict));
        // Whatever head the gate is asked about: at a commit since, it is
        // S's pass that is stale, not R's word that blocks.
        assert!(matches!(
            gated(&said, "feature/x", C, GATE_ROLE, &ended, now()).gate,
            Gate::Stale { verdict } if verdict.by.as_str() == "s"
        ));
        // Kept on the record, it stays lifted when S no longer holds the
        // role, and nothing is lifted twice.
        let kept = vec![lift.verdict.clone(), said[1].clone()];
        let later = gated(
            &kept,
            "feature/x",
            B,
            GATE_ROLE,
            &succession(&["r"], &[], &[]),
            now(),
        );
        assert!(later.gate.approved());
        assert!(later.lifted.is_empty());
        assert_eq!(later.superseded.len(), 1, "still said");
        // S spoke for the reviewers: the tester gate still reads R's word,
        // and so R's word still stands for forgetting.
        assert!(matches!(
            gated(&kept, "feature/x", B, "tester", &succession(&["r"], &[], &[]), now()).gate,
            Gate::Changes { verdict } if verdict.by.as_str() == "r"
        ));
        assert!(standing(&kept).contains(said[0].id.as_str()));
        // The person's pass of the very commit R read lifts it too, with
        // nothing to ask.
        let person = vec![
            said[0].clone(),
            verdict("user", &[], A, Verdict::Approve, 1),
        ];
        let nobody = succession(&["r"], &[], &[]);
        assert!(ancestry_questions(&person, "feature/x", GATE_ROLE, &nobody).is_empty());
        let lifted = gated(&person, "feature/x", A, GATE_ROLE, &nobody, now());
        assert!(lifted.gate.approved());
        assert_eq!(lifted.lifted[0].mark.by.as_str(), "user");
    }

    #[test]
    fn nothing_but_a_later_pass_of_a_later_commit_by_a_role_holder_lifts_it() {
        let changes = verdict("r", &["reviewer"], B, Verdict::Changes, 1);
        let blocked = |said: &[CodeVerdict], head: &str, who: &Succession| {
            let got = gated(said, "feature/x", head, GATE_ROLE, who, now());
            got.lifted.is_empty()
                && matches!(&got.gate, Gate::Changes { verdict } if verdict.by.as_str() == "r")
        };
        // C comes after B; A came before it.
        let order = [(B, C), (A, B), (A, C)];
        let ended = succession(&["r"], &["s"], &order);
        // A pass by somebody who does not hold the role now.
        let outsider = [
            changes.clone(),
            verdict("t", &["reviewer"], C, Verdict::Approve, 2),
        ];
        assert!(blocked(&outsider, C, &ended), "not a holder now");
        // A pass given in another role.
        let tester = [
            changes.clone(),
            verdict("s", &["tester"], C, Verdict::Approve, 2),
        ];
        assert!(blocked(&tester, C, &ended), "another role's word");
        // A comment (`none`) is not a pass.
        let comment = [
            changes.clone(),
            verdict("s", &["reviewer"], C, Verdict::Comment, 2),
        ];
        assert!(blocked(&comment, C, &ended), "a comment");
        // A pass of a commit before the one R read, wherever the gate is
        // asked.
        let older = [
            changes.clone(),
            verdict("s", &["reviewer"], A, Verdict::Approve, 2),
        ];
        assert!(blocked(&older, A, &ended), "an older commit, asked there");
        assert!(blocked(&older, C, &ended), "an older commit, asked later");
        // A pass given before the request for changes.
        let earlier = [
            verdict("s", &["reviewer"], C, Verdict::Approve, 0),
            changes.clone(),
        ];
        assert!(blocked(&earlier, C, &ended), "given before");
        // The departed reviewer's own word is not a successor's.
        let own = [
            changes.clone(),
            verdict("r", &["tester"], C, Verdict::Approve, 2),
        ];
        assert!(blocked(&own, C, &ended), "its own");
        // A live reviewer's request stands, a successor's pass or not.
        let pass = verdict("s", &["reviewer"], C, Verdict::Approve, 2);
        let lifts = [changes.clone(), pass.clone()];
        assert!(
            blocked(&lifts, C, &succession(&[], &["r", "s"], &order)),
            "live"
        );
        // The successor's own later request for changes is its word, not
        // its pass.
        let changed_mind = [
            changes.clone(),
            pass.clone(),
            verdict("s", &["reviewer"], C, Verdict::Changes, 3),
        ];
        let got = gated(&changed_mind, "feature/x", C, GATE_ROLE, &ended, now());
        assert!(got.lifted.is_empty());
        assert!(matches!(&got.gate, Gate::Changes { verdict } if verdict.by.as_str() == "s"));
        // And the pass lifts it when it is all that was missing.
        assert!(!blocked(&lifts, C, &ended));
        assert!(
            gated(&lifts, "feature/x", C, GATE_ROLE, &ended, now())
                .gate
                .approved()
        );
    }

    #[test]
    fn the_person_lifts_any_verdict_at_every_gate_once() {
        let mut said = vec![
            verdict("r", &["reviewer", "tester"], A, Verdict::Changes, 0),
            verdict("s", &["reviewer", "tester"], B, Verdict::Approve, 1),
        ];
        let live = succession(&[], &["r", "s"], &[]);
        assert!(matches!(
            gated(&said, "feature/x", B, GATE_ROLE, &live, now()).gate,
            Gate::Changes { .. }
        ));
        let mark = Supersession {
            by: AgentId::from("user"),
            by_name: "user".into(),
            with: None,
            role: Some("reviewer".into()),
            reason: SupersedeReason::Person,
            note: String::new(),
            at: now(),
        };
        said[0].supersede(mark.clone()).unwrap();
        assert_eq!(said[0].superseded[0].role, None, "at every gate");
        assert_eq!(
            said[0].supersede(mark),
            Err("that verdict is already superseded")
        );
        for role in [GATE_ROLE, "tester"] {
            let got = gated(&said, "feature/x", B, role, &live, now());
            assert!(got.gate.approved(), "{role}");
            assert!(got.lifted.is_empty(), "nothing newly set aside");
            assert_eq!(
                got.superseded
                    .iter()
                    .map(|v| v.superseded_at(role).map(|m| m.reason))
                    .collect::<Vec<_>>(),
                [Some(SupersedeReason::Person)]
            );
        }
        // A verdict set aside everywhere is forgettable.
        let next = verdict("s", &["reviewer"], B, Verdict::Comment, 9);
        assert_eq!(to_forget(&said, &next, 2).unwrap(), [said[0].id.clone()]);
    }

    #[test]
    fn a_verdict_kept_without_marks_reads_back_with_none() {
        let bare: CodeVerdict = serde_json::from_value(serde_json::json!({
            "id": "1", "seq": 1, "project": "p", "branch": "feature/x", "head": A,
            "by": "r", "by_name": "reviewer-1", "roles": ["reviewer"],
            "verdict": "changes", "note": "", "at": "2000-01-01T00:00:00Z"
        }))
        .unwrap();
        assert!(bare.superseded.is_empty());
        let text = serde_json::to_value(&bare).unwrap();
        assert!(text.get("superseded").is_none(), "nothing written for none");
    }

    #[test]
    fn only_the_asked_role_and_the_person_count() {
        let said = [verdict("t", &["tester"], A, Verdict::Approve, 0)];
        assert_eq!(gate(&said, "feature/x", A, GATE_ROLE), Gate::Missing);
        assert!(gate(&said, "feature/x", A, "tester").approved());
        let person = [verdict("user", &[], A, Verdict::Approve, 0)];
        assert!(gate(&person, "feature/x", A, GATE_ROLE).approved());
    }

    #[test]
    fn a_verdict_names_full_commits_a_branch_and_no_approved_p1() {
        let mut v = verdict("r", &["reviewer"], A, Verdict::Approve, 0);
        assert!(v.check().is_ok());
        v.head = "abc1234".into();
        assert!(v.check().is_err(), "a short id");
        v.head = A.to_uppercase();
        assert!(v.check().is_err(), "upper case");
        v.head = A.into();
        v.from = Some(A.into());
        assert!(v.check().is_err(), "from its own head");
        v.from = Some(B.into());
        assert!(v.check().is_ok());
        v.findings.p1 = 1;
        assert!(v.check().is_err(), "an approval with a P1");
        v.verdict = Verdict::Changes;
        assert!(v.check().is_ok());
        for bad in [
            "",
            "-x",
            "a..b",
            "a b",
            "a~1",
            "x.lock",
            "a/",
            "@{u}",
            "@",
            ".x",
            "feature/.hidden",
            "a.lock/b",
            "a//b",
        ] {
            v.branch = bad.into();
            assert!(v.check().is_err(), "{bad:?}");
        }
        assert!(branch_name("claude/review-gate"));
        assert!(branch_name("release/v1.2") && branch_name("a.b/c-d_e"));
    }

    #[test]
    fn the_order_given_decides_not_the_clock() {
        // Changes asked last, on a clock that ran back: it still blocks.
        let mut approve = verdict("r", &["reviewer"], A, Verdict::Approve, 5);
        let mut changes = verdict("r", &["reviewer"], A, Verdict::Changes, 0);
        approve.seq = 1;
        changes.seq = 2;
        assert!(matches!(
            gate(
                &[approve.clone(), changes.clone()],
                "feature/x",
                A,
                GATE_ROLE
            ),
            Gate::Changes { .. }
        ));
        // The same instant: the later given wins.
        changes.at = approve.at;
        changes.seq = 0;
        approve.seq = 3;
        assert!(gate(&[changes, approve], "feature/x", A, GATE_ROLE).approved());
    }

    #[test]
    fn forgetting_keeps_every_standing_word() {
        // R asked for changes; S approved; then comments fill the project.
        let mut all = vec![
            verdict("r", &["reviewer"], A, Verdict::Changes, 0),
            verdict("s", &["reviewer"], A, Verdict::Approve, 1),
        ];
        for minute in 2..6 {
            all.push(verdict("s", &["reviewer"], A, Verdict::Comment, minute));
        }
        let next = verdict("s", &["reviewer"], A, Verdict::Comment, 9);
        let forget = to_forget(&all, &next, all.len()).unwrap();
        assert_eq!(forget.len(), 1);
        assert_eq!(
            forget[0], all[2].id,
            "the oldest comment, never R's changes"
        );
        let only_standing = &all[..2];
        assert_eq!(
            to_forget(only_standing, &next, 2),
            None,
            "refused, not a word lost"
        );
        // A reviewer's replaced word is forgettable once replaced.
        let replaced = verdict("r", &["reviewer"], A, Verdict::Approve, 9);
        assert_eq!(
            to_forget(only_standing, &replaced, 2).unwrap(),
            [all[0].id.clone()]
        );
    }

    #[test]
    fn forgetting_keeps_a_word_another_role_s_gate_still_reads() {
        // R asked for changes as a reviewer, then approved holding only the
        // tester role; S approved as a reviewer. The reviewer gate is held
        // by R's changes, and must still be after forgetting.
        let all = vec![
            verdict("r", &["reviewer"], A, Verdict::Changes, 0),
            verdict("r", &["tester"], A, Verdict::Approve, 1),
            verdict("s", &["reviewer"], A, Verdict::Approve, 2),
            verdict("s", &["reviewer"], A, Verdict::Comment, 3),
        ];
        let before = gate(&all, "feature/x", A, GATE_ROLE);
        assert!(matches!(before, Gate::Changes { .. }));
        let next = verdict("s", &["reviewer"], A, Verdict::Comment, 9);
        let forget = to_forget(&all, &next, all.len()).unwrap();
        assert_eq!(forget, [all[3].id.clone()], "only the comment goes");
        let after: Vec<CodeVerdict> = all
            .iter()
            .filter(|v| !forget.contains(&v.id))
            .cloned()
            .chain([next])
            .collect();
        assert_eq!(gate(&after, "feature/x", A, GATE_ROLE), before);
        assert!(gate(&after, "feature/x", A, "tester").approved());
    }

    #[test]
    fn authorship_is_kept_by_a_branch_s_full_ref_and_a_full_commit() {
        assert_eq!(branch_ref("feature/mine"), "refs/heads/feature/mine");
        let at = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let authored = |branch: &str, commit: &str| Authorship {
            project: ProjectId::from("p"),
            agent: AgentId::from("a"),
            branch: branch.to_owned(),
            commit: commit.to_owned(),
            witness: Witness::Hook,
            at,
        };
        assert!(authored("refs/heads/feature/mine", A).check().is_ok());
        for (branch, commit) in [
            ("feature/mine", A),
            ("refs/tags/feature/mine", A),
            ("refs/heads/", A),
            ("refs/heads/a..b", A),
            ("refs/heads/feature/mine", "abc1234"),
        ] {
            assert!(
                authored(branch, commit).check().is_err(),
                "{branch} {commit}"
            );
        }
        for witness in [Witness::Commit, Witness::Hook] {
            assert_eq!(Witness::parse(witness.as_str()), Some(witness));
        }
        assert_eq!(Witness::parse("reflog"), None);
    }

    #[test]
    fn ids_sort_by_when_given() {
        let early = verdict_id(DateTime::from_timestamp(1_700_000_000, 0).unwrap());
        let late = verdict_id(DateTime::from_timestamp(1_700_000_001, 0).unwrap());
        assert!(early < late);
        assert_eq!(early.len(), 24);
    }
}
