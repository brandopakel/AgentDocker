//! A reviewer's word on a branch, bound to the commit it read, and the gate
//! that says whether a branch may land: only an approval of the branch's
//! head as it is now lets it through. A commit added after the approval,
//! a merge of the target branch included, makes it stale until the
//! reviewer has read that commit too; a request for changes blocks until
//! the same reviewer says otherwise.
//!
//! Whose word counts is the project's to say, by role: a verdict records
//! the roles its reviewer held when it was given, and the gate is asked
//! for one of them (`reviewer` by default). The person's word always
//! counts. Nothing here names a provider or a repository.

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
        for verdict in verdicts
            .iter()
            .filter(|v| v.verdict != Verdict::Comment && v.counts_for(role))
        {
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

/// The gate for `branch` at `head`, counting the verdicts of `role`
/// holders and the person. Each reviewer's latest approval or request for
/// changes on the branch is their word (a comment is neither); one
/// request for changes blocks; otherwise an approval of `head` passes it,
/// and an approval of anything else is stale.
pub fn gate(verdicts: &[CodeVerdict], branch: &str, head: &str, role: &str) -> Gate {
    let mut latest: std::collections::BTreeMap<&AgentId, &CodeVerdict> = Default::default();
    for verdict in verdicts
        .iter()
        .filter(|v| v.branch == branch && v.verdict != Verdict::Comment && v.counts_for(role))
    {
        let kept = latest.entry(&verdict.by).or_insert(verdict);
        if verdict.order() > kept.order() {
            *kept = verdict;
        }
    }
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
    fn ids_sort_by_when_given() {
        let early = verdict_id(DateTime::from_timestamp(1_700_000_000, 0).unwrap());
        let late = verdict_id(DateTime::from_timestamp(1_700_000_001, 0).unwrap());
        assert!(early < late);
        assert_eq!(early.len(), 24);
    }
}
