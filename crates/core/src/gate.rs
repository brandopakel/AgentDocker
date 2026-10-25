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
//!
//! The vocabulary is fixed so that a job can parse it: a verdict is
//! `pass`, `changes` or `none`, and its findings are counted as
//! `important`, `nit` and `pre_existing`. `none` is a verdict given and
//! withheld — the reviewer read the commit and neither passes nor blocks
//! it — which is not the same as a pass with no findings, and not the same
//! as no verdict at all (the gate's `missing`).

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{AgentId, ProjectId};

/// The role whose verdicts the gate counts when asked for none.
pub const GATE_ROLE: &str = "reviewer";
/// Verdicts a project keeps; the oldest go first.
pub const VERDICTS_PER_PROJECT: usize = 2000;
/// Characters a verdict's note holds at most.
pub const NOTE_CHARS: usize = 4000;

/// How many findings of each severity the review raised. A verdict given
/// without counts has none: every count defaults to zero.
///
/// Verdicts written before this vocabulary counted priorities: their `p1`
/// reads as `important` and `p2` as `nit`; a `p3` count was neither and
/// is not carried.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Findings {
    /// Must be fixed before the change lands: a pass cannot carry one.
    #[serde(default, alias = "p1")]
    pub important: u32,
    /// Worth fixing, not worth blocking on.
    #[serde(default, alias = "p2")]
    pub nit: u32,
    /// Found while reading, not caused by this change.
    #[serde(default)]
    pub pre_existing: u32,
}

impl Findings {
    /// The counts as a job reads them: `important=0 nit=1 pre_existing=0`.
    pub fn line(&self) -> String {
        format!(
            "important={} nit={} pre_existing={}",
            self.important, self.nit, self.pre_existing
        )
    }

    /// The counts as a person reads them: `important 0, nit 1, pre-existing 0`.
    pub fn said(&self) -> String {
        format!(
            "important {}, nit {}, pre-existing {}",
            self.important, self.nit, self.pre_existing
        )
    }
}

/// A reviewer's word on a branch at a commit: the fixed vocabulary.
///
/// Verdicts written before it said `approve` and `comment`; those read as
/// `pass` and `none`, and are still accepted when spoken.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Word {
    /// Good to land at this commit, with no important finding.
    #[serde(alias = "approve", alias = "approved")]
    Pass,
    /// Not yet: the findings say what to change.
    Changes,
    /// Read, and no verdict given: neither passes nor blocks.
    #[serde(alias = "comment")]
    None,
}

impl Word {
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text.trim() {
            "pass" | "approve" | "approved" => Self::Pass,
            "changes" => Self::Changes,
            "none" | "comment" => Self::None,
            _ => return Option::None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Changes => "changes",
            Self::None => "none",
        }
    }
}

impl fmt::Display for Word {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
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
    /// The runtime its reviewer ran in when it was given (`claude-code`,
    /// `codex`, `human`, …), so a review by another vendor than the
    /// author's is visible. Empty on a verdict given before it was kept.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub runtime: String,
    pub verdict: Word,
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
        if self.verdict == Word::Pass && self.findings.important > 0 {
            return Err("a pass cannot carry an important finding: ask for changes");
        }
        Ok(())
    }

    /// Whether its word counts at the gate for `role`.
    pub fn counts_for(&self, role: &str) -> bool {
        self.human || self.roles.iter().any(|r| r == role)
    }

    /// The runtime as said: `unknown` on a verdict kept before it was.
    pub fn runtime_said(&self) -> &str {
        if self.runtime.is_empty() {
            "unknown"
        } else {
            &self.runtime
        }
    }

    /// One line a job can parse, `key=value` fields split on spaces:
    /// `verdict=pass head=<full commit> important=0 nit=1 pre_existing=0
    /// runtime=codex by=<reviewer>`. Every value before `by=` has no
    /// space in it; `by=` is last and runs to the end of the line, since a
    /// reviewer's name may have spaces.
    pub fn line(&self) -> String {
        format!(
            "verdict={} head={} {} runtime={} by={}",
            self.verdict,
            self.head,
            self.findings.line(),
            self.runtime_said(),
            self.by_name
        )
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
            .filter(|v| v.verdict != Word::None && v.counts_for(role))
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

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Approved { .. } => "approved",
            Self::Changes { .. } => "changes",
            Self::Stale { .. } => "stale",
            Self::Missing => "missing",
        }
    }

    /// The verdict the gate stands on, if any.
    pub fn verdict(&self) -> Option<&CodeVerdict> {
        match self {
            Self::Approved { verdict } | Self::Changes { verdict } | Self::Stale { verdict } => {
                Some(verdict)
            }
            Self::Missing => Option::None,
        }
    }

    /// One line a job can gate on, `key=value` fields split on spaces:
    /// `gate=approved branch=<branch> head=<asked commit>` followed, when
    /// a verdict stands, by its own [`CodeVerdict::line`] (`verdict=…
    /// head=<commit it read> … by=<reviewer>`, which runs to the end).
    /// A `stale` gate's two heads differ: the asked one and the one read.
    pub fn line(&self, branch: &str, head: &str) -> String {
        let mut line = format!("gate={} branch={branch} head={head}", self.as_str());
        if let Some(verdict) = self.verdict() {
            line.push(' ');
            line.push_str(&verdict.line());
        }
        line
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
        .filter(|v| v.branch == branch && v.verdict != Word::None && v.counts_for(role))
    {
        let kept = latest.entry(&verdict.by).or_insert(verdict);
        if verdict.order() > kept.order() {
            *kept = verdict;
        }
    }
    let newest = |word: Word| {
        latest
            .values()
            .filter(|v| v.verdict == word)
            .max_by_key(|v| v.order())
            .map(|v| Box::new((*v).clone()))
    };
    if let Some(verdict) = newest(Word::Changes) {
        return Gate::Changes { verdict };
    }
    if let Some(verdict) = latest
        .values()
        .filter(|v| v.verdict == Word::Pass && v.head == head)
        .max_by_key(|v| v.order())
    {
        return Gate::Approved {
            verdict: Box::new((*verdict).clone()),
        };
    }
    match newest(Word::Pass) {
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

    fn verdict(by: &str, roles: &[&str], head: &str, word: Word, minutes: i64) -> CodeVerdict {
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
            runtime: if by == "user" { "human" } else { "codex" }.into(),
            verdict: word,
            findings: Findings::default(),
            note: String::new(),
            at,
        }
    }

    #[test]
    fn only_an_approval_of_the_head_as_it_is_now_passes() {
        let approved = [verdict("r", &["reviewer"], A, Word::Pass, 0)];
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
            verdict("r", &["reviewer"], A, Word::Changes, 0),
            verdict("s", &["reviewer"], B, Word::Pass, 1),
        ];
        assert!(matches!(
            gate(&said, "feature/x", B, GATE_ROLE),
            Gate::Changes { verdict } if verdict.by.as_str() == "r"
        ));
        // A verdict withheld (`none`) is not a change of mind.
        said.push(verdict("r", &["reviewer"], B, Word::None, 2));
        assert!(matches!(
            gate(&said, "feature/x", B, GATE_ROLE),
            Gate::Changes { .. }
        ));
        said.push(verdict("r", &["reviewer"], B, Word::Pass, 3));
        assert!(gate(&said, "feature/x", B, GATE_ROLE).approved());
    }

    #[test]
    fn a_verdict_withheld_is_not_a_pass_with_no_findings_nor_a_missing_one() {
        // `none` on its own leaves the gate missing: nobody passed or
        // blocked it, though somebody read it.
        let withheld = [verdict("r", &["reviewer"], A, Word::None, 0)];
        assert_eq!(gate(&withheld, "feature/x", A, GATE_ROLE), Gate::Missing);
        let clean = [verdict("r", &["reviewer"], A, Word::Pass, 0)];
        assert_eq!(clean[0].findings, Findings::default());
        assert!(gate(&clean, "feature/x", A, GATE_ROLE).approved());
    }

    #[test]
    fn the_vocabulary_is_fixed_and_the_older_spellings_still_read() {
        for (text, word) in [
            ("pass", Word::Pass),
            ("approve", Word::Pass),
            ("approved", Word::Pass),
            ("changes", Word::Changes),
            ("none", Word::None),
            ("comment", Word::None),
            (" pass ", Word::Pass),
        ] {
            assert_eq!(Word::parse(text), Some(word), "{text:?}");
        }
        assert_eq!(Word::parse("lgtm"), None);
        assert_eq!(Word::Pass.to_string(), "pass");
        assert_eq!(serde_json::to_string(&Word::None).unwrap(), "\"none\"");
        // A verdict kept before the vocabulary: `approve`, priorities,
        // no runtime.
        let kept: CodeVerdict = serde_json::from_value(serde_json::json!({
            "id": "1", "seq": 1, "project": "p", "branch": "feature/x", "head": A,
            "by": "r", "by_name": "reviewer-1", "roles": ["reviewer"],
            "verdict": "approve", "findings": {"p1": 1, "p2": 2, "p3": 3},
            "note": "", "at": "2026-01-01T00:00:00Z"
        }))
        .unwrap();
        assert_eq!(kept.verdict, Word::Pass);
        assert_eq!(
            kept.findings,
            Findings {
                important: 1,
                nit: 2,
                pre_existing: 0
            }
        );
        assert_eq!(kept.runtime, "");
        assert_eq!(kept.runtime_said(), "unknown");
        // One kept without counts has none.
        let bare: Findings = serde_json::from_str("{}").unwrap();
        assert_eq!(bare, Findings::default());
        let round: CodeVerdict =
            serde_json::from_str(&serde_json::to_string(&kept).unwrap()).unwrap();
        assert_eq!(round, kept);
        assert!(serde_json::to_string(&kept).unwrap().contains("\"pass\""));
    }

    #[test]
    fn the_lines_a_job_parses_say_the_word_the_counts_and_the_runtime() {
        let mut passed = verdict("r", &["reviewer"], A, Word::Pass, 0);
        passed.by_name = "Reviewer One".into();
        passed.findings.nit = 2;
        assert_eq!(
            passed.line(),
            format!(
                "verdict=pass head={A} important=0 nit=2 pre_existing=0 runtime=codex by=Reviewer One"
            )
        );
        assert_eq!(passed.findings.said(), "important 0, nit 2, pre-existing 0");
        let approved = Gate::Approved {
            verdict: Box::new(passed.clone()),
        };
        assert_eq!(
            approved.line("feature/x", A),
            format!(
                "gate=approved branch=feature/x head={A} verdict=pass head={A} important=0 nit=2 pre_existing=0 runtime=codex by=Reviewer One"
            )
        );
        let stale = Gate::Stale {
            verdict: Box::new(passed),
        };
        assert!(stale.line("feature/x", B).starts_with(&format!(
            "gate=stale branch=feature/x head={B} verdict=pass head={A}"
        )));
        assert_eq!(
            Gate::Missing.line("feature/x", B),
            format!("gate=missing branch=feature/x head={B}")
        );
        // Every field before `by=` is one token.
        let line = approved.line("feature/x", A);
        let (before, _) = line.split_once(" by=").unwrap();
        assert!(before.split(' ').all(|field| field.contains('=')));
    }

    #[test]
    fn only_the_asked_role_and_the_person_count() {
        let said = [verdict("t", &["tester"], A, Word::Pass, 0)];
        assert_eq!(gate(&said, "feature/x", A, GATE_ROLE), Gate::Missing);
        assert!(gate(&said, "feature/x", A, "tester").approved());
        let person = [verdict("user", &[], A, Word::Pass, 0)];
        assert!(gate(&person, "feature/x", A, GATE_ROLE).approved());
    }

    #[test]
    fn a_verdict_names_full_commits_a_branch_and_no_important_finding_passed() {
        let mut v = verdict("r", &["reviewer"], A, Word::Pass, 0);
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
        v.findings.important = 1;
        assert!(v.check().is_err(), "a pass with an important finding");
        v.verdict = Word::Changes;
        assert!(v.check().is_ok());
        v.verdict = Word::None;
        assert!(v.check().is_ok(), "withheld, the findings stand as read");
        v.findings = Findings::default();
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
        let mut approve = verdict("r", &["reviewer"], A, Word::Pass, 5);
        let mut changes = verdict("r", &["reviewer"], A, Word::Changes, 0);
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
            verdict("r", &["reviewer"], A, Word::Changes, 0),
            verdict("s", &["reviewer"], A, Word::Pass, 1),
        ];
        for minute in 2..6 {
            all.push(verdict("s", &["reviewer"], A, Word::None, minute));
        }
        let next = verdict("s", &["reviewer"], A, Word::None, 9);
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
        let replaced = verdict("r", &["reviewer"], A, Word::Pass, 9);
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
            verdict("r", &["reviewer"], A, Word::Changes, 0),
            verdict("r", &["tester"], A, Word::Pass, 1),
            verdict("s", &["reviewer"], A, Word::Pass, 2),
            verdict("s", &["reviewer"], A, Word::None, 3),
        ];
        let before = gate(&all, "feature/x", A, GATE_ROLE);
        assert!(matches!(before, Gate::Changes { .. }));
        let next = verdict("s", &["reviewer"], A, Word::None, 9);
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
