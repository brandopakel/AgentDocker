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
//! Four things read a seat: a Start, a Stop, a release, and whoever is
//! shown where a member stands. They read it one way. Everything they
//! are given is first put to one question, [`Seat::case`], which says
//! what kind of situation this is (nothing there, ended, a stop asked,
//! at work, or not to be trusted), and each then answers within that.
//! So a pair of facts that cannot both be true is "not confirmed" to all
//! four, and never a cancellation to one and a running session to
//! another.
//!
//! Nothing here acts. The daemon reads the evidence and commits what
//! these functions decide; so each rule is one place and each can be
//! held to its table.

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

/// The one run a start made: written in the commit that makes the record
/// running, from what identifies that run's owner and child, and never
/// changed. An agent's id can come to name a later run; this cannot. The
/// daemon makes it and compares it; here it is only the same or not.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RunId(String);

impl RunId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for RunId {
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
    /// The launch committed the record as running, and the start's run
    /// with it, before the owner was told to let the command run: a
    /// spawned child held by its owner, not a command that ran.
    Running,
    /// The owner answered the activation: the command runs. It does not
    /// say the run was handed to supervision.
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

/// How a start stopped short of complete, or stopped being the team's.
/// It went no further than its stage, whose effects stand as committed.
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
    /// Its session ended after its start was complete: an ordinary exit
    /// of a member that was running, not a start that failed. The same
    /// whether the daemon saw the exit as it happened or found it saved
    /// by the run's owner later.
    Exited,
    /// The session was released from the team while running: it lives on
    /// as a plain session. Written in the one commit that releases it,
    /// so there is no moment at which a release is asked and not made;
    /// from it on the team hands the session nothing, ends nothing of it
    /// and removes nothing of its checkout.
    Released,
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

/// Words for a member's session, under the id they are queued by. The id
/// is reserved before the words exist; once they are prepared they are
/// kept here, and that id never carries other words: a start that could
/// not queue them, and is tried again after what they say has changed,
/// queues the words it prepared, not new ones.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Words {
    pub id: MessageId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

impl Words {
    pub fn reserved(id: MessageId) -> Self {
        Self { id, text: None }
    }

    /// The words under this id: `text` if none were prepared yet, and
    /// otherwise the ones that were, whatever `text` says now.
    pub fn prepare(&mut self, text: impl Into<String>) -> &str {
        self.text.get_or_insert_with(|| text.into())
    }
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
    /// The run this start made, from the commit that made it running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<RunId>,
    /// Its first words, and what it is told later when the last thing
    /// owed is resolved: each under an id of its own.
    pub first_words: Words,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_up: Option<Words>,
    pub stage: Stage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<End>,
    /// A stop was asked of it and has not been seen to finish. Set once,
    /// under the lock: from then on it is not released, nothing more of
    /// the team's is done for it, and the stop stays owed until the run
    /// is seen to end, whoever has to relay it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stopping: bool,
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

    /// Released: its session is no longer the team's to end or to clean
    /// up after.
    pub fn released(&self) -> bool {
        self.end == Some(End::Released)
    }

    /// The release itself, as its one commit writes it: the start ends
    /// as released, and whatever cleanup the team still owed for it is
    /// given up with it, since what it would have cleaned (the session,
    /// its checkout) is now the session's own to keep. So a released
    /// start is settled at once, and never left owing a cleanup nobody
    /// may do.
    pub fn release(&mut self) {
        self.end = Some(End::Released);
        self.cleanup_owed = false;
    }
}

/// A start asked for while the one before it still owns something. It
/// reserves no name and no checkout; it carries its own authorisation
/// and the member as it was when asked, and begins when the earlier one
/// has settled. A seat has at most one, and the first one asked stands:
/// a repeated Start meets it, and does not replace it.
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

    /// Whether a driver that began `start` at `generation` may still go
    /// on with it. Every write a driver makes for a start's progress is
    /// this comparison first. The generation must be the seat's now and
    /// the start's own: one a stop left behind does not pass with the
    /// seat's newer number, and nothing passes for a start that has
    /// ended or been asked to stop.
    pub fn owns(&self, generation: u64, start: &StartId) -> bool {
        self.generation == generation
            && self.start.as_ref().is_some_and(|current| {
                current.id == *start
                    && current.generation == generation
                    && current.end.is_none()
                    && !current.stopping
                    && !current.uncertain
            })
    }

    /// Whether the cleanup of `start` may be done now: by a daemon that
    /// may write, for that start, with a cleanup still owed, the start
    /// ended and not released, and nothing in doubt about what would be
    /// cleaned. For a start that made a run, the run the record shows
    /// must be positively that run: with another run under the same
    /// agent id, or one that cannot be told, nothing is cleaned. A start
    /// that never made a run, being short of the running stage, has only
    /// what it reserved to clean (its worktree), which is named by the
    /// reservation and needs no run. A start at the running stage or
    /// later with no run written down has no evidence of what is its
    /// own, and nothing of it is cleaned.
    ///
    /// A start that is still live has no cleanup to admit: that is what
    /// keeps a cleanup and a release from both being let at the same
    /// session, since a release is given only to a live start and ends
    /// it. This is not progress and needs no current generation; it is
    /// the one thing an operation a stop left behind is still let do,
    /// and it writes nothing but that its cleanup is done.
    pub fn may_clean(&self, start: &StartId, evidence: Evidence) -> bool {
        evidence.writer
            && self.start.as_ref().is_some_and(|current| {
                current.id == *start
                    && current.cleanup_owed
                    && current.end.is_some()
                    && !current.released()
                    && !current.uncertain
                    && match &current.run {
                        // A run was made: it must be the one seen now.
                        Some(_) => evidence.run == Run::Same,
                        // None was, and none could have been: only what
                        // the reservation names is there to clean.
                        None => current.stage < Stage::Running,
                    }
            })
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

/// Whether the record's run is the one the start made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Run {
    /// The run the record shows is the start's own, positively matched.
    Same,
    /// It is another run under the same agent id.
    Other,
    /// It could not be shown to be either.
    Unproven,
}

/// What, in this daemon, is at work on the start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Driver {
    /// Nothing. That proves nothing about a process: the record and its
    /// owner do.
    None,
    /// The task that launches it and drives it on. It holds the run's
    /// receiver until the run is handed to supervision.
    Launching,
    /// A recovery of the exact run through its owner. There is no
    /// receiver to relay through until it has finished.
    Recovering,
}

/// What a request can see when it meets a seat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Evidence {
    pub record: Record,
    /// Read for a start that has made a run, and ignored before.
    pub run: Run,
    pub driver: Driver,
    /// A supervised receiver exists for this exact run: a stop can be
    /// relayed to it, and the run is known to be let run.
    pub relay: bool,
    /// The receiver there is now is known to have been given the stop
    /// that was asked. That a receiver exists does not show it: one made
    /// since the stop was asked has not been given it.
    pub stop_delivered: bool,
    /// This daemon holds the authority to write. One that is taking over
    /// from another does not yet, and its empty memory says nothing of
    /// the other's drivers.
    pub writer: bool,
}

/// What kind of situation a seat and its evidence are: the one question
/// every reader of a seat asks first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Case<'a> {
    /// This daemon may not write yet: it shows and does nothing.
    Fenced,
    /// No start holds the seat.
    Empty,
    /// The start is marked uncertain, or what is stored and what is seen
    /// cannot both be true, or the run seen is not shown to be the
    /// start's own. Nothing is claimed and nothing is replaced.
    Unclear(&'a Start),
    /// The start has ended (failed, cancelled or released).
    Ended(&'a Start),
    /// A stop was asked and has not been seen to finish.
    StopAsked(&'a Start),
    /// The start is at work, and its evidence agrees with it.
    Live(&'a Start),
}

/// Whether what a start has stored and what is seen can both be true.
///
/// - Reserved has committed no record: any record is not its doing.
/// - Record bound has a created record, or one that ended before it ran.
///   A running one would have written its stage and its run in the same
///   commit; a missing one is not an exit anybody saw.
/// - From running on, the record is running, stopping or ended, and its
///   run is positively the start's own. A created record says less than
///   was proved, a missing one establishes nothing, and another run
///   under the same agent id is another run.
fn agrees(start: &Start, evidence: Evidence) -> bool {
    match (start.stage, evidence.record) {
        (Stage::Reserved, record) => record == Record::None,
        (Stage::RecordBound, record) => matches!(record, Record::Created | Record::Ended),
        (_, Record::Running | Record::Stopping | Record::Ended) => {
            start.run.is_some() && evidence.run == Run::Same
        }
        (_, Record::None | Record::Created) => false,
    }
}

impl Seat {
    /// The situation `seat` and `evidence` are.
    pub fn case(seat: Option<&Seat>, evidence: Evidence) -> Case<'_> {
        if !evidence.writer {
            return Case::Fenced;
        }
        let Some(start) = seat.and_then(|seat| seat.start.as_ref()) else {
            return Case::Empty;
        };
        if start.uncertain {
            return Case::Unclear(start);
        }
        if start.end.is_some() {
            return Case::Ended(start);
        }
        if !agrees(start, evidence) {
            return Case::Unclear(start);
        }
        if start.stopping {
            Case::StopAsked(start)
        } else {
            Case::Live(start)
        }
    }
}

/// What a Start finds, and so what the daemon does for that member.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admit {
    /// No operation holds the seat and none waits: reserve a new start
    /// from this request.
    Reserve,
    /// The seat is free and a start has been waiting for it: that one
    /// begins, from the member and the authorisation it was asked with,
    /// not from this request. Promoting it empties the wait, so it
    /// happens once.
    Promote,
    /// Running as its team's member with nothing owed, its relay
    /// verified: nothing is done.
    AlreadyActive,
    /// A launch or a recovery is at work on it in this daemon: nothing
    /// is done.
    Starting,
    /// Reserved, with no record and nothing at work: nothing was
    /// committed beyond the reservation, so the same start goes on with
    /// what it reserved.
    Continue,
    /// Its run is let run and under supervision here (a verified relay),
    /// and nothing drives its start: the same start goes on with what it
    /// still owes (the cards, the first words), for the same session.
    /// Without a verified relay this is never the answer, whatever stage
    /// is recorded: see [`Admit::Recover`].
    Resume,
    /// Told its first words with something still owed, under a verified
    /// relay: what is owed is tried again for the same session, and said
    /// again if it still is.
    RetryOwed,
    /// The record was created and never ran, and nothing drives it: the
    /// record is failed through the launch's own exit and the start with
    /// it; only then may another be reserved.
    FailUnstarted,
    /// A running record with nothing driving it and no verified relay,
    /// at any stage from running to complete: whether its command was
    /// let run, and whether anything still supervises it, is not known
    /// here. The exact run is recovered through its owner before
    /// anything else is decided, and its activation is never sent again.
    /// Reattaching to an owner takes its controller over, so this is
    /// never the answer for a run that is supervised.
    Recover,
    /// Supervised here, with its activation not yet written down: a
    /// verified relay proves the command was let run. The activation is
    /// recorded and the start goes on under the relay there is, which is
    /// not touched.
    RecordActivation,
    /// The record is being stopped.
    Stopping,
    /// The record shows the start's own run has ended and the start
    /// does not say so yet: the exit is committed as the start's end, as
    /// the kind of exit it is, with any cleanup it owes kept. Then the
    /// question is asked again: the seat is free, or waits for that
    /// cleanup.
    CommitExit(Exit),
    /// The start before this one still owns something, or a stop of it
    /// has not finished: this request is recorded as the wanted start.
    Wait,
    /// A start is already waiting for this seat: it stands, and answers.
    Waiting,
    /// Nothing is claimed of the start there is, and nothing replaces it
    /// until the person settles it.
    Uncertain,
    /// This daemon may not write yet.
    Recovering,
}

/// The exit a start has still to be given: its own run is seen to have
/// ended, by the record, and the start has no end written. An ordinary
/// one where the start was complete, a failure where it was not, and
/// the stop done where one was asked. None where the start has its end
/// already, or nothing has ended, or what is seen is not to be trusted.
///
/// Writing it is what lets a seat move on: an ended record with no end
/// on its start is neither cleaned up after nor given up.
pub fn exit_owed(seat: Option<&Seat>, evidence: Evidence) -> Option<Exit> {
    if evidence.record != Record::Ended {
        return None;
    }
    match Seat::case(seat, evidence) {
        Case::StopAsked(_) => Some(Exit::Cancelled),
        Case::Live(start) if start.stage == Stage::Complete => Some(Exit::Ordinary),
        Case::Live(_) => Some(Exit::Failed),
        _ => None,
    }
}

/// What a Start does for a member, from its seat and the evidence.
pub fn admit(seat: Option<&Seat>, evidence: Evidence) -> Admit {
    if let Some(exit) = exit_owed(seat, evidence) {
        return Admit::CommitExit(exit);
    }
    let waiting = seat.is_some_and(|seat| seat.wanted.is_some());
    let free = if waiting {
        Admit::Promote
    } else {
        Admit::Reserve
    };
    let held = if waiting { Admit::Waiting } else { Admit::Wait };
    match Seat::case(seat, evidence) {
        Case::Fenced => Admit::Recovering,
        Case::Empty => free,
        Case::Unclear(_) => Admit::Uncertain,
        Case::Ended(start) if start.settled() => free,
        // A stop that has won holds the seat until it is seen to finish,
        // whether or not the driver it stopped is still alive. Whatever
        // of that stop is still to be done is [`pending`]'s to say, and
        // is done on this request as on any other.
        Case::Ended(_) | Case::StopAsked(_) => held,
        Case::Live(_) if evidence.driver != Driver::None => Admit::Starting,
        Case::Live(start) => match (start.stage, evidence.record) {
            (Stage::Reserved, _) => Admit::Continue,
            (Stage::RecordBound, Record::Created) => Admit::FailUnstarted,
            (_, Record::Stopping) => Admit::Stopping,
            _ if !evidence.relay => Admit::Recover,
            (Stage::Running, _) => Admit::RecordActivation,
            (Stage::Instructed, _) if !start.cards.owed.is_empty() => Admit::RetryOwed,
            (Stage::Complete, _) => Admit::AlreadyActive,
            _ => Admit::Resume,
        },
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

/// How an exit that a recovery found saved is committed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit {
    /// The start was complete: its member ran and has ended. The start
    /// ends as [`End::Exited`], as it would had the daemon seen the exit
    /// when it happened.
    Ordinary,
    /// The start was not complete: it is failed at its stage.
    Failed,
    /// A stop was asked: it is cancelled.
    Cancelled,
    /// The start had ended already: its end stands, and only the
    /// record's exit is committed.
    AlreadyEnded,
}

/// What a recovery does with what the owner answered. It reads the seat
/// again first, and is given the start and the run it set out to
/// recover: what the person asked while it ran wins over going on, and
/// a reply that comes back to a seat that has moved on authorises
/// nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recovered {
    /// The run is under supervision, and its start goes on from
    /// activated with what it still owes. `install` is whether a
    /// supervised receiver is still to be made for it: one that is
    /// already there is kept and never displaced.
    Resume { install: bool },
    /// The run lives and is to be stopped: a stop was asked, or the
    /// start has ended and stopping its run is the cleanup it owes. It
    /// is relayed through the run's receiver, made first if there is
    /// none, and nothing more of the team's is done.
    RelayStop { install: bool },
    /// The child is ended through its owner; when that outcome is known
    /// and committed, the start is failed at running if it has not ended
    /// already. A stop that was only sent proves nothing.
    EndUnactivated,
    /// The exit is committed before it is acknowledged, as the kind of
    /// exit it is.
    CommitExit(Exit),
    /// Nothing is claimed: the start is, or is now marked, uncertain.
    /// Nothing is installed.
    Uncertain,
    /// The session was released meanwhile: it is not the team's, and the
    /// recovery leaves it as it found it.
    Leave,
    /// The seat no longer holds the start, or the run, this recovery
    /// set out for: another has taken its place. Nothing is written,
    /// nothing is installed, and the reply is dropped.
    Stale,
    /// The owner's saved exit is this run's and is on the record
    /// already: a commit that was made and not acknowledged. The owner
    /// is acknowledged and its report retired, and no second exit is
    /// written. (What the start itself still needs is `exit_owed`'s.)
    Acknowledge,
    /// There is nothing for this reply to do: what it would have done
    /// is done already (the run has ended and the reply is about a
    /// live one), or is no longer owed (the start ended and its cleanup
    /// is finished).
    Nothing,
    /// This daemon lost the authority to write while the recovery ran:
    /// the same, until it is given back.
    Fenced,
}

/// The start and the run a recovery set out to recover.
#[derive(Clone, Copy, Debug)]
pub struct Expected<'a> {
    pub start: &'a StartId,
    pub run: &'a RunId,
}

pub fn recovered(
    seat: Option<&Seat>,
    expected: Expected<'_>,
    answer: Reattached,
    evidence: Evidence,
) -> Recovered {
    if !evidence.writer {
        return Recovered::Fenced;
    }
    let Some(start) = seat
        .and_then(|seat| seat.start.as_ref())
        .filter(|start| start.id == *expected.start && start.run.as_ref() == Some(expected.run))
    else {
        return Recovered::Stale;
    };
    if start.released() {
        return Recovered::Leave;
    }
    // An ended start that owes nothing is done with: no reply, whatever
    // it says, changes that or casts doubt on it, so nothing that waits
    // behind it is held up by an answer that came late. One thing is
    // still due, and it is the owner's, not the start's: a report of
    // this very run's exit, which the record already shows, is
    // acknowledged and retired, or its owner would wait out its time
    // and leave the report behind. The seat stays settled.
    if start.end.is_some() && !start.cleanup_owed && !start.uncertain {
        let durable = answer == Reattached::Exited
            && evidence.record == Record::Ended
            && agrees(start, evidence);
        return if durable {
            Recovered::Acknowledge
        } else {
            Recovered::Nothing
        };
    }
    // What is seen now must still be this start's own run: the registry
    // can have moved to another run under the same agent id while the
    // answer was on its way, and an answer about the first is no leave
    // to install on, stop, advance or end the second.
    if start.uncertain || answer == Reattached::Unidentified || !agrees(start, evidence) {
        return Recovered::Uncertain;
    }
    // The run has ended, by the record. An owner that still holds its
    // exit report is acknowledged; a reply about a live run is about
    // the past, and installs and stops nothing.
    if evidence.record == Record::Ended {
        return match answer {
            Reattached::Exited => Recovered::Acknowledge,
            _ => Recovered::Nothing,
        };
    }
    // An owner that says the child was never let run contradicts what
    // is proved where the start's activation is written down, and
    // equally where a supervised receiver is verified for the run, which
    // proves the same before the stage is written: nothing is claimed,
    // ended start or not, and above all the child is not ended.
    if answer == Reattached::Unactivated && (start.stage >= Stage::Activated || evidence.relay) {
        return Recovered::Uncertain;
    }
    let install = !evidence.relay;
    let exit = match &start.end {
        Some(_) => Exit::AlreadyEnded,
        None if start.stopping => Exit::Cancelled,
        None if start.stage == Stage::Complete => Exit::Ordinary,
        None => Exit::Failed,
    };
    match answer {
        Reattached::Exited => Recovered::CommitExit(exit),
        Reattached::Unactivated => Recovered::EndUnactivated,
        // An ended start's live run is the cleanup it owes; a stop asked
        // of the team, or an ordinary stop of the session that shows
        // only on its record, wins over going on.
        _ if start.end.is_some() || start.stopping || evidence.record == Record::Stopping => {
            Recovered::RelayStop { install }
        }
        _ => Recovered::Resume { install },
    }
}

/// What a Stop does about the run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopRun {
    /// Nothing of the team's runs.
    Nothing,
    /// The run is seen to have ended and its start does not say so yet:
    /// the exit is committed as the start's end ([`exit_owed`]).
    CommitExit(Exit),
    /// Nothing was committed beyond the reservation: the start is
    /// cancelled, and its reserved worktree, if it was made, is owed its
    /// cleanup.
    CancelReserved,
    /// The launch is alive and has not yet committed running: the seat's
    /// generation is advanced, and the launch's own check, made under
    /// the lock before that commit, ends the child unactivated. The
    /// command never runs.
    Refuse,
    /// A created record with nothing launching it: it never ran. It is
    /// failed through the launch's own exit, as a Start would.
    EndUnstarted,
    /// The stop can be relayed now, to this exact run: through its
    /// supervised receiver, or through the launch that still holds it.
    Relay,
    /// A recovery of the run is at work and there is nothing to relay
    /// through yet: the stop is owed, durably, and the recovery relays
    /// it.
    Owe,
    /// Nothing here can relay a stop and nothing is recovering the run:
    /// the stop is owed, durably, and the exact run is recovered through
    /// its owner to relay it.
    Recover,
    /// A stop was already asked, and the receiver there is now is known
    /// to have been given it, or the launch's own check is about to
    /// refuse the run: there is nothing more to do but wait.
    Already,
    /// Nothing is claimed of the run: a stop cannot be confirmed, and is
    /// not answered as stopping.
    NotConfirmable,
    /// This daemon may not write yet.
    Recovering,
}

/// What a Stop does for a member: about the run, and about a start that
/// waits for the seat. The two are decided apart: a wanted start is
/// withdrawn whatever is known of the run before it, and withdrawing it
/// changes nothing of what that run still owes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stop {
    pub run: StopRun,
    pub withdraw: bool,
}

pub fn stop(seat: Option<&Seat>, evidence: Evidence) -> Stop {
    let case = Seat::case(seat, evidence);
    let withdraw = case != Case::Fenced && seat.is_some_and(|seat| seat.wanted.is_some());
    // Whether a stop can reach the run now, or will without help. A
    // receiver that is there is given the stop unless it is known to
    // have it: that it exists does not show it was told.
    let reach = || match (evidence.relay, evidence.driver) {
        (true, _) | (false, Driver::Launching) => {
            if evidence.stop_delivered {
                StopRun::Already
            } else {
                StopRun::Relay
            }
        }
        (false, Driver::Recovering) => StopRun::Owe,
        (false, Driver::None) => StopRun::Recover,
    };
    let run = match case {
        Case::Fenced => StopRun::Recovering,
        // Ended already; a released session is not the team's to stop.
        Case::Empty | Case::Ended(_) => StopRun::Nothing,
        Case::Unclear(_) => StopRun::NotConfirmable,
        // A stop asked before is not a stop done. What it still needs is
        // worked out again from what is there now, so that one whose
        // delivery was interrupted is finished by the next request or
        // the next reopening and not answered as already under way.
        Case::Live(start) | Case::StopAsked(start) => {
            match (start.stage, evidence.record) {
                (_, Record::Ended) => match exit_owed(seat, evidence) {
                    Some(exit) => StopRun::CommitExit(exit),
                    None => StopRun::Nothing,
                },
                (Stage::Reserved, _) => StopRun::CancelReserved,
                (Stage::RecordBound, _) if evidence.driver == Driver::Launching => {
                    if start.stopping {
                        // The launch's check has the stop to find.
                        StopRun::Already
                    } else {
                        StopRun::Refuse
                    }
                }
                // Created, and nothing is launching it: it never ran,
                // whether or not a stop was asked while something was.
                (Stage::RecordBound, _) => StopRun::EndUnstarted,
                _ => reach(),
            }
        }
    };
    Stop { run, withdraw }
}

/// What a stop that was asked still needs done, if anything: for the
/// daemon to do on any request that meets the seat, and when it reopens.
/// None where no stop is asked, or the one asked needs only waiting for.
pub fn pending(seat: Option<&Seat>, evidence: Evidence) -> Option<StopRun> {
    match Seat::case(seat, evidence) {
        Case::StopAsked(_) => match stop(seat, evidence).run {
            StopRun::Already | StopRun::Nothing => None,
            needed => Some(needed),
        },
        _ => None,
    }
}

/// What a release of a member's session does: it stays a plain session
/// and keeps its cards, and the team's work on it ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Release {
    /// There is no session yet to keep: the start is cancelled, and the
    /// answer says the session had not been created.
    Cancel,
    /// The start is ended as released, in one commit, for that exact
    /// run ([`Start::release`]), and the team's cleanup for it is given
    /// up in the same commit. From then on no stop, cleanup or late
    /// driver of the team ends it or removes its checkout.
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
pub const RELEASE_UNSUPERVISED: &str = "its session is not confirmed running under this daemon; start it again to recover it, or stop it";
pub const RELEASE_RECOVERING: &str =
    "the daemon is taking over from another and decides nothing yet; ask again in a moment";

/// What a release does for a member, or why it is refused. A refusal
/// changes nothing and authorises nothing.
///
/// A release and a stop exclude each other, and it is whichever was
/// admitted first that stands: after a stop was asked, or a start has
/// ended and its cleanup been admitted, nothing is released; after a
/// release, the start has ended as released and nothing of the team's
/// is admitted for it. A release is given only for a run that is
/// positively this start's, let run, and supervised here: by the same
/// evidence a Start would go on by, so a person never has to start a
/// member's work in order to release it from it.
pub fn release(seat: Option<&Seat>, evidence: Evidence) -> Result<Release, &'static str> {
    match Seat::case(seat, evidence) {
        Case::Fenced => Err(RELEASE_RECOVERING),
        Case::Empty => Err(RELEASE_NOTHING),
        Case::Unclear(_) => Err(RELEASE_UNCERTAIN),
        Case::Ended(start) if start.released() => Err(RELEASE_NOTHING),
        Case::Ended(_) => Err(RELEASE_ENDED),
        Case::StopAsked(_) => Err(RELEASE_STOPPED),
        Case::Live(start) => match (start.stage, evidence.record) {
            (Stage::Reserved, _) => Ok(Release::Cancel),
            (_, Record::Ended) => Err(RELEASE_ENDED),
            (_, Record::Stopping) => Err(RELEASE_STOPPED),
            (Stage::RecordBound, _) => Err(RELEASE_STARTING),
            // A verified relay proves the command was let run, written
            // down or not: the same evidence a Start goes on by. The
            // release's commit ends the start, so nothing is owed a
            // stage it never wrote.
            _ if evidence.relay => Ok(Release::Mark),
            // Its activation is not resolved.
            (Stage::Running, _) => Err(RELEASE_STARTING),
            _ if evidence.driver != Driver::None => Err(RELEASE_STARTING),
            _ => Err(RELEASE_UNSUPERVISED),
        },
    }
}

/// Where a member's session stands, for a reader: one word from the seat
/// and the evidence, by the same situation a Start, a Stop and a release
/// read. Running is said only of a run that is supervised here; one that
/// is being recovered, or is yet to be, is starting.
pub fn standing(seat: Option<&Seat>, evidence: Evidence) -> Standing {
    let waiting = seat.is_some_and(|seat| seat.wanted.is_some());
    match Seat::case(seat, evidence) {
        Case::Fenced => {
            if seat.is_some_and(|seat| seat.start.is_some() || seat.wanted.is_some()) {
                Standing::Recovering
            } else {
                Standing::NotStarted
            }
        }
        Case::Unclear(_) => Standing::Uncertain,
        _ if waiting => Standing::Waiting,
        Case::Empty => Standing::NotStarted,
        Case::Ended(start) => match &start.end {
            Some(End::Failed { .. }) => Standing::Failed,
            Some(End::Cancelled { .. }) if start.cleanup_owed => Standing::Stopping,
            // Cancelled and cleaned up, released, or an ordinary exit.
            _ => Standing::NotStarted,
        },
        Case::StopAsked(_) => Standing::Stopping,
        Case::Live(start) => match (start.stage, evidence.record) {
            (_, Record::Stopping) => Standing::Stopping,
            (Stage::Complete, Record::Ended) => Standing::NotStarted,
            (_, Record::Ended) => Standing::Failed,
            (Stage::Complete, _) if evidence.relay => Standing::Running,
            _ if evidence.relay && start.needs_attention() => Standing::NeedsAttention,
            _ => Standing::Starting,
        },
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
            run: (stage >= Stage::Running).then(|| RunId::from("run-one")),
            first_words: Words::reserved(MessageId::from("message-one".to_owned())),
            follow_up: None,
            stage,
            end: None,
            stopping: false,
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

    fn wanted(revision: u64) -> Wanted {
        Wanted {
            revision,
            member: member(),
            authorised_by: "user".into(),
            at: "2026-01-02T03:05:05Z".parse().unwrap(),
        }
    }

    /// The record seen, of the start's own run, with nothing at work on
    /// it here and no relay.
    fn orphan(record: Record) -> Evidence {
        Evidence {
            record,
            run: Run::Same,
            driver: Driver::None,
            relay: false,
            stop_delivered: false,
            writer: true,
        }
    }

    /// The same, under a verified relay.
    fn supervised(record: Record) -> Evidence {
        Evidence {
            relay: true,
            ..orphan(record)
        }
    }

    fn driven(record: Record, driver: Driver) -> Evidence {
        Evidence {
            driver,
            ..orphan(record)
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

    /// The record a stage's own commit leaves: what agrees with it.
    fn record_of(stage: Stage) -> Record {
        match stage {
            Stage::Reserved => Record::None,
            Stage::RecordBound => Record::Created,
            _ => Record::Running,
        }
    }

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

    /// A start with nothing at work on it, by its stage and its record:
    /// what the next Start does. Whether the run is supervised here (a
    /// verified relay) decides between going on and recovering it; a
    /// recorded stage alone never does.
    #[test]
    fn what_a_start_finds_with_nothing_at_work() {
        use Admit::*;
        use Record as R;
        // (stage, record, with no relay, under a verified relay)
        let table = [
            // Nothing committed beyond the reservation: the same start.
            (Stage::Reserved, R::None, Continue, Continue),
            // Created and never ran: no command was let run.
            (Stage::RecordBound, R::Created, FailUnstarted, FailUnstarted),
            // Running with its activation unrecorded. With no relay the
            // run is recovered through its owner. Under a verified
            // relay the activation is proved: it is written down and the
            // start goes on, and the owner is not reattached, which
            // would take the controller there is away.
            (Stage::Running, R::Running, Recover, RecordActivation),
            // Activated says the owner answered; it does not say the
            // run reached supervision. Only a verified relay does.
            (Stage::Activated, R::Running, Recover, Resume),
            (Stage::CardsSettled, R::Running, Recover, Resume),
            (Stage::Instructed, R::Running, Recover, Resume),
            (Stage::Complete, R::Running, Recover, AlreadyActive),
            // An exit that is seen and not yet written to the start is
            // written first, as what it is: a failure before complete,
            // an ordinary exit after.
            (
                Stage::RecordBound,
                R::Ended,
                CommitExit(Exit::Failed),
                CommitExit(Exit::Failed),
            ),
            (
                Stage::Running,
                R::Ended,
                CommitExit(Exit::Failed),
                CommitExit(Exit::Failed),
            ),
            (
                Stage::Instructed,
                R::Ended,
                CommitExit(Exit::Failed),
                CommitExit(Exit::Failed),
            ),
            (
                Stage::Complete,
                R::Ended,
                CommitExit(Exit::Ordinary),
                CommitExit(Exit::Ordinary),
            ),
            (Stage::Activated, R::Stopping, Stopping, Stopping),
            (Stage::Complete, R::Stopping, Stopping, Stopping),
        ];
        for (stage, record, alone, relayed) in table {
            let seat = seat(start(stage));
            assert_eq!(
                admit(Some(&seat), orphan(record)),
                alone,
                "{stage:?} with the record {record:?} and no relay"
            );
            assert_eq!(
                admit(Some(&seat), supervised(record)),
                relayed,
                "{stage:?} with the record {record:?} under a relay"
            );
        }
        // No seat, or a seat nothing was started in.
        assert_eq!(admit(None, orphan(Record::None)), Reserve);
        assert_eq!(
            admit(
                Some(&Seat::new(MemberId::from("member-one"))),
                orphan(Record::None)
            ),
            Reserve
        );
    }

    /// A launch or a recovery at work here means starting, at every
    /// stage with the record that agrees with it, and nothing is done;
    /// a daemon that may not write does nothing at all, whatever it
    /// sees, for a Start, a Stop, a release and a reader alike.
    #[test]
    fn something_at_work_and_a_daemon_that_may_not_write() {
        for stage in STAGES {
            let seat = seat(start(stage));
            for driver in [Driver::Launching, Driver::Recovering] {
                assert_eq!(
                    admit(Some(&seat), driven(record_of(stage), driver)),
                    Admit::Starting,
                    "{stage:?} {driver:?}"
                );
            }
            for record in RECORDS {
                for driver in [Driver::None, Driver::Launching, Driver::Recovering] {
                    for relay in [true, false] {
                        let fenced = Evidence {
                            record,
                            run: Run::Same,
                            driver,
                            relay,
                            stop_delivered: false,
                            writer: false,
                        };
                        assert_eq!(Seat::case(Some(&seat), fenced), Case::Fenced);
                        assert_eq!(admit(Some(&seat), fenced), Admit::Recovering);
                        assert_eq!(
                            stop(Some(&seat), fenced),
                            Stop {
                                run: StopRun::Recovering,
                                withdraw: false
                            }
                        );
                        assert_eq!(release(Some(&seat), fenced), Err(RELEASE_RECOVERING));
                        assert_eq!(standing(Some(&seat), fenced), Standing::Recovering);
                    }
                }
            }
        }
        let fenced = Evidence {
            writer: false,
            ..orphan(Record::None)
        };
        assert_eq!(
            admit(None, fenced),
            Admit::Recovering,
            "a reservation is a write too"
        );
        assert_eq!(standing(None, fenced), Standing::NotStarted);
    }

    /// What is stored and what is seen that cannot both be true is one
    /// situation to all four readers: nothing is claimed. A reservation
    /// with a record it never committed; a bound or later start whose
    /// record is missing, which is not an exit anybody saw; a start that
    /// proved a process and a record that says none ever was.
    #[test]
    fn what_cannot_both_be_true_is_not_confirmed_to_every_reader() {
        use Record as R;
        let contradictions = [
            (Stage::Reserved, R::Created),
            (Stage::Reserved, R::Running),
            (Stage::Reserved, R::Stopping),
            (Stage::Reserved, R::Ended),
            (Stage::RecordBound, R::None),
            (Stage::RecordBound, R::Running),
            (Stage::RecordBound, R::Stopping),
            (Stage::Running, R::None),
            (Stage::Running, R::Created),
            (Stage::Activated, R::None),
            (Stage::Instructed, R::Created),
            (Stage::Complete, R::None),
            (Stage::Complete, R::Created),
        ];
        for (stage, record) in contradictions {
            let seat = seat(start(stage));
            for evidence in [
                orphan(record),
                supervised(record),
                driven(record, Driver::Launching),
                driven(record, Driver::Recovering),
            ] {
                let said = format!("{stage:?} with the record {record:?}: {evidence:?}");
                assert!(
                    matches!(Seat::case(Some(&seat), evidence), Case::Unclear(_)),
                    "{said}"
                );
                assert_eq!(admit(Some(&seat), evidence), Admit::Uncertain, "{said}");
                assert_eq!(
                    stop(Some(&seat), evidence).run,
                    StopRun::NotConfirmable,
                    "{said}"
                );
                assert_eq!(
                    release(Some(&seat), evidence),
                    Err(RELEASE_UNCERTAIN),
                    "{said}"
                );
                assert_eq!(
                    standing(Some(&seat), evidence),
                    Standing::Uncertain,
                    "{said}"
                );
            }
        }
        // A start marked uncertain is the same, at every stage, whatever
        // is seen; and it is not settled.
        for stage in STAGES {
            let mut unsure = start(stage);
            unsure.uncertain = true;
            assert!(!unsure.settled());
            let seat = seat(unsure);
            for record in RECORDS {
                let evidence = supervised(record);
                assert_eq!(admit(Some(&seat), evidence), Admit::Uncertain);
                assert_eq!(stop(Some(&seat), evidence).run, StopRun::NotConfirmable);
                assert_eq!(release(Some(&seat), evidence), Err(RELEASE_UNCERTAIN));
                assert_eq!(standing(Some(&seat), evidence), Standing::Uncertain);
            }
        }
    }

    /// An action is for the run the start made, and for no other. A
    /// later run under the same agent id, with the same status and even
    /// a relay of its own, is not the old healthy session: it is not
    /// already active, not stopped, not released and not shown as
    /// running; and neither is one that cannot be shown to be either.
    /// Both runs are protected.
    #[test]
    fn two_runs_under_one_agent_id_are_two_runs() {
        for stage in &STAGES[2..] {
            for record in [Record::Running, Record::Stopping, Record::Ended] {
                for run in [Run::Other, Run::Unproven] {
                    for relay in [true, false] {
                        let seat = seat(start(*stage));
                        let evidence = Evidence {
                            run,
                            relay,
                            ..orphan(record)
                        };
                        let said = format!("{stage:?} {record:?} {run:?} relay {relay}");
                        assert_eq!(admit(Some(&seat), evidence), Admit::Uncertain, "{said}");
                        assert_eq!(
                            stop(Some(&seat), evidence).run,
                            StopRun::NotConfirmable,
                            "{said}"
                        );
                        assert_eq!(
                            release(Some(&seat), evidence),
                            Err(RELEASE_UNCERTAIN),
                            "{said}"
                        );
                        assert_eq!(
                            standing(Some(&seat), evidence),
                            Standing::Uncertain,
                            "{said}"
                        );
                    }
                }
            }
        }
        // The same sight, positively matched, is the healthy session.
        let healthy = seat(start(Stage::Complete));
        assert_eq!(
            admit(Some(&healthy), supervised(Record::Running)),
            Admit::AlreadyActive
        );
        // A start that reached running without a run of its own written
        // down cannot match anything.
        let mut unbound = start(Stage::Complete);
        unbound.run = None;
        assert_eq!(
            admit(Some(&seat(unbound)), supervised(Record::Running)),
            Admit::Uncertain
        );
        // Before a run exists there is none to compare: the field is
        // not read.
        let reserved = seat(start(Stage::Reserved));
        assert_eq!(
            admit(
                Some(&reserved),
                Evidence {
                    run: Run::Unproven,
                    ..orphan(Record::None)
                }
            ),
            Admit::Continue
        );
    }

    /// With something owed after its first words, the member is tried
    /// again for the same session; with nothing owed the start only has
    /// to be completed. Both need the run to be supervised here.
    #[test]
    fn what_is_owed_is_tried_again_for_the_same_session() {
        let mut owing = start(Stage::Instructed);
        owing.cards.owed.push(Held {
            card: TaskId::from("task-one".to_owned()),
            why: "another session holds a lease on it".into(),
        });
        assert!(owing.needs_attention());
        let owing = seat(owing);
        assert_eq!(
            admit(Some(&owing), supervised(Record::Running)),
            Admit::RetryOwed
        );
        assert_eq!(
            standing(Some(&owing), supervised(Record::Running)),
            Standing::NeedsAttention
        );
        assert_eq!(admit(Some(&owing), orphan(Record::Running)), Admit::Recover);
        assert_eq!(
            standing(Some(&owing), orphan(Record::Running)),
            Standing::Starting
        );
        let clear = seat(start(Stage::Instructed));
        assert_eq!(
            admit(Some(&clear), supervised(Record::Running)),
            Admit::Resume
        );
    }

    /// The waiting start, through its whole life. Stop, then Start while
    /// the stopped driver is still held: the request is recorded as
    /// wanted, not answered as starting. A repeat meets the same wanted
    /// start. The old operation settles: the wanted one is promoted,
    /// from its own snapshot, once. A later Stop withdraws a wanted
    /// start and leaves the predecessor's unfinished business alone.
    #[test]
    fn a_waiting_start_is_made_met_promoted_once_and_withdrawn() {
        let a_seat = seat;
        // Stop won; its driver is still alive and its run still there.
        let mut stopped = start(Stage::Activated);
        stopped.stopping = true;
        let mut seat = seat(stopped);
        seat.generation = 2;
        let held = driven(Record::Running, Driver::Launching);
        assert_eq!(admit(Some(&seat), held), Admit::Wait, "not Starting");
        // The daemon records the request as the wanted start.
        seat.wanted = Some(wanted(2));
        assert_eq!(admit(Some(&seat), held), Admit::Waiting);
        assert_eq!(admit(Some(&seat), orphan(Record::Stopping)), Admit::Waiting);
        assert_eq!(standing(Some(&seat), held), Standing::Waiting);
        // The run is seen to end, with its cleanup still to do.
        let old = seat.start.as_mut().unwrap();
        old.end = Some(End::Cancelled {
            why: "stopped".into(),
        });
        old.cleanup_owed = true;
        assert_eq!(admit(Some(&seat), orphan(Record::Ended)), Admit::Waiting);
        // A later Stop withdraws the wanted start and keeps what the old
        // one owes.
        assert_eq!(
            stop(Some(&seat), orphan(Record::Ended)),
            Stop {
                run: StopRun::Nothing,
                withdraw: true
            }
        );
        // (Not withdrawn here.) The cleanup is done, also after the
        // daemon reopened with no driver: settled, and the first
        // authorised snapshot begins.
        seat.start.as_mut().unwrap().cleanup_owed = false;
        assert_eq!(admit(Some(&seat), orphan(Record::Ended)), Admit::Promote);
        assert_eq!(
            seat.wanted.as_ref().unwrap().revision,
            2,
            "its own snapshot"
        );
        // The daemon promotes it: the wait is emptied in the commit that
        // reserves the new start, so it cannot be promoted again.
        let mut promoted = start(Stage::Reserved);
        promoted.id = StartId::from("start-two");
        promoted.generation = 3;
        promoted.revision = seat.wanted.take().unwrap().revision;
        seat.generation = 3;
        seat.start = Some(promoted);
        assert_eq!(admit(Some(&seat), orphan(Record::None)), Admit::Continue);
        assert!(seat.wanted.is_none());
        // With nothing waiting, a free seat is reserved from the request.
        let mut ended = start(Stage::Running);
        ended.end = Some(End::Failed {
            why: "its owner went away".into(),
        });
        assert_eq!(
            admit(Some(&a_seat(ended.clone())), orphan(Record::Ended)),
            Admit::Reserve
        );
        // An ended start with a cleanup owed and nothing yet waiting.
        ended.cleanup_owed = true;
        assert!(!ended.settled());
        for record in [Record::Running, Record::Ended] {
            assert_eq!(
                admit(Some(&a_seat(ended.clone())), orphan(record)),
                Admit::Wait
            );
        }
        // A complete start whose session was seen to end: the exit is
        // written first; with a cleanup it owes the seat is waited for,
        // and what waited behind it is promoted after.
        let mut complete = start(Stage::Complete);
        complete.cleanup_owed = true;
        let mut after = a_seat(complete);
        assert_eq!(
            admit(Some(&after), orphan(Record::Ended)),
            Admit::CommitExit(Exit::Ordinary)
        );
        after.start.as_mut().unwrap().end = Some(End::Exited);
        assert_eq!(admit(Some(&after), orphan(Record::Ended)), Admit::Wait);
        after.wanted = Some(wanted(4));
        assert_eq!(admit(Some(&after), orphan(Record::Ended)), Admit::Waiting);
        after.start.as_mut().unwrap().cleanup_owed = false;
        assert_eq!(admit(Some(&after), orphan(Record::Ended)), Admit::Promote);
        // A wanted start in a seat with no start at all is promoted too.
        let mut bare = Seat::new(MemberId::from("member-one"));
        bare.wanted = Some(wanted(5));
        assert_eq!(admit(Some(&bare), orphan(Record::None)), Admit::Promote);
        assert_eq!(
            standing(Some(&bare), orphan(Record::None)),
            Standing::Waiting
        );
    }

    /// A wanted start is withdrawn by a Stop whatever is known of the
    /// run before it, a run nothing can be claimed of included.
    #[test]
    fn a_stop_withdraws_what_waits_whatever_the_run_before_it() {
        let mut unsure = start(Stage::Activated);
        unsure.uncertain = true;
        let mut seat = seat(unsure);
        seat.wanted = Some(wanted(2));
        assert_eq!(
            stop(Some(&seat), orphan(Record::Running)),
            Stop {
                run: StopRun::NotConfirmable,
                withdraw: true
            }
        );
        seat.wanted = None;
        assert!(!stop(Some(&seat), orphan(Record::Running)).withdraw);
    }

    /// A stop goes where it can be delivered. To a run supervised here,
    /// or still held by its launch, it is relayed. While the run is
    /// being recovered there is nothing to relay through: it is owed.
    /// With nothing at work and no relay, in either window a driver can
    /// be lost in (running committed and the command not yet let run;
    /// let run and not yet handed to supervision), it is owed and the
    /// run is recovered to relay it. Refusing at the launch's own check
    /// is only for a launch that is alive and has not committed running.
    #[test]
    fn a_stop_goes_where_it_can_be_delivered() {
        use Driver as D;
        use Record as R;
        use StopRun::*;
        // (stage, record, no relay: nothing at work / launching /
        // recovering, under a relay)
        let table = [
            (
                Stage::Reserved,
                R::None,
                CancelReserved,
                CancelReserved,
                CancelReserved,
                CancelReserved,
            ),
            (
                Stage::RecordBound,
                R::Created,
                EndUnstarted,
                Refuse,
                EndUnstarted,
                EndUnstarted,
            ),
            // The first window: running committed, activation unknown.
            (Stage::Running, R::Running, Recover, Relay, Owe, Relay),
            // The second: let run, supervision not shown.
            (Stage::Activated, R::Running, Recover, Relay, Owe, Relay),
            (Stage::Instructed, R::Running, Recover, Relay, Owe, Relay),
            (Stage::Complete, R::Running, Recover, Relay, Owe, Relay),
            // Somebody's stop is under way already: the team's is given
            // to the receiver there is all the same, since nothing shows
            // it has it.
            (Stage::Complete, R::Stopping, Recover, Relay, Owe, Relay),
            (
                Stage::Complete,
                R::Ended,
                CommitExit(Exit::Ordinary),
                CommitExit(Exit::Ordinary),
                CommitExit(Exit::Ordinary),
                CommitExit(Exit::Ordinary),
            ),
            (
                Stage::Running,
                R::Ended,
                CommitExit(Exit::Failed),
                CommitExit(Exit::Failed),
                CommitExit(Exit::Failed),
                CommitExit(Exit::Failed),
            ),
            (
                Stage::RecordBound,
                R::Ended,
                CommitExit(Exit::Failed),
                CommitExit(Exit::Failed),
                CommitExit(Exit::Failed),
                CommitExit(Exit::Failed),
            ),
        ];
        for (stage, record, alone, launching, recovering, relayed) in table {
            let seat = seat(start(stage));
            let said = |how: &str| format!("{stage:?} with the record {record:?}, {how}");
            assert_eq!(
                stop(Some(&seat), orphan(record)).run,
                alone,
                "{}",
                said("nothing at work")
            );
            assert_eq!(
                stop(Some(&seat), driven(record, D::Launching)).run,
                launching,
                "{}",
                said("its launch alive")
            );
            assert_eq!(
                stop(Some(&seat), driven(record, D::Recovering)).run,
                recovering,
                "{}",
                said("being recovered")
            );
            assert_eq!(
                stop(Some(&seat), supervised(record)).run,
                relayed,
                "{}",
                said("under a relay")
            );
        }
        assert_eq!(
            stop(None, orphan(Record::None)),
            Stop {
                run: Nothing,
                withdraw: false
            }
        );
        // A released session is not the team's to stop.
        let mut released = start(Stage::Complete);
        released.release();
        assert_eq!(
            stop(Some(&seat(released)), supervised(Record::Running)).run,
            Nothing
        );
    }

    /// A stop that was asked is not a stop that was done. Its delivery
    /// can be interrupted at two points, and what it still needs is
    /// worked out again from what is there, on the next Stop, the next
    /// Start, or the daemon's reopening.
    ///
    /// Before running: the launch that was to refuse the run at its own
    /// check dies first. The created record is abandoned, and is ended
    /// as one that never ran, exactly as it would be with no stop asked.
    ///
    /// After running: the stop was written down and the receiver went
    /// before it was given it. A receiver that is there afterwards, a
    /// newly made one included, has not been told because it exists: it
    /// is given the stop, unless it is known to carry it.
    #[test]
    fn a_stop_that_was_asked_is_finished_by_whoever_comes_next() {
        use StopRun::*;
        // The first point.
        let mut bound = start(Stage::RecordBound);
        let live = seat(bound.clone());
        assert_eq!(
            stop(Some(&live), driven(Record::Created, Driver::Launching)).run,
            Refuse
        );
        bound.stopping = true;
        let asked = seat(bound);
        // While its launch lives, the launch's check has it: wait.
        let launching = driven(Record::Created, Driver::Launching);
        assert_eq!(stop(Some(&asked), launching).run, Already);
        assert_eq!(pending(Some(&asked), launching), None);
        // The launch died: nothing will refuse the run now.
        let abandoned = orphan(Record::Created);
        assert_eq!(
            stop(Some(&asked), abandoned).run,
            EndUnstarted,
            "not Already"
        );
        assert_eq!(pending(Some(&asked), abandoned), Some(EndUnstarted));
        assert_eq!(
            admit(Some(&asked), abandoned),
            Admit::Wait,
            "and a Start waits for it"
        );
        // It is seen to end: what is left of the stop is to write that
        // down, as the stop done.
        assert_eq!(
            stop(Some(&asked), orphan(Record::Ended)).run,
            CommitExit(Exit::Cancelled)
        );
        assert_eq!(
            pending(Some(&asked), orphan(Record::Ended)),
            Some(CommitExit(Exit::Cancelled))
        );

        // The second point, at every stage from running on.
        for stage in &STAGES[2..] {
            let mut running = start(*stage);
            running.stopping = true;
            let asked = seat(running);
            // The receiver was lost before it was given the stop.
            assert_eq!(
                pending(Some(&asked), orphan(Record::Running)),
                Some(Recover)
            );
            assert_eq!(
                pending(Some(&asked), driven(Record::Running, Driver::Recovering)),
                Some(Owe)
            );
            // A receiver made since has not been told: it is given the
            // stop, whether the record still says running or already
            // says stopping.
            for record in [Record::Running, Record::Stopping] {
                assert_eq!(
                    stop(Some(&asked), supervised(record)).run,
                    Relay,
                    "{stage:?} {record:?}: not Already"
                );
                assert_eq!(pending(Some(&asked), supervised(record)), Some(Relay));
                assert_eq!(
                    stop(Some(&asked), driven(record, Driver::Launching)).run,
                    Relay
                );
                // Known to carry it: nothing more to do but wait.
                let told = Evidence {
                    stop_delivered: true,
                    ..supervised(record)
                };
                assert_eq!(stop(Some(&asked), told).run, Already);
                assert_eq!(pending(Some(&asked), told), None);
            }
        }
        // A stop asked of a reservation that was not cancelled yet.
        let mut reserved = start(Stage::Reserved);
        reserved.stopping = true;
        assert_eq!(
            pending(Some(&seat(reserved)), orphan(Record::None)),
            Some(CancelReserved)
        );
        // No stop asked, nothing pending.
        assert_eq!(
            pending(Some(&seat(start(Stage::Complete))), orphan(Record::Running)),
            None
        );
    }

    /// A recovery's reply is judged against the seat as it is when the
    /// reply comes, and against the start and the run the recovery set
    /// out for. A stop asked while it ran is relayed and the team's work
    /// is not continued; a session released meanwhile is left as found;
    /// a receiver that is already there is never displaced. A reply that
    /// comes back to a seat another start has taken, to a daemon that
    /// lost its authority, or to a start marked uncertain or ended while
    /// the recovery was away, resumes nothing and installs nothing.
    #[test]
    fn a_recovery_is_judged_against_the_seat_it_comes_back_to() {
        let id = StartId::from("start-one");
        let run = RunId::from("run-one");
        let expected = Expected {
            start: &id,
            run: &run,
        };
        let answers = [
            Reattached::Activated,
            Reattached::Unactivated,
            Reattached::Exited,
            Reattached::Unidentified,
        ];
        let going = seat(start(Stage::Activated));
        for relay in [true, false] {
            let seen = Evidence {
                relay,
                ..orphan(Record::Running)
            };
            assert_eq!(
                recovered(Some(&going), expected, Reattached::Activated, seen),
                Recovered::Resume { install: !relay }
            );
        }
        let seen = orphan(Record::Running);
        // Never let run, of a start whose activation is not written.
        let held = seat(start(Stage::Running));
        assert_eq!(
            recovered(Some(&held), expected, Reattached::Unactivated, seen),
            Recovered::EndUnactivated
        );
        // The same answer of a start whose activation is written down
        // contradicts what is proved: nothing is claimed.
        assert_eq!(
            recovered(Some(&going), expected, Reattached::Unactivated, seen),
            Recovered::Uncertain
        );
        assert_eq!(
            recovered(Some(&going), expected, Reattached::Unidentified, seen),
            Recovered::Uncertain
        );
        // A stop became owed during the recovery.
        let mut asked = start(Stage::Activated);
        asked.stopping = true;
        let asked = seat(asked);
        for relay in [true, false] {
            let seen = Evidence {
                relay,
                ..orphan(Record::Running)
            };
            assert_eq!(
                recovered(Some(&asked), expected, Reattached::Activated, seen),
                Recovered::RelayStop { install: !relay },
                "never resumed"
            );
        }
        // Released during the recovery: whatever the owner answers.
        let mut released = start(Stage::Activated);
        released.release();
        let released = seat(released);
        for answer in answers {
            assert_eq!(
                recovered(Some(&released), expected, answer, seen),
                Recovered::Leave
            );
        }
        // An old reply after the seat was given to another start, or
        // after its run was replaced under the same start id: dropped.
        let mut replaced = start(Stage::Running);
        replaced.id = StartId::from("start-two");
        let replaced = seat(replaced);
        let mut another_run = start(Stage::Activated);
        another_run.run = Some(RunId::from("run-two"));
        let another_run = seat(another_run);
        for answer in answers {
            for relay in [true, false] {
                let seen = Evidence {
                    relay,
                    ..orphan(Record::Running)
                };
                assert_eq!(
                    recovered(Some(&replaced), expected, answer, seen),
                    Recovered::Stale
                );
                assert_eq!(
                    recovered(Some(&another_run), expected, answer, seen),
                    Recovered::Stale
                );
                assert_eq!(recovered(None, expected, answer, seen), Recovered::Stale);
                // The daemon lost its authority while it was away.
                let fenced = Evidence {
                    writer: false,
                    ..seen
                };
                assert_eq!(
                    recovered(Some(&going), expected, answer, fenced),
                    Recovered::Fenced
                );
            }
        }
        // Marked uncertain while the recovery was away: nothing is
        // resumed and nothing installed, whatever the owner says; and
        // such a start's driver may not go on either.
        let mut unsure = start(Stage::Activated);
        unsure.uncertain = true;
        let unsure = seat(unsure);
        for answer in answers {
            assert_eq!(
                recovered(Some(&unsure), expected, answer, seen),
                Recovered::Uncertain
            );
        }
        assert!(!unsure.owns(1, &id), "no progress on a start in doubt");
        assert!(going.owns(1, &id));
        // Ended while the recovery was away: a live run is the cleanup
        // it owes and is stopped, never gone on with; an exit is only
        // committed, and its end stands.
        let mut failed = start(Stage::Activated);
        failed.end = Some(End::Failed {
            why: "its cards could not be read".into(),
        });
        failed.cleanup_owed = true;
        let failed = seat(failed);
        assert_eq!(
            recovered(Some(&failed), expected, Reattached::Activated, seen),
            Recovered::RelayStop { install: true }
        );
        assert_eq!(
            recovered(Some(&failed), expected, Reattached::Exited, seen),
            Recovered::CommitExit(Exit::AlreadyEnded)
        );
        // Never let run, said of an ended start whose activation is
        // written down: as little believed as of a live one.
        assert_eq!(
            recovered(Some(&failed), expected, Reattached::Unactivated, seen),
            Recovered::Uncertain
        );
        let mut early = start(Stage::Running);
        early.end = Some(End::Failed {
            why: "its owner went away".into(),
        });
        early.cleanup_owed = true;
        assert_eq!(
            recovered(Some(&seat(early)), expected, Reattached::Unactivated, seen),
            Recovered::EndUnactivated
        );
    }

    /// An exit means what it is, not when the daemon learned of it. A
    /// member whose start was complete and whose session then ended has
    /// simply stopped running: that is so when the record already shows
    /// the exit, and equally when the record still says running and the
    /// exit is found saved by the run's owner at a recovery. An exit
    /// before the start was complete is a start that failed; one after
    /// a stop was asked is the stop done.
    #[test]
    fn an_exit_is_the_same_exit_whenever_it_is_learned_of() {
        let id = StartId::from("start-one");
        let run = RunId::from("run-one");
        let expected = Expected {
            start: &id,
            run: &run,
        };
        let stale = orphan(Record::Running);
        // Complete, the record stale: recovered, and the saved exit is
        // an ordinary one.
        let complete = seat(start(Stage::Complete));
        assert_eq!(admit(Some(&complete), stale), Admit::Recover);
        assert_eq!(
            recovered(Some(&complete), expected, Reattached::Exited, stale),
            Recovered::CommitExit(Exit::Ordinary)
        );
        // The same exit, already on the record: it is the same ordinary
        // exit, written to the start the same way, and the reader is
        // told the same.
        assert_eq!(
            admit(Some(&complete), orphan(Record::Ended)),
            Admit::CommitExit(Exit::Ordinary)
        );
        assert_eq!(
            standing(Some(&complete), orphan(Record::Ended)),
            Standing::NotStarted
        );
        // Either way the start then ends as exited, and the seat is free.
        let mut exited = start(Stage::Complete);
        exited.end = Some(End::Exited);
        assert!(exited.settled());
        assert_eq!(
            admit(Some(&seat(exited.clone())), orphan(Record::Ended)),
            Admit::Reserve
        );
        assert_eq!(
            standing(Some(&seat(exited.clone())), orphan(Record::Ended)),
            Standing::NotStarted
        );
        // With a cleanup outstanding the exit is still written first,
        // the seat is then waited for, and the cleanup is the ended
        // start's own to do.
        let mut owing = start(Stage::Complete);
        owing.cleanup_owed = true;
        assert_eq!(
            admit(Some(&seat(owing)), orphan(Record::Ended)),
            Admit::CommitExit(Exit::Ordinary)
        );
        exited.cleanup_owed = true;
        let exited = seat(exited);
        assert_eq!(admit(Some(&exited), orphan(Record::Ended)), Admit::Wait);
        assert!(exited.may_clean(&id, orphan(Record::Ended)));
        // An unfinished start's exit is a failure, learned of either way.
        for stage in &STAGES[2..6] {
            let unfinished = seat(start(*stage));
            assert_eq!(
                recovered(Some(&unfinished), expected, Reattached::Exited, stale),
                Recovered::CommitExit(Exit::Failed),
                "{stage:?}"
            );
            assert_eq!(
                admit(Some(&unfinished), orphan(Record::Ended)),
                Admit::CommitExit(Exit::Failed),
                "{stage:?}"
            );
        }
        // After a stop was asked, it is the stop done, complete or not.
        for stage in [Stage::Activated, Stage::Complete] {
            let mut asked = start(stage);
            asked.stopping = true;
            assert_eq!(
                recovered(Some(&seat(asked)), expected, Reattached::Exited, stale),
                Recovered::CommitExit(Exit::Cancelled)
            );
        }
    }

    /// An answer about one run is no leave to act on another. The seat
    /// and what the recovery set out for are unchanged, and meanwhile
    /// the registry has moved to a later run under the same agent id, or
    /// can no longer show which run it has: nothing is installed,
    /// resumed, stopped or ended on the strength of the old answer. And
    /// an answer that was overtaken while it was on its way gives way:
    /// to a stop that shows only on the record, and to an exit already
    /// committed there.
    #[test]
    fn an_answer_about_one_run_is_no_leave_to_act_on_another() {
        let id = StartId::from("start-one");
        let run = RunId::from("run-one");
        let expected = Expected {
            start: &id,
            run: &run,
        };
        let answers = [
            Reattached::Activated,
            Reattached::Unactivated,
            Reattached::Exited,
        ];
        for stage in [Stage::Running, Stage::Activated, Stage::Complete] {
            let seat = seat(start(stage));
            for now in [Run::Other, Run::Unproven] {
                for record in [Record::Running, Record::Stopping, Record::Ended] {
                    for relay in [true, false] {
                        let moved = Evidence {
                            run: now,
                            relay,
                            ..orphan(record)
                        };
                        for answer in answers {
                            assert_eq!(
                                recovered(Some(&seat), expected, answer, moved),
                                Recovered::Uncertain,
                                "{stage:?} {now:?} {record:?} {answer:?}"
                            );
                        }
                    }
                }
            }
            // A record that vanished or says less than was proved.
            for record in [Record::None, Record::Created] {
                assert_eq!(
                    recovered(Some(&seat), expected, Reattached::Activated, orphan(record)),
                    Recovered::Uncertain
                );
            }
        }
        let going = seat(start(Stage::Activated));
        // Overtaken by an ordinary stop of the session: it shows only as
        // the record stopping, and the team's work is not gone on with.
        for relay in [true, false] {
            let stopping = Evidence {
                relay,
                ..orphan(Record::Stopping)
            };
            assert_eq!(
                recovered(Some(&going), expected, Reattached::Activated, stopping),
                Recovered::RelayStop { install: !relay }
            );
        }
        // Overtaken by an exit already on the record: a reply about a
        // live run does nothing; an owner still holding the exit's
        // report is acknowledged. Writing the exit to the start is the
        // next request's, as the kind of exit it is.
        for answer in [Reattached::Activated, Reattached::Unactivated] {
            assert_eq!(
                recovered(Some(&going), expected, answer, orphan(Record::Ended)),
                Recovered::Nothing,
                "{answer:?}"
            );
        }
        assert_eq!(
            recovered(
                Some(&going),
                expected,
                Reattached::Exited,
                orphan(Record::Ended)
            ),
            Recovered::Acknowledge
        );
        assert_eq!(
            exit_owed(Some(&going), orphan(Record::Ended)),
            Some(Exit::Failed)
        );
    }

    /// A record that has ended does not leave its seat waiting for ever.
    /// A stop asked, the run seen to end, and no end yet on the start: a
    /// Stop, a Start and the daemon's own reconciliation each say the
    /// exit is to be written, as the stop done. Written, with a cleanup
    /// still owed, the seat is waited for and the cleanup is admitted;
    /// the cleanup done, what waited is promoted. The same for a
    /// complete start whose exit is on the record: an ordinary exit.
    /// Each step is read from the stored seat alone, as a daemon that
    /// reopened would read it.
    #[test]
    fn an_ended_record_is_written_down_cleaned_up_after_and_given_up() {
        let id = StartId::from("start-one");
        let ended = orphan(Record::Ended);
        let reopened = |seat: &Seat| -> Seat {
            serde_json::from_value(serde_json::to_value(seat).unwrap()).unwrap()
        };
        for (stage, stopping, exit, end) in [
            (
                Stage::Activated,
                true,
                Exit::Cancelled,
                End::Cancelled {
                    why: "stopped".into(),
                },
            ),
            (Stage::Complete, false, Exit::Ordinary, End::Exited),
        ] {
            let mut left = start(stage);
            left.stopping = stopping;
            left.cleanup_owed = true;
            let mut seat = seat(left);
            seat.wanted = Some(wanted(2));
            // Nothing is admitted to clean while the exit is unwritten,
            // and nothing waits for ever either: every reader says what
            // is to be done.
            assert_eq!(exit_owed(Some(&seat), ended), Some(exit));
            assert_eq!(admit(Some(&seat), ended), Admit::CommitExit(exit));
            assert_eq!(stop(Some(&seat), ended).run, StopRun::CommitExit(exit));
            if stopping {
                assert_eq!(pending(Some(&seat), ended), Some(StopRun::CommitExit(exit)));
            }
            assert!(!seat.may_clean(&id, ended));
            // The exit is written, keeping the cleanup; the daemon
            // reopens.
            seat.start.as_mut().unwrap().end = Some(end);
            let mut seat = reopened(&seat);
            assert_eq!(exit_owed(Some(&seat), ended), None);
            assert_eq!(admit(Some(&seat), ended), Admit::Waiting);
            assert_eq!(pending(Some(&seat), ended), None);
            assert!(
                seat.may_clean(&id, ended),
                "{stage:?}: the cleanup is admitted"
            );
            // The cleanup is done, and reopened again: promoted, with
            // the snapshot that was asked first.
            seat.start.as_mut().unwrap().cleanup_owed = false;
            let seat = reopened(&seat);
            assert_eq!(admit(Some(&seat), ended), Admit::Promote);
            assert_eq!(seat.wanted.as_ref().unwrap().revision, 2);
        }
        // With nothing waiting and nothing to clean, the seat is free as
        // soon as the exit is written.
        let mut plain = seat(start(Stage::Complete));
        assert_eq!(
            admit(Some(&plain), ended),
            Admit::CommitExit(Exit::Ordinary)
        );
        plain.start.as_mut().unwrap().end = Some(End::Exited);
        assert_eq!(admit(Some(&plain), ended), Admit::Reserve);
        // What is seen and not to be trusted writes nothing.
        let other = Evidence {
            run: Run::Other,
            ..ended
        };
        assert_eq!(exit_owed(Some(&seat(start(Stage::Complete))), other), None);
        assert_eq!(exit_owed(None, ended), None);
    }

    /// A destructive act is for an obligation that is still outstanding,
    /// on what is positively the start's own. A late reply that the run
    /// lives, after the ended start's cleanup was finished, stops
    /// nothing: there is no obligation left for it. A cleanup presented
    /// when another run stands under the same agent id, or the run
    /// cannot be told, is refused, as it is to a daemon that may not
    /// write. What a start reserved before it ever made a run (its
    /// worktree) is named by the reservation, and is cleaned whatever
    /// the registry shows.
    #[test]
    fn a_destructive_act_needs_an_obligation_still_owed_on_its_own_run() {
        let id = StartId::from("start-one");
        let run = RunId::from("run-one");
        let expected = Expected {
            start: &id,
            run: &run,
        };
        let seen = orphan(Record::Running);
        let mut failed = start(Stage::Activated);
        failed.end = Some(End::Failed {
            why: "its cards could not be read".into(),
        });
        failed.cleanup_owed = true;
        let owing = seat(failed.clone());
        assert_eq!(
            recovered(Some(&owing), expected, Reattached::Activated, seen),
            Recovered::RelayStop { install: true }
        );
        assert!(owing.may_clean(&id, seen));
        // The cleanup finished while the reply was on its way.
        failed.cleanup_owed = false;
        let done = seat(failed);
        assert_eq!(admit(Some(&done), seen), Admit::Reserve, "the seat is free");
        for answer in [
            Reattached::Activated,
            Reattached::Unactivated,
            Reattached::Exited,
        ] {
            assert_eq!(
                recovered(Some(&done), expected, answer, seen),
                Recovered::Nothing,
                "{answer:?}"
            );
        }
        assert!(!done.may_clean(&id, seen));
        // A replacement run under the same agent id, or one that cannot
        // be told, and a daemon that may not write.
        for refused in [
            Evidence {
                run: Run::Other,
                ..seen
            },
            Evidence {
                run: Run::Unproven,
                ..seen
            },
            Evidence {
                writer: false,
                ..seen
            },
        ] {
            assert!(!owing.may_clean(&id, refused), "{refused:?}");
        }
        // Before any run: the reserved worktree is cleaned by its
        // reservation, with no run to compare.
        for stage in [Stage::Reserved, Stage::RecordBound] {
            let mut cancelled = start(stage);
            cancelled.end = Some(End::Cancelled {
                why: "stopped".into(),
            });
            cancelled.cleanup_owed = true;
            let cancelled = seat(cancelled);
            for now in [Run::Same, Run::Other, Run::Unproven] {
                for record in [Record::None, Record::Ended, Record::Running] {
                    assert!(
                        cancelled.may_clean(
                            &id,
                            Evidence {
                                run: now,
                                ..orphan(record)
                            }
                        ),
                        "{stage:?} {now:?} {record:?}"
                    );
                }
            }
        }
    }

    /// A Start and a release read a supervised run the same way. A
    /// running-stage start with its own running record under a verified
    /// relay is proved let run, whether or not that was written down:
    /// a Start goes on from it, and a release is given for it, on the
    /// same evidence. So nobody has to start a member's work in order
    /// to release its session from that work; and once released, the
    /// session is given none. With no relay its activation is not
    /// resolved, and it is not released.
    #[test]
    fn a_supervised_run_is_released_on_the_evidence_a_start_goes_on_by() {
        let id = StartId::from("start-one");
        let relayed = supervised(Record::Running);
        let mut proved = seat(start(Stage::Running));
        assert_eq!(admit(Some(&proved), relayed), Admit::RecordActivation);
        assert_eq!(release(Some(&proved), relayed), Ok(Release::Mark));
        // The release's commit, with no Start in between.
        proved.start.as_mut().unwrap().release();
        let released = proved;
        assert!(!released.owns(1, &id), "no team work is driven for it");
        assert_eq!(
            admit(Some(&released), relayed),
            Admit::Reserve,
            "not resumed"
        );
        assert_eq!(stop(Some(&released), relayed).run, StopRun::Nothing);
        assert_eq!(pending(Some(&released), relayed), None);
        assert_eq!(standing(Some(&released), relayed), Standing::NotStarted);
        // Unresolved: no relay, whatever is or is not at work.
        let unresolved = seat(start(Stage::Running));
        for evidence in [
            orphan(Record::Running),
            driven(Record::Running, Driver::Launching),
            driven(Record::Running, Driver::Recovering),
        ] {
            assert_eq!(release(Some(&unresolved), evidence), Err(RELEASE_STARTING));
        }
    }

    /// An ended start is held to the same evidence as a live one. With
    /// a cleanup still owed and its run still running, a reply that the
    /// run lives stops it: that is the cleanup. Once the registry shows
    /// that run ended, the same late reply stops and installs nothing.
    /// A reply that the run was never let run is not believed of a start
    /// whose activation is written. And an ended start that owes
    /// nothing is done with: a late reply that could not identify its
    /// owner casts no doubt on it, and what waits behind it is promoted.
    #[test]
    fn an_ended_start_is_held_to_what_is_seen_and_what_it_still_owes() {
        let id = StartId::from("start-one");
        let run = RunId::from("run-one");
        let expected = Expected {
            start: &id,
            run: &run,
        };
        let mut failed = start(Stage::Activated);
        failed.end = Some(End::Failed {
            why: "its cards could not be read".into(),
        });
        failed.cleanup_owed = true;
        let owing = seat(failed.clone());
        // Genuinely owing and still running.
        for relay in [true, false] {
            let running = Evidence {
                relay,
                ..orphan(Record::Running)
            };
            assert_eq!(
                recovered(Some(&owing), expected, Reattached::Activated, running),
                Recovered::RelayStop { install: !relay }
            );
        }
        // The matching exit reached the record before the old answer.
        let ended = orphan(Record::Ended);
        assert_eq!(
            recovered(Some(&owing), expected, Reattached::Activated, ended),
            Recovered::Nothing,
            "nothing is installed on a run that has ended"
        );
        assert_eq!(
            recovered(Some(&owing), expected, Reattached::Unactivated, ended),
            Recovered::Nothing
        );
        // Contradicting durable activation.
        assert_eq!(
            recovered(
                Some(&owing),
                expected,
                Reattached::Unactivated,
                orphan(Record::Running)
            ),
            Recovered::Uncertain
        );
        // Settled: nothing owed. No reply does anything or casts doubt.
        failed.cleanup_owed = false;
        let mut settled = seat(failed);
        settled.wanted = Some(wanted(2));
        for answer in [
            Reattached::Activated,
            Reattached::Unactivated,
            Reattached::Exited,
            Reattached::Unidentified,
        ] {
            for record in [Record::Running, Record::Ended] {
                // One reply still has something due, and it is the
                // owner's: the report of this run's exit, already on
                // the record. Everything else is nothing.
                let due = answer == Reattached::Exited && record == Record::Ended;
                assert_eq!(
                    recovered(Some(&settled), expected, answer, orphan(record)),
                    if due {
                        Recovered::Acknowledge
                    } else {
                        Recovered::Nothing
                    },
                    "{answer:?} {record:?}"
                );
            }
        }
        assert_eq!(
            admit(Some(&settled), orphan(Record::Ended)),
            Admit::Promote,
            "what waits is not held up"
        );
        // While it still owes, an owner that cannot be identified is a
        // reason for doubt, as for a live start.
        assert_eq!(
            recovered(
                Some(&owing),
                expected,
                Reattached::Unidentified,
                orphan(Record::Running)
            ),
            Recovered::Uncertain
        );
    }

    /// An exit is committed once and acknowledged for as long as its
    /// owner holds the report. The owner says the run exited: the exit
    /// is committed. The daemon is interrupted before it acknowledges;
    /// on the retry the same owner says the same, and the record already
    /// shows it: the owner is acknowledged and its report retired, with
    /// no second exit. A commit that was refused left the record as it
    /// was, and the retry commits. The same report offered against
    /// another run under the agent's id is not acknowledged at all.
    #[test]
    fn a_saved_exit_is_committed_once_and_acknowledged_until_it_is_retired() {
        let id = StartId::from("start-one");
        let run = RunId::from("run-one");
        let expected = Expected {
            start: &id,
            run: &run,
        };
        for (stage, exit) in [
            (Stage::Activated, Exit::Failed),
            (Stage::Complete, Exit::Ordinary),
        ] {
            let seat = seat(start(stage));
            let before = orphan(Record::Running);
            assert_eq!(
                recovered(Some(&seat), expected, Reattached::Exited, before),
                Recovered::CommitExit(exit)
            );
            // The commit was refused: nothing changed, and the retry is
            // the same commit, not an acknowledgement of one never made.
            assert_eq!(
                recovered(Some(&seat), expected, Reattached::Exited, before),
                Recovered::CommitExit(exit)
            );
            // The commit was made and the acknowledgement was not.
            let after = orphan(Record::Ended);
            assert_eq!(
                recovered(Some(&seat), expected, Reattached::Exited, after),
                Recovered::Acknowledge,
                "{stage:?}: acknowledged, not committed again"
            );
            // And again, as often as the owner still asks.
            assert_eq!(
                recovered(Some(&seat), expected, Reattached::Exited, after),
                Recovered::Acknowledge
            );
            // The start's own end is the next request's to write.
            assert_eq!(exit_owed(Some(&seat), after), Some(exit));
            // Another run under the same agent id: not this report's.
            for now in [Run::Other, Run::Unproven] {
                assert_eq!(
                    recovered(
                        Some(&seat),
                        expected,
                        Reattached::Exited,
                        Evidence { run: now, ..after }
                    ),
                    Recovered::Uncertain,
                    "{now:?}"
                );
            }
            // A reply about a live run, to the same ended record.
            assert_eq!(
                recovered(Some(&seat), expected, Reattached::Activated, after),
                Recovered::Nothing
            );
            // Through settlement: the exit is written to the start (as
            // exited or failed), nothing is owed, a start waits behind
            // it, and the daemon is interrupted once more before the
            // acknowledgement. The same report is still acknowledged,
            // the seat stays settled, and what waits is promoted.
            let mut ended = start(stage);
            ended.end = Some(match exit {
                Exit::Ordinary => End::Exited,
                _ => End::Failed {
                    why: "its session ended before its start was complete".into(),
                },
            });
            assert!(ended.settled());
            let mut settled = crate::crew::seat::tests::seat(ended);
            settled.wanted = Some(wanted(2));
            assert_eq!(
                recovered(Some(&settled), expected, Reattached::Exited, after),
                Recovered::Acknowledge,
                "{stage:?}: acknowledged after settlement too"
            );
            assert_eq!(admit(Some(&settled), after), Admit::Promote);
            assert!(settled.start.as_ref().unwrap().settled());
            // Still not for another run, or one that cannot be told; and
            // a late reply that identified nobody stays harmless.
            for now in [Run::Other, Run::Unproven] {
                assert_eq!(
                    recovered(
                        Some(&settled),
                        expected,
                        Reattached::Exited,
                        Evidence { run: now, ..after }
                    ),
                    Recovered::Nothing,
                    "{now:?}"
                );
            }
            assert_eq!(
                recovered(Some(&settled), expected, Reattached::Unidentified, after),
                Recovered::Nothing
            );
        }
        // An ended start that still owes its cleanup acknowledges the
        // same way.
        let mut ended = start(Stage::Activated);
        ended.end = Some(End::Cancelled {
            why: "stopped".into(),
        });
        ended.cleanup_owed = true;
        let ended = seat(ended);
        assert_eq!(
            recovered(
                Some(&ended),
                expected,
                Reattached::Exited,
                orphan(Record::Running)
            ),
            Recovered::CommitExit(Exit::AlreadyEnded)
        );
        assert_eq!(
            recovered(
                Some(&ended),
                expected,
                Reattached::Exited,
                orphan(Record::Ended)
            ),
            Recovered::Acknowledge
        );
    }

    /// A start with no run written down is cleaned by its reservation
    /// only where it could not have made one. A later-stage start that
    /// was stored and reads back with its run missing has no evidence
    /// of what is its own, and nothing of it is cleaned, whatever the
    /// registry shows.
    #[test]
    fn a_missing_run_is_a_reservation_only_before_running() {
        let id = StartId::from("start-one");
        let cancelled = End::Cancelled {
            why: "stopped".into(),
        };
        for stage in STAGES {
            let mut ended = start(stage);
            ended.end = Some(cancelled.clone());
            ended.cleanup_owed = true;
            // As stored by something that never wrote its run.
            let mut stored = serde_json::to_value(&ended).unwrap();
            stored.as_object_mut().unwrap().remove("run");
            let runless: Start = serde_json::from_value(stored).unwrap();
            assert_eq!(runless.run, None);
            let seat = seat(runless);
            for now in [Run::Same, Run::Other, Run::Unproven] {
                for record in [Record::None, Record::Running, Record::Ended] {
                    let evidence = Evidence {
                        run: now,
                        ..orphan(record)
                    };
                    assert_eq!(
                        seat.may_clean(&id, evidence),
                        stage < Stage::Running,
                        "{stage:?} {now:?} {record:?}"
                    );
                }
            }
        }
        // With its run written, a later-stage start is cleaned on a
        // positive match and not otherwise.
        let mut ended = start(Stage::Complete);
        ended.end = Some(cancelled);
        ended.cleanup_owed = true;
        let seat = seat(ended);
        assert!(seat.may_clean(&id, orphan(Record::Ended)));
        assert!(!seat.may_clean(
            &id,
            Evidence {
                run: Run::Other,
                ..orphan(Record::Ended)
            }
        ));
    }

    /// A verified relay proves a run was let run before its stage says
    /// so, to a recovery as to a Start and a release. An owner's older
    /// answer that the child was never let run, arriving for a
    /// running-stage start that is supervised here, is a contradiction:
    /// nothing is claimed, and the child its supervisor proves is
    /// running is not ended. With no such evidence the held child is
    /// ended, as before. The same for an ended start that still owes
    /// its cleanup.
    #[test]
    fn a_child_its_supervisor_proves_running_is_not_ended_as_unactivated() {
        let id = StartId::from("start-one");
        let run = RunId::from("run-one");
        let expected = Expected {
            start: &id,
            run: &run,
        };
        let relayed = supervised(Record::Running);
        let alone = orphan(Record::Running);
        let live = seat(start(Stage::Running));
        // The three readers agree on the relayed case.
        assert_eq!(admit(Some(&live), relayed), Admit::RecordActivation);
        assert_eq!(release(Some(&live), relayed), Ok(Release::Mark));
        assert_eq!(
            recovered(Some(&live), expected, Reattached::Unactivated, relayed),
            Recovered::Uncertain,
            "not EndUnactivated"
        );
        // No evidence of activation: the held child is ended.
        assert_eq!(
            recovered(Some(&live), expected, Reattached::Unactivated, alone),
            Recovered::EndUnactivated
        );
        // An ended running-stage start that still owes its cleanup.
        let mut failed = start(Stage::Running);
        failed.end = Some(End::Failed {
            why: "its owner went away".into(),
        });
        failed.cleanup_owed = true;
        let owing = seat(failed);
        assert_eq!(
            recovered(Some(&owing), expected, Reattached::Unactivated, relayed),
            Recovered::Uncertain
        );
        assert_eq!(
            recovered(Some(&owing), expected, Reattached::Unactivated, alone),
            Recovered::EndUnactivated
        );
        // An activated answer under the relay keeps the relay.
        assert_eq!(
            recovered(Some(&live), expected, Reattached::Activated, relayed),
            Recovered::Resume { install: false }
        );
    }

    /// A release is given only for a run that is this start's, let run
    /// and supervised here. With no session yet it cancels; while the
    /// session is starting, or its supervision is not shown, it is
    /// refused, promising nothing the launch could not keep.
    #[test]
    fn what_a_release_finds() {
        use Record as R;
        // (stage, record, with nothing at work and no relay, under a
        // relay)
        let table = [
            (
                Stage::Reserved,
                R::None,
                Ok(Release::Cancel),
                Ok(Release::Cancel),
            ),
            (
                Stage::RecordBound,
                R::Created,
                Err(RELEASE_STARTING),
                Err(RELEASE_STARTING),
            ),
            (
                Stage::Running,
                R::Running,
                Err(RELEASE_STARTING),
                Ok(Release::Mark),
            ),
            (
                Stage::Activated,
                R::Running,
                Err(RELEASE_UNSUPERVISED),
                Ok(Release::Mark),
            ),
            (
                Stage::CardsSettled,
                R::Running,
                Err(RELEASE_UNSUPERVISED),
                Ok(Release::Mark),
            ),
            (
                Stage::Instructed,
                R::Running,
                Err(RELEASE_UNSUPERVISED),
                Ok(Release::Mark),
            ),
            (
                Stage::Complete,
                R::Running,
                Err(RELEASE_UNSUPERVISED),
                Ok(Release::Mark),
            ),
            (
                Stage::Complete,
                R::Stopping,
                Err(RELEASE_STOPPED),
                Err(RELEASE_STOPPED),
            ),
            (
                Stage::Complete,
                R::Ended,
                Err(RELEASE_ENDED),
                Err(RELEASE_ENDED),
            ),
            (
                Stage::RecordBound,
                R::Ended,
                Err(RELEASE_ENDED),
                Err(RELEASE_ENDED),
            ),
        ];
        for (stage, record, alone, relayed) in table {
            let seat = seat(start(stage));
            assert_eq!(
                release(Some(&seat), orphan(record)),
                alone,
                "{stage:?} with the record {record:?} and no relay"
            );
            assert_eq!(
                release(Some(&seat), supervised(record)),
                relayed,
                "{stage:?} with the record {record:?} under a relay"
            );
        }
        // Let run, in the gap before supervision, its launch still at
        // work: still starting.
        for driver in [Driver::Launching, Driver::Recovering] {
            assert_eq!(
                release(
                    Some(&seat(start(Stage::Activated))),
                    driven(Record::Running, driver)
                ),
                Err(RELEASE_STARTING)
            );
        }
        assert_eq!(release(None, orphan(R::None)), Err(RELEASE_NOTHING));
    }

    /// A release, a stop and a cleanup exclude one another, and the one
    /// admitted first stands. After a stop, nothing is released. A
    /// cleanup is admitted only for a start that has ended, and a
    /// release only for one that is live, so the two are never both let
    /// at one session: not after an end is written, and not before. A
    /// release ends the start as released in its one commit and gives
    /// up the team's cleanup with it, so the start is settled and the
    /// seat free at once; nothing of the team's is admitted for it
    /// after: no progress, no cleanup, no stop, no resuming by a late
    /// recovery. There is no state in which a release is asked and not
    /// yet made.
    #[test]
    fn the_first_of_a_release_a_stop_and_a_cleanup_stands() {
        let id = StartId::from("start-one");
        // A stop first.
        let mut stopped = start(Stage::Complete);
        stopped.stopping = true;
        let stopped = seat(stopped);
        assert_eq!(
            release(Some(&stopped), supervised(Record::Running)),
            Err(RELEASE_STOPPED)
        );
        assert!(!stopped.owns(1, &id), "nothing more of the team's for it");
        // A cleanup first: the start has ended, and is not released.
        let mut cleaning = start(Stage::Running);
        cleaning.end = Some(End::Cancelled {
            why: "stopped".into(),
        });
        cleaning.cleanup_owed = true;
        let cleaning = seat(cleaning);
        assert_eq!(
            release(Some(&cleaning), supervised(Record::Running)),
            Err(RELEASE_ENDED)
        );
        assert!(cleaning.may_clean(&id, orphan(Record::Running)));
        // The live combination: let run, supervised, with something of
        // its start's still to clean. It can be released, and for that
        // very reason nothing may clean it: a cleanup that passed its
        // check and paused would otherwise touch a checkout that became
        // the session's own.
        let mut live = start(Stage::Activated);
        live.cleanup_owed = true;
        let mut live = seat(live);
        assert_eq!(
            release(Some(&live), supervised(Record::Running)),
            Ok(Release::Mark)
        );
        assert!(
            !live.may_clean(&id, supervised(Record::Running)),
            "a live start has no cleanup to admit"
        );
        // The release's own commit.
        live.start.as_mut().unwrap().release();
        let released = live;
        let ended = released.start.as_ref().unwrap();
        assert!(ended.released());
        assert!(!ended.cleanup_owed, "given up with the release");
        assert!(ended.settled(), "and so settled at once");
        assert!(!released.owns(1, &id), "no progress");
        assert!(
            !released.may_clean(&id, supervised(Record::Running)),
            "no cleanup of what is not the team's"
        );
        assert_eq!(
            stop(Some(&released), supervised(Record::Running)).run,
            StopRun::Nothing
        );
        assert_eq!(
            release(Some(&released), supervised(Record::Running)),
            Err(RELEASE_NOTHING),
            "not twice"
        );
        assert_eq!(
            standing(Some(&released), supervised(Record::Running)),
            Standing::NotStarted,
            "a plain session, not the member's"
        );
        // The next admission: the seat is free, with the released
        // session still running under its own record.
        assert_eq!(
            admit(Some(&released), supervised(Record::Running)),
            Admit::Reserve
        );
        // A start in doubt is cleaned by nobody, ended or not.
        let mut unsure = start(Stage::Running);
        unsure.end = Some(End::Failed {
            why: "its owner could not be identified".into(),
        });
        unsure.cleanup_owed = true;
        unsure.uncertain = true;
        assert!(!seat(unsure).may_clean(&id, orphan(Record::Ended)));
    }

    /// A driver goes on with a start only at the generation that is the
    /// seat's now and the start's own. After a stop advanced the seat's
    /// generation and kept the old start, neither the old number nor
    /// the seat's new one passes for it; what the old operation may
    /// still do, once it has ended, is its own cleanup, and only that.
    #[test]
    fn an_old_driver_cannot_go_on_in_a_seat_that_moved_on() {
        let mine = StartId::from("start-one");
        let mut seat = seat(start(Stage::Running));
        assert!(seat.owns(1, &mine));
        assert!(!seat.owns(1, &StartId::from("another")));
        // A stop: the seat's generation moves, the old start is kept.
        seat.generation = 2;
        seat.start.as_mut().unwrap().stopping = true;
        seat.start.as_mut().unwrap().cleanup_owed = true;
        assert!(!seat.owns(1, &mine), "the generation it was begun at");
        assert!(!seat.owns(2, &mine), "the seat's current one, read anew");
        assert!(
            !seat.may_clean(&mine, orphan(Record::Running)),
            "not while its run has not ended"
        );
        // The generation alone, without the stop being written, does
        // not pass either.
        seat.start.as_mut().unwrap().stopping = false;
        assert!(!seat.owns(1, &mine) && !seat.owns(2, &mine));
        // The stopped run is seen to end: its own cleanup is its to do.
        seat.start.as_mut().unwrap().end = Some(End::Cancelled {
            why: "stopped".into(),
        });
        assert!(seat.may_clean(&mine, orphan(Record::Ended)));
        assert!(!seat.may_clean(&StartId::from("another"), orphan(Record::Ended)));
        assert!(!seat.owns(1, &mine) && !seat.owns(2, &mine));
        seat.start.as_mut().unwrap().cleanup_owed = false;
        assert!(
            !seat.may_clean(&mine, orphan(Record::Ended)),
            "nothing left to clean"
        );
        // A new start took the seat.
        let mut next = start(Stage::Reserved);
        next.id = StartId::from("start-two");
        next.generation = 3;
        seat.generation = 3;
        seat.start = Some(next);
        assert!(!seat.owns(1, &mine) && !seat.owns(3, &mine));
        assert!(seat.owns(3, &StartId::from("start-two")));
        assert!(!Seat::new(MemberId::from("member-one")).owns(0, &mine));
    }

    /// An id carries the words first prepared under it and no others.
    /// The first words could not be queued (a full inbox); what is owed
    /// changed before the start was tried again; the seat was stored and
    /// read back in between: the retry queues the words it prepared.
    /// What the member is told later goes under an id of its own.
    #[test]
    fn an_id_carries_the_words_first_prepared_under_it() {
        let mut interrupted = start(Stage::CardsSettled);
        assert_eq!(
            interrupted.first_words.text, None,
            "reserved before they exist"
        );
        let first = "You are the writer. AD-7 is yours; AD-9 waits on a lease.";
        assert_eq!(interrupted.first_words.prepare(first), first);
        // Stored and read back, as a daemon that reopened would.
        let stored = serde_json::to_value(seat(interrupted)).unwrap();
        let mut reopened: Seat = serde_json::from_value(stored).unwrap();
        let again = reopened.start.as_mut().unwrap();
        assert_eq!(
            again.first_words.id,
            MessageId::from("message-one".to_owned())
        );
        // The lease ended meanwhile: the words that would be written
        // now are different, and are not what goes under that id.
        let now = "You are the writer. AD-7 and AD-9 are yours.";
        assert_eq!(again.first_words.prepare(now), first);
        assert_eq!(again.first_words.text.as_deref(), Some(first));
        // The later word has its own id and its own text.
        again.follow_up = Some(Words::reserved(MessageId::from("message-two".to_owned())));
        let follow_up = again.follow_up.as_mut().unwrap();
        assert_eq!(follow_up.prepare(now), now);
        assert_ne!(follow_up.id, again.first_words.id);
        assert_eq!(again.first_words.text.as_deref(), Some(first));
    }

    /// A reader's word for a member, by the same situation the others
    /// read. Running is said only of a supervised run.
    #[test]
    fn where_a_member_stands_for_a_reader() {
        assert_eq!(standing(None, orphan(Record::None)), Standing::NotStarted);
        // (stage, record, with no relay, under a relay)
        let table = [
            (
                Stage::Reserved,
                Record::None,
                Standing::Starting,
                Standing::Starting,
            ),
            (
                Stage::RecordBound,
                Record::Created,
                Standing::Starting,
                Standing::Starting,
            ),
            (
                Stage::Running,
                Record::Running,
                Standing::Starting,
                Standing::Starting,
            ),
            (
                Stage::Activated,
                Record::Running,
                Standing::Starting,
                Standing::Starting,
            ),
            (
                Stage::CardsSettled,
                Record::Running,
                Standing::Starting,
                Standing::Starting,
            ),
            (
                Stage::Complete,
                Record::Running,
                Standing::Starting,
                Standing::Running,
            ),
            (
                Stage::Complete,
                Record::Stopping,
                Standing::Stopping,
                Standing::Stopping,
            ),
            (
                Stage::Complete,
                Record::Ended,
                Standing::NotStarted,
                Standing::NotStarted,
            ),
            (
                Stage::Activated,
                Record::Ended,
                Standing::Failed,
                Standing::Failed,
            ),
        ];
        for (stage, record, alone, relayed) in table {
            let seat = seat(start(stage));
            assert_eq!(
                standing(Some(&seat), orphan(record)),
                alone,
                "{stage:?} with the record {record:?} and no relay"
            );
            assert_eq!(
                standing(Some(&seat), supervised(record)),
                relayed,
                "{stage:?} with the record {record:?} under a relay"
            );
        }
        let mut failed = start(Stage::RecordBound);
        failed.end = Some(End::Failed {
            why: "the runtime is not installed".into(),
        });
        assert_eq!(
            standing(Some(&seat(failed)), orphan(Record::Ended)),
            Standing::Failed
        );
        let mut cancelled = start(Stage::Running);
        cancelled.end = Some(End::Cancelled {
            why: "stopped".into(),
        });
        cancelled.cleanup_owed = true;
        assert_eq!(
            standing(Some(&seat(cancelled.clone())), orphan(Record::Running)),
            Standing::Stopping
        );
        cancelled.cleanup_owed = false;
        assert_eq!(
            standing(Some(&seat(cancelled)), orphan(Record::Ended)),
            Standing::NotStarted
        );
        let mut asked = start(Stage::Activated);
        asked.stopping = true;
        assert_eq!(
            standing(Some(&seat(asked)), supervised(Record::Running)),
            Standing::Stopping
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
        stored.first_words.prepare("You are the writer.");
        let mut seat = seat(stored);
        seat.wanted = Some(wanted(2));
        let json = serde_json::to_value(&seat).unwrap();
        assert_eq!(serde_json::from_value::<Seat>(json.clone()).unwrap(), seat);
        let written = json["start"].as_object().unwrap();
        for unset in ["end", "stopping", "cleanup_owed", "uncertain", "follow_up"] {
            assert!(!written.contains_key(unset), "{unset}");
        }
        assert_eq!(json["start"]["stage"], "instructed");
        assert_eq!(json["start"]["run"], "run-one");
        assert_eq!(json["start"]["first_words"]["text"], "You are the writer.");
        assert_eq!(json["start"]["cards"]["owed"][0]["card"], "task-two");
        // A bare seat, and a reserved start with no run yet.
        let bare = Seat::new(MemberId::from("member-one"));
        assert_eq!(
            serde_json::to_value(&bare).unwrap(),
            serde_json::json!({"member": "member-one", "generation": 0})
        );
        let reserved = serde_json::to_value(start(Stage::Reserved)).unwrap();
        assert!(reserved.get("run").is_none());
        assert!(reserved["first_words"].get("text").is_none());
        // An end, and a stop asked, as they are written.
        let mut failed = start(Stage::Running);
        failed.end = Some(End::Failed {
            why: "its owner went away".into(),
        });
        failed.stopping = true;
        let json = serde_json::to_value(&failed).unwrap();
        assert_eq!(
            json["end"],
            serde_json::json!({"how": "failed", "why": "its owner went away"})
        );
        assert_eq!(json["stopping"], true);
        assert_eq!(serde_json::from_value::<Start>(json).unwrap(), failed);
        assert_eq!(
            serde_json::to_value(End::Released).unwrap(),
            serde_json::json!({"how": "released"})
        );
        assert_eq!(
            serde_json::to_value(End::Exited).unwrap(),
            serde_json::json!({"how": "exited"})
        );
    }
}
