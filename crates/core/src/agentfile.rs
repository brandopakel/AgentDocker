//! The order an `Agentfile.toml` asks for, decided without I/O.
//!
//! An entry waits on others in two ways. `depends_on = ["db"]` waits until
//! `db` has been started. `after = "migrate exits 0"` waits until `migrate`
//! has ended with status zero, and when `migrate` ends any other way — a
//! nonzero status, a signal, a stop, a start that failed — the entry is
//! never started, and neither is anything that waits on it, however far
//! down. Checking a file ([`check`]), ordering it ([`order`]), telling when
//! an agent has ended for good ([`Ending::of`]) and deciding whether an
//! entry starts, waits or never will ([`readiness`]) are all here, so each
//! rule is a unit test; `agentdocker up` does the I/O.

use std::collections::{BTreeMap, HashMap};
use std::fmt;

use crate::event::EventKind;
use crate::{AgentId, AgentRecord, AgentStatus, RestartPolicy};

/// How a condition is written, for the sentences that say one is not.
pub const AFTER_FORM: &str = "write it as \"<name> exits 0\"";

/// `A exits 0`: start once `A` has ended with status zero.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct After {
    /// The entry waited on.
    pub name: String,
}

impl After {
    /// Read `<name> exits 0`. The name is everything before the last two
    /// words, so a name with a space in it still reads; only status zero
    /// can be waited for, because success is the only ending something
    /// else can sensibly be built on.
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        let mut words = text.split_whitespace();
        match (words.next(), words.next(), words.next()) {
            (None, ..) => return Err(format!("it is empty; {AFTER_FORM}")),
            (Some("exits"), _, None) => return Err(format!("it names no entry; {AFTER_FORM}")),
            (_, _, None) => return Err(format!("it is not a condition; {AFTER_FORM}")),
            _ => {}
        }
        // Three words or more, so both splits find a space.
        let (rest, status) = text.rsplit_once(char::is_whitespace).unwrap_or((text, ""));
        let (name, verb) = rest
            .trim_end()
            .rsplit_once(char::is_whitespace)
            .unwrap_or(("", rest));
        if verb != "exits" {
            return Err(format!("`{verb}` is not `exits`; {AFTER_FORM}"));
        }
        if status != "0" {
            return Err(if status.parse::<i64>().is_ok() {
                format!("only exit status 0 can be waited for, not {status}")
            } else {
                format!("`{status}` is not an exit status; {AFTER_FORM}")
            });
        }
        let name = name.trim_end();
        Ok(Self {
            name: name.to_owned(),
        })
    }
}

impl fmt::Display for After {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} exits 0", self.name)
    }
}

/// One entry's ordering, as the file gives it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    /// Started before this one.
    pub depends_on: Vec<String>,
    /// Ended with status zero before this one starts.
    pub after: Vec<After>,
    pub restart: RestartPolicy,
}

impl Entry {
    /// Every name this entry waits on, `depends_on` first.
    fn waits(&self) -> impl Iterator<Item = &str> {
        self.depends_on
            .iter()
            .map(String::as_str)
            .chain(self.after.iter().map(|after| after.name.as_str()))
    }
}

/// Which of the two ways an entry waits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wait {
    DependsOn,
    After,
}

/// What is wrong with a file's ordering, said with the entry it is on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OrderError {
    /// An entry waits on itself.
    Itself { name: String, wait: Wait },
    /// An entry waits on a name the file does not have.
    Unknown {
        name: String,
        needed: String,
        wait: Wait,
    },
    /// Entries wait on each other in a ring: the names round it, the
    /// first repeated at the end.
    Cycle(Vec<String>),
    /// `after` names an entry that `restart = "always"` starts again
    /// whenever it exits, so the wait would never end.
    NeverEnds { name: String, needed: String },
}

impl fmt::Display for OrderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Itself {
                name,
                wait: Wait::DependsOn,
            } => write!(f, "agent `{name}` depends on itself"),
            Self::Itself {
                name,
                wait: Wait::After,
            } => write!(
                f,
                "agent `{name}` has after = \"{name} exits 0\", which waits on itself"
            ),
            Self::Unknown {
                name,
                needed,
                wait: Wait::DependsOn,
            } => write!(
                f,
                "agent `{name}` depends on `{needed}`, which is not in this file"
            ),
            Self::Unknown {
                name,
                needed,
                wait: Wait::After,
            } => write!(
                f,
                "agent `{name}` has after = \"{needed} exits 0\", but `{needed}` is not in this file"
            ),
            Self::Cycle(ring) => write!(
                f,
                "agents wait on each other in a cycle: {}",
                ring.join(" → ")
            ),
            Self::NeverEnds { name, needed } => write!(
                f,
                "agent `{name}` has after = \"{needed} exits 0\", but `{needed}` has \
                 restart = \"always\" and is started again every time it exits"
            ),
        }
    }
}

impl std::error::Error for OrderError {}

/// Refuse an ordering that could only end in a wait that never ends: an
/// entry waiting on itself or on a name the file does not have, a ring of
/// entries waiting on each other (through `depends_on`, `after` or both),
/// or an `after` on an entry that is restarted whatever it exits with.
/// The first problem in file order is the one reported.
pub fn check(entries: &[Entry]) -> Result<(), OrderError> {
    let index = index(entries);
    for entry in entries {
        let name = &entry.name;
        for needed in &entry.depends_on {
            if needed == name {
                return Err(OrderError::Itself {
                    name: name.clone(),
                    wait: Wait::DependsOn,
                });
            }
            if !index.contains_key(needed.as_str()) {
                return Err(OrderError::Unknown {
                    name: name.clone(),
                    needed: needed.clone(),
                    wait: Wait::DependsOn,
                });
            }
        }
        for after in &entry.after {
            if &after.name == name {
                return Err(OrderError::Itself {
                    name: name.clone(),
                    wait: Wait::After,
                });
            }
            let Some(&needed) = index.get(after.name.as_str()) else {
                return Err(OrderError::Unknown {
                    name: name.clone(),
                    needed: after.name.clone(),
                    wait: Wait::After,
                });
            };
            if entries[needed].restart == RestartPolicy::Always {
                return Err(OrderError::NeverEnds {
                    name: name.clone(),
                    needed: after.name.clone(),
                });
            }
        }
    }
    match cycle(entries, &index) {
        Some(ring) => Err(OrderError::Cycle(ring)),
        None => Ok(()),
    }
}

fn index(entries: &[Entry]) -> HashMap<&str, usize> {
    entries
        .iter()
        .enumerate()
        .map(|(i, entry)| (entry.name.as_str(), i))
        .collect()
}

/// The first ring of waits, found depth-first from each entry in file
/// order. Names that are not in the file are skipped: [`check`] reports
/// them first.
fn cycle(entries: &[Entry], index: &HashMap<&str, usize>) -> Option<Vec<String>> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Mark {
        New,
        Open,
        Done,
    }
    fn visit(
        at: usize,
        entries: &[Entry],
        index: &HashMap<&str, usize>,
        marks: &mut [Mark],
        path: &mut Vec<usize>,
    ) -> Option<Vec<String>> {
        marks[at] = Mark::Open;
        path.push(at);
        for next in entries[at].waits().filter_map(|name| index.get(name)) {
            match marks[*next] {
                Mark::Open => {
                    let start = path.iter().position(|p| p == next).unwrap_or(0);
                    let mut ring: Vec<String> = path[start..]
                        .iter()
                        .map(|&p| entries[p].name.clone())
                        .collect();
                    ring.push(entries[*next].name.clone());
                    return Some(ring);
                }
                Mark::New => {
                    if let Some(ring) = visit(*next, entries, index, marks, path) {
                        return Some(ring);
                    }
                }
                Mark::Done => {}
            }
        }
        path.pop();
        marks[at] = Mark::Done;
        None
    }
    let mut marks = vec![Mark::New; entries.len()];
    let mut path = Vec::new();
    for start in 0..entries.len() {
        if marks[start] == Mark::New
            && let Some(ring) = visit(start, entries, index, &mut marks, &mut path)
        {
            return Some(ring);
        }
    }
    None
}

/// The order to consider entries in: what an entry waits on before it,
/// and otherwise the order the file gives — the order somebody wrote them
/// in is information. Indices into `entries`. A ring (which [`check`]
/// refuses) cannot make this loop: an entry is placed once.
pub fn order(entries: &[Entry]) -> Vec<usize> {
    fn place(
        at: usize,
        entries: &[Entry],
        index: &HashMap<&str, usize>,
        placed: &mut [bool],
        ordered: &mut Vec<usize>,
    ) {
        if placed[at] {
            return;
        }
        placed[at] = true;
        for next in entries[at].waits().filter_map(|name| index.get(name)) {
            place(*next, entries, index, placed, ordered);
        }
        ordered.push(at);
    }
    let index = index(entries);
    let mut placed = vec![false; entries.len()];
    let mut ordered = Vec::with_capacity(entries.len());
    for at in 0..entries.len() {
        place(at, entries, &index, &mut placed, &mut ordered);
    }
    ordered
}

/// How an entry came out, once it will not run again on its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ending {
    /// Its process ended with this status, or by a signal (`None`).
    Exited { code: Option<i32> },
    /// Somebody stopped it. Whatever it then exited with is not success:
    /// a stop is a decision that what waits on it should not run.
    Stopped,
    /// The daemon recorded it as failed: a spawn that never got off the
    /// ground, or an ending it could not read.
    Failed { reason: String },
    /// Asking the daemon to start it was refused.
    FailedToStart { reason: String },
    /// It was never started, because something it waited on failed.
    /// `cause` is that first failure, so a chain of entries says what
    /// went wrong at its head rather than a sentence per link.
    NotStarted { cause: String },
    /// Left out of this run and not on record as ever having run.
    NeverRan,
    /// It had exited 0 before anything here watched it, and the record
    /// cannot show that the exit was its own rather than the end of a stop:
    /// once a stopped agent has exited, its record no longer says it was
    /// stopped. Not success — an `after` fails closed on it.
    Unproven,
}

impl Ending {
    /// The ending an agent's record shows, or `None` while it may still
    /// run: live, or exited with a restart policy that starts it again.
    /// `stopped` is whether a stop was seen for it — an `agent_stopping`
    /// event, or `stopping` on an earlier look at the record; the exit a
    /// stop ends in no longer says so. Only a managed agent is restarted,
    /// so for any other the policy on its spec does not keep it going.
    pub fn of(record: &AgentRecord, stopped: bool) -> Option<Self> {
        if record.status.is_live() {
            return None;
        }
        if stopped {
            return Some(Self::Stopped);
        }
        let policy = if record.managed {
            record.spec.restart
        } else {
            RestartPolicy::No
        };
        if policy.restarts(&record.status, record.restarts) {
            return None;
        }
        Some(match &record.status {
            AgentStatus::Failed { reason } => Self::Failed {
                reason: reason.clone(),
            },
            AgentStatus::Exited { code } => Self::Exited { code: *code },
            // Live statuses returned above.
            AgentStatus::Created | AgentStatus::Running | AgentStatus::Stopping => {
                Self::Exited { code: None }
            }
        })
    }

    /// The ending of an agent that had already ended before anything here
    /// watched it, from its record alone: what [`Ending::of`] says, except
    /// that an exit 0 is [`Ending::Unproven`]. No stop was seen, and the
    /// record cannot say there was none, so a success cannot be shown;
    /// any other ending is a failure whatever caused it. `None` while the
    /// agent may still run, as for [`Ending::of`].
    pub fn before(record: &AgentRecord) -> Option<Self> {
        match Self::of(record, false)? {
            ending if ending.succeeded() => Some(Self::Unproven),
            ending => Some(ending),
        }
    }

    /// Exited with status zero: the one ending an `after` is met by.
    pub fn succeeded(&self) -> bool {
        matches!(self, Self::Exited { code: Some(0) })
    }

    /// Whether it ran at all, which is what `depends_on` asks.
    fn ran(&self) -> bool {
        matches!(self, Self::Exited { .. } | Self::Stopped)
    }

    /// Why what waits on `name` will not start: `migrate exited 1`.
    pub fn reason(&self, name: &str) -> String {
        format!("{name} {self}")
    }
}

impl fmt::Display for Ending {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exited { code: Some(code) } => write!(f, "exited {code}"),
            Self::Exited { code: None } => f.write_str("was ended by a signal"),
            Self::Stopped => f.write_str("was stopped"),
            Self::Failed { reason } => write!(f, "failed: {reason}"),
            Self::FailedToStart { reason } => write!(f, "failed to start: {reason}"),
            Self::NotStarted { cause } => write!(f, "was not started: {cause}"),
            Self::NeverRan => f.write_str("has not run"),
            Self::Unproven => {
                f.write_str("ended before this run; its exit cannot be shown to be its own")
            }
        }
    }
}

/// What is known of an entry while a file is being brought up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Progress {
    /// Not started, and not yet decided.
    Pending,
    /// Started, or found running, and not ended for good.
    Started,
    /// Over: it will not run again on its own.
    Ended(Ending),
}

/// Whether an entry can start now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Readiness {
    /// Everything it waits on is met.
    Ready,
    /// Still waiting, on these (`migrate exits 0`, `db to start`).
    Waiting(Vec<String>),
    /// It never will. `reason` says why (`migrate exited 1`), and `ending`
    /// is what to record for it, so what waits on it is not started either.
    Blocked { reason: String, ending: Ending },
}

/// Decide one entry from what is known of the others. A condition that can
/// no longer be met decides it, even while others are still waiting: there
/// is no point waiting for the rest. A name with nothing known of it is
/// still to come.
pub fn readiness(entry: &Entry, progress: &BTreeMap<String, Progress>) -> Readiness {
    let mut waiting = Vec::new();
    for after in &entry.after {
        match progress.get(&after.name) {
            Some(Progress::Ended(ending)) if ending.succeeded() => {}
            Some(Progress::Ended(ending)) => return blocked(ending.reason(&after.name), ending),
            _ => waiting.push(after.to_string()),
        }
    }
    for needed in &entry.depends_on {
        match progress.get(needed) {
            Some(Progress::Started) => {}
            // It ran, but it is not running now, and nothing will start
            // it again: what needed it running cannot have it.
            Some(Progress::Ended(ending)) if ending.ran() => {
                return blocked(
                    format!("{needed} is no longer running: it {ending}"),
                    ending,
                );
            }
            Some(Progress::Ended(ending)) => return blocked(ending.reason(needed), ending),
            _ => waiting.push(format!("{needed} to start")),
        }
    }
    if waiting.is_empty() {
        Readiness::Ready
    } else {
        Readiness::Waiting(waiting)
    }
}

fn blocked(reason: String, because: &Ending) -> Readiness {
    let cause = match because {
        Ending::NotStarted { cause } => cause.clone(),
        _ => reason.clone(),
    };
    Readiness::Blocked {
        reason,
        ending: Ending::NotStarted { cause },
    }
}

/// The agent an event about an ending is for: a stop under way, an exit, a
/// restart, a restart policy cleared. These are what `up` watches for an
/// entry something waits on; every other event is `None`.
pub fn lifecycle_agent(kind: &EventKind) -> Option<&AgentId> {
    match kind {
        EventKind::AgentStopping { agent, .. }
        | EventKind::AgentExited { agent, .. }
        | EventKind::AgentRestarted { agent, .. }
        | EventKind::AgentRestartCleared { agent } => Some(agent),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AgentSpec;

    fn entry(name: &str, depends_on: &[&str], after: &[&str]) -> Entry {
        Entry {
            name: name.to_owned(),
            depends_on: depends_on.iter().map(|s| (*s).to_owned()).collect(),
            after: after
                .iter()
                .map(|s| After {
                    name: (*s).to_owned(),
                })
                .collect(),
            restart: RestartPolicy::No,
        }
    }

    fn names(entries: &[Entry], order: &[usize]) -> Vec<String> {
        order.iter().map(|&i| entries[i].name.clone()).collect()
    }

    #[test]
    fn a_condition_reads_as_a_name_and_exit_status_zero() {
        assert_eq!(After::parse("build exits 0").unwrap().name, "build");
        assert_eq!(After::parse("  build   exits\t0 ").unwrap().name, "build");
        // A name with a space in it (a quoted TOML key) still reads.
        assert_eq!(After::parse("db seed exits 0").unwrap().name, "db seed");
        assert_eq!(
            After::parse("build exits 0").unwrap().to_string(),
            "build exits 0"
        );
    }

    #[test]
    fn a_malformed_condition_says_what_is_wrong() {
        for (text, says) in [
            ("", "it is empty"),
            ("  ", "it is empty"),
            ("build", "not a condition"),
            ("exits 0", "names no entry"),
            ("build exits", "not a condition"),
            ("build finishes 0", "`finishes` is not `exits`"),
            ("build exits 1", "only exit status 0"),
            ("build exits zero", "`zero` is not an exit status"),
            ("build exits 0 1", "`0` is not `exits`"),
        ] {
            let error = After::parse(text).unwrap_err();
            assert!(error.contains(says), "{text:?}: {error}");
        }
    }

    #[test]
    fn check_names_the_entry_for_each_wait_that_cannot_end() {
        let unknown = check(&[entry("test", &[], &["build"])]).unwrap_err();
        assert_eq!(
            unknown.to_string(),
            "agent `test` has after = \"build exits 0\", but `build` is not in this file"
        );
        let itself = check(&[entry("test", &[], &["test"])]).unwrap_err();
        assert!(itself.to_string().contains("`test`"), "{itself}");
        assert!(itself.to_string().contains("waits on itself"), "{itself}");

        let unknown = check(&[entry("web", &["db"], &[])]).unwrap_err();
        assert_eq!(
            unknown.to_string(),
            "agent `web` depends on `db`, which is not in this file"
        );
        let itself = check(&[entry("web", &["web"], &[])]).unwrap_err();
        assert_eq!(itself.to_string(), "agent `web` depends on itself");

        let mut always = entry("serve", &[], &[]);
        always.restart = RestartPolicy::Always;
        let never = check(&[always, entry("probe", &[], &["serve"])]).unwrap_err();
        assert!(matches!(never, OrderError::NeverEnds { .. }), "{never}");
        assert!(never.to_string().contains("`probe`"), "{never}");
    }

    #[test]
    fn a_ring_of_waits_is_refused_through_either_kind_of_wait() {
        let after_only = check(&[entry("a", &[], &["b"]), entry("b", &[], &["a"])]).unwrap_err();
        assert_eq!(
            after_only,
            OrderError::Cycle(vec!["a".into(), "b".into(), "a".into()])
        );
        assert_eq!(
            after_only.to_string(),
            "agents wait on each other in a cycle: a → b → a"
        );

        // `a` is started before `b`, which waits for `a` to exit, while `a`
        // waits for `b` to be running: neither can go first.
        let mixed = check(&[entry("a", &["b"], &[]), entry("b", &[], &["a"])]).unwrap_err();
        assert!(matches!(mixed, OrderError::Cycle(_)), "{mixed}");

        // The ring goes through a second dependency, not the first.
        let second = check(&[
            entry("a", &["c", "b"], &[]),
            entry("b", &[], &["a"]),
            entry("c", &[], &[]),
        ])
        .unwrap_err();
        assert_eq!(
            second,
            OrderError::Cycle(vec!["a".into(), "b".into(), "a".into()])
        );

        let long = check(&[
            entry("a", &[], &["c"]),
            entry("b", &[], &["a"]),
            entry("c", &[], &["b"]),
        ])
        .unwrap_err();
        assert!(matches!(long, OrderError::Cycle(ring) if ring.len() == 4));
    }

    #[test]
    fn a_diamond_is_not_a_ring() {
        let diamond = [
            entry("top", &[], &[]),
            entry("left", &[], &["top"]),
            entry("right", &[], &["top"]),
            entry("bottom", &[], &["left", "right"]),
        ];
        assert_eq!(check(&diamond), Ok(()));
    }

    #[test]
    fn order_puts_what_is_waited_on_first_and_keeps_the_file_order_otherwise() {
        let entries = [
            entry("deploy", &[], &["test"]),
            entry("zeta", &[], &[]),
            entry("test", &["db"], &["build"]),
            entry("build", &[], &[]),
            entry("db", &[], &[]),
            entry("alpha", &[], &[]),
        ];
        assert_eq!(
            names(&entries, &order(&entries)),
            ["db", "build", "test", "deploy", "zeta", "alpha"]
        );
    }

    fn progress(known: &[(&str, Progress)]) -> BTreeMap<String, Progress> {
        known
            .iter()
            .map(|(name, p)| ((*name).to_owned(), p.clone()))
            .collect()
    }

    fn exited(code: i32) -> Progress {
        Progress::Ended(Ending::Exited { code: Some(code) })
    }

    #[test]
    fn an_entry_after_another_waits_for_it_and_starts_on_success() {
        let test = entry("test", &[], &["build"]);
        for still in [Progress::Pending, Progress::Started] {
            assert_eq!(
                readiness(&test, &progress(&[("build", still)])),
                Readiness::Waiting(vec!["build exits 0".into()])
            );
        }
        assert_eq!(
            readiness(&test, &progress(&[("build", exited(0))])),
            Readiness::Ready
        );
    }

    #[test]
    fn every_other_ending_blocks_it_and_says_why() {
        let test = entry("test", &[], &["build"]);
        for (ending, reason) in [
            (Ending::Exited { code: Some(1) }, "build exited 1"),
            (Ending::Exited { code: None }, "build was ended by a signal"),
            (Ending::Stopped, "build was stopped"),
            (
                Ending::FailedToStart {
                    reason: "no such file".into(),
                },
                "build failed to start: no such file",
            ),
            (
                Ending::Failed {
                    reason: "spawn".into(),
                },
                "build failed: spawn",
            ),
            (Ending::NeverRan, "build has not run"),
            (
                Ending::Unproven,
                "build ended before this run; its exit cannot be shown to be its own",
            ),
        ] {
            assert_eq!(
                readiness(&test, &progress(&[("build", Progress::Ended(ending))])),
                Readiness::Blocked {
                    reason: reason.into(),
                    ending: Ending::NotStarted {
                        cause: reason.into()
                    },
                }
            );
        }
    }

    #[test]
    fn a_failure_decides_even_while_another_condition_still_waits() {
        let deploy = entry("deploy", &[], &["test", "lint"]);
        let known = progress(&[("test", Progress::Started), ("lint", exited(2))]);
        assert!(matches!(
            readiness(&deploy, &known),
            Readiness::Blocked { reason, .. } if reason == "lint exited 2"
        ));
    }

    #[test]
    fn depends_on_waits_for_a_start_and_is_blocked_by_one_that_never_comes() {
        let web = entry("web", &["db"], &[]);
        assert_eq!(
            readiness(&web, &progress(&[("db", Progress::Pending)])),
            Readiness::Waiting(vec!["db to start".into()])
        );
        assert_eq!(
            readiness(&web, &progress(&[("db", Progress::Started)])),
            Readiness::Ready
        );
        let not_started = Ending::NotStarted {
            cause: "migrate exited 1".into(),
        };
        assert_eq!(
            readiness(&web, &progress(&[("db", Progress::Ended(not_started))])),
            Readiness::Blocked {
                reason: "db was not started: migrate exited 1".into(),
                ending: Ending::NotStarted {
                    cause: "migrate exited 1".into()
                },
            }
        );
        assert!(matches!(
            readiness(&web, &progress(&[("db", exited(0))])),
            Readiness::Blocked { reason, .. } if reason == "db is no longer running: it exited 0"
        ));
    }

    /// Bring a file up the way `up` does, with each started entry ending at
    /// once with the status given for it: decide entries in order until a
    /// pass changes nothing. Returns what each entry came to.
    fn bring_up(entries: &[Entry], codes: &[(&str, i32)]) -> BTreeMap<String, Progress> {
        let mut known: BTreeMap<String, Progress> = entries
            .iter()
            .map(|e| (e.name.clone(), Progress::Pending))
            .collect();
        let order = order(entries);
        loop {
            let mut changed = false;
            for &i in &order {
                let entry = &entries[i];
                if known[&entry.name] != Progress::Pending {
                    continue;
                }
                match readiness(entry, &known) {
                    Readiness::Ready => {
                        let code = codes
                            .iter()
                            .find(|(name, _)| *name == entry.name)
                            .map_or(0, |(_, code)| *code);
                        known.insert(entry.name.clone(), exited(code));
                        changed = true;
                    }
                    Readiness::Waiting(_) => {}
                    Readiness::Blocked { ending, .. } => {
                        known.insert(entry.name.clone(), Progress::Ended(ending));
                        changed = true;
                    }
                }
            }
            if !changed {
                return known;
            }
        }
    }

    fn not_started(cause: &str) -> Progress {
        Progress::Ended(Ending::NotStarted {
            cause: cause.into(),
        })
    }

    #[test]
    fn success_starts_the_next() {
        let entries = [entry("build", &[], &[]), entry("test", &[], &["build"])];
        let known = bring_up(&entries, &[("build", 0)]);
        assert_eq!(known["test"], exited(0), "test ran");
    }

    #[test]
    fn a_failure_stops_what_waits_on_it_however_far_down() {
        let entries = [
            entry("build", &[], &[]),
            entry("test", &[], &["build"]),
            entry("deploy", &[], &["test"]),
            entry("announce", &["deploy"], &[]),
            entry("lint", &[], &[]),
        ];
        let known = bring_up(&entries, &[("build", 1)]);
        assert_eq!(known["build"], exited(1));
        for name in ["test", "deploy", "announce"] {
            assert_eq!(known[name], not_started("build exited 1"), "{name}");
        }
        assert_eq!(known["lint"], exited(0), "unrelated entries still run");
        // Each says what it waited on, and what went wrong at the head.
        let mut said = known.clone();
        said.insert("deploy".into(), Progress::Pending);
        assert!(matches!(
            readiness(&entries[2], &said),
            Readiness::Blocked { reason, .. } if reason == "test was not started: build exited 1"
        ));
    }

    #[test]
    fn a_chain_runs_link_by_link() {
        let entries = [
            entry("one", &[], &[]),
            entry("two", &[], &["one"]),
            entry("three", &[], &["two"]),
            entry("four", &[], &["three"]),
        ];
        let known = bring_up(&entries, &[]);
        assert!(known.values().all(|p| *p == exited(0)), "{known:?}");

        let known = bring_up(&entries, &[("three", 4)]);
        assert_eq!(known["two"], exited(0));
        assert_eq!(known["three"], exited(4));
        assert_eq!(known["four"], not_started("three exited 4"));
    }

    #[test]
    fn a_diamond_joins_both_sides() {
        let entries = [
            entry("top", &[], &[]),
            entry("left", &[], &["top"]),
            entry("right", &[], &["top"]),
            entry("bottom", &[], &["left", "right"]),
        ];
        let known = bring_up(&entries, &[]);
        assert_eq!(known["bottom"], exited(0));

        // One side failing is enough to keep the join from starting, and
        // the other side still runs.
        let known = bring_up(&entries, &[("right", 3)]);
        assert_eq!(known["left"], exited(0));
        assert_eq!(known["bottom"], not_started("right exited 3"));

        let known = bring_up(&entries, &[("top", 1)]);
        for name in ["left", "right", "bottom"] {
            assert_eq!(known[name], not_started("top exited 1"), "{name}");
        }
    }

    fn record(status: AgentStatus, restart: RestartPolicy, restarts: u32) -> AgentRecord {
        let mut record = AgentRecord::new(
            AgentSpec {
                name: "build".into(),
                restart,
                ..AgentSpec::default()
            },
            true,
            chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
        );
        record.status = status;
        record.restarts = restarts;
        record
    }

    #[test]
    fn an_agent_has_ended_for_good_only_when_nothing_will_start_it_again() {
        let no = RestartPolicy::No;
        for live in [
            AgentStatus::Created,
            AgentStatus::Running,
            AgentStatus::Stopping,
        ] {
            assert_eq!(Ending::of(&record(live.clone(), no, 0), false), None);
            assert_eq!(Ending::of(&record(live, no, 0), true), None);
        }
        let failed = AgentStatus::Exited { code: Some(1) };
        assert_eq!(
            Ending::of(&record(failed.clone(), no, 0), false),
            Some(Ending::Exited { code: Some(1) })
        );
        // `on-failure` brings it back while it has retries left: not over.
        let retry = RestartPolicy::OnFailure { max: 2 };
        assert_eq!(Ending::of(&record(failed.clone(), retry, 1), false), None);
        assert_eq!(
            Ending::of(&record(failed.clone(), retry, 2), false),
            Some(Ending::Exited { code: Some(1) })
        );
        // A clean zero ends `on-failure` whatever is left.
        assert_eq!(
            Ending::of(
                &record(AgentStatus::Exited { code: Some(0) }, retry, 0),
                false
            ),
            Some(Ending::Exited { code: Some(0) })
        );
        // Only a managed agent is restarted.
        let mut adopted = record(failed, retry, 0);
        adopted.managed = false;
        assert_eq!(
            Ending::of(&adopted, false),
            Some(Ending::Exited { code: Some(1) })
        );
        assert_eq!(
            Ending::of(
                &record(
                    AgentStatus::Failed {
                        reason: "spawn".into()
                    },
                    no,
                    0
                ),
                false
            ),
            Some(Ending::Failed {
                reason: "spawn".into()
            })
        );
    }

    #[test]
    fn an_earlier_exit_0_is_not_taken_as_success_but_an_earlier_failure_is_a_failure() {
        let no = RestartPolicy::No;
        // A stop leaves no mark on the record once the agent has exited,
        // so an exit 0 found on record may be the end of one.
        let earlier = Ending::before(&record(AgentStatus::Exited { code: Some(0) }, no, 0));
        assert_eq!(earlier, Some(Ending::Unproven));
        assert!(!Ending::Unproven.succeeded());
        assert_eq!(
            Ending::before(&record(AgentStatus::Exited { code: Some(1) }, no, 0)),
            Some(Ending::Exited { code: Some(1) })
        );
        assert_eq!(
            Ending::before(&record(AgentStatus::Exited { code: None }, no, 0)),
            Some(Ending::Exited { code: None })
        );
        // Still running, or about to be restarted: nothing to judge yet.
        assert_eq!(Ending::before(&record(AgentStatus::Running, no, 0)), None);
        let retry = RestartPolicy::OnFailure { max: 2 };
        assert_eq!(
            Ending::before(&record(AgentStatus::Exited { code: Some(1) }, retry, 0)),
            None
        );
        // What waits on it is not started, and says why.
        let report = entry("report", &[], &["serve"]);
        assert_eq!(
            readiness(
                &report,
                &progress(&[("serve", Progress::Ended(Ending::Unproven))])
            ),
            Readiness::Blocked {
                reason: "serve ended before this run; its exit cannot be shown to be its own"
                    .into(),
                ending: Ending::NotStarted {
                    cause: "serve ended before this run; its exit cannot be shown to be its own"
                        .into()
                },
            }
        );
    }

    #[test]
    fn a_stopped_agent_has_not_succeeded_whatever_it_exited_with() {
        let ending = Ending::of(
            &record(AgentStatus::Exited { code: Some(0) }, RestartPolicy::No, 0),
            true,
        )
        .unwrap();
        assert_eq!(ending, Ending::Stopped);
        assert!(!ending.succeeded());
    }

    #[test]
    fn only_ending_events_name_an_agent_to_look_at() {
        let id = AgentId::from("a1");
        assert_eq!(
            lifecycle_agent(&EventKind::AgentStopping {
                agent: id.clone(),
                force: false
            }),
            Some(&id)
        );
        assert_eq!(
            lifecycle_agent(&EventKind::AgentExited {
                agent: id.clone(),
                status: AgentStatus::Exited { code: Some(0) }
            }),
            Some(&id)
        );
        assert_eq!(
            lifecycle_agent(&EventKind::AgentStarted {
                agent: id,
                pid: None
            }),
            None
        );
    }
}
