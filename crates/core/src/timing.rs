//! Time on cards: a card's history folded into the stays it made in each
//! lane, the time each lane has had of it, and a board's numbers over a
//! range of cards — how many are done, how long a card takes from its
//! pull to Done, how long Review keeps one. Pure: the caller says what
//! time it is, so a card still in its lane counts up to `now`.
//!
//! The history is the record (`TaskEntry`: created, pulled, moved,
//! archived, unarchived); the card's own `lane_since` and `archived_at`
//! say where it is now and since when, and bound the last stay. What the
//! record does not show is left out, never guessed: a span no line
//! accounts for is assigned to no lane, a stay whose end or start was not
//! seen counts no time, a first pull is claimed only from a record unbroken
//! up to it, and the result says it is incomplete.
use crate::board::Board;
use crate::task::{Task, TaskAction, TaskEntry, TaskId};
use crate::{AgentId, Column};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The most stays a card's detail carries, its newest; the older ones are
/// counted in [`CardTimes::stays_omitted`].
pub const DETAIL_STAYS: usize = 200;

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// One stay of a card in a lane: from when it came in until it left,
/// or open (`to` absent) while it is still there.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stay {
    /// The lane, by slug.
    pub lane: String,
    pub from: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<DateTime<Utc>>,
    /// Its end was not seen: the lines that took the card out of the lane
    /// are not in the record, so `to` is only the latest it can have ended
    /// (when the record next knows where the card was), and the stay
    /// counts no time.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unseen_end: bool,
    /// Its start was not seen: the card is back on the board with no line
    /// saying when it came back, so `from` is only the earliest it can have
    /// begun (the archive the record shows), and the stay counts no time.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unseen_start: bool,
    /// Opened by an unarchive, going on with the stay the card was in when
    /// it was archived: no new arrival in the lane.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub resumed: bool,
}

impl Stay {
    fn new(lane: &str, from: DateTime<Utc>) -> Self {
        Self {
            lane: lane.to_owned(),
            from,
            to: None,
            unseen_end: false,
            unseen_start: false,
            resumed: false,
        }
    }

    /// Both its ends were seen, so its length is known.
    fn seen(&self) -> bool {
        !self.unseen_end && !self.unseen_start
    }

    /// How long it lasted, or has lasted so far at `now`; for a stay whose
    /// end or start was not seen, the longest it can have lasted.
    pub fn seconds(&self, now: DateTime<Utc>) -> u64 {
        let end = self.to.unwrap_or(now);
        u64::try_from((end - self.from).num_seconds()).unwrap_or(0)
    }
}

/// What one lane has had of a card: every stay there added up.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneTime {
    /// The lane, by slug.
    pub lane: String,
    /// Over every stay whose end was seen, the open one counted up to
    /// `now`.
    pub seconds: u64,
    /// How many times it came into the lane.
    pub stays: u32,
    /// The card is there now.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub current: bool,
    /// Over the stays that have ended, and were seen to.
    #[serde(default)]
    pub finished: u64,
    /// How many of its stays have ended, and were seen to.
    #[serde(default)]
    pub left: u32,
}

/// A card's time on the board: its stays in order, the time each lane
/// has had of it, and the two moments the board's numbers are made of.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardTimes {
    pub task: TaskId,
    /// Oldest first; the last is open while the card is on the board. A
    /// list carries only the last ([`Self::summary`]), a card's detail its
    /// newest [`DETAIL_STAYS`].
    pub stays: Vec<Stay>,
    /// The older stays not carried here.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub stays_omitted: usize,
    /// In the order the card first came into each lane.
    pub lanes: Vec<LaneTime>,
    /// Its first pull, when an agent ever pulled it and the record shows
    /// that it is the first: every line from the card's creation through
    /// it, read whole, with none found missing between.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pulled_at: Option<DateTime<Utc>>,
    /// When it came into the Done lane it is in; absent unless it is done.
    /// An unarchive does not move it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done_at: Option<DateTime<Utc>>,
    /// The record does not show all of the card's time: lines lost, kept
    /// from after it was filed, or more than were read. What is shown was
    /// seen; what is not was left out.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub incomplete: bool,
}

impl CardTimes {
    /// The stay the card is in now, while it is on the board.
    pub fn current(&self) -> Option<&Stay> {
        self.stays.last().filter(|s| s.to.is_none())
    }

    /// How long the card has been in its lane, at `now`; nothing when the
    /// record did not see it come back to the board.
    pub fn in_lane(&self, now: DateTime<Utc>) -> Option<u64> {
        self.current()
            .filter(|s| !s.unseen_start)
            .map(|s| s.seconds(now))
    }

    /// From its first pull to its arrival in Done, for a card that was
    /// pulled and is done, when the record shows both.
    pub fn pull_to_done(&self) -> Option<u64> {
        let pulled = self.pulled_at?;
        let done = self.done_at?;
        Some(u64::try_from((done - pulled).num_seconds()).unwrap_or(0))
    }

    /// Time in lanes of `category` on `board` over stays that have ended:
    /// what the lane kept the card for, once it let it go. None when it
    /// was never seen to leave such a lane.
    pub fn finished_in(&self, board: &Board, category: Column) -> Option<u64> {
        let mut total = None;
        for lane in self.lanes.iter().filter(|l| l.left > 0) {
            if board
                .lane(&lane.lane)
                .is_some_and(|l| l.category == category)
            {
                total = Some(total.unwrap_or(0) + lane.finished);
            }
        }
        total
    }

    /// The same, carrying only the stay it is in (or its last), the rest
    /// counted: what a list of cards needs, bounded however long the
    /// card's history is.
    pub fn summary(&self) -> Self {
        self.newest(1)
    }

    /// The same, carrying its newest `n` stays, the rest counted, and at
    /// most [`crate::board::LANES_MAX`] lanes (the lane it is in among
    /// them); a card with more lanes than that is incomplete.
    pub fn newest(&self, n: usize) -> Self {
        let keep = self.stays.len().min(n);
        let cut = self.stays.len() - keep;
        // One place is the lane it is in, wherever that came in the order.
        let mut room = crate::board::LANES_MAX
            .saturating_sub(usize::from(self.lanes.iter().any(|l| l.current)));
        let lanes: Vec<LaneTime> = self
            .lanes
            .iter()
            .filter(|lane| {
                let kept = lane.current || room > 0;
                if kept && !lane.current {
                    room -= 1;
                }
                kept
            })
            .cloned()
            .collect();
        Self {
            stays: self.stays[cut..].to_vec(),
            stays_omitted: self.stays_omitted + cut,
            incomplete: self.incomplete || lanes.len() < self.lanes.len(),
            lanes,
            ..self.clone()
        }
    }
}

/// The lane a card is in, by slug, or its category's name for a card
/// from before lanes.
fn current_lane(task: &Task) -> String {
    task.lane
        .clone()
        .unwrap_or_else(|| task.column.as_str().to_owned())
}

/// Whether a history line says where the card was: the lines a fold reads
/// (the daemon reads only these).
pub fn moves(action: &TaskAction) -> bool {
    matches!(
        action,
        TaskAction::Created { .. }
            | TaskAction::Pulled { .. }
            | TaskAction::Moved { .. }
            | TaskAction::Archived
            | TaskAction::Unarchived
    )
}

/// Fold a card's history into its stays. Entries are taken in time order
/// whatever order they come in; `complete` says the history was read
/// whole, not cut to its newest lines. The card's own fields bound the
/// result: its last stay is in the lane it is in now, from `lane_since`,
/// and an archived card's ends when it was archived. What the record
/// does not show is assigned to no lane and makes the result incomplete:
/// time before the first line when that line is not the card's creation;
/// a line that finds the card elsewhere than the record has it (a move out
/// of another lane, a move or an archive while the record has it off the
/// board, an unarchive while it has it on), where the stay it had ends
/// unseen; a stay the record leaves it in that is not where the card is
/// now, or that began before its stay there did (its end not seen); a
/// card back on the board, or archived again, after the archive the record
/// ends with; and a first pull, unless every line from the card's creation
/// through it was read with none found missing.
pub fn card_times(
    task: &Task,
    history: &[TaskEntry],
    complete: bool,
    now: DateTime<Utc>,
) -> CardTimes {
    let mut entries: Vec<&TaskEntry> = history.iter().filter(|e| moves(&e.action)).collect();
    entries.sort_by(|a, b| a.at.cmp(&b.at).then(a.id.cmp(&b.id)));
    let lane_now = current_lane(task);
    let mut incomplete = !complete
        || entries
            .first()
            .is_some_and(|e| !matches!(e.action, TaskAction::Created { .. }));
    // Every line from the card's creation is here so far, which is what
    // claiming a first pull needs.
    let mut unbroken = !incomplete;
    let mut stays: Vec<Stay> = Vec::new();
    let mut open: Option<Stay> = None;
    let mut pulled_at = None;
    let mut pulled = false;
    let close = |open: &mut Option<Stay>, stays: &mut Vec<Stay>, at: DateTime<Utc>| {
        if let Some(mut stay) = open.take() {
            stay.to = Some(at.max(stay.from));
            stays.push(stay);
        }
    };
    // The lines that took the card out of the stay the record has it in
    // are lost: that stay's end is unseen, no later than `at`.
    let lose = |stay: Option<Stay>, stays: &mut Vec<Stay>, at: DateTime<Utc>| {
        if let Some(mut stay) = stay {
            stay.to = Some(at.max(stay.from));
            stay.unseen_end = true;
            stays.push(stay);
        }
    };
    // Into a lane at `at`: nothing when the card is there already (a pull
    // of a card in progress), else the stay so far ends and a new one
    // begins. Where it was before a first line that is not its creation
    // is not assumed.
    let enter = |open: &mut Option<Stay>, stays: &mut Vec<Stay>, lane: &str, at: DateTime<Utc>| {
        if open.as_ref().is_some_and(|s| s.lane == lane) {
            return;
        }
        close(open, stays, at);
        *open = Some(Stay::new(lane, at));
    };
    for entry in entries {
        // Off the board, as far as the record knows: its last line so far
        // archived the card.
        let off = open.is_none() && !stays.is_empty();
        // A line that finds the card elsewhere than the record has it: the
        // lines between are lost. A move names the lane it took the card
        // out of; a pull names only the lane it took it into (its `from`
        // is the holder it took over from, not a lane).
        let lost = match &entry.action {
            TaskAction::Moved { from, .. } => match &open {
                Some(stay) => !from.is_empty() && stay.lane != *from,
                None => off,
            },
            TaskAction::Pulled { .. } | TaskAction::Archived => off,
            TaskAction::Unarchived => open.is_some(),
            _ => false,
        };
        if lost {
            incomplete = true;
            unbroken = false;
        }
        match &entry.action {
            TaskAction::Created { lane } => {
                if open.is_none() && stays.is_empty() {
                    let lane = if lane.is_empty() { &lane_now } else { lane };
                    open = Some(Stay::new(lane, entry.at));
                }
            }
            TaskAction::Pulled { lane, .. } => {
                // The first pull the record shows is the card's first only
                // when no line before it is missing.
                if !pulled {
                    pulled = true;
                    if unbroken {
                        pulled_at = Some(entry.at);
                    }
                }
                let lane = if lane.is_empty() { &lane_now } else { lane };
                enter(&mut open, &mut stays, lane, entry.at);
            }
            TaskAction::Moved { to, .. } => {
                if lost {
                    lose(open.take(), &mut stays, entry.at);
                }
                let to = if to.is_empty() { &lane_now } else { to };
                enter(&mut open, &mut stays, to, entry.at);
            }
            TaskAction::Archived => close(&mut open, &mut stays, entry.at),
            TaskAction::Unarchived => {
                // Back in the lane it was archived from; an archive the
                // record did not see ends the stay it had, unseen.
                let lane = match open.take() {
                    Some(stay) => {
                        let lane = stay.lane.clone();
                        lose(Some(stay), &mut stays, entry.at);
                        lane
                    }
                    None => stays
                        .last()
                        .map_or_else(|| lane_now.clone(), |s| s.lane.clone()),
                };
                let mut stay = Stay::new(&lane, entry.at);
                stay.resumed = stays.last().is_some_and(|s| s.lane == lane);
                open = Some(stay);
            }
            TaskAction::Updated { .. }
            | TaskAction::Assigned { .. }
            | TaskAction::Commented { .. } => {}
        }
    }
    // Where the card is now, and since when, closes the record: the stay
    // the record leaves it in is that stay only when it is in the same
    // lane and began no earlier than the card's stay there.
    let end = task.archived_at;
    let since = task.in_lane_since();
    // The archive the record ends with, when it leaves the card off the
    // board.
    let off = match &open {
        None => stays.last().and_then(|s| s.to),
        Some(_) => None,
    };
    match (off, end) {
        // Archived again after the record, which saw neither its return
        // nor this archive: the stays it saw stand as they ended, and of the
        // time between only the card's own stay in its lane is shown, when
        // the lane got it after the record did.
        (Some(off), Some(archived)) if archived > off => {
            incomplete = true;
            if since > off {
                let mut stay = Stay::new(&lane_now, since);
                stay.to = Some(archived);
                stays.push(stay);
            }
        }
        // Back on the board with no line saying so. Its lane got it after
        // the record: from then. Still the lane it was archived from: the
        // stay goes on, but when it came back is not known, so its start
        // is unseen (no earlier than the archive) and it counts no time.
        (Some(off), None) => {
            incomplete = true;
            let archived_from = stays.last().map(|s| s.lane.clone());
            let mut stay = Stay::new(&lane_now, since.max(off));
            if since <= off {
                stay.unseen_start = true;
                stay.resumed = archived_from.as_deref() == Some(lane_now.as_str());
            }
            stays.push(stay);
        }
        _ => reconcile(
            &mut stays,
            &mut open,
            &lane_now,
            since,
            end,
            task.created_at,
            &mut incomplete,
        ),
    }
    let mut lanes: Vec<LaneTime> = Vec::new();
    for stay in &stays {
        let secs = if stay.seen() { stay.seconds(now) } else { 0 };
        let seen_out = stay.to.is_some() && stay.seen();
        let entered = u32::from(!stay.resumed);
        match lanes.iter_mut().find(|l| l.lane == stay.lane) {
            Some(lane) => {
                lane.seconds += secs;
                lane.stays += entered;
                lane.current |= stay.to.is_none();
                if seen_out {
                    lane.finished += secs;
                    lane.left += 1;
                }
            }
            None => lanes.push(LaneTime {
                lane: stay.lane.clone(),
                seconds: secs,
                stays: entered.max(1),
                current: stay.to.is_none(),
                finished: if seen_out { secs } else { 0 },
                left: u32::from(seen_out),
            }),
        }
    }
    // The Done stay it is in began with the move that brought it there,
    // not with an unarchive that put it back.
    let done_at = (task.column == Column::Done)
        .then(|| {
            let mut at = stays.len().checked_sub(1)?;
            while at > 0 && stays[at].resumed && stays[at - 1].lane == stays[at].lane {
                at -= 1;
            }
            Some(stays[at].from)
        })
        .flatten();
    CardTimes {
        task: task.id.clone(),
        stays,
        stays_omitted: 0,
        lanes,
        pulled_at,
        done_at,
        incomplete,
    }
}

/// The record's last stay against the card's own fields, when the record
/// leaves the card on the board, or off it by the archive the card names:
/// the stay is the card's own only in the same lane and begun no earlier
/// than `since`; else its end is unseen and the card's stay starts at
/// `since`. With no stay at all, the card's stay is from `since`.
fn reconcile(
    stays: &mut Vec<Stay>,
    open: &mut Option<Stay>,
    lane_now: &str,
    since: DateTime<Utc>,
    end: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    incomplete: &mut bool,
) {
    let ended = |from: DateTime<Utc>| end.map(|at| at.max(from));
    let last = match open.take() {
        Some(stay) => Some(stay),
        None if end.is_some() => stays.pop(),
        None => None,
    };
    match last {
        Some(mut stay) if stay.lane == lane_now && stay.from >= since => {
            if end.is_some() {
                stay.to = ended(stay.from);
            }
            stays.push(stay);
        }
        Some(mut stay) => {
            stay.to = Some(since.max(stay.from));
            stay.unseen_end = true;
            let from = since.max(stay.from);
            stays.push(stay);
            *incomplete = true;
            let mut now_in = Stay::new(lane_now, from);
            now_in.to = ended(from);
            stays.push(now_in);
        }
        None => {
            // No stay at all: nothing shows where it was before `since`
            // unless it has been there since it was filed.
            if since > created_at {
                *incomplete = true;
            }
            let mut now_in = Stay::new(lane_now, since);
            now_in.to = ended(since);
            stays.push(now_in);
        }
    }
}

/// A board's numbers over a range of cards: labelled, for a strip.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stats {
    /// Cards in a Done lane.
    pub done: usize,
    /// Median seconds from a card's first pull to its arrival in Done,
    /// over the done cards that were pulled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_to_done: Option<u64>,
    /// Median seconds a card spent in Review lanes before leaving them,
    /// over the cards that have left Review at least once and whose
    /// record is complete.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_review: Option<u64>,
    /// Cards on view whose record does not show all their time
    /// ([`CardTimes::incomplete`]): left out of the Review median, and
    /// out of pull to done where their first pull is not shown.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub incomplete: usize,
    /// The same, per holder of the done cards: most done first.
    pub holders: Vec<HolderStats>,
}

/// One holder's numbers over the done cards it holds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HolderStats {
    pub holder: AgentId,
    pub done: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_to_done: Option<u64>,
}

/// The middle value, the mean of the two middle ones for an even count;
/// none of nothing.
pub fn median(mut values: Vec<u64>) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    let n = values.len();
    Some(if n % 2 == 1 {
        values[n / 2]
    } else {
        values[n / 2 - 1].midpoint(values[n / 2])
    })
}

impl Stats {
    /// Over `cards` — each with its times — on `board`, whose lanes say
    /// which are Review.
    pub fn of<'a>(
        board: &Board,
        cards: impl IntoIterator<Item = (&'a Task, &'a CardTimes)>,
    ) -> Self {
        let mut done = 0;
        let mut pull_to_done = Vec::new();
        let mut in_review = Vec::new();
        let mut holders: Vec<(AgentId, usize, Vec<u64>)> = Vec::new();
        let mut incomplete = 0;
        for (task, times) in cards {
            if times.incomplete {
                incomplete += 1;
            } else if let Some(secs) = times.finished_in(board, Column::Review) {
                in_review.push(secs);
            }
            if task.column != Column::Done || task.archived_at.is_some() {
                continue;
            }
            done += 1;
            let span = times.pull_to_done();
            pull_to_done.extend(span);
            if let Some(holder) = &task.assignee {
                match holders.iter_mut().find(|(h, _, _)| h == holder) {
                    Some((_, count, spans)) => {
                        *count += 1;
                        spans.extend(span);
                    }
                    None => holders.push((holder.clone(), 1, span.into_iter().collect())),
                }
            }
        }
        holders.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.as_str().cmp(b.0.as_str())));
        Stats {
            done,
            pull_to_done: median(pull_to_done),
            in_review: median(in_review),
            incomplete,
            holders: holders
                .into_iter()
                .map(|(holder, done, spans)| HolderStats {
                    holder,
                    done,
                    pull_to_done: median(spans),
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProjectId;
    use chrono::{Duration, TimeZone};

    fn at(hours: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2000, 1, 1, 0, 0, 0).unwrap() + Duration::hours(hours)
    }

    fn board() -> Board {
        Board::main(ProjectId::from("p"), "p", at(0))
    }

    fn card(lane: &str, column: Column, since: i64) -> Task {
        let mut task = Task::new(
            ProjectId::from("p"),
            "Fix login",
            "",
            Some(Column::Ready),
            "user",
            Vec::new(),
            at(0),
        )
        .unwrap();
        task.lane = Some(lane.to_owned());
        task.column = column;
        task.lane_since = Some(at(since));
        task
    }

    fn entry(task: &Task, hours: i64, action: TaskAction) -> TaskEntry {
        TaskEntry::new(task, "agent-1", action, at(hours))
    }

    fn created(task: &Task, lane: &str) -> TaskEntry {
        entry(
            task,
            0,
            TaskAction::Created {
                lane: lane.to_owned(),
            },
        )
    }

    /// A pull into `lane`; its `from` is the holder a take-over took it
    /// from, which an ordinary pull has none of.
    fn pulled(task: &Task, hours: i64, lane: &str) -> TaskEntry {
        entry(
            task,
            hours,
            TaskAction::Pulled {
                lane: lane.to_owned(),
                from: None,
            },
        )
    }

    fn moved(task: &Task, hours: i64, from: &str, to: &str) -> TaskEntry {
        entry(
            task,
            hours,
            TaskAction::Moved {
                from: from.to_owned(),
                to: to.to_owned(),
            },
        )
    }

    fn lane_secs(times: &CardTimes, lane: &str) -> u64 {
        times
            .lanes
            .iter()
            .find(|l| l.lane == lane)
            .map_or(0, |l| l.seconds)
    }

    const H: u64 = 3600;

    /// Ready, pulled at 2, to Review at 5, sent back at 6, to Review at 8,
    /// done at 9, read at 20: each lane adds its stays up, Review twice.
    #[test]
    fn a_card_that_went_back_and_forth_adds_its_stays_up() {
        let task = card("done", Column::Done, 9);
        let history = vec![
            created(&task, "ready"),
            pulled(&task, 2, "in_progress"),
            moved(&task, 5, "in_progress", "review"),
            moved(&task, 6, "review", "in_progress"),
            moved(&task, 8, "in_progress", "review"),
            moved(&task, 9, "review", "done"),
        ];
        let times = card_times(&task, &history, true, at(20));
        let lanes: Vec<(&str, u64, u32, bool)> = times
            .lanes
            .iter()
            .map(|l| (l.lane.as_str(), l.seconds, l.stays, l.current))
            .collect();
        assert_eq!(
            lanes,
            vec![
                ("ready", 2 * H, 1, false),
                ("in_progress", 5 * H, 2, false),
                ("review", 2 * H, 2, false),
                ("done", 11 * H, 1, true),
            ]
        );
        assert_eq!(times.stays.len(), 6);
        assert_eq!(times.pulled_at, Some(at(2)));
        assert_eq!(times.done_at, Some(at(9)));
        assert_eq!(times.pull_to_done(), Some(7 * H));
        assert_eq!(times.finished_in(&board(), Column::Review), Some(2 * H));
        assert_eq!(times.in_lane(at(20)), Some(11 * H));
    }

    /// A card the person moved by hand, never pulled: no pull, no pull to
    /// done, its lanes still counted.
    #[test]
    fn a_card_never_pulled_has_no_pull_to_done() {
        let task = card("done", Column::Done, 4);
        let history = vec![
            created(&task, "backlog"),
            moved(&task, 1, "backlog", "ready"),
            moved(&task, 4, "ready", "done"),
        ];
        let times = card_times(&task, &history, true, at(10));
        assert_eq!(times.pulled_at, None);
        assert_eq!(times.pull_to_done(), None);
        assert_eq!(times.done_at, Some(at(4)));
        assert_eq!(lane_secs(&times, "backlog"), H);
        assert_eq!(lane_secs(&times, "ready"), 3 * H);
        assert_eq!(lane_secs(&times, "done"), 6 * H);
        assert_eq!(times.finished_in(&board(), Column::Review), None);
    }

    /// A card still in its lane counts up to now, and only there.
    #[test]
    fn a_card_still_in_its_lane_counts_up_to_now() {
        let task = card("in_progress", Column::InProgress, 3);
        let history = vec![created(&task, "ready"), pulled(&task, 3, "in_progress")];
        let early = card_times(&task, &history, true, at(4));
        let later = card_times(&task, &history, true, at(10));
        assert_eq!(early.in_lane(at(4)), Some(H));
        assert_eq!(later.in_lane(at(10)), Some(7 * H));
        assert_eq!(lane_secs(&later, "ready"), 3 * H);
        assert!(later.current().is_some_and(|s| s.lane == "in_progress"));
        assert_eq!(later.done_at, None);
        assert_eq!(
            later.lanes.iter().filter(|l| l.current).count(),
            1,
            "one lane has it now"
        );
    }

    /// A pull of a card already in progress (a renewal) opens no stay,
    /// and the first pull is the one kept.
    #[test]
    fn a_pull_where_the_card_already_is_changes_nothing() {
        let task = card("in_progress", Column::InProgress, 2);
        let history = vec![
            created(&task, "ready"),
            pulled(&task, 2, "in_progress"),
            pulled(&task, 5, "in_progress"),
        ];
        let times = card_times(&task, &history, true, at(6));
        assert_eq!(times.stays.len(), 2);
        assert_eq!(times.pulled_at, Some(at(2)));
        assert_eq!(times.in_lane(at(6)), Some(4 * H));
    }

    /// Lines out of order are folded in time order.
    #[test]
    fn entries_are_folded_in_time_order() {
        let task = card("review", Column::Review, 5);
        let history = vec![
            moved(&task, 5, "in_progress", "review"),
            pulled(&task, 2, "in_progress"),
            created(&task, "ready"),
        ];
        let times = card_times(&task, &history, true, at(6));
        let lanes: Vec<&str> = times.lanes.iter().map(|l| l.lane.as_str()).collect();
        assert_eq!(lanes, vec!["ready", "in_progress", "review"]);
        assert_eq!(lane_secs(&times, "in_progress"), 3 * H);
    }

    /// A history that begins after the card (its first lines lost) gives
    /// no lane the time before its first line; one that ends elsewhere
    /// than the card is now leaves that stay's end unseen, counting no
    /// time, and the card's lane opens at `lane_since`. Both are said to
    /// be incomplete.
    #[test]
    fn the_card_itself_bounds_a_history_with_gaps() {
        let task = card("review", Column::Review, 7);
        let history = vec![pulled(&task, 2, "in_progress")];
        let times = card_times(&task, &history, true, at(8));
        let stays: Vec<(&str, i64, Option<i64>)> = times
            .stays
            .iter()
            .map(|s| {
                (
                    s.lane.as_str(),
                    (s.from - at(0)).num_hours(),
                    s.to.map(|t| (t - at(0)).num_hours()),
                )
            })
            .collect();
        assert_eq!(
            stays,
            vec![("in_progress", 2, Some(7)), ("review", 7, None)]
        );
        assert!(times.stays[0].unseen_end);
        assert!(times.incomplete);
        assert_eq!(lane_secs(&times, "in_progress"), 0, "its end was not seen");
        assert_eq!(times.in_lane(at(8)), Some(H));

        // No history at all: one stay, from when its lane got it.
        let bare = card_times(&task, &[], true, at(8));
        assert_eq!(bare.stays.len(), 1);
        assert_eq!(bare.stays[0].lane, "review");
        assert_eq!(bare.stays[0].from, at(7));
        assert_eq!(bare.in_lane(at(8)), Some(H));
        assert!(bare.incomplete, "nothing shows where it was before 7");
        // A card in its lane since it was filed, with no history, is whole.
        let filed = card("review", Column::Review, 0);
        assert!(!card_times(&filed, &[], true, at(8)).incomplete);
    }

    /// Time off the board is nobody's: an archived card's last stay ends
    /// when it was archived, and one unarchived starts again in its lane.
    #[test]
    fn time_off_the_board_is_not_counted() {
        let mut task = card("done", Column::Done, 3);
        task.archived_at = Some(at(5));
        let history = vec![
            created(&task, "ready"),
            moved(&task, 3, "ready", "done"),
            entry(&task, 5, TaskAction::Archived),
        ];
        let times = card_times(&task, &history, true, at(20));
        assert_eq!(lane_secs(&times, "done"), 2 * H);
        assert!(times.current().is_none());
        assert_eq!(times.done_at, Some(at(3)));

        // Unarchived at 9: `Task::unarchive` leaves `lane_since` as it
        // was. The stay goes on, no new arrival, and it was done at 3.
        let mut back = task.clone();
        back.archived_at = None;
        let mut history = history;
        history.push(entry(&back, 9, TaskAction::Unarchived));
        let times = card_times(&back, &history, true, at(10));
        assert_eq!(lane_secs(&times, "done"), 3 * H);
        assert_eq!(times.in_lane(at(10)), Some(H));
        assert!(times.stays.last().is_some_and(|s| s.resumed));
        assert_eq!(
            times
                .lanes
                .iter()
                .find(|l| l.lane == "done")
                .map(|l| l.stays),
            Some(1)
        );
        assert_eq!(times.done_at, Some(at(3)), "an unarchive is no completion");
        assert!(!times.incomplete);
    }

    /// The board's numbers: cards done, the median pull to done over the
    /// pulled ones, the median finished time in Review over the cards
    /// that left it, and each holder's own, most done first.
    #[test]
    fn a_boards_numbers_are_medians_over_the_range() {
        let board = board();
        let mut done = Vec::new();
        for (n, hours) in [(1, 2), (2, 4), (3, 12)] {
            let mut task = card("done", Column::Done, 1 + hours + n);
            task.assignee = Some(AgentId::from(if n == 3 { "agent-2" } else { "agent-1" }));
            let history = vec![
                created(&task, "ready"),
                pulled(&task, 1, "in_progress"),
                moved(&task, 1 + hours, "in_progress", "review"),
                moved(&task, 1 + hours + n, "review", "done"),
            ];
            done.push((task, history));
        }
        // One sent back and still in progress: its finished Review time
        // counts, its pull to done does not.
        let open = card("in_progress", Column::InProgress, 6);
        let open_history = vec![
            created(&open, "ready"),
            pulled(&open, 1, "in_progress"),
            moved(&open, 2, "in_progress", "review"),
            moved(&open, 6, "review", "in_progress"),
        ];
        // One done by hand, never pulled: counted done, no pull to done.
        let hand = card("done", Column::Done, 3);
        let hand_history = vec![created(&hand, "ready"), moved(&hand, 3, "ready", "done")];
        let now = at(30);
        let mut folded: Vec<(Task, CardTimes)> = done
            .iter()
            .map(|(t, h)| (t.clone(), card_times(t, h, true, now)))
            .collect();
        folded.push((open.clone(), card_times(&open, &open_history, true, now)));
        folded.push((hand.clone(), card_times(&hand, &hand_history, true, now)));
        let stats = Stats::of(&board, folded.iter().map(|(t, c)| (t, c)));
        assert_eq!(stats.done, 4);
        // Pull to done: 3, 6 and 15 hours; the median is 6.
        assert_eq!(stats.pull_to_done, Some(6 * H));
        // Review: 1, 2, 3 and the open card's 4 hours; the median is 2.5.
        assert_eq!(stats.in_review, Some(2 * H + H / 2));
        assert_eq!(stats.holders.len(), 2);
        assert_eq!(stats.holders[0].holder.as_str(), "agent-1");
        assert_eq!(stats.holders[0].done, 2);
        assert_eq!(stats.holders[0].pull_to_done, Some((3 * H).midpoint(6 * H)));
        assert_eq!(stats.holders[1].holder.as_str(), "agent-2");
        assert_eq!(stats.holders[1].pull_to_done, Some(15 * H));
        assert_eq!(median(Vec::new()), None);
        assert_eq!(median(vec![5]), Some(5));
        assert_eq!(median(vec![4, 1, 3, 2]), Some(2));
        assert_eq!(Stats::of(&board, std::iter::empty()), Stats::default());
    }

    /// The stays as `(lane, from, to, end unseen)`, in hours.
    fn hours(times: &CardTimes) -> Vec<(&str, i64, Option<i64>, bool)> {
        times
            .stays
            .iter()
            .map(|s| {
                (
                    s.lane.as_str(),
                    (s.from - at(0)).num_hours(),
                    s.to.map(|t| (t - at(0)).num_hours()),
                    s.unseen_end,
                )
            })
            .collect()
    }

    /// A history that ends in the lane the card is in, but before its stay
    /// there began, lost the lines that took it out and back: the card's
    /// own stay starts at `lane_since`, the old one's end is unseen and
    /// counts no time, and the span between is no lane's. A Done card
    /// done again is done from its last arrival.
    #[test]
    fn a_return_the_record_did_not_see_starts_the_stay_where_the_card_says() {
        let task = card("in_progress", Column::InProgress, 7);
        let history = vec![created(&task, "ready"), pulled(&task, 2, "in_progress")];
        let times = card_times(&task, &history, true, at(8));
        assert_eq!(
            hours(&times),
            vec![
                ("ready", 0, Some(2), false),
                ("in_progress", 2, Some(7), true),
                ("in_progress", 7, None, false),
            ]
        );
        assert_eq!(times.in_lane(at(8)), Some(H), "the authoritative hour");
        assert_eq!(lane_secs(&times, "in_progress"), H);
        assert!(times.incomplete);

        let done = card("done", Column::Done, 7);
        let history = vec![created(&done, "ready"), moved(&done, 2, "ready", "done")];
        let times = card_times(&done, &history, true, at(8));
        assert_eq!(times.done_at, Some(at(7)));
        assert!(times.incomplete);
    }

    /// An archived card's last stay is the lane it was archived from, from
    /// `lane_since` to `archived_at`, whatever the record kept: none of
    /// it, or only its first line.
    #[test]
    fn an_archived_card_keeps_its_last_lane() {
        let mut task = card("done", Column::Done, 3);
        task.archived_at = Some(at(5));
        let bare = card_times(&task, &[], true, at(9));
        assert_eq!(hours(&bare), vec![("done", 3, Some(5), false)]);
        assert_eq!(bare.done_at, Some(at(3)));
        assert_eq!(lane_secs(&bare, "done"), 2 * H);
        assert!(bare.incomplete, "nothing shows where it was before 3");

        let first = card_times(&task, &[created(&task, "ready")], true, at(9));
        assert_eq!(
            hours(&first),
            vec![("ready", 0, Some(3), true), ("done", 3, Some(5), false)]
        );
        assert_eq!(first.done_at, Some(at(3)));
        assert_eq!(lane_secs(&first, "ready"), 0);
        assert!(first.current().is_none());
    }

    /// A history read only in its newest lines says so: incomplete, no
    /// first pull, so no pull to done, its stays still from what it read.
    #[test]
    fn a_history_cut_to_its_newest_lines_claims_no_first_pull() {
        let task = card("done", Column::Done, 9);
        let history = vec![
            pulled(&task, 4, "in_progress"),
            moved(&task, 9, "in_progress", "done"),
        ];
        let times = card_times(&task, &history, false, at(10));
        assert!(times.incomplete);
        assert_eq!(times.pulled_at, None);
        assert_eq!(times.pull_to_done(), None);
        assert_eq!(times.done_at, Some(at(9)));
        assert_eq!(lane_secs(&times, "in_progress"), 5 * H);
    }

    /// A move out of a lane the record does not have the card in shows
    /// that lines between were lost: the stay the record had ends unseen
    /// at that move, counting no time and no leaving, and the span the lost
    /// lines covered is no lane's. A pull's `from` is the holder it took
    /// the card over from, not a lane, so it shows no gap.
    #[test]
    fn a_move_from_another_lane_than_the_record_has_leaves_its_stay_unseen() {
        let task = card("done", Column::Done, 4);
        let history = vec![
            created(&task, "ready"),
            moved(&task, 1, "ready", "review"),
            moved(&task, 4, "in_progress", "done"),
        ];
        let times = card_times(&task, &history, true, at(10));
        assert_eq!(
            hours(&times),
            vec![
                ("ready", 0, Some(1), false),
                ("review", 1, Some(4), true),
                ("done", 4, None, false),
            ]
        );
        assert!(times.incomplete);
        assert_eq!(lane_secs(&times, "review"), 0, "when it left is not known");
        assert_eq!(times.finished_in(&board(), Column::Review), None);
        assert_eq!(times.done_at, Some(at(4)));

        // A take-over names the lapsed holder in `from`: nothing is lost.
        let taken = card("review", Column::Review, 3);
        let history = vec![
            created(&taken, "ready"),
            entry(
                &taken,
                1,
                TaskAction::Pulled {
                    lane: "in_progress".into(),
                    from: Some("agent-0".into()),
                },
            ),
            moved(&taken, 3, "in_progress", "review"),
        ];
        let times = card_times(&taken, &history, true, at(5));
        assert!(!times.incomplete);
        assert_eq!(times.pulled_at, Some(at(1)));
        assert_eq!(lane_secs(&times, "in_progress"), 2 * H);

        // An unarchive while the record has the card on the board, and a
        // move while it has it off: an archive and a return were lost.
        let back = card("ready", Column::Ready, 0);
        let history = vec![
            created(&back, "ready"),
            entry(&back, 5, TaskAction::Unarchived),
        ];
        let times = card_times(&back, &history, true, at(6));
        assert_eq!(
            hours(&times),
            vec![("ready", 0, Some(5), true), ("ready", 5, None, false)]
        );
        assert!(times.incomplete && times.stays[1].resumed);
        let moved_off = card("review", Column::Review, 6);
        let history = vec![
            created(&moved_off, "ready"),
            moved(&moved_off, 2, "ready", "done"),
            entry(&moved_off, 3, TaskAction::Archived),
            moved(&moved_off, 6, "done", "review"),
        ];
        let times = card_times(&moved_off, &history, true, at(7));
        assert_eq!(
            hours(&times),
            vec![
                ("ready", 0, Some(2), false),
                ("done", 2, Some(3), false),
                ("review", 6, None, false),
            ]
        );
        assert!(times.incomplete);
    }

    /// The first pull the record shows is claimed only when every line
    /// from the card's creation through it is there: a history whose first
    /// lines are gone, though the store read all it had, or one with a
    /// line missing before the pull, claims none, and the stays it saw
    /// still count. The board's numbers leave such a card out of pull to
    /// done.
    #[test]
    fn a_first_pull_needs_every_line_from_the_cards_creation() {
        let mut task = card("done", Column::Done, 9);
        task.assignee = Some(AgentId::from("agent-1"));
        let history = vec![
            pulled(&task, 4, "in_progress"),
            moved(&task, 9, "in_progress", "done"),
        ];
        let times = card_times(&task, &history, true, at(10));
        assert!(times.incomplete);
        assert_eq!(times.pulled_at, None);
        assert_eq!(times.pull_to_done(), None);
        assert_eq!(times.done_at, Some(at(9)));
        assert_eq!(lane_secs(&times, "in_progress"), 5 * H);
        let stats = Stats::of(&board(), [(&task, &times)]);
        assert_eq!((stats.done, stats.pull_to_done), (1, None));
        assert_eq!(stats.holders[0].pull_to_done, None);

        let gap = card("done", Column::Done, 9);
        let history = vec![
            created(&gap, "ready"),
            moved(&gap, 1, "backlog", "ready"),
            pulled(&gap, 3, "in_progress"),
            moved(&gap, 9, "in_progress", "done"),
        ];
        let times = card_times(&gap, &history, true, at(10));
        assert!(times.incomplete);
        assert_eq!(times.pulled_at, None, "a line before the pull is missing");
        assert_eq!(lane_secs(&times, "ready"), 2 * H);
        assert_eq!(lane_secs(&times, "in_progress"), 6 * H);
    }

    /// The archive the record ends with is where the stay it saw ended. A
    /// card archived again since, its return and new archive unseen, keeps
    /// that stay as it ended; the time between is no lane's but the card's
    /// own stay from `lane_since` when its lane got it after the record.
    /// One back on the board with no line saying so goes on in its lane
    /// from no earlier than that archive, its start unseen: it counts no
    /// time, says no time in lane, and was done when it came into Done.
    #[test]
    fn an_archive_the_record_shows_is_never_stretched_to_a_later_one() {
        let history = |task: &Task| {
            vec![
                created(task, "ready"),
                moved(task, 3, "ready", "done"),
                entry(task, 5, TaskAction::Archived),
            ]
        };
        let mut again = card("done", Column::Done, 3);
        again.archived_at = Some(at(12));
        let times = card_times(&again, &history(&again), true, at(20));
        assert_eq!(
            hours(&times),
            vec![("ready", 0, Some(3), false), ("done", 3, Some(5), false)]
        );
        assert_eq!(lane_secs(&times, "done"), 2 * H);
        assert!(times.incomplete);
        assert_eq!(times.done_at, Some(at(3)));

        let mut elsewhere = card("review", Column::Review, 8);
        elsewhere.archived_at = Some(at(12));
        let times = card_times(&elsewhere, &history(&elsewhere), true, at(20));
        assert_eq!(
            times.stays.last().map(|s| (s.from, s.to)),
            Some((at(8), Some(at(12))))
        );
        assert_eq!(lane_secs(&times, "review"), 4 * H);
        assert_eq!(lane_secs(&times, "done"), 2 * H);
        assert!(times.incomplete);

        let back = card("done", Column::Done, 3);
        let times = card_times(&back, &history(&back), true, at(10));
        assert_eq!(
            hours(&times),
            vec![
                ("ready", 0, Some(3), false),
                ("done", 3, Some(5), false),
                ("done", 5, None, false),
            ]
        );
        let current = times.current().unwrap();
        assert!(current.unseen_start && current.resumed);
        assert_eq!(
            times.in_lane(at(10)),
            None,
            "when it came back is not known"
        );
        assert_eq!(lane_secs(&times, "done"), 2 * H);
        assert_eq!(times.done_at, Some(at(3)), "an unarchive is no completion");
        assert!(times.incomplete);
        let json = serde_json::to_value(&times).unwrap();
        assert_eq!(json["stays"][2]["unseen_start"], true);
        assert!(json["stays"][1].get("unseen_start").is_none());
    }

    /// A list carries a card's current stay alone, the rest counted, and
    /// its lanes say what the board's numbers need; a detail its newest.
    #[test]
    fn a_summary_carries_one_stay_and_the_lanes() {
        let task = card("done", Column::Done, 9);
        let history = vec![
            created(&task, "ready"),
            pulled(&task, 2, "in_progress"),
            moved(&task, 5, "in_progress", "review"),
            moved(&task, 9, "review", "done"),
        ];
        let whole = card_times(&task, &history, true, at(10));
        let summary = whole.summary();
        assert_eq!(summary.stays, whole.stays[3..]);
        assert_eq!(summary.stays_omitted, 3);
        assert_eq!(summary.in_lane(at(10)), whole.in_lane(at(10)));
        assert_eq!(summary.pull_to_done(), Some(7 * H));
        assert_eq!(
            summary.finished_in(&board(), Column::Review),
            Some(4 * H),
            "from the lanes, not the stays"
        );
        let newest = whole.newest(2);
        assert_eq!((newest.stays.len(), newest.stays_omitted), (2, 2));
        assert_eq!(newest.newest(1).stays_omitted, 3);
        assert!(!summary.incomplete);

        // A card that went through more lanes than a board holds keeps
        // the most a board holds, the one it is in among them.
        let many = card("lane-30", Column::InProgress, 30);
        let mut history = vec![created(&many, "lane-0")];
        for n in 1..=30 {
            history.push(moved(
                &many,
                n,
                &format!("lane-{}", n - 1),
                &format!("lane-{n}"),
            ));
        }
        let summary = card_times(&many, &history, true, at(31)).summary();
        assert_eq!(summary.lanes.len(), crate::board::LANES_MAX);
        assert!(
            summary
                .lanes
                .iter()
                .any(|l| l.current && l.lane == "lane-30")
        );
        assert!(summary.incomplete);
    }

    /// A card whose record is incomplete is counted as such, and left out
    /// of the Review median.
    #[test]
    fn the_boards_numbers_count_incomplete_cards_apart() {
        let board = board();
        let whole = card("done", Column::Done, 4);
        let whole_history = vec![
            created(&whole, "ready"),
            moved(&whole, 1, "ready", "review"),
            moved(&whole, 4, "review", "done"),
        ];
        let partial = card("done", Column::Done, 9);
        let partial_history = vec![
            moved(&partial, 2, "ready", "review"),
            moved(&partial, 9, "review", "done"),
        ];
        let folded = [
            (
                whole.clone(),
                card_times(&whole, &whole_history, true, at(10)),
            ),
            (
                partial.clone(),
                card_times(&partial, &partial_history, true, at(10)),
            ),
        ];
        let stats = Stats::of(&board, folded.iter().map(|(t, c)| (t, c)));
        assert_eq!(stats.done, 2);
        assert_eq!(stats.incomplete, 1);
        assert_eq!(stats.in_review, Some(3 * H), "the complete card's alone");
    }

    /// The wire form: lanes by slug with their seconds, stays and whether
    /// the card is there, an open stay without `to`.
    #[test]
    fn the_wire_form_is_lanes_and_stays() {
        let task = card("in_progress", Column::InProgress, 2);
        let history = vec![created(&task, "ready"), pulled(&task, 2, "in_progress")];
        let times = card_times(&task, &history, true, at(3));
        let json = serde_json::to_value(&times).unwrap();
        assert_eq!(json["lanes"][0]["lane"], "ready");
        assert_eq!(json["lanes"][0]["seconds"], 2 * H);
        assert!(json["lanes"][0].get("current").is_none());
        assert_eq!(json["lanes"][1]["current"], true);
        assert!(json["stays"][1].get("to").is_none());
        assert!(json.get("done_at").is_none());
        let back: CardTimes = serde_json::from_value(json).unwrap();
        assert_eq!(back, times);
    }
}
