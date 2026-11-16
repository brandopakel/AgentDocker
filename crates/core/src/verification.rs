//! A local gate run (`scripts/verify.sh check`, `quick` or `test`) as its
//! runner reported it: the steps it ran, how each ended, the counts its own
//! artifacts established, and the checkout's head and state at its start
//! and end. It is the runner's report of its own run, kept as given: never
//! independent verification, and never proof of what every compiler and
//! test consumed. What it says of a head is only "reported against that
//! head, with clean boundaries".

use crate::{AgentId, ProjectId};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// The modes a run is recorded for: the gates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateMode {
    Check,
    Quick,
    Test,
}

impl std::fmt::Display for GateMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            GateMode::Check => "check",
            GateMode::Quick => "quick",
            GateMode::Test => "test",
        })
    }
}

/// A step, by what it ran.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepName {
    /// `scripts/docs_check.py`.
    Docs,
    /// `cargo fmt --check`.
    Fmt,
    /// The fuzz workspace's lockfile, resolved locked.
    FuzzLock,
    /// `scripts/touched_crates.py`: which packages a quick run covers.
    Selection,
    Clippy,
    Nextest,
    /// `cargo test --doc`.
    Doctests,
    /// The Python suite (`python3 -m unittest`).
    Installer,
    /// A command not in the table: the run is then never passed.
    Other,
}

impl std::fmt::Display for StepName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            StepName::Docs => "docs",
            StepName::Fmt => "fmt",
            StepName::FuzzLock => "fuzz_lock",
            StepName::Selection => "selection",
            StepName::Clippy => "clippy",
            StepName::Nextest => "nextest",
            StepName::Doctests => "doctests",
            StepName::Installer => "installer",
            StepName::Other => "other",
        })
    }
}

/// What a step's own artifact established. Each count is there only when
/// the artifact said it: a missing count is missing, never zero.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counts {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passed: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped: Option<u64>,
}

/// One step as it ran.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    /// Its place in the run, from 1.
    pub order: u32,
    pub name: StepName,
    /// The program of an [`StepName::Other`] step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program: Option<String>,
    pub started_at: DateTime<Utc>,
    /// Absent for a step that started and never ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<Utc>>,
    /// Its exit status, absent with `finished_at`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counts: Option<Counts>,
}

/// The checkout's state at one end of the run, as Git said it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TreeStatus {
    Clean,
    Dirty,
    /// Git could not be read: never taken as clean.
    Unknown,
}

/// The head and state at one end of the run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Boundary {
    /// The full head, absent when Git could not be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    pub status: TreeStatus,
}

/// What a quick run covered: measured against `base`, these packages, and
/// these for doctests. Either may be empty.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuickScope {
    /// The full commit the change was measured against.
    pub base: String,
    pub packages: Vec<String>,
    pub doc_packages: Vec<String>,
}

/// What the runner reports, kept exactly as given.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateRunReport {
    /// The run's own key, made at its start: the same run reported twice
    /// is recorded once.
    pub run_key: String,
    /// The folder it ran in.
    pub checkout: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    pub mode: GateMode,
    /// A quick run's coverage; absent for the others.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<QuickScope>,
    pub start: Boundary,
    pub end: Boundary,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    /// The run's own exit status.
    pub exit: i32,
    /// Whether the mode reached its last line.
    pub completed: bool,
    pub steps: Vec<Step>,
}

/// A recorded run: the report, and what the daemon knew of it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateRun {
    /// Sortable by when it was recorded.
    pub id: String,
    pub project: ProjectId,
    pub report: GateRunReport,
    /// Who asked for the run, as the daemon resolved the requester; absent
    /// when nobody could be, and never the person by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<AgentId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by_name: Option<String>,
    /// The temporary agent a run registered to hold the build slot when
    /// nobody was known: the runner, never the author.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner: Option<AgentId>,
    /// When the daemon received it, apart from the runner's own times.
    pub received_at: DateTime<Utc>,
}

/// What the two boundaries say of the tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tree {
    /// The same head, clean at the start and at the end: the run was
    /// reported against that head with clean boundaries, which says
    /// nothing of a change made and undone between them.
    CleanBoundaries,
    /// Uncommitted changes at the start.
    Dirty,
    /// A change was observed between the start and the end.
    Changed,
    /// Git could not be read at one end.
    Unknown,
}

pub fn tree(start: &Boundary, end: &Boundary) -> Tree {
    let (Some(first), Some(last)) = (&start.head, &end.head) else {
        return Tree::Unknown;
    };
    match (start.status, end.status) {
        (TreeStatus::Unknown, _) | (_, TreeStatus::Unknown) => Tree::Unknown,
        (TreeStatus::Dirty, _) => Tree::Dirty,
        _ if first != last => Tree::Changed,
        (TreeStatus::Clean, TreeStatus::Clean) => Tree::CleanBoundaries,
        (TreeStatus::Clean, TreeStatus::Dirty) => Tree::Changed,
    }
}

/// How a run ended, from its steps, its required list, its exit and its
/// completion. Exactly one holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Passed,
    Failed,
    Incomplete,
}

impl Outcome {
    /// The outcome as a word.
    pub fn word(self) -> &'static str {
        match self {
            Outcome::Passed => "passed",
            Outcome::Failed => "failed",
            Outcome::Incomplete => "incomplete",
        }
    }
}

/// The status `verify.sh` exits with when it lost the build slot.
pub const LOST: i32 = 75;

/// A status that says the run or a step was stopped, not that it failed:
/// the slot lost, or a signal.
fn stopped(status: i32) -> bool {
    status == LOST || status >= 128
}

/// The steps a mode must run, in order; for a quick run, by its scope.
/// `None` when a quick run reported no scope.
pub fn required(mode: GateMode, scope: Option<&QuickScope>) -> Option<Vec<StepName>> {
    use StepName::*;
    Some(match mode {
        GateMode::Check => vec![Docs, Fmt, FuzzLock, Clippy, Nextest, Doctests, Installer],
        GateMode::Test => vec![Nextest, Doctests],
        GateMode::Quick => {
            let scope = scope?;
            let mut steps = vec![Docs, Fmt, FuzzLock, Selection];
            if !scope.packages.is_empty() {
                steps.extend([Clippy, Nextest]);
                if !scope.doc_packages.is_empty() {
                    steps.push(Doctests);
                }
            }
            steps.push(Installer);
            steps
        }
    })
}

/// The run's outcome. A stopped run (the slot lost, a signal, a step that
/// never ended) is incomplete before anything else; then a failed step,
/// or a failure with no step failing, is failed; a run that exited 0 is
/// passed only when it completed, ran nothing outside the table, and ran
/// exactly its required steps, in order, each with status 0.
pub fn outcome(report: &GateRunReport) -> Outcome {
    let steps = &report.steps;
    if stopped(report.exit) || steps.iter().any(|step| step.status.is_none_or(stopped)) {
        return Outcome::Incomplete;
    }
    if steps.iter().any(|step| step.status != Some(0)) || report.exit != 0 {
        return Outcome::Failed;
    }
    let Some(required) = required(report.mode, report.scope.as_ref()) else {
        return Outcome::Incomplete;
    };
    let ran: Vec<StepName> = steps.iter().map(|step| step.name).collect();
    if !report.completed || ran != required {
        return Outcome::Incomplete;
    }
    Outcome::Passed
}

/// How one step stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepState {
    Passed,
    Failed,
    /// Stopped: the slot lost, or a signal.
    Lost,
    /// Started and never ended.
    Incomplete,
    /// Required, and never reached.
    NotReached,
    /// Left out by a quick run's scope: not the same as never reached.
    OutOfScope,
}

/// One step for display: recorded, never reached, or out of scope.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepView {
    pub name: StepName,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program: Option<String>,
    pub state: StepState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counts: Option<Counts>,
}

/// The steps as they stand: those recorded, in order; then each required
/// one never reached; then a quick run's Cargo steps its scope left out.
pub fn steps(report: &GateRunReport) -> Vec<StepView> {
    let mut views: Vec<StepView> = report
        .steps
        .iter()
        .map(|step| StepView {
            name: step.name,
            program: step.program.clone(),
            state: match step.status {
                None => StepState::Incomplete,
                Some(0) => StepState::Passed,
                Some(status) if stopped(status) => StepState::Lost,
                Some(_) => StepState::Failed,
            },
            counts: step.counts.clone(),
        })
        .collect();
    let required = required(report.mode, report.scope.as_ref()).unwrap_or_default();
    for name in &required {
        if !report.steps.iter().any(|step| step.name == *name) {
            views.push(StepView {
                name: *name,
                program: None,
                state: StepState::NotReached,
                counts: None,
            });
        }
    }
    if report.mode == GateMode::Quick && report.scope.is_some() {
        use StepName::*;
        for name in [Clippy, Nextest, Doctests] {
            if !required.contains(&name) && !report.steps.iter().any(|step| step.name == name) {
                views.push(StepView {
                    name,
                    program: None,
                    state: StepState::OutOfScope,
                    counts: None,
                });
            }
        }
    }
    views
}

/// How far a reported time may run past the daemon's own clock.
pub const SKEW: i64 = 60;

/// The most steps a run reports.
pub const MAX_STEPS: usize = 64;

/// Why a report cannot be recorded, if it cannot.
pub fn validate(report: &GateRunReport, received_at: DateTime<Utc>) -> Result<(), String> {
    let key = &report.run_key;
    if !(8..=64).contains(&key.len())
        || !key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("a run key is 8 to 64 letters, digits, `-` or `_`".into());
    }
    if !report.checkout.is_absolute() {
        return Err("a run's checkout is an absolute path".into());
    }
    if let Some(branch) = &report.branch
        && (branch.is_empty() || branch.len() > 255 || branch.contains(['\n', '\r', '\0']))
    {
        return Err("a branch is one line of at most 255 bytes".into());
    }
    for boundary in [&report.start, &report.end] {
        if let Some(head) = &boundary.head
            && !crate::gate::full_commit(head)
        {
            return Err(format!("{head} is not a full commit"));
        }
    }
    let skew = Duration::seconds(SKEW);
    if report.started_at > report.finished_at {
        return Err("a run finishes after it starts".into());
    }
    if report.finished_at > received_at + skew {
        return Err("a run finished after it was received".into());
    }
    match (report.mode, &report.scope) {
        (GateMode::Quick, Some(scope)) => {
            if !crate::gate::full_commit(&scope.base) {
                return Err(format!(
                    "a quick run's base {} is not a full commit",
                    scope.base
                ));
            }
            for package in scope.packages.iter().chain(&scope.doc_packages) {
                if package.is_empty()
                    || package.len() > 200
                    || package.contains(char::is_whitespace)
                {
                    return Err(format!("{package:?} is not a package"));
                }
            }
            if scope.packages.len() + scope.doc_packages.len() > 400 {
                return Err("a quick run's scope names at most 400 packages".into());
            }
        }
        (GateMode::Quick, None) => {}
        (_, Some(_)) => return Err("only a quick run has a scope".into()),
        (_, None) => {}
    }
    if report.steps.len() > MAX_STEPS {
        return Err(format!("a run reports at most {MAX_STEPS} steps"));
    }
    let mut seen = Vec::new();
    for (index, step) in report.steps.iter().enumerate() {
        if step.order as usize != index + 1 {
            return Err("steps are numbered from 1, in order".into());
        }
        if step.started_at < report.started_at - skew || step.started_at > report.finished_at + skew
        {
            return Err(format!("step {} started outside its run", step.order));
        }
        match (step.finished_at, step.status) {
            (Some(finished), Some(_)) => {
                if finished < step.started_at || finished > report.finished_at + skew {
                    return Err(format!("step {} ended outside its run", step.order));
                }
            }
            (None, None) => {}
            _ => {
                return Err(format!(
                    "step {} has an end without a status, or one without the other",
                    step.order
                ));
            }
        }
        match (step.name, &step.program) {
            (StepName::Other, Some(program)) if !program.is_empty() && program.len() <= 200 => {}
            (StepName::Other, _) => return Err("an other step names its program".into()),
            (_, Some(_)) => return Err("only an other step names its program".into()),
            (name, None) => {
                if seen.contains(&name) {
                    return Err(format!("{name} is reported twice"));
                }
                seen.push(name);
            }
        }
        if let Some(counts) = &step.counts {
            if !matches!(step.name, StepName::Nextest | StepName::Installer) {
                return Err(format!("{} reports no counts", step.name));
            }
            let parts = counts.passed.unwrap_or(0)
                + counts.failed.unwrap_or(0)
                + counts.skipped.unwrap_or(0);
            if counts.run.is_some_and(|run| parts > run) {
                return Err(format!(
                    "step {}'s counts add up to more than it ran",
                    step.order
                ));
            }
            if step.status == Some(0) && counts.failed.is_some_and(|failed| failed > 0) {
                return Err(format!("step {} passed with failures", step.order));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + seconds, 0).unwrap()
    }

    fn clean(head: &str) -> Boundary {
        Boundary {
            head: Some(head.into()),
            status: TreeStatus::Clean,
        }
    }

    fn step(order: u32, name: StepName, status: Option<i32>) -> Step {
        Step {
            order,
            name,
            program: (name == StepName::Other).then(|| "make".to_owned()),
            started_at: at(order as i64),
            finished_at: status.map(|_| at(order as i64 + 1)),
            status,
            counts: None,
        }
    }

    fn report(mode: GateMode, names: &[StepName], scope: Option<QuickScope>) -> GateRunReport {
        GateRunReport {
            run_key: "run-key-0001".into(),
            checkout: "/work/app".into(),
            branch: Some("feature".into()),
            mode,
            scope,
            start: clean(A),
            end: clean(A),
            started_at: at(0),
            finished_at: at(100),
            exit: 0,
            completed: true,
            steps: names
                .iter()
                .enumerate()
                .map(|(index, name)| step(index as u32 + 1, *name, Some(0)))
                .collect(),
        }
    }

    fn scope(packages: &[&str], docs: &[&str]) -> Option<QuickScope> {
        Some(QuickScope {
            base: B.into(),
            packages: packages.iter().map(|p| (*p).to_owned()).collect(),
            doc_packages: docs.iter().map(|p| (*p).to_owned()).collect(),
        })
    }

    use StepName::*;
    const CHECK: &[StepName] = &[Docs, Fmt, FuzzLock, Clippy, Nextest, Doctests, Installer];

    /// Exactly one outcome holds, a stop before a failure, and only a whole
    /// required run that completed and exited 0 passes.
    #[test]
    fn a_run_has_one_outcome() {
        assert_eq!(
            outcome(&report(GateMode::Check, CHECK, None)),
            Outcome::Passed
        );
        assert_eq!(
            outcome(&report(GateMode::Test, &[Nextest, Doctests], None)),
            Outcome::Passed
        );
        // A successful empty quick: its Cargo steps out of scope.
        let empty = report(
            GateMode::Quick,
            &[Docs, Fmt, FuzzLock, Selection, Installer],
            scope(&[], &[]),
        );
        assert_eq!(outcome(&empty), Outcome::Passed);
        assert_eq!(
            steps(&empty)
                .iter()
                .filter(|view| view.state == StepState::OutOfScope)
                .count(),
            3
        );
        // A quick with packages and no doctest packages.
        let some = report(
            GateMode::Quick,
            &[Docs, Fmt, FuzzLock, Selection, Clippy, Nextest, Installer],
            scope(&["agentd"], &[]),
        );
        assert_eq!(outcome(&some), Outcome::Passed);
        // A quick that reports no scope, or no selection step, never passes.
        let mut unscoped = empty.clone();
        unscoped.scope = None;
        assert_eq!(outcome(&unscoped), Outcome::Incomplete);
        let unselected = report(
            GateMode::Quick,
            &[Docs, Fmt, FuzzLock, Installer],
            scope(&[], &[]),
        );
        assert_eq!(outcome(&unselected), Outcome::Incomplete);
        // A failure before any step: a quick with no merge base.
        let mut before = report(GateMode::Quick, &[], None);
        before.exit = 1;
        before.completed = false;
        assert_eq!(outcome(&before), Outcome::Failed);
        // fuzz_lock and selection failures are failures.
        for failing in [FuzzLock, Selection] {
            let mut run = report(
                GateMode::Quick,
                &[Docs, Fmt, FuzzLock, Selection],
                scope(&[], &[]),
            );
            let position = run.steps.iter().position(|s| s.name == failing).unwrap();
            run.steps.truncate(position + 1);
            run.steps[position].status = Some(1);
            run.exit = 1;
            run.completed = false;
            assert_eq!(outcome(&run), Outcome::Failed, "{failing}");
            assert!(
                steps(&run)
                    .iter()
                    .any(|view| view.state == StepState::NotReached)
            );
        }
        // The slot lost between steps: exit 75 with no step running.
        let mut lost = report(GateMode::Check, &CHECK[..3], None);
        lost.exit = LOST;
        lost.completed = false;
        assert_eq!(outcome(&lost), Outcome::Incomplete);
        // A signal after a completed step, even beside a failed one.
        let mut signalled = report(GateMode::Check, &CHECK[..4], None);
        signalled.steps[1].status = Some(1);
        signalled.exit = 143;
        signalled.completed = false;
        assert_eq!(outcome(&signalled), Outcome::Incomplete);
        assert_eq!(
            steps(&signalled)[1].state,
            StepState::Failed,
            "a known failure is kept"
        );
        // A step that never ended.
        let mut unended = report(GateMode::Test, &[Nextest], None);
        unended.steps[0].status = None;
        unended.steps[0].finished_at = None;
        unended.exit = 1;
        assert_eq!(outcome(&unended), Outcome::Incomplete);
        // Exit 0 with an other step, or a required step missing, or no
        // completion: never passed.
        let mut other = report(GateMode::Test, &[Nextest, Doctests, Other], None);
        other.exit = 0;
        assert_eq!(outcome(&other), Outcome::Incomplete);
        let missing = report(GateMode::Check, &CHECK[..6], None);
        assert_eq!(outcome(&missing), Outcome::Incomplete);
        let mut uncompleted = report(GateMode::Check, CHECK, None);
        uncompleted.completed = false;
        assert_eq!(outcome(&uncompleted), Outcome::Incomplete);
        // A step that failed and a run that says 0: failed, not passed.
        let mut contradictory = report(GateMode::Check, CHECK, None);
        contradictory.steps[4].status = Some(100);
        assert_eq!(outcome(&contradictory), Outcome::Failed);
    }

    #[test]
    fn the_boundaries_say_what_they_saw() {
        let dirty = |head: &str| Boundary {
            head: Some(head.into()),
            status: TreeStatus::Dirty,
        };
        let unknown = Boundary {
            head: None,
            status: TreeStatus::Unknown,
        };
        assert_eq!(tree(&clean(A), &clean(A)), Tree::CleanBoundaries);
        assert_eq!(tree(&clean(A), &clean(B)), Tree::Changed);
        assert_eq!(tree(&clean(A), &dirty(A)), Tree::Changed);
        assert_eq!(tree(&dirty(A), &dirty(A)), Tree::Dirty);
        assert_eq!(tree(&dirty(A), &clean(A)), Tree::Dirty);
        assert_eq!(tree(&clean(A), &unknown), Tree::Unknown);
        let unread = Boundary {
            head: Some(A.into()),
            status: TreeStatus::Unknown,
        };
        assert_eq!(tree(&unread, &clean(A)), Tree::Unknown);
    }

    #[test]
    fn only_a_well_formed_report_is_recorded() {
        let now = at(200);
        assert_eq!(validate(&report(GateMode::Check, CHECK, None), now), Ok(()));
        let quick = report(
            GateMode::Quick,
            &[Docs, Fmt, FuzzLock, Selection, Installer],
            scope(&[], &[]),
        );
        assert_eq!(validate(&quick, now), Ok(()));
        type Change = Box<dyn Fn(&mut GateRunReport)>;
        let bad: Vec<(&str, Change)> = vec![
            ("short key", Box::new(|r| r.run_key = "k".into())),
            (
                "relative checkout",
                Box::new(|r| r.checkout = "work".into()),
            ),
            (
                "short head",
                Box::new(|r| r.start.head = Some("abc".into())),
            ),
            ("backwards", Box::new(|r| r.finished_at = at(-1))),
            ("from the future", Box::new(|r| r.finished_at = at(400))),
            ("scope on check", Box::new(|r| r.scope = scope(&[], &[]))),
            ("numbering", Box::new(|r| r.steps[1].order = 5)),
            ("twice", Box::new(|r| r.steps[1].name = Docs)),
            ("other unnamed", Box::new(|r| r.steps[1].name = Other)),
            (
                "program on a named step",
                Box::new(|r| r.steps[1].program = Some("x".into())),
            ),
            ("end without status", Box::new(|r| r.steps[1].status = None)),
            (
                "step before its run",
                Box::new(|r| r.steps[0].started_at = at(-500)),
            ),
            (
                "counts on clippy",
                Box::new(|r| r.steps[3].counts = Some(Counts::default())),
            ),
            (
                "more than ran",
                Box::new(|r| {
                    r.steps[4].counts = Some(Counts {
                        run: Some(2),
                        passed: Some(3),
                        ..Counts::default()
                    })
                }),
            ),
            (
                "passed with failures",
                Box::new(|r| {
                    r.steps[4].counts = Some(Counts {
                        failed: Some(1),
                        ..Counts::default()
                    })
                }),
            ),
        ];
        for (case, change) in bad {
            let mut run = report(GateMode::Check, CHECK, None);
            change(&mut run);
            assert!(validate(&run, now).is_err(), "{case}");
        }
        let mut short_base = quick.clone();
        short_base.scope.as_mut().unwrap().base = "abc".into();
        assert!(validate(&short_base, now).is_err());
    }

    /// A report reads back exactly as written; a missing count stays absent.
    #[test]
    fn a_report_round_trips_with_missing_counts_absent() {
        let mut run = report(GateMode::Check, CHECK, None);
        run.steps[4].counts = Some(Counts {
            run: Some(10),
            passed: Some(10),
            failed: Some(0),
            skipped: None,
        });
        let text = serde_json::to_string(&run).unwrap();
        assert!(!text.contains("skipped"), "{text}");
        assert_eq!(serde_json::from_str::<GateRunReport>(&text).unwrap(), run);
    }
}
