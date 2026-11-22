//! A member's seat: who its session is, the start that puts one there,
//! and what a request finds when it asks for one.
//!
//! Starting a member is not one act. A session's record is created, its
//! process is spawned, its command is let run, its cards are handed to
//! it and it is told its first words; a daemon can stop between any two
//! of those, a client can go away, and a person can press Stop or Start
//! again at any moment. So a start is kept as an operation with stages,
//! each written in the same commit as what it records, and every request
//! that meets one (Start again, Stop, a release, a daemon that reopened)
//! is answered from the evidence there is and never from what a missing
//! marker might mean.
//!
//! Nothing here acts. The daemon reads the evidence (the session's record,
//! whether a driver of this start is alive in this process, what the
//! owner of a run answered) and commits what these functions decide; so
//! each rule is one place and each can be held to its table.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{Member, MemberId, Standing};
use crate::{AgentId, MessageId, TaskId};

/// A start's own id: a later start of the same member is another id, so
/// nothing an old one still holds can be taken for the new one's.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StartId(String);

impl StartId {
    pub fn generate() -> Self {
        Self(uuid::Uuid::new_v4().simple().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for StartId {
    fn from(id: &str) -> Self {
        Self(id.to_owned())
    }
}

/// How far a start has got: the last stage whose commit was made. Each
/// is named for what its commit proves, and they are reached in this
/// order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// Reserved under the lock before anything was awaited: the session's
    /// id, its name and the worktree it will be given are fixed. Nothing
    /// is committed beside it and nothing was spawned.
    Reserved,
    /// The session's record is inserted, with its roles and the seat's
    /// binding, in one commit. A created record proves no process.
    RecordBound,
    /// The launch committed the record as running, before the owner was
    /// told to let the command run: a spawned child held by its owner,
    /// not a command that ran.
    Running,
    /// The owner answered the activation: the command runs.
    Activated,
    /// Every card owed to the member is handed, excluded or owed.
    CardsSettled,
    /// Its first words are in its queue under the id reserved for them.
    /// Queued is not received, and received is not done.
    Instructed,
    /// Nothing is owed: the member is running as its team's member.
    Complete,
}

impl Stage {
    pub fn word(self) -> &'static str {
        match self {
            Stage::Reserved => "reserved",
            Stage::RecordBound => "record bound",
            Stage::Running => "running",
            Stage::Activated => "activated",
            Stage::CardsSettled => "cards settled",
            Stage::Instructed => "instructed",
            Stage::Complete => "complete",
        }
    }
}

/// How a start stopped short of complete. It went no further than its
/// stage, whose effects stand as committed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "how", rename_all = "snake_case")]
pub enum End {
    Failed {
        why: String,
    },
    /// A stop won: the start was ended on purpose.
    Cancelled {
        why: String,
    },
    /// The session was released from the team while running: it lives on
    /// as a plain session, and the team hands it nothing more and ends
    /// nothing of it.
    Released,
}

/// What the person asked to become of a start's session. Set once, under
/// the lock, and never changed: a stop cannot later be read as a release,
/// nor a release as a stop.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    Stop,
    Release,
}

/// The worktree reserved for a member that works in one of its own: fixed
/// at the reservation, so a preparation that was dropped and tried again
/// makes this one again and never a second.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkout {
    pub path: String,
    pub branch: String,
}

/// A card and why it stands where it does.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Held {
    pub card: TaskId,
    pub why: String,
}

/// Where each card owed to a member stands. Each card's change and its
/// entry here are one commit, so a daemon that stops between two cards
/// leaves all three sets exact.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cards {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub handed: Vec<TaskId>,
    /// For good: done, archived, or given to someone else since.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded: Vec<Held>,
    /// To be tried again: a lease in the way, evidence that could not be
    /// read, or work elsewhere that the person has not settled.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub owed: Vec<Held>,
}

/// One start of one member.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Start {
    pub id: StartId,
    /// The seat's generation this start was reserved at. A stop advances
    /// the seat's, which ends every start of an older one.
    pub generation: u64,
    /// The team's revision the person authorised it at, and the member as
    /// it was then: a start never reads a later team.
    pub revision: u64,
    pub member: Member,
    /// The session's id and its name, reserved with the start.
    pub agent: AgentId,
    pub session: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<Checkout>,
    /// The id its first words are queued under: reserved, so that trying
    /// again never makes a second message, and never reused for other
    /// words.
    pub first_words: MessageId,
    pub stage: Stage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<End>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disposition: Option<Disposition>,
    #[serde(default)]
    pub cards: Cards,
    /// Something of this start is still to be undone or seen to end: a
    /// reserved worktree to remove, a record not yet ended, an exit not
    /// yet committed. While it is, no other start takes the seat.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cleanup_owed: bool,
    /// A recovery could not establish whose run this is. Nothing is
    /// claimed of it, nothing replaces it, and a stop of it cannot be
    /// confirmed, until the person settles it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub uncertain: bool,
    pub authorised_by: String,
    pub reserved_at: DateTime<Utc>,
}

impl Start {
    /// The start has nothing left to run and owns nothing: another may
    /// take its seat. An ended record alone does not make it so.
    pub fn settled(&self) -> bool {
        self.end.is_some() && !self.cleanup_owed && !self.uncertain
    }

    /// Instructed, with something still owed: the member runs, was told
    /// what stands in the way, and the next Start tries it again.
    pub fn needs_attention(&self) -> bool {
        self.end.is_none() && self.stage == Stage::Instructed && !self.cards.owed.is_empty()
    }
}

/// A start asked for while the one before it still owns something. It
/// reserves no name and no checkout; it carries its own authorisation
/// and the member as it was when asked, and begins when the earlier one
/// has settled. A seat has at most one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Wanted {
    pub revision: u64,
    pub member: Member,
    pub authorised_by: String,
    pub at: DateTime<Utc>,
}

/// A member's seat. Who the member's session is, is this binding and
/// nothing else: a label on a record is for a person's eyes and is never
/// read back as evidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seat {
    pub member: MemberId,
    /// Advanced by every stop and every new reservation.
    pub generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bound: Option<AgentId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<Start>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wanted: Option<Wanted>,
}

impl Seat {
    pub fn new(member: MemberId) -> Self {
        Self {
            member,
            generation: 0,
            bound: None,
            start: None,
            wanted: None,
        }
    }

    /// Whether a driver that began `start` at `generation` may still
    /// write to this seat. Every write a driver makes is this comparison
    /// first: one that no longer matches writes nothing here, and may
    /// stop only the run it made itself.
    pub fn owns(&self, generation: u64, start: &StartId) -> bool {
        self.generation == generation
            && self
                .start
                .as_ref()
                .is_some_and(|current| current.id == *start && current.end.is_none())
    }
}

/// The session's record, as the registry has it now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Record {
    /// No record under the reserved id.
    None,
    Created,
    Running,
    Stopping,
    Ended,
}

/// What a request can see when it meets a seat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Evidence {
    pub record: Record,
    /// A driver of the seat's current start is alive in this daemon. Its
    /// absence proves nothing about a process: the record and its owner
    /// do.
    pub driver: bool,
    /// This daemon holds the authority to write. One that is taking over
    /// from another does not yet, and its empty memory says nothing of
    /// the other's drivers.
    pub writer: bool,
}

/// What a Start finds, and so what the daemon does for that member.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admit {
    /// No operation holds the seat: reserve a new start.
    Reserve,
    /// Running as its team's member with nothing owed: nothing is done.
    AlreadyActive,
    /// A driver is at work on it in this daemon: nothing is done.
    Starting,
    /// Reserved, with no record and no driver: nothing was committed
    /// beyond the reservation, so the same start goes on with what it
    /// reserved.
    Continue,
    /// Activated or beyond with no driver: the same start goes on with
    /// what it still owes (the cards, the first words), for the same
    /// session.
    Resume,
    /// Told its first words with something still owed: what is owed is
    /// tried again for the same session, and said again if it still is.
    RetryOwed,
    /// The record was created and never ran, and its driver is gone: the
    /// record is failed through the launch's own exit and the start with
    /// it; only then may another be reserved.
    FailUnstarted,
    /// A run nobody here is driving, whose command may or may not have
    /// been let run: the exact run is recovered through its owner before
    /// anything else is decided. Its activation is never sent again.
    Recover,
    /// A stop was asked and has not finished.
    Stopping,
    /// The session ended before its start was complete: the start is
    /// failed at its stage.
    FailEnded,
    /// The start before this one still owns something: this one is
    /// recorded as wanted and begins when that has settled.
    Wait,
    /// A start is already waiting for this seat: the same one answers.
    Waiting,
    /// Whose run it is could not be established: nothing replaces it
    /// until the person settles it.
    Uncertain,
    /// This daemon may not write yet: it shows the seat as recovering and
    /// does nothing.
    Recovering,
}

/// What a Start does for a member, from its seat and the evidence.
pub fn admit(seat: Option<&Seat>, evidence: Evidence) -> Admit {
    if !evidence.writer {
        return Admit::Recovering;
    }
    let Some(seat) = seat else {
        return Admit::Reserve;
    };
    let Some(start) = &seat.start else {
        return Admit::Reserve;
    };
    let waits = || {
        if seat.wanted.is_some() {
            Admit::Waiting
        } else {
            Admit::Wait
        }
    };
    if start.uncertain {
        return Admit::Uncertain;
    }
    if start.end.is_some() {
        return if start.settled() {
            Admit::Reserve
        } else {
            waits()
        };
    }
    if evidence.driver {
        return Admit::Starting;
    }
    if start.disposition == Some(Disposition::Stop) {
        // Stopped and not yet seen to end: a new start waits for it.
        return waits();
    }
    match (start.stage, evidence.record) {
        (Stage::Reserved, Record::None) => Admit::Continue,
        // A record under a start that never committed one is not this
        // start's doing.
        (Stage::Reserved, _) => Admit::Uncertain,
        (_, Record::Stopping) => Admit::Stopping,
        (Stage::Complete, Record::Created | Record::Running) => Admit::AlreadyActive,
        (Stage::Complete, Record::Ended | Record::None) => {
            if start.cleanup_owed {
                waits()
            } else {
                Admit::Reserve
            }
        }
        (_, Record::Ended | Record::None) => Admit::FailEnded,
        (Stage::RecordBound, Record::Created) => Admit::FailUnstarted,
        // A record that says less than its start's stage proved.
        (_, Record::Created) => Admit::Uncertain,
        (Stage::RecordBound | Stage::Running | Stage::Activated, Record::Running) => Admit::Recover,
        (Stage::CardsSettled, Record::Running) => Admit::Resume,
        (Stage::Instructed, Record::Running) => {
            if start.cards.owed.is_empty() {
                Admit::Resume
            } else {
                Admit::RetryOwed
            }
        }
    }
}

/// What the owner of a run answered when the run was reattached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reattached {
    /// The command was let run and runs.
    Activated,
    /// The child is held and was never let run.
    Unactivated,
    /// The run has ended and its exit is saved.
    Exited,
    /// The owner or the child could not be shown to be this run's.
    Unidentified,
}

/// What a recovery does with what the owner answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recovered {
    /// The run gets a fresh supervised receiver under its own identity,
    /// and its start goes on from activated with what it still owes.
    Resume,
    /// The child is ended through its owner; when that outcome is known
    /// and committed, the start is failed at running. A stop that was
    /// only sent proves nothing.
    EndUnactivated,
    /// The exit is committed before it is acknowledged; the start is
    /// failed.
    CommitExit,
    /// Nothing is claimed: the start is marked uncertain.
    Uncertain,
}

pub fn recovered(answer: Reattached) -> Recovered {
    match answer {
        Reattached::Activated => Recovered::Resume,
        Reattached::Unactivated => Recovered::EndUnactivated,
        Reattached::Exited => Recovered::CommitExit,
        Reattached::Unidentified => Recovered::Uncertain,
    }
}

/// What a Stop finds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// Nothing runs and nothing waits.
    Nothing,
    /// Only a wanted start waits: it is withdrawn, and what the start
    /// before it still owes is left as it is.
    Withdraw,
    /// Nothing was committed beyond the reservation: the start is
    /// cancelled, and its reserved worktree, if it was made, is owed its
    /// cleanup.
    CancelReserved,
    /// A record exists and its command has not been let run: the seat's
    /// generation is advanced, and the launch's own check, made under the
    /// lock before it commits running, ends the child unactivated. Where
    /// running was already committed, the ordinary stop follows.
    Refuse,
    /// The run is stopped by the ordinary stop, for that exact run.
    StopRun,
    /// A stop was already asked.
    Already,
    /// Whose run it is is not established: a stop cannot be confirmed,
    /// and is not answered as stopping.
    NotConfirmable,
    /// This daemon may not write yet.
    Recovering,
}

/// What a Stop does for a member. In every case but `Nothing`,
/// `Recovering` and `NotConfirmable` a wanted start is withdrawn too.
pub fn stop(seat: Option<&Seat>, evidence: Evidence) -> Stop {
    if !evidence.writer {
        return Stop::Recovering;
    }
    let Some(seat) = seat else {
        return Stop::Nothing;
    };
    let Some(start) = &seat.start else {
        return if seat.wanted.is_some() {
            Stop::Withdraw
        } else {
            Stop::Nothing
        };
    };
    if start.uncertain {
        return Stop::NotConfirmable;
    }
    if start.end.is_some() {
        // Ended already; a released session is not the team's to stop.
        return if seat.wanted.is_some() {
            Stop::Withdraw
        } else {
            Stop::Nothing
        };
    }
    if start.disposition.is_some() {
        return Stop::Already;
    }
    match (start.stage, evidence.record) {
        (Stage::Reserved, _) => Stop::CancelReserved,
        (_, Record::Ended | Record::None) => {
            if seat.wanted.is_some() {
                Stop::Withdraw
            } else {
                Stop::Nothing
            }
        }
        (_, Record::Stopping) => Stop::Already,
        (Stage::RecordBound | Stage::Running, _) => Stop::Refuse,
        (_, Record::Created | Record::Running) => Stop::StopRun,
    }
}

/// What a release of a member's session does: it stays a plain session
/// and keeps its cards, and the team's work on it ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Release {
    /// There is no session yet to keep: the start is cancelled, and the
    /// answer says the session had not been created.
    Cancel,
    /// The run is marked released, durably, for that exact run. From then
    /// on no stop, cleanup or late driver of the team ends it or removes
    /// its checkout.
    Mark,
}

/// Why a release is refused, in words.
pub const RELEASE_STARTING: &str =
    "its session is still starting; release it when it is running, or stop it";
pub const RELEASE_STOPPED: &str = "a stop was already asked of its session";
pub const RELEASE_ENDED: &str = "its session has ended; there is nothing to release";
pub const RELEASE_NOTHING: &str = "it has no session to release";
pub const RELEASE_UNCERTAIN: &str =
    "whose run its session is is not established; settle that before releasing it";

/// What a release does for a member, or why it is refused. A refusal
/// changes nothing and authorises nothing.
pub fn release(seat: Option<&Seat>, record: Record) -> Result<Release, &'static str> {
    let start = seat
        .and_then(|seat| seat.start.as_ref())
        .ok_or(RELEASE_NOTHING)?;
    if start.uncertain {
        return Err(RELEASE_UNCERTAIN);
    }
    if start.end.is_some() {
        return Err(match start.end {
            Some(End::Released) => RELEASE_NOTHING,
            _ => RELEASE_ENDED,
        });
    }
    if start.disposition.is_some() {
        return Err(RELEASE_STOPPED);
    }
    match (start.stage, record) {
        (Stage::Reserved, _) => Ok(Release::Cancel),
        (_, Record::Ended | Record::None) => Err(RELEASE_ENDED),
        (_, Record::Stopping) => Err(RELEASE_STOPPED),
        (Stage::RecordBound | Stage::Running, _) => Err(RELEASE_STARTING),
        (_, Record::Created) => Err(RELEASE_STARTING),
        (_, Record::Running) => Ok(Release::Mark),
    }
}

/// Where a member's session stands, for a reader: one word from the seat
/// and the evidence, the same one a Start would act on.
pub fn standing(seat: Option<&Seat>, evidence: Evidence) -> Standing {
    let Some(seat) = seat else {
        return Standing::NotStarted;
    };
    let Some(start) = &seat.start else {
        return if seat.wanted.is_some() {
            Standing::Waiting
        } else {
            Standing::NotStarted
        };
    };
    if !evidence.writer {
        return Standing::Recovering;
    }
    if start.uncertain {
        return Standing::Uncertain;
    }
    if seat.wanted.is_some() {
        return Standing::Waiting;
    }
    match &start.end {
        Some(End::Failed { .. }) => return Standing::Failed,
        Some(End::Cancelled { .. }) => {
            return if start.cleanup_owed {
                Standing::Stopping
            } else {
                Standing::NotStarted
            };
        }
        Some(End::Released) => return Standing::NotStarted,
        None => {}
    }
    if start.disposition == Some(Disposition::Stop) || evidence.record == Record::Stopping {
        return Standing::Stopping;
    }
    match (start.stage, evidence.record) {
        (Stage::Complete, Record::Created | Record::Running) => Standing::Running,
        (Stage::Complete, _) => Standing::NotStarted,
        (Stage::Reserved, Record::None) => Standing::Starting,
        (_, Record::Ended | Record::None) => Standing::Failed,
        _ if start.needs_attention() => Standing::NeedsAttention,
        _ => Standing::Starting,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member() -> Member {
        Member {
            id: MemberId::from("member-one"),
            name: "writer".into(),
            runtime: "claude-code".into(),
            model: None,
            effort: None,
            roles: vec!["implementer".into()],
            own_worktree: true,
            plan: None,
            first_words: None,
        }
    }

    fn start(stage: Stage) -> Start {
        Start {
            id: StartId::from("start-one"),
            generation: 1,
            revision: 1,
            member: member(),
            agent: AgentId::from("agent-one"),
            session: "writer-member".into(),
            worktree: Some(Checkout {
                path: "/work/app/.agentdocker/worktrees/writer".into(),
                branch: "team/writer".into(),
            }),
            first_words: MessageId::from("message-one".to_owned()),
            stage,
            end: None,
            disposition: None,
            cards: Cards::default(),
            cleanup_owed: false,
            uncertain: false,
            authorised_by: "user".into(),
            reserved_at: "2026-01-02T03:04:05Z".parse().unwrap(),
        }
    }

    fn seat(start: Start) -> Seat {
        Seat {
            generation: start.generation,
            bound: (start.stage >= Stage::RecordBound).then(|| start.agent.clone()),
            start: Some(start),
            ..Seat::new(MemberId::from("member-one"))
        }
    }

    fn wanted() -> Wanted {
        Wanted {
            revision: 2,
            member: member(),
            authorised_by: "user".into(),
            at: "2026-01-02T03:05:05Z".parse().unwrap(),
        }
    }

    fn seen(record: Record) -> Evidence {
        Evidence {
            record,
            driver: false,
            writer: true,
        }
    }

    const STAGES: [Stage; 7] = [
        Stage::Reserved,
        Stage::RecordBound,
        Stage::Running,
        Stage::Activated,
        Stage::CardsSettled,
        Stage::Instructed,
        Stage::Complete,
    ];
    const RECORDS: [Record; 5] = [
        Record::None,
        Record::Created,
        Record::Running,
        Record::Stopping,
        Record::Ended,
    ];

    /// The stages are reached in the order of their commits, and each
    /// has its word and its wire form.
    #[test]
    fn the_stages_are_in_the_order_of_their_commits() {
        let mut sorted = STAGES;
        sorted.sort();
        assert_eq!(sorted, STAGES);
        assert_eq!(
            STAGES.map(|stage| serde_json::to_value(stage).unwrap()),
            [
                "reserved",
                "record_bound",
                "running",
                "activated",
                "cards_settled",
                "instructed",
                "complete"
            ]
        );
        assert_eq!(Stage::RecordBound.word(), "record bound");
    }

    /// A start with no driver, by its stage and its record: the table of
    /// what the next Start does. Nothing is inferred from a driver's
    /// absence but that nobody here is driving.
    #[test]
    fn what_a_start_finds_with_no_driver() {
        use Admit::*;
        use Record as R;
        let table = [
            // Nothing committed beyond the reservation: the same start.
            (Stage::Reserved, R::None, Continue),
            // A record this start never committed is not its doing.
            (Stage::Reserved, R::Created, Uncertain),
            (Stage::Reserved, R::Running, Uncertain),
            // Created and never ran: no command was let run.
            (Stage::RecordBound, R::Created, FailUnstarted),
            // The launch committed running after the stage was written:
            // the record is the evidence, and the run is recovered.
            (Stage::RecordBound, R::Running, Recover),
            (Stage::Running, R::Running, Recover),
            // Activated, and whether it was handed to supervision is not
            // known here: the same recovery tells.
            (Stage::Activated, R::Running, Recover),
            (Stage::CardsSettled, R::Running, Resume),
            (Stage::Instructed, R::Running, Resume),
            (Stage::Complete, R::Running, AlreadyActive),
            (Stage::Complete, R::Created, AlreadyActive),
            // Ended before complete, at whatever stage.
            (Stage::RecordBound, R::Ended, FailEnded),
            (Stage::Running, R::Ended, FailEnded),
            (Stage::Activated, R::None, FailEnded),
            (Stage::Instructed, R::Ended, FailEnded),
            // Complete and ended: simply not running.
            (Stage::Complete, R::Ended, Reserve),
            (Stage::Complete, R::None, Reserve),
            // A record that says less than its start proved.
            (Stage::Running, R::Created, Uncertain),
            (Stage::Instructed, R::Created, Uncertain),
        ];
        for (stage, record, expected) in table {
            assert_eq!(
                admit(Some(&seat(start(stage))), seen(record)),
                expected,
                "{stage:?} with the record {record:?}"
            );
        }
        // A record being stopped answers stopping at every stage past
        // the reservation.
        for stage in &STAGES[1..] {
            assert_eq!(
                admit(Some(&seat(start(*stage))), seen(Record::Stopping)),
                Stopping,
                "{stage:?}"
            );
        }
        // No seat, or a seat nothing was started in.
        assert_eq!(admit(None, seen(Record::None)), Reserve);
        assert_eq!(
            admit(
                Some(&Seat::new(MemberId::from("member-one"))),
                seen(Record::None)
            ),
            Reserve
        );
    }

    /// A driver alive here means starting, whatever the stage and the
    /// record, and nothing is done; a daemon that may not write does
    /// nothing at all, driver or no driver.
    #[test]
    fn a_driver_at_work_and_a_daemon_that_may_not_write() {
        for stage in STAGES {
            for record in RECORDS {
                let seat = seat(start(stage));
                assert_eq!(
                    admit(
                        Some(&seat),
                        Evidence {
                            record,
                            driver: true,
                            writer: true
                        }
                    ),
                    Admit::Starting,
                    "{stage:?} {record:?}"
                );
                for driver in [true, false] {
                    let fenced = Evidence {
                        record,
                        driver,
                        writer: false,
                    };
                    assert_eq!(admit(Some(&seat), fenced), Admit::Recovering);
                    assert_eq!(stop(Some(&seat), fenced), Stop::Recovering);
                    assert_eq!(standing(Some(&seat), fenced), Standing::Recovering);
                }
            }
        }
        assert_eq!(
            admit(
                None,
                Evidence {
                    record: Record::None,
                    driver: false,
                    writer: false
                }
            ),
            Admit::Recovering,
            "a reservation is a write too"
        );
    }

    /// With something owed after its first words, the member is tried
    /// again for the same session; with nothing owed the start only has
    /// to be completed.
    #[test]
    fn what_is_owed_is_tried_again_for_the_same_session() {
        let mut owing = start(Stage::Instructed);
        owing.cards.owed.push(Held {
            card: TaskId::from("task-one".to_owned()),
            why: "another session holds a lease on it".into(),
        });
        assert!(owing.needs_attention());
        let owing = seat(owing);
        assert_eq!(admit(Some(&owing), seen(Record::Running)), Admit::RetryOwed);
        assert_eq!(
            standing(Some(&owing), seen(Record::Running)),
            Standing::NeedsAttention
        );
        let clear = seat(start(Stage::Instructed));
        assert_eq!(admit(Some(&clear), seen(Record::Running)), Admit::Resume);
        assert_eq!(
            standing(Some(&clear), seen(Record::Running)),
            Standing::Starting
        );
    }

    /// A replacement waits for the start before it to settle: while that
    /// one owns anything (a cleanup, an unconfirmed stop), a Start is
    /// recorded as wanted, a second Start meets the same wanted one, and
    /// only a settled start gives the seat up. An ended record alone
    /// settles nothing.
    #[test]
    fn a_replacement_waits_for_the_old_start_to_settle() {
        let mut ended = start(Stage::Running);
        ended.end = Some(End::Cancelled {
            why: "stopped".into(),
        });
        ended.cleanup_owed = true;
        assert!(!ended.settled());
        let mut waiting = seat(ended.clone());
        for record in RECORDS {
            assert_eq!(
                admit(Some(&waiting), seen(record)),
                Admit::Wait,
                "{record:?}"
            );
        }
        waiting.wanted = Some(wanted());
        for record in RECORDS {
            assert_eq!(
                admit(Some(&waiting), seen(record)),
                Admit::Waiting,
                "{record:?}"
            );
        }
        assert_eq!(
            standing(Some(&waiting), seen(Record::Ended)),
            Standing::Waiting
        );
        // Its cleanup done: settled, and the seat can be reserved.
        ended.cleanup_owed = false;
        assert!(ended.settled());
        assert_eq!(
            admit(Some(&seat(ended)), seen(Record::Ended)),
            Admit::Reserve
        );
        // A stop asked and not yet seen to end, with no end written:
        // still the old start's.
        let mut stopping = start(Stage::Activated);
        stopping.disposition = Some(Disposition::Stop);
        assert_eq!(
            admit(Some(&seat(stopping.clone())), seen(Record::Running)),
            Admit::Wait
        );
        assert_eq!(
            standing(Some(&seat(stopping)), seen(Record::Running)),
            Standing::Stopping
        );
        // A complete start whose session ended, with a cleanup still
        // owed, is waited for too.
        let mut complete = start(Stage::Complete);
        complete.cleanup_owed = true;
        assert_eq!(
            admit(Some(&seat(complete)), seen(Record::Ended)),
            Admit::Wait
        );
    }

    /// A run whose owner could not be identified is nobody's to replace
    /// or to claim stopped, at any stage and whatever its record says.
    #[test]
    fn an_uncertain_run_is_not_replaced_and_not_claimed_stopped() {
        for stage in STAGES {
            let mut unsure = start(stage);
            unsure.uncertain = true;
            assert!(!unsure.settled());
            let unsure = seat(unsure);
            for record in RECORDS {
                assert_eq!(admit(Some(&unsure), seen(record)), Admit::Uncertain);
                assert_eq!(stop(Some(&unsure), seen(record)), Stop::NotConfirmable);
                assert_eq!(release(Some(&unsure), record), Err(RELEASE_UNCERTAIN));
                assert_eq!(standing(Some(&unsure), seen(record)), Standing::Uncertain);
            }
        }
    }

    /// What the owner answered decides a recovery, and activation is in
    /// none of its outcomes.
    #[test]
    fn a_recovery_follows_what_the_owner_answered() {
        assert_eq!(recovered(Reattached::Activated), Recovered::Resume);
        assert_eq!(
            recovered(Reattached::Unactivated),
            Recovered::EndUnactivated
        );
        assert_eq!(recovered(Reattached::Exited), Recovered::CommitExit);
        assert_eq!(recovered(Reattached::Unidentified), Recovered::Uncertain);
    }

    /// A stop against every unfinished stage.
    #[test]
    fn what_a_stop_finds() {
        use Record as R;
        let table = [
            (Stage::Reserved, R::None, Stop::CancelReserved),
            // Created, or running and not yet let run: the launch's own
            // check refuses it, and the command never runs.
            (Stage::RecordBound, R::Created, Stop::Refuse),
            (Stage::RecordBound, R::Running, Stop::Refuse),
            (Stage::Running, R::Running, Stop::Refuse),
            (Stage::Activated, R::Running, Stop::StopRun),
            (Stage::CardsSettled, R::Running, Stop::StopRun),
            (Stage::Instructed, R::Running, Stop::StopRun),
            (Stage::Complete, R::Running, Stop::StopRun),
            (Stage::Complete, R::Stopping, Stop::Already),
            (Stage::Activated, R::Stopping, Stop::Already),
            (Stage::Complete, R::Ended, Stop::Nothing),
            (Stage::Running, R::Ended, Stop::Nothing),
        ];
        for (stage, record, expected) in table {
            assert_eq!(
                stop(Some(&seat(start(stage))), seen(record)),
                expected,
                "{stage:?} with the record {record:?}"
            );
        }
        assert_eq!(stop(None, seen(Record::None)), Stop::Nothing);
        // A second stop, and a stop after a release.
        let mut stopped = start(Stage::Activated);
        stopped.disposition = Some(Disposition::Stop);
        assert_eq!(
            stop(Some(&seat(stopped)), seen(Record::Running)),
            Stop::Already
        );
        let mut released = start(Stage::Complete);
        released.end = Some(End::Released);
        assert_eq!(
            stop(Some(&seat(released)), seen(Record::Running)),
            Stop::Nothing,
            "a released session is not the team's to stop"
        );
        // A wanted start alone is withdrawn, and the start before it
        // keeps what it owes.
        let mut old = start(Stage::Running);
        old.end = Some(End::Failed {
            why: "it could not be started".into(),
        });
        old.cleanup_owed = true;
        let mut waiting = seat(old);
        waiting.wanted = Some(wanted());
        assert_eq!(stop(Some(&waiting), seen(Record::Ended)), Stop::Withdraw);
    }

    /// A release is decided for the exact run: with no session yet it
    /// cancels; while the session is starting it is refused, promising
    /// nothing the launch could not keep; once activated it is marked;
    /// and a stop already asked is never turned into a release.
    #[test]
    fn what_a_release_finds() {
        use Record as R;
        let table = [
            (Stage::Reserved, R::None, Ok(Release::Cancel)),
            (Stage::RecordBound, R::Created, Err(RELEASE_STARTING)),
            (Stage::RecordBound, R::Running, Err(RELEASE_STARTING)),
            (Stage::Running, R::Running, Err(RELEASE_STARTING)),
            (Stage::Activated, R::Running, Ok(Release::Mark)),
            (Stage::CardsSettled, R::Running, Ok(Release::Mark)),
            (Stage::Instructed, R::Running, Ok(Release::Mark)),
            (Stage::Complete, R::Running, Ok(Release::Mark)),
            (Stage::Complete, R::Stopping, Err(RELEASE_STOPPED)),
            (Stage::Complete, R::Ended, Err(RELEASE_ENDED)),
            (Stage::Activated, R::None, Err(RELEASE_ENDED)),
        ];
        for (stage, record, expected) in table {
            assert_eq!(
                release(Some(&seat(start(stage))), record),
                expected,
                "{stage:?} with the record {record:?}"
            );
        }
        assert_eq!(release(None, R::None), Err(RELEASE_NOTHING));
        let mut stopped = start(Stage::Complete);
        stopped.disposition = Some(Disposition::Stop);
        assert_eq!(
            release(Some(&seat(stopped)), R::Running),
            Err(RELEASE_STOPPED)
        );
        let mut released = start(Stage::Complete);
        released.end = Some(End::Released);
        assert_eq!(
            release(Some(&seat(released)), R::Running),
            Err(RELEASE_NOTHING)
        );
    }

    /// A driver writes to a seat only while it is that seat's current,
    /// unended start at that generation: after a stop advanced the
    /// generation, after a new start took the seat, or after its own
    /// start ended, it writes nothing there.
    #[test]
    fn an_old_driver_cannot_write_to_a_seat_that_moved_on() {
        let mine = StartId::from("start-one");
        let mut seat = seat(start(Stage::Running));
        assert!(seat.owns(1, &mine));
        assert!(!seat.owns(1, &StartId::from("another")));
        seat.generation = 2;
        assert!(!seat.owns(1, &mine), "a stop advanced the generation");
        seat.generation = 1;
        seat.start.as_mut().unwrap().end = Some(End::Cancelled {
            why: "stopped".into(),
        });
        assert!(!seat.owns(1, &mine), "its own start has ended");
        let mut next = start(Stage::Reserved);
        next.id = StartId::from("start-two");
        next.generation = 3;
        seat.generation = 3;
        seat.start = Some(next);
        assert!(!seat.owns(1, &mine) && !seat.owns(3, &mine));
        assert!(seat.owns(3, &StartId::from("start-two")));
        assert!(!Seat::new(MemberId::from("member-one")).owns(0, &mine));
    }

    /// A reader's word for a member, stage by stage.
    #[test]
    fn where_a_member_stands_for_a_reader() {
        assert_eq!(standing(None, seen(Record::None)), Standing::NotStarted);
        let table = [
            (Stage::Reserved, Record::None, Standing::Starting),
            (Stage::RecordBound, Record::Created, Standing::Starting),
            (Stage::Running, Record::Running, Standing::Starting),
            (Stage::Activated, Record::Running, Standing::Starting),
            (Stage::CardsSettled, Record::Running, Standing::Starting),
            (Stage::Complete, Record::Running, Standing::Running),
            (Stage::Complete, Record::Stopping, Standing::Stopping),
            (Stage::Complete, Record::Ended, Standing::NotStarted),
            (Stage::Activated, Record::Ended, Standing::Failed),
        ];
        for (stage, record, expected) in table {
            assert_eq!(
                standing(Some(&seat(start(stage))), seen(record)),
                expected,
                "{stage:?} with the record {record:?}"
            );
        }
        let mut failed = start(Stage::RecordBound);
        failed.end = Some(End::Failed {
            why: "the runtime is not installed".into(),
        });
        assert_eq!(
            standing(Some(&seat(failed)), seen(Record::Ended)),
            Standing::Failed
        );
        let mut cancelled = start(Stage::Running);
        cancelled.end = Some(End::Cancelled {
            why: "stopped".into(),
        });
        cancelled.cleanup_owed = true;
        assert_eq!(
            standing(Some(&seat(cancelled.clone())), seen(Record::Running)),
            Standing::Stopping
        );
        cancelled.cleanup_owed = false;
        assert_eq!(
            standing(Some(&seat(cancelled)), seen(Record::Ended)),
            Standing::NotStarted
        );
        // Every word has its wire form, and a reader that does not know
        // a word reads unknown, never not started.
        for (standing, wire, word) in [
            (Standing::NotStarted, "not_started", "not started"),
            (Standing::Starting, "starting", "starting"),
            (Standing::Running, "running", "running"),
            (
                Standing::NeedsAttention,
                "needs_attention",
                "needs attention",
            ),
            (Standing::Stopping, "stopping", "stopping"),
            (Standing::Failed, "failed", "failed"),
            (
                Standing::Waiting,
                "waiting",
                "waiting on the start before it",
            ),
            (Standing::Uncertain, "uncertain", "not confirmed"),
            (Standing::Recovering, "recovering", "recovering"),
        ] {
            assert_eq!(serde_json::to_value(standing).unwrap(), wire);
            assert_eq!(standing.word(), word);
        }
        assert_eq!(
            serde_json::from_value::<Standing>(serde_json::json!("a later word")).unwrap(),
            Standing::Unknown
        );
    }

    /// A seat and its start as stored: every field round-trips, what is
    /// not set is not written, and a seat written before a field existed
    /// reads with that field unset.
    #[test]
    fn a_seat_is_stored_and_read_back() {
        let mut stored = start(Stage::Instructed);
        stored
            .cards
            .handed
            .push(TaskId::from("task-one".to_owned()));
        stored.cards.owed.push(Held {
            card: TaskId::from("task-two".to_owned()),
            why: "another session holds a lease on it".into(),
        });
        let mut seat = seat(stored);
        seat.wanted = Some(wanted());
        let json = serde_json::to_value(&seat).unwrap();
        assert_eq!(serde_json::from_value::<Seat>(json.clone()).unwrap(), seat);
        let written = json["start"].as_object().unwrap();
        for unset in ["end", "disposition", "cleanup_owed", "uncertain"] {
            assert!(!written.contains_key(unset), "{unset}");
        }
        assert_eq!(json["start"]["stage"], "instructed");
        assert_eq!(json["start"]["cards"]["owed"][0]["card"], "task-two");
        // A bare seat.
        let bare = Seat::new(MemberId::from("member-one"));
        assert_eq!(
            serde_json::to_value(&bare).unwrap(),
            serde_json::json!({"member": "member-one", "generation": 0})
        );
        // An end, as it is written.
        let mut failed = start(Stage::Running);
        failed.end = Some(End::Failed {
            why: "its owner went away".into(),
        });
        failed.disposition = Some(Disposition::Stop);
        let json = serde_json::to_value(&failed).unwrap();
        assert_eq!(
            json["end"],
            serde_json::json!({"how": "failed", "why": "its owner went away"})
        );
        assert_eq!(json["disposition"], "stop");
        assert_eq!(serde_json::from_value::<Start>(json).unwrap(), failed);
    }
}
