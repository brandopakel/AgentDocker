//! What an earlier session of a member still owes, and what its
//! successor is handed.
//!
//! A member's session ends and another takes its seat. What the first
//! one left is not nothing because its record ended: cards still name it,
//! and the checkout it worked in may hold commits that are in no other
//! branch, or changes that were never committed. A successor handed the
//! card and told to carry on, in another checkout, would start the work
//! again beside the work already done.
//!
//! So a seat keeps, for each run of each session it was bound to, what
//! that run left: typed evidence, read from its checkout, that outlives
//! its record. A card passes to the successor only when the checks here
//! all hold; anything else is left where it is, named with why, and kept
//! until it is resolved or the person settles it. Nothing unresolved is
//! ever dropped for want of room, and nothing that could not be counted
//! or read is taken for nothing.
//!
//! Nothing here reads a checkout or moves a card. The daemon reads and
//! commits; these are the rules it commits by. Two things are the
//! daemon's that these rules cannot do for it. A card's facts ([`Card`])
//! are read under the lock that commits its hand-over, but where the work
//! is ([`Work`]) comes from a reading of a checkout made off that lock:
//! the reading is taken again before a card is handed over or an entry
//! retired on the strength of it, and [`Earlier::read_at`] says when the
//! one that stands was made. And an entry that still owes is kept against
//! whatever would fold or forget its record.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::seat::RunId;
use crate::{AgentId, TaskId};

/// The most earlier sessions a seat keeps. At the bound the resolved
/// ones are retired first; with every one unresolved, the member's start
/// is refused before another could be added.
pub const EARLIER: usize = 16;

/// What an earlier session left in the checkout it worked in, as it was
/// read: where, on what, and what in it is nowhere else.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Remains {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    /// The branch its work is to go into, and where the two parted, as
    /// the branch review works them out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_base: Option<String>,
    /// Commits on its branch that are not in its target. `None` where
    /// they could not be counted: there is no target to count against (a
    /// detached checkout, the shared checkout on its own target), or no
    /// merge base. A count nobody took is not a count of none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unmerged: Option<u32>,
    /// Whether it holds changes that are in no commit, untracked files
    /// among them. `None` where the status failed or was not taken.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uncommitted: Option<bool>,
    /// Another live session works in the same checkout: said, not
    /// assumed away. It is company, not work: who shares the checkout
    /// can change without the work changing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shared_with: Vec<AgentId>,
}

/// What a checkout's remains come to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Left {
    /// Counted and listed, and nothing is there that is nowhere else.
    Nothing,
    /// Unmerged commits or uncommitted changes.
    Work,
    /// Nothing was found, and something could not be counted or listed.
    Unknown(String),
}

impl Remains {
    /// What these remains come to. Known work is work whatever else is
    /// unknown; nothing is nothing only when both facts were taken.
    pub fn left(&self) -> Left {
        if self.unmerged.is_some_and(|commits| commits > 0) || self.uncommitted == Some(true) {
            return Left::Work;
        }
        match (self.unmerged, self.uncommitted) {
            (Some(_), Some(_)) => Left::Nothing,
            (None, _) => {
                Left::Unknown("the commits it holds beyond its target could not be counted".into())
            }
            (_, None) => Left::Unknown("its uncommitted changes could not be listed".into()),
        }
    }

    /// The same checkout on the same branch (for a detached one, at the
    /// same commit): the only later reading that speaks for this one.
    fn same_place(&self, other: &Remains) -> bool {
        self.path == other.path
            && match (&self.branch, &other.branch) {
                (Some(branch), Some(other)) => branch == other,
                (None, None) => self.head.is_some() && self.head == other.head,
                _ => false,
            }
    }

    /// The same work: everything but the company.
    fn same_work(&self, other: &Remains) -> bool {
        self.path == other.path
            && self.branch == other.branch
            && self.head == other.head
            && self.target == other.target
            && self.merge_base == other.merge_base
            && self.unmerged == other.unmerged
            && self.uncommitted == other.uncommitted
    }
}

/// What is known of the checkout an earlier session worked in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Observed {
    /// Its ending made an observation owed and none is made yet. It is
    /// unknown and unsettled from that moment, not "ended with nothing".
    Pending,
    Read {
        remains: Remains,
    },
    /// The checkout or its git state could not be read.
    Unreadable {
        why: String,
    },
    /// It worked in no checkout of the project.
    None,
}

/// What a look at an earlier session's checkout found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reading {
    Remains(Remains),
    Unreadable {
        why: String,
    },
    /// It worked in no checkout of the project.
    None,
    /// The commit recorded as its head is in its target now. Asked of
    /// the repository, not of the checkout, which may stand on another
    /// branch by now.
    HeadMerged,
}

/// What became of a reading offered to an entry ([`Earlier::observe`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Taken {
    /// The evidence changed, and with it the entry's revision.
    Recorded,
    /// It says what is recorded already.
    Same,
    /// It does not speak for this entry, and what is recorded stands.
    Refused(&'static str),
}

pub const READING_OF_ANOTHER_RUN: &str = "the reading is of another run of this session";
pub const READING_OF_ANOTHER_PLACE: &str = "the reading is of another checkout or branch than the one recorded; what was recorded stands until its last commit is known to be in its target";
pub const READING_FOUND_LESS: &str =
    "what was read before stands; a reading that found no checkout does not replace it";
pub const NOTHING_RECORDED_TO_MERGE: &str =
    "no checkout is recorded whose last commit could have been merged";
pub const NO_CHECKOUT: &str = "it worked in no checkout of the project";

/// The person's word that what an earlier session left is dealt with. It
/// is their override, not evidence of a merge. It names what it clears,
/// and clears no more: the cards it lists by id, and the checkout as it
/// was read at the revision it names, of the run it names. It gives leave
/// neither to delete a checkout nor to stop anyone.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settlement {
    pub by: String,
    pub at: DateTime<Utc>,
    pub note: String,
    /// The run it settles. The same session bound again is another run,
    /// and what it leaves is not settled by this.
    pub run: RunId,
    /// The cards it settles, each by id.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cards: Vec<TaskId>,
    /// The checkout, as it was read at this revision of the entry.
    /// Evidence read since is newer than the settlement and is not
    /// cleared by it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkout: Option<u64>,
}

pub const SETTLES_ANOTHER_RUN: &str =
    "that settles another run of this session; this one ended since and is not settled by it";
pub const SETTLES_NOTHING: &str = "a settlement names the cards or the checkout it settles";
pub const SETTLES_OLDER_EVIDENCE: &str =
    "its checkout was read again since; look at what it holds now and settle that";
pub const NOTHING_READ_YET: &str = "its checkout has not been read yet: there is nothing to settle until it has been, or could not be";

/// What an earlier session still owes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Owed {
    /// Open cards that still name it and that the person has not
    /// settled.
    pub cards: Vec<TaskId>,
    /// Work in its checkout that is nowhere else, or a checkout that was
    /// not read, could not be read, or could not be counted.
    pub checkout: bool,
}

impl Owed {
    pub fn nothing(&self) -> bool {
        self.cards.is_empty() && !self.checkout
    }
}

/// One run of a session a seat was bound to, and what it left.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Earlier {
    pub agent: AgentId,
    /// The run of it that ended. An agent's id can come to name a later
    /// run (a session resumed under its own record); this cannot.
    pub run: RunId,
    pub name: String,
    pub runtime: String,
    pub ended_at: DateTime<Utc>,
    /// How it ended, in the record's own words.
    pub how: String,
    pub observed: Observed,
    /// When the reading that stands was made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_at: Option<DateTime<Utc>>,
    /// Counted from one, and raised each time the evidence changes and
    /// each time the session ends again.
    pub revision: u64,
    /// Every settlement made of it, each kept with its note and time.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub settled: Vec<Settlement>,
}

impl Earlier {
    /// A run that has just ended: its checkout is owed a reading.
    pub fn ended(
        agent: AgentId,
        run: RunId,
        name: String,
        runtime: String,
        ended_at: DateTime<Utc>,
        how: String,
    ) -> Self {
        Self {
            agent,
            run,
            name,
            runtime,
            ended_at,
            how,
            observed: Observed::Pending,
            read_at: None,
            revision: 1,
            settled: Vec::new(),
        }
    }

    /// The same session was bound again and has ended again. What the
    /// earlier run left was read, and maybe settled; what this one left
    /// is neither. Always a new revision, and nothing settled of the run
    /// before carries over.
    pub fn ended_again(&mut self, run: RunId, ended_at: DateTime<Utc>, how: String) {
        self.run = run;
        self.ended_at = ended_at;
        self.how = how;
        self.observed = Observed::Pending;
        self.read_at = None;
        self.revision += 1;
    }

    /// The person settled this card for the run this entry is of.
    pub fn settled_card(&self, card: &TaskId) -> bool {
        self.settled
            .iter()
            .any(|settlement| settlement.run == self.run && settlement.cards.contains(card))
    }

    /// The person settled the checkout as it stands now.
    pub fn settled_checkout(&self) -> bool {
        self.settled.iter().any(|settlement| {
            settlement.run == self.run && settlement.checkout == Some(self.revision)
        })
    }

    /// Take the person's settlement, if it speaks of this run and of the
    /// evidence as it stands. Each one is kept.
    pub fn settle(&mut self, settlement: Settlement) -> Result<(), &'static str> {
        if settlement.run != self.run {
            return Err(SETTLES_ANOTHER_RUN);
        }
        if settlement.cards.is_empty() && settlement.checkout.is_none() {
            return Err(SETTLES_NOTHING);
        }
        if let Some(revision) = settlement.checkout {
            if revision != self.revision {
                return Err(SETTLES_OLDER_EVIDENCE);
            }
            if self.observed == Observed::Pending {
                return Err(NOTHING_READ_YET);
            }
        }
        self.settled.push(settlement);
        Ok(())
    }

    /// Its checkout still owes: work that is nowhere else, or a state
    /// nobody read, could read or could count, which is not known to be
    /// nothing.
    pub fn owes_checkout(&self) -> bool {
        let unsettled = match &self.observed {
            Observed::Pending | Observed::Unreadable { .. } => true,
            Observed::Read { remains } => remains.left() != Left::Nothing,
            Observed::None => false,
        };
        unsettled && !self.settled_checkout()
    }

    /// What it still owes. `open_cards` are the cards that still name it
    /// and are neither ended (done or cancelled) nor archived, which the
    /// board says; the ones the person settled are not owed.
    pub fn owes(&self, open_cards: &[TaskId]) -> Owed {
        Owed {
            cards: open_cards
                .iter()
                .filter(|card| !self.settled_card(card))
                .cloned()
                .collect(),
            checkout: self.owes_checkout(),
        }
    }

    /// Offer a reading of its checkout, made at `at` for run `run`.
    ///
    /// A reading speaks for the entry only where it is of the same run
    /// and, once a checkout is recorded, of that checkout on that branch.
    /// A later look at the same folder on another branch, or one that
    /// found no folder, says nothing of the commits recorded: they are
    /// owed until their last one is known to be in its target
    /// ([`Reading::HeadMerged`]), which clears the commits and leaves
    /// what was uncommitted as it was recorded. Who shares the checkout
    /// is kept up to date without that counting as new evidence.
    pub fn observe(&mut self, run: &RunId, reading: Reading, at: DateTime<Utc>) -> Taken {
        if *run != self.run {
            return Taken::Refused(READING_OF_ANOTHER_RUN);
        }
        let next = match (&mut self.observed, reading) {
            (Observed::None, _) => return Taken::Refused(NO_CHECKOUT),
            (Observed::Read { remains }, Reading::Remains(read)) => {
                if !remains.same_place(&read) {
                    return Taken::Refused(READING_OF_ANOTHER_PLACE);
                }
                if remains.same_work(&read) {
                    remains.shared_with = read.shared_with;
                    self.read_at = Some(at);
                    return Taken::Same;
                }
                Observed::Read { remains: read }
            }
            (Observed::Read { remains }, Reading::HeadMerged) => {
                if remains.unmerged == Some(0) {
                    self.read_at = Some(at);
                    return Taken::Same;
                }
                let mut merged = remains.clone();
                merged.unmerged = Some(0);
                Observed::Read { remains: merged }
            }
            (Observed::Read { .. }, Reading::Unreadable { .. } | Reading::None) => {
                return Taken::Refused(READING_FOUND_LESS);
            }
            (Observed::Pending | Observed::Unreadable { .. }, Reading::HeadMerged) => {
                return Taken::Refused(NOTHING_RECORDED_TO_MERGE);
            }
            (Observed::Pending | Observed::Unreadable { .. }, Reading::Remains(read)) => {
                Observed::Read { remains: read }
            }
            (Observed::Pending | Observed::Unreadable { .. }, Reading::Unreadable { why }) => {
                Observed::Unreadable { why }
            }
            (Observed::Pending | Observed::Unreadable { .. }, Reading::None) => Observed::None,
        };
        self.read_at = Some(at);
        if self.observed == next {
            return Taken::Same;
        }
        self.observed = next;
        self.revision += 1;
        Taken::Recorded
    }
}

/// Why a member cannot be given another session yet.
pub const EARLIER_FULL: &str = "sixteen earlier sessions of this member each still have work unsettled; settle one (`agentdocker team settle`) before it is started again";

/// Make room in a seat's earlier sessions for one more. `open_cards`
/// answers, for an entry, which cards still name its session and are
/// neither ended nor archived; whether an entry is resolved is asked of
/// the entry here ([`Earlier::owes`]), not taken from a list worked out
/// elsewhere. The resolved are retired first, the one that ended longest
/// ago first, and only as many as are needed. With too few resolved at
/// the bound, nothing is retired and the start is refused: nothing
/// unresolved is cut off.
pub fn make_room(
    earlier: &mut Vec<Earlier>,
    open_cards: impl Fn(&Earlier) -> Vec<TaskId>,
) -> Result<(), &'static str> {
    let excess = (earlier.len() + 1).saturating_sub(EARLIER);
    if excess == 0 {
        return Ok(());
    }
    let mut resolved: Vec<(DateTime<Utc>, usize)> = earlier
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.owes(&open_cards(entry)).nothing())
        .map(|(at, entry)| (entry.ended_at, at))
        .collect();
    if resolved.len() < excess {
        return Err(EARLIER_FULL);
    }
    resolved.sort();
    let retired: BTreeSet<usize> = resolved
        .into_iter()
        .take(excess)
        .map(|(_, at)| at)
        .collect();
    let mut at = 0;
    earlier.retain(|_| {
        let keep = !retired.contains(&at);
        at += 1;
        keep
    });
    Ok(())
}

/// Who holds a card that named an earlier session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Holder {
    /// Still that earlier session, and no one else.
    Earlier,
    /// Nobody: it was released.
    Nobody,
    /// Someone else since.
    Another,
}

/// What became of the earlier session a card names, against the run its
/// entry is of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Session {
    /// Its record shows that run ended, and it has not run since.
    Ended,
    /// It was resumed and is running: it keeps its card.
    Running,
    /// It ran again and ended again since: the entry's reading is of an
    /// earlier run, and what the last one left is unread.
    EndedAgain,
    /// Its record is missing, or cannot show which run it is.
    Unproven,
}

/// A card that names an earlier session, as it stands when its hand-over
/// is about to be committed. Every fact is read under the lock that
/// commits it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Card {
    /// It is this project's.
    pub here: bool,
    pub archived: bool,
    /// It is in a lane that ends it: done or cancelled.
    pub closed: bool,
    /// It is in a lane where work is under way, not waiting or blocked.
    pub working: bool,
    pub holder: Holder,
    pub session: Session,
    /// A live lease stands on the card: the whole claim, not only a hold
    /// that lapsed.
    pub leased: bool,
    /// The person settled it for that session's run
    /// ([`Earlier::settled_card`]).
    pub settled: bool,
}

/// Where the earlier session's work on a card is, against the checkout
/// its successor will work in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Work {
    /// Its checkout holds nothing that is nowhere else.
    None,
    /// What it holds is in the checkout the successor is given: the same
    /// folder, on the same branch, at or past its last commit.
    Here,
    /// The person settled what it holds. That is their word, not a
    /// merge: the work may still be where it was, and where that is is
    /// kept for the successor to be told.
    Settled {
        path: Option<String>,
        branch: Option<String>,
    },
    /// It is in another checkout, or in this one on another branch.
    Elsewhere {
        path: String,
        branch: Option<String>,
    },
    /// Its checkout has not been read, could not be, or could not be
    /// counted.
    Unknown { why: String },
}

/// The checkout a successor is given.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Successor<'a> {
    pub path: &'a str,
    pub branch: Option<&'a str>,
    /// Whether the earlier session's recorded head is in this checkout
    /// (it stands at that commit or past it). `None` where that could
    /// not be told.
    pub has_head: Option<bool>,
}

/// The work of `earlier`, against the checkout a successor is given.
pub fn work(earlier: &Earlier, successor: Option<Successor<'_>>) -> Work {
    let remains = match &earlier.observed {
        Observed::None => return Work::None,
        Observed::Pending => {
            return Work::Unknown {
                why: "its checkout has not been read yet".into(),
            };
        }
        Observed::Unreadable { .. } if earlier.settled_checkout() => {
            return Work::Settled {
                path: None,
                branch: None,
            };
        }
        Observed::Unreadable { why } => return Work::Unknown { why: why.clone() },
        Observed::Read { remains } => remains,
    };
    let left = remains.left();
    if left == Left::Nothing {
        return Work::None;
    }
    if earlier.settled_checkout() {
        return Work::Settled {
            path: Some(remains.path.clone()),
            branch: remains.branch.clone(),
        };
    }
    if let Left::Unknown(why) = left {
        return Work::Unknown { why };
    }
    let elsewhere = Work::Elsewhere {
        path: remains.path.clone(),
        branch: remains.branch.clone(),
    };
    match successor {
        Some(successor)
            if successor.path == remains.path
                && remains.branch.is_some()
                && remains.branch.as_deref() == successor.branch =>
        {
            match successor.has_head {
                Some(true) => Work::Here,
                Some(false) => elsewhere,
                None => Work::Unknown {
                    why: "whether its successor's checkout holds its last commit could not be told"
                        .into(),
                },
            }
        }
        _ => elsewhere,
    }
}

/// What becomes of a card that names an earlier session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HandOver {
    /// It passes to the successor.
    Hand,
    /// It is not the successor's, for good: ended, archived, another
    /// project's, released, or given to someone else since.
    Exclude(String),
    /// It is left where it is and does not hold the start up: it is
    /// blocked or waiting, its holder is running again, or the person
    /// settled it. It becomes a candidate when that changes.
    Leave(String),
    /// It should pass and cannot yet: a lease in the way, a session whose
    /// last run is unread or whose record cannot be read, evidence that
    /// could not be read, or work elsewhere the person has not settled.
    /// Tried again at the next start, and said to the member meanwhile.
    Owe(String),
}

/// What becomes of `card`, whose earlier holder's work is `work`. A card
/// that fails a check is left as it is and named with why; nothing falls
/// back to a name, a role or the newest time.
pub fn hand_over(card: Card, work: &Work) -> HandOver {
    if !card.here {
        return HandOver::Exclude("it is another project's".into());
    }
    if card.archived {
        return HandOver::Exclude("it is archived".into());
    }
    if card.closed {
        return HandOver::Exclude("it is done or cancelled".into());
    }
    match card.holder {
        Holder::Earlier => {}
        Holder::Nobody => {
            return HandOver::Exclude("it was released and has no holder".into());
        }
        Holder::Another => {
            return HandOver::Exclude("it was given to someone else since".into());
        }
    }
    if card.session == Session::Running {
        return HandOver::Leave("its earlier session is running again and keeps it".into());
    }
    if card.settled {
        return HandOver::Leave(
            "the person settled it for its earlier session; it is not handed on".into(),
        );
    }
    if !card.working {
        return HandOver::Leave(
            "it is not in a lane where work is under way; it is handed over when it returns to one"
                .into(),
        );
    }
    match card.session {
        Session::Ended | Session::Running => {}
        Session::EndedAgain => {
            return HandOver::Owe(
                "its earlier session ran again and ended since, and what that run left has not been read"
                    .into(),
            );
        }
        Session::Unproven => {
            return HandOver::Owe(
                "its earlier session's record is missing or cannot say which run it is".into(),
            );
        }
    }
    if card.leased {
        return HandOver::Owe("a live lease stands on it".into());
    }
    match work {
        Work::None | Work::Here | Work::Settled { .. } => HandOver::Hand,
        Work::Elsewhere { path, branch } => HandOver::Owe(match branch {
            Some(branch) => format!(
                "its earlier session's work is in {path} on {branch}, which is not where its successor works, and is not merged"
            ),
            None => format!(
                "its earlier session's work is in {path}, which is not where its successor works, and is not merged"
            ),
        }),
        Work::Unknown { why } => HandOver::Owe(format!(
            "its earlier session's checkout could not be accounted for: {why}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(clock: &str) -> DateTime<Utc> {
        clock.parse().unwrap()
    }

    const NOON: &str = "2020-01-01T12:00:00Z";

    fn run(name: &str) -> RunId {
        RunId::from(name)
    }

    fn card(name: &str) -> TaskId {
        TaskId::from(name.to_owned())
    }

    /// A checkout on `team/writer` against `main`, counted and listed.
    fn remains(path: &str, unmerged: u32, uncommitted: bool) -> Remains {
        Remains {
            path: path.into(),
            branch: Some("team/writer".into()),
            head: Some("abc1234".into()),
            target: Some("main".into()),
            merge_base: Some("0000000".into()),
            unmerged: Some(unmerged),
            uncommitted: Some(uncommitted),
            shared_with: Vec::new(),
        }
    }

    /// An entry of run one whose checkout is still to be read.
    fn pending() -> Earlier {
        Earlier::ended(
            AgentId::from("writer-one".to_owned()),
            run("run-one"),
            "writer".into(),
            "claude-code".into(),
            at(NOON),
            "exited".into(),
        )
    }

    /// An entry of run one whose checkout read as `remains`.
    fn read(remains: Remains) -> Earlier {
        let mut entry = pending();
        assert_eq!(
            entry.observe(&run("run-one"), Reading::Remains(remains), at(NOON)),
            Taken::Recorded
        );
        entry
    }

    fn settles(entry: &Earlier, cards: &[&str], checkout: bool) -> Settlement {
        Settlement {
            by: "person".into(),
            at: at(NOON),
            note: "looked at it".into(),
            run: entry.run.clone(),
            cards: cards.iter().map(|name| card(name)).collect(),
            checkout: checkout.then_some(entry.revision),
        }
    }

    /// The successor is given `path` on `team/writer`, holding the head.
    fn given(path: &str) -> Option<Successor<'_>> {
        Some(Successor {
            path,
            branch: Some("team/writer"),
            has_head: Some(true),
        })
    }

    /// A card that passes every check.
    const SOUND: Card = Card {
        here: true,
        archived: false,
        closed: false,
        working: true,
        holder: Holder::Earlier,
        session: Session::Ended,
        leased: false,
        settled: false,
    };

    /// A checkout nobody read, could read or could count is not a
    /// checkout with nothing in it. A count that could not be taken (no
    /// target to count against, no merge base, the shared checkout on its
    /// own target) and a status that failed are unknown, and unknown is
    /// owed and never handed over. Only both facts taken and both empty
    /// is nothing; known work is work whatever else is unknown.
    #[test]
    fn what_could_not_be_counted_is_not_counted_as_none() {
        assert!(pending().owes_checkout());
        assert!(matches!(
            work(&pending(), given("/work/app")),
            Work::Unknown { .. }
        ));
        let mut unreadable = pending();
        unreadable.observe(
            &run("run-one"),
            Reading::Unreadable {
                why: "the folder is gone".into(),
            },
            at(NOON),
        );
        assert!(unreadable.owes_checkout());

        let no_target = Remains {
            branch: None,
            target: None,
            merge_base: None,
            unmerged: None,
            ..remains("/work/app", 0, false)
        };
        let no_merge_base = Remains {
            merge_base: None,
            unmerged: None,
            ..remains("/work/app", 0, false)
        };
        let on_its_target = Remains {
            branch: Some("main".into()),
            unmerged: None,
            uncommitted: None,
            ..remains("/work/app", 0, false)
        };
        let status_failed = Remains {
            uncommitted: None,
            ..remains("/work/app", 0, false)
        };
        for unknown in [no_target, no_merge_base, on_its_target, status_failed] {
            assert!(matches!(unknown.left(), Left::Unknown(_)), "{unknown:?}");
            let entry = read(unknown.clone());
            assert!(entry.owes_checkout(), "{unknown:?}");
            let found = work(&entry, given("/work/app"));
            assert!(matches!(found, Work::Unknown { .. }), "{found:?}");
            assert!(matches!(hand_over(SOUND, &found), HandOver::Owe(_)));
        }
        // Known work beside an unknown count is work.
        let dirty_uncounted = Remains {
            unmerged: None,
            ..remains("/work/app", 0, true)
        };
        assert_eq!(dirty_uncounted.left(), Left::Work);
        // Counted and listed, and empty: nothing.
        let clean = read(remains("/work/app", 0, false));
        assert!(!clean.owes_checkout());
        assert_eq!(work(&clean, None), Work::None);
        // It worked in no checkout at all.
        let mut none = pending();
        none.observe(&run("run-one"), Reading::None, at(NOON));
        assert!(!none.owes_checkout());
        assert_eq!(work(&none, None), Work::None);
    }

    /// The work is where its successor stands only when that is the same
    /// folder, on the same branch, at or past the recorded head. The same
    /// folder switched to another branch is elsewhere; a head the
    /// successor's checkout does not hold is elsewhere; one that could
    /// not be told is unknown.
    #[test]
    fn here_is_the_recorded_branch_and_head_not_a_path() {
        let entry = read(remains("/work/app", 2, false));
        let elsewhere = Work::Elsewhere {
            path: "/work/app".into(),
            branch: Some("team/writer".into()),
        };
        assert_eq!(work(&entry, given("/work/app")), Work::Here);
        // Somebody switched the shared checkout to main since.
        assert_eq!(
            work(
                &entry,
                Some(Successor {
                    path: "/work/app",
                    branch: Some("main"),
                    has_head: Some(false),
                })
            ),
            elsewhere
        );
        // The branch was reset behind the head.
        assert_eq!(
            work(
                &entry,
                Some(Successor {
                    path: "/work/app",
                    branch: Some("team/writer"),
                    has_head: Some(false),
                })
            ),
            elsewhere
        );
        assert!(matches!(
            work(
                &entry,
                Some(Successor {
                    path: "/work/app",
                    branch: Some("team/writer"),
                    has_head: None,
                })
            ),
            Work::Unknown { .. }
        ));
        assert_eq!(work(&entry, given("/work/other")), elsewhere);
        assert_eq!(work(&entry, None), elsewhere);
        match hand_over(SOUND, &elsewhere) {
            HandOver::Owe(why) => assert!(
                why.contains("/work/app on team/writer") && why.contains("not merged"),
                "{why}"
            ),
            other => panic!("{other:?}"),
        }
    }

    /// A reading replaces what is recorded only where it is of the same
    /// run and of that checkout on that branch. The folder read again
    /// after it was switched to main says nothing of the commits recorded
    /// on the other branch: the entry goes on owing them, until their
    /// last one is known to be in its target. That clears the commits and
    /// not what was uncommitted.
    #[test]
    fn a_reading_of_another_branch_clears_nothing() {
        let one = run("run-one");
        let mut entry = read(remains("/work/app", 2, true));
        let revision = entry.revision;
        let switched = Remains {
            branch: Some("main".into()),
            head: Some("fff0000".into()),
            unmerged: Some(0),
            uncommitted: Some(false),
            ..remains("/work/app", 0, false)
        };
        assert_eq!(
            entry.observe(&one, Reading::Remains(switched), at(NOON)),
            Taken::Refused(READING_OF_ANOTHER_PLACE)
        );
        assert_eq!(
            entry.observe(
                &one,
                Reading::Remains(remains("/work/other", 0, false)),
                at(NOON)
            ),
            Taken::Refused(READING_OF_ANOTHER_PLACE)
        );
        for less in [
            Reading::None,
            Reading::Unreadable {
                why: "the folder is gone".into(),
            },
        ] {
            assert_eq!(
                entry.observe(&one, less, at(NOON)),
                Taken::Refused(READING_FOUND_LESS)
            );
        }
        assert_eq!(
            entry.observe(&run("run-two"), Reading::HeadMerged, at(NOON)),
            Taken::Refused(READING_OF_ANOTHER_RUN)
        );
        assert!(entry.owes_checkout());
        assert_eq!(entry.revision, revision, "nothing was recorded");
        // Its last commit is in its target now: the commits are cleared,
        // the uncommitted changes are as they were recorded.
        assert_eq!(
            entry.observe(&one, Reading::HeadMerged, at(NOON)),
            Taken::Recorded
        );
        assert_eq!(entry.revision, revision + 1);
        assert!(entry.owes_checkout(), "what was uncommitted is still owed");
        // Without uncommitted changes, a merged head leaves nothing.
        let mut committed = read(remains("/work/app", 2, false));
        assert_eq!(
            committed.observe(&one, Reading::HeadMerged, at(NOON)),
            Taken::Recorded
        );
        assert!(!committed.owes_checkout());
        assert_eq!(
            committed.observe(&one, Reading::HeadMerged, at(NOON)),
            Taken::Same
        );
        // The same checkout on the same branch, read again, does replace.
        let mut entry = read(remains("/work/app", 2, false));
        assert_eq!(
            entry.observe(
                &one,
                Reading::Remains(remains("/work/app", 0, false)),
                at(NOON)
            ),
            Taken::Recorded
        );
        assert!(!entry.owes_checkout());
        // Nothing recorded: nothing to call merged; and no checkout at all.
        assert_eq!(
            pending().observe(&one, Reading::HeadMerged, at(NOON)),
            Taken::Refused(NOTHING_RECORDED_TO_MERGE)
        );
        let mut none = pending();
        none.observe(&one, Reading::None, at(NOON));
        assert_eq!(
            none.observe(
                &one,
                Reading::Remains(remains("/work/app", 1, false)),
                at(NOON)
            ),
            Taken::Refused(NO_CHECKOUT)
        );
    }

    /// A settlement of cards names them, and clears those and no others.
    /// A card that comes to name the session later (reopened) is owed
    /// though an older settlement stands, so the entry is not resolved
    /// and is not retired. And a settled card is not handed on when it
    /// returns to a working lane: the person said it was dealt with.
    #[test]
    fn a_settlement_of_cards_clears_the_cards_it_names() {
        let mut entry = read(remains("/work/app", 0, false));
        entry.settle(settles(&entry, &["task-x"], false)).unwrap();
        assert!(entry.settled_card(&card("task-x")));
        assert!(entry.owes(&[card("task-x")]).nothing());
        // Y is reopened and names it.
        let owed = entry.owes(&[card("task-x"), card("task-y")]);
        assert_eq!(owed.cards, [card("task-y")]);
        assert!(!owed.nothing());
        // So at the bound it is not the one retired.
        let mut earlier: Vec<Earlier> = (0..EARLIER).map(|_| entry.clone()).collect();
        assert_eq!(
            make_room(&mut earlier, |_| vec![card("task-x"), card("task-y")]),
            Err(EARLIER_FULL)
        );
        assert_eq!(earlier.len(), EARLIER);
        // X returns to a working lane: left, with why.
        let settled = Card {
            settled: true,
            ..SOUND
        };
        match hand_over(settled, &Work::None) {
            HandOver::Leave(why) => assert!(why.contains("the person settled it"), "{why}"),
            other => panic!("{other:?}"),
        }
        // A settlement that names nothing is not one.
        assert_eq!(
            entry.settle(settles(&entry, &[], false)),
            Err(SETTLES_NOTHING)
        );
    }

    /// A settlement is of one run, and of the checkout as it was read at
    /// one revision. The same session bound again is another run: what
    /// was settled of the first does not fit the second, whether the seat
    /// keeps one entry or two, and a new ending is always a new revision.
    /// A checkout not yet read cannot be settled; one that could not be
    /// read can. Each settlement is kept: the checkout today and the
    /// cards tomorrow are two, with their notes and times.
    #[test]
    fn a_settlement_is_of_one_run_and_one_reading() {
        // Pending cannot be settled; unreadable can.
        let mut entry = pending();
        assert_eq!(
            entry.settle(settles(&entry, &[], true)),
            Err(NOTHING_READ_YET)
        );
        entry.observe(
            &run("run-one"),
            Reading::Unreadable {
                why: "the folder is gone".into(),
            },
            at(NOON),
        );
        entry.settle(settles(&entry, &[], true)).unwrap();
        assert!(!entry.owes_checkout());
        assert_eq!(
            work(&entry, None),
            Work::Settled {
                path: None,
                branch: None
            }
        );
        // One entry kept, the session resumed and ended again: unread,
        // and not settled by what was said of the run before.
        let first = entry.clone();
        entry.ended_again(run("run-two"), at("2020-01-02T12:00:00Z"), "exited".into());
        assert_eq!(entry.observed, Observed::Pending);
        assert_eq!(entry.revision, first.revision + 1);
        assert!(entry.owes_checkout());
        assert!(!entry.settled_checkout());
        assert_eq!(entry.settled.len(), 1, "what was said is kept as said");
        // A settlement composed against the first run does not fit.
        assert_eq!(
            entry.settle(settles(&first, &["task-x"], true)),
            Err(SETTLES_ANOTHER_RUN)
        );
        // Nor one of this run at a revision read since.
        let mut stale = settles(&entry, &[], true);
        entry.observe(
            &run("run-two"),
            Reading::Remains(remains("/work/app", 1, false)),
            at(NOON),
        );
        assert_eq!(entry.settle(stale.clone()), Err(SETTLES_OLDER_EVIDENCE));
        stale.checkout = Some(entry.revision);
        entry.settle(stale).unwrap();
        // Two entries of one agent: the second's own revision two is not
        // the first's.
        let mut second = pending();
        second.run = run("run-two");
        second.observe(
            &run("run-two"),
            Reading::Remains(remains("/work/app", 3, false)),
            at(NOON),
        );
        let first_entry = read(remains("/work/app", 3, false));
        assert_eq!(first_entry.revision, second.revision);
        assert_eq!(
            second.settle(settles(&first_entry, &[], true)),
            Err(SETTLES_ANOTHER_RUN)
        );
        // The checkout today, the cards tomorrow: both stand.
        let mut entry = read(remains("/work/app", 2, false));
        entry.settle(settles(&entry, &[], true)).unwrap();
        let mut tomorrow = settles(&entry, &["task-x"], false);
        tomorrow.at = at("2020-01-02T12:00:00Z");
        tomorrow.note = "the card is covered elsewhere".into();
        entry.settle(tomorrow).unwrap();
        assert_eq!(entry.settled.len(), 2);
        assert!(entry.settled_checkout() && entry.settled_card(&card("task-x")));
        assert!(entry.owes(&[card("task-x")]).nothing());
        // New evidence is newer than the settlement of the checkout, and
        // leaves the cards settled.
        entry.observe(
            &run("run-one"),
            Reading::Remains(remains("/work/app", 3, false)),
            at(NOON),
        );
        assert!(entry.owes_checkout());
        assert!(entry.settled_card(&card("task-x")));
    }

    /// Who shares a checkout is company, not work: it is kept up to date,
    /// and a session joining or leaving does not make new evidence of the
    /// same work or void the person's settlement of it.
    #[test]
    fn the_company_in_a_checkout_is_not_its_work() {
        let one = run("run-one");
        let mut entry = read(remains("/work/app", 2, false));
        entry.settle(settles(&entry, &[], true)).unwrap();
        let revision = entry.revision;
        let shared = Remains {
            shared_with: vec![AgentId::from("reviewer-one".to_owned())],
            ..remains("/work/app", 2, false)
        };
        assert_eq!(
            entry.observe(
                &one,
                Reading::Remains(shared.clone()),
                at("2020-01-03T12:00:00Z")
            ),
            Taken::Same
        );
        assert_eq!(entry.revision, revision);
        assert!(entry.settled_checkout());
        assert_eq!(entry.observed, Observed::Read { remains: shared });
        assert_eq!(entry.read_at, Some(at("2020-01-03T12:00:00Z")));
        // A settled checkout hands its card over and still says where the
        // work is.
        let found = work(&entry, given("/work/other"));
        assert_eq!(
            found,
            Work::Settled {
                path: Some("/work/app".into()),
                branch: Some("team/writer".into())
            }
        );
        assert_eq!(hand_over(SOUND, &found), HandOver::Hand);
    }

    /// Room is made by asking each entry what it owes, here, and retiring
    /// the resolved ones that ended longest ago, whatever order the list
    /// is in. With too few resolved nothing is retired and the start is
    /// refused: nothing unresolved is cut off for want of room.
    #[test]
    fn nothing_unresolved_is_cut_off_for_want_of_room() {
        let entry = |name: &str, day: u32, unmerged: u32| {
            let mut entry = read(remains("/work/app", unmerged, false));
            entry.name = name.into();
            entry.ended_at = at(&format!("2020-01-{day:02}T12:00:00Z"));
            entry
        };
        // Below the bound: nothing to do.
        let mut few = vec![entry("a", 1, 0)];
        assert_eq!(make_room(&mut few, |_| Vec::new()), Ok(()));
        assert_eq!(few.len(), 1);
        // At the bound: the resolved one that ended longest ago goes,
        // though it is not first in the list.
        let mut full: Vec<Earlier> = (0..EARLIER - 2)
            .map(|at| entry(&format!("owing-{at}"), 10, 1))
            .collect();
        full.insert(3, entry("resolved-new", 9, 0));
        full.push(entry("resolved-old", 2, 0));
        assert_eq!(full.len(), EARLIER);
        assert_eq!(make_room(&mut full, |_| Vec::new()), Ok(()));
        assert_eq!(full.len(), EARLIER - 1);
        assert!(full.iter().all(|entry| entry.name != "resolved-old"));
        assert!(full.iter().any(|entry| entry.name == "resolved-new"));
        // An entry resolved but for a card that still names it is not
        // resolved.
        full.push(entry("owing-more", 11, 1));
        assert_eq!(
            make_room(&mut full, |entry| {
                if entry.name == "resolved-new" {
                    vec![card("task-y")]
                } else {
                    Vec::new()
                }
            }),
            Err(EARLIER_FULL)
        );
        assert_eq!(full.len(), EARLIER);
        assert!(EARLIER_FULL.contains("agentdocker team settle"));
        // Past the bound (an ending cannot be refused), as many go as
        // are needed and there are.
        let mut over: Vec<Earlier> = (0..EARLIER + 1)
            .map(|at| entry(&format!("resolved-{at}"), 1 + at as u32, 0))
            .collect();
        assert_eq!(make_room(&mut over, |_| Vec::new()), Ok(()));
        assert_eq!(over.len(), EARLIER - 1);
        assert_eq!(over[0].name, "resolved-2", "the two oldest went");
    }

    /// A card passes to the successor only when every check holds, and
    /// each check that fails says what it is. What excludes a card for
    /// good is said before what only leaves it, and that before what is
    /// owed.
    #[test]
    fn a_card_passes_only_when_every_check_holds() {
        assert_eq!(hand_over(SOUND, &Work::None), HandOver::Hand);
        assert_eq!(hand_over(SOUND, &Work::Here), HandOver::Hand);
        let excluded = [
            (
                Card {
                    here: false,
                    ..SOUND
                },
                "another project's",
            ),
            (
                Card {
                    archived: true,
                    ..SOUND
                },
                "archived",
            ),
            (
                Card {
                    closed: true,
                    ..SOUND
                },
                "done or cancelled",
            ),
            (
                Card {
                    holder: Holder::Nobody,
                    ..SOUND
                },
                "released and has no holder",
            ),
            (
                Card {
                    holder: Holder::Another,
                    ..SOUND
                },
                "given to someone else",
            ),
        ];
        for (card, word) in excluded {
            match hand_over(card, &Work::None) {
                HandOver::Exclude(why) => assert!(why.contains(word), "{why}"),
                other => panic!("{word}: {other:?}"),
            }
        }
        let left = [
            (
                Card {
                    session: Session::Running,
                    ..SOUND
                },
                "running again",
            ),
            (
                Card {
                    settled: true,
                    ..SOUND
                },
                "the person settled it",
            ),
            (
                Card {
                    working: false,
                    ..SOUND
                },
                "not in a lane where work is under way",
            ),
        ];
        for (card, word) in left {
            match hand_over(card, &Work::None) {
                HandOver::Leave(why) => assert!(why.contains(word), "{why}"),
                other => panic!("{word}: {other:?}"),
            }
        }
        // A session that ran again and ended, or whose record cannot say
        // which run it is, is not "running again": the card is owed, and
        // the member is told.
        let owed = [
            (
                Card {
                    session: Session::EndedAgain,
                    ..SOUND
                },
                "ran again and ended since",
            ),
            (
                Card {
                    session: Session::Unproven,
                    ..SOUND
                },
                "record is missing",
            ),
            (
                Card {
                    leased: true,
                    ..SOUND
                },
                "a live lease",
            ),
        ];
        for (card, word) in owed {
            match hand_over(card, &Work::None) {
                HandOver::Owe(why) => assert!(why.contains(word), "{why}"),
                other => panic!("{word}: {other:?}"),
            }
        }
        match hand_over(
            SOUND,
            &Work::Unknown {
                why: "the folder is gone".into(),
            },
        ) {
            HandOver::Owe(why) => assert!(why.ends_with("the folder is gone"), "{why}"),
            other => panic!("{other:?}"),
        }
        // Excluded before left, left before owed: a closed card under a
        // lease is closed; a blocked card of a session whose record
        // cannot be read is left until it returns to a working lane.
        assert!(matches!(
            hand_over(
                Card {
                    closed: true,
                    leased: true,
                    working: false,
                    session: Session::Unproven,
                    ..SOUND
                },
                &Work::None
            ),
            HandOver::Exclude(_)
        ));
        assert!(matches!(
            hand_over(
                Card {
                    working: false,
                    session: Session::Unproven,
                    ..SOUND
                },
                &Work::None
            ),
            HandOver::Leave(_)
        ));
    }

    /// An entry as stored: what is unset is not written, an unknown count
    /// is absent and not zero, and each state of the checkout has its
    /// word.
    #[test]
    fn an_earlier_session_is_stored_and_read_back() {
        let mut entry = read(remains("/work/app", 2, true));
        entry.settle(settles(&entry, &["task-x"], true)).unwrap();
        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(
            serde_json::from_value::<Earlier>(json.clone()).unwrap(),
            entry
        );
        assert_eq!(json["run"], "run-one");
        assert_eq!(json["observed"]["state"], "read");
        assert_eq!(json["observed"]["remains"]["unmerged"], 2);
        assert!(json["observed"]["remains"].get("shared_with").is_none());
        assert_eq!(json["settled"][0]["cards"], serde_json::json!(["task-x"]));
        assert_eq!(json["settled"][0]["checkout"], entry.revision);
        let uncounted = read(Remains {
            unmerged: None,
            uncommitted: None,
            ..remains("/work/app", 0, false)
        });
        let json = serde_json::to_value(&uncounted).unwrap();
        assert!(json["observed"]["remains"].get("unmerged").is_none());
        assert!(json["observed"]["remains"].get("uncommitted").is_none());
        assert_eq!(serde_json::from_value::<Earlier>(json).unwrap(), uncounted);
        for (observed, word) in [
            (Observed::Pending, "pending"),
            (Observed::None, "none"),
            (
                Observed::Unreadable {
                    why: "the folder is gone".into(),
                },
                "unreadable",
            ),
        ] {
            assert_eq!(serde_json::to_value(&observed).unwrap()["state"], word);
        }
        let bare = serde_json::to_value(pending()).unwrap();
        assert!(bare.get("settled").is_none() && bare.get("read_at").is_none());
    }
}
