//! A reviewer's word on a branch, bound to the commit it read, and the gate
//! that says whether a branch may land: only an approval of the branch's
//! head as it is now lets it through. A commit added after the approval,
//! a merge of the target branch included, makes it stale until the
//! reviewer has read that commit too; a request for changes blocks until
//! the same reviewer says otherwise — or, once that reviewer's record has
//! ended, until somebody who holds the role now, or the person, passes
//! that commit or a later one. A verdict so set aside is marked
//! superseded at that role's gate, and the person can set any verdict
//! aside at every gate. Each reviewer's latest word is their word: one
//! set aside leaves them none, and never brings back what they said
//! before.
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

use std::collections::BTreeSet;

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

/// A reviewer's word on a branch at a commit: the fixed vocabulary as
/// spoken and shown (`pass`, `changes`, `none`).
///
/// On the wire and in storage it is written `approve`, `changes` and
/// `comment`, the spelling clients built before this vocabulary decode,
/// so a retained CLI still reads a verdict, a gate and an event; either
/// spelling is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Word {
    /// Good to land at this commit, with no important finding.
    #[serde(rename = "approve", alias = "approved", alias = "pass")]
    Pass,
    /// Not yet: the findings say what to change.
    Changes,
    /// Read, and no verdict given: neither passes nor blocks.
    #[serde(rename = "comment", alias = "none")]
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

/// `text` as one token of a parseable line: every space, control
/// character, `=` and `%` percent-encoded (`%20`, `%0A`, `%3D`, `%25`),
/// so the line splits on spaces and each field on its first `=`. Decoding
/// is percent-decoding.
pub fn token(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_whitespace() || c.is_control() || c == '=' || c == '%' {
            let mut bytes = [0u8; 4];
            for byte in c.encode_utf8(&mut bytes).bytes() {
                out.push_str(&format!("%{byte:02X}"));
            }
        } else {
            out.push(c);
        }
    }
    out
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
    /// Where it was set aside, one mark per role (or one for every gate):
    /// at a gate a mark covers, its reviewer has no word.
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
        if self.verdict == Word::Pass && self.findings.important > 0 {
            return Err("a pass cannot carry an important finding: ask for changes");
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

    /// Whether it is a word at `role`'s gate, set aside or not: a pass or
    /// a request for changes that counts for the role. A reviewer's latest
    /// such word replaces what they said before, even once it is set aside.
    fn speaks_for(&self, role: &str) -> bool {
        self.verdict != Word::None && self.counts_for(role)
    }

    /// Set it aside at every gate, as the person does. Refused once it is.
    pub fn supersede(&mut self, mark: Supersession) -> Result<(), &'static str> {
        if self.superseded.iter().any(|m| m.role.is_none()) {
            return Err("that verdict is already superseded");
        }
        self.superseded.push(Supersession { role: None, ..mark });
        Ok(())
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
    /// runtime=codex by=<reviewer>`. Every value before `by=` is one
    /// token: the word, the commit and the counts are so by construction,
    /// and the runtime, free text when an agent registered, is
    /// percent-encoded ([`token`]). `by=` is last and runs to the end of
    /// the line, since a reviewer's name may have spaces (never a control
    /// character).
    pub fn line(&self) -> String {
        format!(
            "verdict={} head={} {} runtime={} by={}",
            self.verdict,
            self.head,
            self.findings.line(),
            token(self.runtime_said()),
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
        for verdict in verdicts.iter().filter(|v| v.speaks_for(role)) {
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

/// Each reviewer's latest word on `branch` at `role`'s gate, set aside or
/// not.
fn latest_given<'a>(
    verdicts: &'a [CodeVerdict],
    branch: &str,
    role: &str,
) -> std::collections::BTreeMap<&'a AgentId, &'a CodeVerdict> {
    let mut latest: std::collections::BTreeMap<&AgentId, &CodeVerdict> = Default::default();
    for verdict in verdicts
        .iter()
        .filter(|v| v.branch == branch && v.speaks_for(role))
    {
        let kept = latest.entry(&verdict.by).or_insert(verdict);
        if verdict.order() > kept.order() {
            *kept = verdict;
        }
    }
    latest
}

/// Each reviewer's word on `branch` at `role`'s gate: their latest, unless
/// it is set aside there, when they have none. What they said before stays
/// replaced.
fn latest_words<'a>(
    verdicts: &'a [CodeVerdict],
    branch: &str,
    role: &str,
) -> std::collections::BTreeMap<&'a AgentId, &'a CodeVerdict> {
    let mut latest = latest_given(verdicts, branch, role);
    latest.retain(|_, verdict| verdict.superseded_at(role).is_none());
    latest
}

/// The gate for `branch` at `head`, counting the verdicts of `role`
/// holders and the person. Each reviewer's latest pass or request for
/// changes on the branch is their word (a `none` is neither), and one set
/// aside at this gate leaves them none; one request for changes blocks;
/// otherwise a pass of `head` passes it, and a pass of anything else is
/// stale.
pub fn gate(verdicts: &[CodeVerdict], branch: &str, head: &str, role: &str) -> Gate {
    let latest = latest_words(verdicts, branch, role);
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
        .filter(|v| v.verdict == Word::Changes && !v.human && succession.ended.contains(&v.by))
        .map(|changes| {
            let passes = latest
                .values()
                .filter(|v| {
                    v.verdict == Word::Pass
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
    /// The reviewers' latest words on the branch that are set aside at
    /// this gate, the ones lifted now included, oldest first.
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
    let mut superseded: Vec<CodeVerdict> = latest_given(&all, branch, role)
        .into_values()
        .filter(|v| v.superseded_at(role).is_some())
        .cloned()
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
            superseded: Vec::new(),
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
            verdict("r", &["reviewer", "tester"], A, Word::Changes, 0),
            verdict("s", &["reviewer"], B, Word::Pass, 1),
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
        let person = vec![said[0].clone(), verdict("user", &[], A, Word::Pass, 1)];
        let nobody = succession(&["r"], &[], &[]);
        assert!(ancestry_questions(&person, "feature/x", GATE_ROLE, &nobody).is_empty());
        let lifted = gated(&person, "feature/x", A, GATE_ROLE, &nobody, now());
        assert!(lifted.gate.approved());
        assert_eq!(lifted.lifted[0].mark.by.as_str(), "user");
    }

    #[test]
    fn nothing_but_a_later_pass_of_a_later_commit_by_a_role_holder_lifts_it() {
        let changes = verdict("r", &["reviewer"], B, Word::Changes, 1);
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
            verdict("t", &["reviewer"], C, Word::Pass, 2),
        ];
        assert!(blocked(&outsider, C, &ended), "not a holder now");
        // A pass given in another role.
        let tester = [changes.clone(), verdict("s", &["tester"], C, Word::Pass, 2)];
        assert!(blocked(&tester, C, &ended), "another role's word");
        // A comment (`none`) is not a pass.
        let comment = [
            changes.clone(),
            verdict("s", &["reviewer"], C, Word::None, 2),
        ];
        assert!(blocked(&comment, C, &ended), "a comment");
        // A pass of a commit before the one R read, wherever the gate is
        // asked.
        let older = [
            changes.clone(),
            verdict("s", &["reviewer"], A, Word::Pass, 2),
        ];
        assert!(blocked(&older, A, &ended), "an older commit, asked there");
        assert!(blocked(&older, C, &ended), "an older commit, asked later");
        // A pass given before the request for changes.
        let earlier = [
            verdict("s", &["reviewer"], C, Word::Pass, 0),
            changes.clone(),
        ];
        assert!(blocked(&earlier, C, &ended), "given before");
        // The departed reviewer's own word is not a successor's.
        let own = [changes.clone(), verdict("r", &["tester"], C, Word::Pass, 2)];
        assert!(blocked(&own, C, &ended), "its own");
        // A live reviewer's request stands, a successor's pass or not.
        let pass = verdict("s", &["reviewer"], C, Word::Pass, 2);
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
            verdict("s", &["reviewer"], C, Word::Changes, 3),
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
            verdict("r", &["reviewer", "tester"], A, Word::Changes, 0),
            verdict("s", &["reviewer", "tester"], B, Word::Pass, 1),
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
        // Set aside, it still replaces what R said before, so it stands
        // for forgetting; R's earlier word goes first.
        let mut earlier = verdict("r", &["reviewer", "tester"], B, Word::Pass, -1);
        earlier.seq = 0;
        let mut kept = vec![earlier.clone()];
        kept.extend(said.iter().cloned());
        let next = verdict("s", &["reviewer"], B, Word::None, 9);
        assert_eq!(
            to_forget(&kept, &next, 3).unwrap(),
            std::slice::from_ref(&earlier.id)
        );
        assert_eq!(to_forget(&said, &next, 2), None, "never the word set aside");
        // A pass set aside counts nowhere: its reviewer has no word.
        let mut pass = said[1].clone();
        pass.supersede(Supersession {
            by: AgentId::from("user"),
            by_name: "user".into(),
            with: None,
            role: None,
            reason: SupersedeReason::Person,
            note: String::new(),
            at: now(),
        })
        .unwrap();
        assert_eq!(pass.verdict, Word::Pass, "kept as given");
        let gone = [pass];
        assert_eq!(
            gated(&gone, "feature/x", B, GATE_ROLE, &live, now()).gate,
            Gate::Missing
        );
    }

    #[test]
    fn a_departed_reviewer_s_earlier_words_stay_replaced_once_its_latest_is_lifted() {
        // R asked for changes at X, a commit since discarded, passed Y,
        // then asked for changes at A; S passed B, after A, and R left.
        const X: &str = "dddddddddddddddddddddddddddddddddddddddd";
        const Y: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
        let said = vec![
            verdict("r", &["reviewer"], X, Word::Changes, 0),
            verdict("r", &["reviewer"], Y, Word::Pass, 1),
            verdict("r", &["reviewer"], A, Word::Changes, 2),
            verdict("s", &["reviewer"], B, Word::Pass, 3),
        ];
        let ended = succession(&["r"], &["s"], &[(A, B), (Y, A), (Y, B)]);
        // Only R's latest word is in question, never one it replaced.
        assert_eq!(
            ancestry_questions(&said, "feature/x", GATE_ROLE, &ended),
            [(A.to_owned(), B.to_owned())].into_iter().collect()
        );
        let after = gated(&said, "feature/x", B, GATE_ROLE, &ended, now());
        assert!(after.gate.approved(), "{after:?}");
        assert_eq!(
            after
                .lifted
                .iter()
                .map(|l| l.verdict.id.as_str())
                .collect::<Vec<_>>(),
            [said[2].id.as_str()]
        );
        assert_eq!(
            after.superseded,
            std::slice::from_ref(&after.lifted[0].verdict)
        );
        // Kept, the lift leaves R no word at all: the discarded commit's
        // request does not block, and the pass of Y does not pass Y.
        let mut kept = said.clone();
        kept[2] = after.lifted[0].verdict.clone();
        let nobody = succession(&["r"], &[], &[]);
        let again = gated(&kept, "feature/x", B, GATE_ROLE, &nobody, now());
        assert!(again.gate.approved() && again.lifted.is_empty());
        assert!(matches!(
            gated(&kept, "feature/x", Y, GATE_ROLE, &nobody, now()).gate,
            Gate::Stale { verdict } if verdict.by.as_str() == "s"
        ));
        assert!(matches!(
            gated(&kept, "feature/x", X, GATE_ROLE, &nobody, now()).gate,
            Gate::Stale { .. }
        ));
        // And forgetting takes R's replaced words, never the lifted one.
        let standing = standing(&kept);
        assert!(standing.contains(kept[2].id.as_str()) && standing.contains(kept[3].id.as_str()));
        assert!(!standing.contains(kept[0].id.as_str()) && !standing.contains(kept[1].id.as_str()));
        let next = verdict("s", &["reviewer"], B, Word::None, 9);
        assert_eq!(
            to_forget(&kept, &next, kept.len()).unwrap(),
            [kept[0].id.clone()]
        );
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
        // Spoken and shown as pass/none; written as approve/comment, the
        // spelling retained clients decode; both read back.
        assert_eq!(serde_json::to_string(&Word::None).unwrap(), "\"comment\"");
        assert_eq!(serde_json::to_string(&Word::Pass).unwrap(), "\"approve\"");
        for (json, word) in [
            ("\"pass\"", Word::Pass),
            ("\"approve\"", Word::Pass),
            ("\"none\"", Word::None),
            ("\"comment\"", Word::None),
            ("\"changes\"", Word::Changes),
        ] {
            assert_eq!(serde_json::from_str::<Word>(json).unwrap(), word);
        }
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
        assert!(
            serde_json::to_string(&kept)
                .unwrap()
                .contains("\"approve\"")
        );
    }

    /// A client built before the vocabulary — its enum `approve`,
    /// `changes`, `comment` and its findings `p1`, `p2`, `p3` — decodes a
    /// verdict reply, a gate payload and a `code_verdict_given` event
    /// written now, so an old `--approve` reports what it did.
    #[test]
    fn a_retained_client_decodes_what_is_written_now() {
        #[derive(Debug, PartialEq, Deserialize)]
        #[serde(rename_all = "snake_case")]
        enum OldVerdict {
            Approve,
            Changes,
            Comment,
        }
        #[derive(Debug, Default, PartialEq, Deserialize)]
        struct OldFindings {
            #[serde(default)]
            p1: u32,
            #[serde(default)]
            p2: u32,
            #[serde(default)]
            p3: u32,
        }
        #[derive(Debug, Deserialize)]
        struct OldCodeVerdict {
            id: String,
            branch: String,
            head: String,
            by_name: String,
            #[serde(default)]
            roles: Vec<String>,
            verdict: OldVerdict,
            #[serde(default)]
            findings: OldFindings,
            note: String,
        }
        #[derive(Debug, Deserialize)]
        #[serde(tag = "state", rename_all = "snake_case")]
        enum OldGate {
            Approved { verdict: Box<OldCodeVerdict> },
            Changes { verdict: Box<OldCodeVerdict> },
            Stale { verdict: Box<OldCodeVerdict> },
            Missing,
        }
        #[derive(Debug, Deserialize)]
        #[serde(tag = "event", rename_all = "snake_case")]
        enum OldKind {
            CodeVerdictGiven {
                branch: String,
                head: String,
                by: AgentId,
                verdict: OldVerdict,
            },
        }
        let mut passed = verdict("r", &["reviewer"], A, Word::Pass, 0);
        passed.findings.nit = 2;
        let json = serde_json::to_value(&passed).unwrap();
        let old: OldCodeVerdict = serde_json::from_value(json).unwrap();
        assert_eq!(old.verdict, OldVerdict::Approve);
        assert_eq!(old.findings, OldFindings::default(), "counts it never had");
        assert_eq!(
            (
                old.id,
                old.branch,
                old.head,
                old.by_name,
                old.roles,
                old.note
            ),
            (
                passed.id.clone(),
                "feature/x".into(),
                A.into(),
                "r".into(),
                vec!["reviewer".into()],
                String::new()
            )
        );
        let withheld = verdict("r", &["reviewer"], A, Word::None, 1);
        let old: OldCodeVerdict =
            serde_json::from_value(serde_json::to_value(&withheld).unwrap()).unwrap();
        assert_eq!(old.verdict, OldVerdict::Comment);
        for (gate, state) in [
            (
                Gate::Approved {
                    verdict: Box::new(passed.clone()),
                },
                "approved",
            ),
            (
                Gate::Changes {
                    verdict: Box::new(verdict("r", &["reviewer"], A, Word::Changes, 2)),
                },
                "changes",
            ),
            (
                Gate::Stale {
                    verdict: Box::new(passed.clone()),
                },
                "stale",
            ),
            (Gate::Missing, "missing"),
        ] {
            let json = serde_json::to_value(&gate).unwrap();
            assert_eq!(json["state"], state);
            let old: OldGate = serde_json::from_value(json).unwrap();
            match (&gate, &old) {
                (Gate::Approved { .. }, OldGate::Approved { verdict })
                | (Gate::Stale { .. }, OldGate::Stale { verdict }) => {
                    assert_eq!(verdict.verdict, OldVerdict::Approve);
                }
                (Gate::Changes { .. }, OldGate::Changes { verdict }) => {
                    assert_eq!(verdict.verdict, OldVerdict::Changes);
                }
                (Gate::Missing, OldGate::Missing) => {}
                other => panic!("{other:?}"),
            }
        }
        let event = crate::EventKind::CodeVerdictGiven {
            project: ProjectId::from("p"),
            branch: "feature/x".into(),
            head: A.into(),
            by: AgentId::from("r"),
            verdict: Word::Pass,
            findings: passed.findings,
            runtime: "codex".into(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["verdict"], "approve");
        assert_eq!(json["findings"]["nit"], 2);
        let OldKind::CodeVerdictGiven {
            branch,
            head,
            by,
            verdict,
        } = serde_json::from_value(json).unwrap();
        assert_eq!(
            (branch.as_str(), head.as_str(), by.as_str(), verdict),
            ("feature/x", A, "r", OldVerdict::Approve)
        );
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
        // Every field before `by=` is one token, a runtime an agent
        // registered as free text included.
        let line = approved.line("feature/x", A);
        let (before, _) = line.split_once(" by=").unwrap();
        assert!(before.split(' ').all(|field| field.contains('=')));
        let mut odd = verdict("r", &["reviewer"], A, Word::Pass, 0);
        odd.runtime = "custom runtime\n=100%\t".into();
        let line = odd.line();
        assert_eq!(line.lines().count(), 1, "{line:?}");
        let fields: Vec<(&str, &str)> = line
            .split_once(" by=")
            .unwrap()
            .0
            .split(' ')
            .map(|field| field.split_once('=').unwrap())
            .collect();
        assert_eq!(
            fields.iter().find(|(key, _)| *key == "runtime").unwrap().1,
            "custom%20runtime%0A%3D100%25%09"
        );
        assert_eq!(fields.len(), 6, "{fields:?}");
        assert_eq!(token("codex"), "codex");
        assert_eq!(token("é b"), "é%20b");
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
