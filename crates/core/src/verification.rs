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
    /// The fuzz targets, compiled (`cargo check` on the fuzz workspace).
    FuzzBuild,
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
    /// A step name this build does not know, read from a record a newer
    /// one wrote: never passed, and never a reason to refuse the record.
    #[serde(other)]
    Unknown,
}

impl std::fmt::Display for StepName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            StepName::Docs => "docs",
            StepName::Fmt => "fmt",
            StepName::FuzzLock => "fuzz_lock",
            StepName::FuzzBuild => "fuzz_build",
            StepName::Selection => "selection",
            StepName::Clippy => "clippy",
            StepName::Nextest => "nextest",
            StepName::Doctests => "doctests",
            StepName::Installer => "installer",
            StepName::Other => "other",
            StepName::Unknown => "unknown",
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
    /// Whether the change reaches the fuzz targets (a package they depend
    /// on, directly or not, or a file under `fuzz/`), so the run had to
    /// compile them. Absent from a report written before it was kept;
    /// from gate version 2 its absence leaves the run incomplete.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fuzz: Option<bool>,
}

/// What the runner reports, kept exactly as given.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateRunReport {
    /// Which `verify.sh` wrote it, by the steps its modes require: 1
    /// before the fuzz targets were compiled, 2 since. A report keeps the
    /// required list of its own version.
    #[serde(default = "first_gate_version")]
    pub gate_version: u32,
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

/// The gate version a report written before it was kept is.
fn first_gate_version() -> u32 {
    1
}

/// The newest gate version this build knows.
pub const GATE_VERSION: u32 = 2;

/// The status `verify.sh` exits with when it lost the build slot.
pub const LOST: i32 = 75;

/// A status that says the run or a step was stopped, not that it failed:
/// the slot lost, or a signal.
fn stopped(status: i32) -> bool {
    status == LOST || status >= 128
}

/// The steps a mode must run, in order, by the report's gate version;
/// for a quick run, by its scope. `None` when a quick run reported no
/// scope, or, from version 2, no word on the fuzz targets, or when the
/// version is newer than this build knows.
pub fn required(mode: GateMode, scope: Option<&QuickScope>, version: u32) -> Option<Vec<StepName>> {
    use StepName::*;
    if !(1..=GATE_VERSION).contains(&version) {
        return None;
    }
    let fuzz = version >= 2;
    Some(match mode {
        GateMode::Check if fuzz => {
            vec![
                Docs, Fmt, FuzzLock, FuzzBuild, Clippy, Nextest, Doctests, Installer,
            ]
        }
        GateMode::Check => vec![Docs, Fmt, FuzzLock, Clippy, Nextest, Doctests, Installer],
        GateMode::Test => vec![Nextest, Doctests],
        GateMode::Quick => {
            let scope = scope?;
            let mut steps = vec![Docs, Fmt, FuzzLock, Selection];
            if fuzz && scope.fuzz? {
                steps.push(FuzzBuild);
            }
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
    let Some(required) = required(report.mode, report.scope.as_ref(), report.gate_version) else {
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
    /// Not run, and whether it had to be cannot be told: a quick report
    /// with no word on the fuzz targets.
    Unknown,
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
    let ran = |name: StepName| report.steps.iter().any(|step| step.name == name);
    let view = |name, state| StepView {
        name,
        program: None,
        state,
        counts: None,
    };
    // Only against a required list that can be told: never for a version
    // this build does not know, nor a quick run with no word on the fuzz
    // targets.
    if let Some(required) = required(report.mode, report.scope.as_ref(), report.gate_version) {
        for name in required {
            if !ran(name) {
                views.push(view(name, StepState::NotReached));
            }
        }
    }
    // What a quick run's selection established it left out, and nothing
    // more: Cargo steps for an empty package selection (doctests too for
    // an empty doctest selection), the fuzz targets for a `false` word.
    let known = (1..=GATE_VERSION).contains(&report.gate_version);
    if let (GateMode::Quick, Some(scope), true) = (report.mode, &report.scope, known) {
        use StepName::*;
        let mut left: Vec<(StepName, StepState)> = Vec::new();
        if report.gate_version >= 2 {
            match scope.fuzz {
                Some(false) => left.push((FuzzBuild, StepState::OutOfScope)),
                None => left.push((FuzzBuild, StepState::Unknown)),
                Some(true) => {}
            }
        }
        if scope.packages.is_empty() {
            left.extend([Clippy, Nextest, Doctests].map(|name| (name, StepState::OutOfScope)));
        } else if scope.doc_packages.is_empty() {
            left.push((Doctests, StepState::OutOfScope));
        }
        for (name, state) in left {
            if !ran(name) {
                views.push(view(name, state));
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
    if !(1..=GATE_VERSION).contains(&report.gate_version) {
        return Err(format!(
            "gate version {} is not one this daemon knows",
            report.gate_version
        ));
    }
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
            (StepName::Unknown, _) => return Err("a runner reports only the steps it names".into()),
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
            let parts = [counts.passed, counts.failed, counts.skipped]
                .into_iter()
                .flatten()
                .try_fold(0u64, u64::checked_add)
                .ok_or_else(|| format!("step {}'s counts cannot add up", step.order))?;
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
            gate_version: 1,
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
            fuzz: None,
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

    /// From gate version 2 a check run compiles the fuzz targets, and a
    /// quick run says whether its change reached them; a version-1 report
    /// keeps its own required list.
    #[test]
    fn the_fuzz_targets_are_required_by_version_and_scope() {
        const CHECK_2: &[StepName] = &[
            Docs, Fmt, FuzzLock, FuzzBuild, Clippy, Nextest, Doctests, Installer,
        ];
        let at = |version: u32, mut run: GateRunReport| {
            run.gate_version = version;
            outcome(&run)
        };
        assert_eq!(at(1, report(GateMode::Check, CHECK, None)), Outcome::Passed);
        assert_eq!(
            at(2, report(GateMode::Check, CHECK, None)),
            Outcome::Incomplete
        );
        assert_eq!(
            at(2, report(GateMode::Check, CHECK_2, None)),
            Outcome::Passed
        );
        let quick = |fuzz: Option<bool>, names: &[StepName], packages: &[&str]| {
            let mut scope = scope(packages, &[]);
            scope.as_mut().unwrap().fuzz = fuzz;
            report(GateMode::Quick, names, scope)
        };
        const QUICK: &[StepName] = &[Docs, Fmt, FuzzLock, Selection, Installer];
        const QUICK_FUZZ: &[StepName] = &[Docs, Fmt, FuzzLock, Selection, FuzzBuild, Installer];
        // No word on the fuzz targets: never passed from version 2.
        assert_eq!(at(2, quick(None, QUICK, &[])), Outcome::Incomplete);
        assert_eq!(at(1, quick(None, QUICK, &[])), Outcome::Passed);
        // Not reached: the step is out of scope.
        let unreached = quick(Some(false), QUICK, &[]);
        let mut seen = unreached.clone();
        seen.gate_version = 2;
        assert_eq!(outcome(&seen), Outcome::Passed);
        assert!(
            steps(&seen)
                .iter()
                .any(|view| view.name == FuzzBuild && view.state == StepState::OutOfScope)
        );
        // A change under fuzz/ alone: required with an empty selection.
        assert_eq!(at(2, quick(Some(true), QUICK_FUZZ, &[])), Outcome::Passed);
        assert_eq!(at(2, quick(Some(true), QUICK, &[])), Outcome::Incomplete);
        // A version this build does not know reads incomplete.
        assert_eq!(
            at(3, report(GateMode::Check, CHECK_2, None)),
            Outcome::Incomplete
        );
        let mut newer = report(GateMode::Check, CHECK_2, None);
        newer.gate_version = 3;
        assert!(validate(&newer, at_time(200)).is_err());
    }

    fn at_time(seconds: i64) -> DateTime<Utc> {
        at(seconds)
    }

    /// What is shown claims no exclusion the selection did not establish:
    /// a quick run with no fuzz word and packages selected lists the fuzz
    /// targets as unknown and no Cargo step as out of scope; an unknown
    /// version lists only what ran.
    #[test]
    fn only_established_exclusions_are_shown() {
        let state = |run: &GateRunReport, name: StepName| {
            steps(run)
                .into_iter()
                .find(|view| view.name == name)
                .map(|view| view.state)
        };
        let mut unworded = report(
            GateMode::Quick,
            &[Docs, Fmt, FuzzLock, Selection, Clippy, Nextest, Installer],
            scope(&["agentd"], &[]),
        );
        unworded.gate_version = 2;
        assert_eq!(outcome(&unworded), Outcome::Incomplete);
        assert_eq!(state(&unworded, FuzzBuild), Some(StepState::Unknown));
        assert_eq!(state(&unworded, Clippy), Some(StepState::Passed));
        assert_eq!(
            state(&unworded, Doctests),
            Some(StepState::OutOfScope),
            "no doctest selection"
        );
        assert!(
            !steps(&unworded)
                .iter()
                .any(|view| view.state == StepState::NotReached)
        );
        // An empty selection with no word: the Cargo steps are out of
        // scope, the fuzz targets unknown.
        let mut empty = report(
            GateMode::Quick,
            &[Docs, Fmt, FuzzLock, Selection, Installer],
            scope(&[], &[]),
        );
        empty.gate_version = 2;
        assert_eq!(state(&empty, Nextest), Some(StepState::OutOfScope));
        assert_eq!(state(&empty, FuzzBuild), Some(StepState::Unknown));
        // An unknown version: only what ran.
        let mut newer = report(GateMode::Check, &[Docs, Fmt], None);
        newer.gate_version = 3;
        assert_eq!(steps(&newer).len(), 2);
        let mut newer_quick = empty.clone();
        newer_quick.gate_version = 3;
        assert_eq!(steps(&newer_quick).len(), 5);
    }

    /// A step name a newer build wrote reads as unknown, never a failure to
    /// read the record, and never passed; a runner may not report one.
    #[test]
    fn an_unknown_step_name_is_read_and_never_passed() {
        let mut run = report(GateMode::Test, &[Nextest, Doctests], None);
        let mut text = serde_json::to_value(&run).unwrap();
        text["steps"][1]["name"] = "telepathy".into();
        let read: GateRunReport = serde_json::from_value(text).unwrap();
        assert_eq!(read.steps[1].name, StepName::Unknown);
        assert_eq!(outcome(&read), Outcome::Incomplete);
        run.steps[1].name = StepName::Unknown;
        assert!(validate(&run, at(200)).is_err());
        // A report written before the version was kept is version 1.
        let mut old =
            serde_json::to_value(report(GateMode::Test, &[Nextest, Doctests], None)).unwrap();
        old.as_object_mut().unwrap().remove("gate_version");
        let old: GateRunReport = serde_json::from_value(old).unwrap();
        assert_eq!(old.gate_version, 1);
        assert_eq!(outcome(&old), Outcome::Passed);
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
                "counts that overflow",
                Box::new(|r| {
                    r.steps[4].counts = Some(Counts {
                        run: Some(0),
                        passed: Some(u64::MAX),
                        failed: Some(0),
                        skipped: Some(1),
                    })
                }),
            ),
            (
                "counts that overflow with no run",
                Box::new(|r| {
                    r.steps[4].counts = Some(Counts {
                        passed: Some(u64::MAX),
                        skipped: Some(1),
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
