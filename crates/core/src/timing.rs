//! Time on cards: a card's history folded into the stays it made in each
//! lane, the time each lane has had of it, and a board's numbers over a
//! range of cards — how many are done, how long a card takes from its
//! pull to Done, how long Review keeps one. Pure: the caller says what
//! time it is, so a card still in its lane counts up to `now`.
//!
//! The history is the record (`TaskEntry`: created, pulled, moved,
//! archived, unarchived); the card's own `lane_since` and `created_at`
//! close the gaps a history cannot — a card from before the history was
//! kept, or one whose lines were lost — so the last stay always ends
//! where the card is now.
use crate::board::Board;
use crate::task::{Task, TaskAction, TaskEntry, TaskId};
use crate::{AgentId, Column};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// One stay of a card in a lane: from when it came in until it left,
/// or open (`to` absent) while it is still there.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stay {
    /// The lane, by slug.
    pub lane: String,
    pub from: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<DateTime<Utc>>,
}

impl Stay {
    /// How long it lasted, or has lasted so far at `now`.
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
    /// Over every stay, the open one counted up to `now`.
    pub seconds: u64,
    /// How many times it came into the lane.
    pub stays: u32,
    /// The card is there now.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub current: bool,
}

/// A card's time on the board: its stays in order, the time each lane
/// has had of it, and the two moments the board's numbers are made of.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardTimes {
    pub task: TaskId,
    /// Oldest first; the last is open while the card is on the board.
    pub stays: Vec<Stay>,
    /// In the order the card first came into each lane.
    pub lanes: Vec<LaneTime>,
    /// Its first pull, when an agent ever pulled it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pulled_at: Option<DateTime<Utc>>,
    /// When it came into the Done lane it is in; absent unless it is done.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done_at: Option<DateTime<Utc>>,
}

impl CardTimes {
    /// The stay the card is in now, while it is on the board.
    pub fn current(&self) -> Option<&Stay> {
        self.stays.last().filter(|s| s.to.is_none())
    }

    /// How long the card has been in its lane, at `now`.
    pub fn in_lane(&self, now: DateTime<Utc>) -> Option<u64> {
        self.current().map(|s| s.seconds(now))
    }

    /// From its first pull to its arrival in Done, for a card that was
    /// pulled and is done.
    pub fn pull_to_done(&self) -> Option<u64> {
        let pulled = self.pulled_at?;
        let done = self.done_at?;
        Some(u64::try_from((done - pulled).num_seconds()).unwrap_or(0))
    }

    /// Time in lanes of `category` on `board` over stays that have ended:
    /// what the lane kept the card for, once it let it go. None when it
    /// never left such a lane.
    pub fn finished_in(&self, board: &Board, category: Column) -> Option<u64> {
        let mut total = None;
        for stay in &self.stays {
            let Some(to) = stay.to else { continue };
            if board
                .lane(&stay.lane)
                .is_some_and(|l| l.category == category)
            {
                let secs = u64::try_from((to - stay.from).num_seconds()).unwrap_or(0);
                total = Some(total.unwrap_or(0) + secs);
            }
        }
        total
    }
}

/// The lane a card is in, by slug, or its category's name for a card
/// from before lanes.
fn current_lane(task: &Task) -> String {
    task.lane
        .clone()
        .unwrap_or_else(|| task.column.as_str().to_owned())
}

/// Fold a card's history into its stays. Entries are taken in time
/// order whatever order they come in; the card's own fields bound the
/// result: a history that starts after the card (lines lost) opens its
/// first stay at `created_at`, one that ends elsewhere than the card is
/// now is closed at `lane_since` and the card's lane opened from there,
/// and an archived card's last stay ends when it was archived.
pub fn card_times(task: &Task, history: &[TaskEntry], now: DateTime<Utc>) -> CardTimes {
    let mut entries: Vec<&TaskEntry> = history.iter().collect();
    entries.sort_by(|a, b| a.at.cmp(&b.at).then(a.id.cmp(&b.id)));
    let lane_now = current_lane(task);
    let mut stays: Vec<Stay> = Vec::new();
    let mut open: Option<Stay> = None;
    let mut pulled_at = None;
    let close = |open: &mut Option<Stay>, stays: &mut Vec<Stay>, at: DateTime<Utc>| {
        if let Some(mut stay) = open.take() {
            stay.to = Some(at.max(stay.from));
            stays.push(stay);
        }
    };
    // Into a lane at `at`: nothing when the card is there already (a pull
    // of a card in progress), else the stay so far ends and a new one
    // begins. With no stay open, the lane it is said to come `from` has
    // had it since it was filed.
    let enter = |open: &mut Option<Stay>,
                 stays: &mut Vec<Stay>,
                 lane: &str,
                 from: Option<&str>,
                 at: DateTime<Utc>| {
        if open.as_ref().is_some_and(|s| s.lane == lane) {
            return;
        }
        if open.is_none()
            && stays.is_empty()
            && let Some(from) = from.filter(|f| !f.is_empty() && *f != lane)
        {
            *open = Some(Stay {
                lane: from.to_owned(),
                from: task.created_at,
                to: None,
            });
        }
        close(open, stays, at);
        *open = Some(Stay {
            lane: lane.to_owned(),
            from: at,
            to: None,
        });
    };
    for entry in entries {
        match &entry.action {
            TaskAction::Created { lane } => {
                if open.is_none() && stays.is_empty() {
                    let lane = if lane.is_empty() { &lane_now } else { lane };
                    open = Some(Stay {
                        lane: lane.clone(),
                        from: entry.at,
                        to: None,
                    });
                }
            }
            TaskAction::Pulled { lane, from } => {
                pulled_at.get_or_insert(entry.at);
                let lane = if lane.is_empty() { &lane_now } else { lane };
                enter(&mut open, &mut stays, lane, from.as_deref(), entry.at);
            }
            TaskAction::Moved { from, to } => {
                let to = if to.is_empty() { &lane_now } else { to };
                enter(&mut open, &mut stays, to, Some(from.as_str()), entry.at);
            }
            TaskAction::Archived => close(&mut open, &mut stays, entry.at),
            TaskAction::Unarchived => {
                if open.is_none() {
                    let lane = stays
                        .last()
                        .map_or_else(|| lane_now.clone(), |s| s.lane.clone());
                    open = Some(Stay {
                        lane,
                        from: entry.at,
                        to: None,
                    });
                }
            }
            TaskAction::Updated { .. }
            | TaskAction::Assigned { .. }
            | TaskAction::Commented { .. } => {}
        }
    }
    // Where the card is now closes the record.
    match task.archived_at {
        Some(archived) => close(&mut open, &mut stays, archived),
        None => {
            let since = task.in_lane_since();
            if open.as_ref().is_none_or(|s| s.lane != lane_now) {
                close(&mut open, &mut stays, since);
                open = Some(Stay {
                    lane: lane_now.clone(),
                    from: stays.last().map_or(since, |s| s.to.unwrap_or(since)),
                    to: None,
                });
            }
            if let Some(stay) = open.take() {
                stays.push(stay);
            }
        }
    }
    let mut lanes: Vec<LaneTime> = Vec::new();
    for stay in &stays {
        let secs = stay.seconds(now);
        match lanes.iter_mut().find(|l| l.lane == stay.lane) {
            Some(lane) => {
                lane.seconds += secs;
                lane.stays += 1;
                lane.current |= stay.to.is_none();
            }
            None => lanes.push(LaneTime {
                lane: stay.lane.clone(),
                seconds: secs,
                stays: 1,
                current: stay.to.is_none(),
            }),
        }
    }
    let done_at = (task.column == Column::Done)
        .then(|| stays.last().map(|s| s.from))
        .flatten();
    CardTimes {
        task: task.id.clone(),
        stays,
        lanes,
        pulled_at,
        done_at,
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
    /// over the cards that have left Review at least once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_review: Option<u64>,
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
        for (task, times) in cards {
            if let Some(secs) = times.finished_in(board, Column::Review) {
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

    fn pulled(task: &Task, hours: i64, from: &str, lane: &str) -> TaskEntry {
        entry(
            task,
            hours,
            TaskAction::Pulled {
                lane: lane.to_owned(),
                from: Some(from.to_owned()),
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
            pulled(&task, 2, "ready", "in_progress"),
            moved(&task, 5, "in_progress", "review"),
            moved(&task, 6, "review", "in_progress"),
            moved(&task, 8, "in_progress", "review"),
            moved(&task, 9, "review", "done"),
        ];
        let times = card_times(&task, &history, at(20));
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
        let times = card_times(&task, &history, at(10));
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
        let history = vec![
            created(&task, "ready"),
            pulled(&task, 3, "ready", "in_progress"),
        ];
        let early = card_times(&task, &history, at(4));
        let later = card_times(&task, &history, at(10));
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
            pulled(&task, 2, "ready", "in_progress"),
            pulled(&task, 5, "in_progress", "in_progress"),
        ];
        let times = card_times(&task, &history, at(6));
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
            pulled(&task, 2, "ready", "in_progress"),
            created(&task, "ready"),
        ];
        let times = card_times(&task, &history, at(6));
        let lanes: Vec<&str> = times.lanes.iter().map(|l| l.lane.as_str()).collect();
        assert_eq!(lanes, vec!["ready", "in_progress", "review"]);
        assert_eq!(lane_secs(&times, "in_progress"), 3 * H);
    }

    /// A history that begins after the card (its first lines lost) has
    /// the lane the first move came from holding the card since it was
    /// filed; one that ends elsewhere than the card is now is closed at
    /// `lane_since`, where the card's lane opens.
    #[test]
    fn the_card_itself_bounds_a_history_with_gaps() {
        let task = card("review", Column::Review, 7);
        let history = vec![pulled(&task, 2, "ready", "in_progress")];
        let times = card_times(&task, &history, at(8));
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
            vec![
                ("ready", 0, Some(2)),
                ("in_progress", 2, Some(7)),
                ("review", 7, None)
            ]
        );
        assert_eq!(times.in_lane(at(8)), Some(H));

        // No history at all: one stay, from when its lane got it.
        let bare = card_times(&task, &[], at(8));
        assert_eq!(bare.stays.len(), 1);
        assert_eq!(bare.stays[0].lane, "review");
        assert_eq!(bare.stays[0].from, at(7));
        assert_eq!(bare.in_lane(at(8)), Some(H));
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
        let times = card_times(&task, &history, at(20));
        assert_eq!(lane_secs(&times, "done"), 2 * H);
        assert!(times.current().is_none());
        assert_eq!(times.done_at, Some(at(3)));

        let mut back = task.clone();
        back.archived_at = None;
        back.lane_since = Some(at(9));
        let mut history = history;
        history.push(entry(&back, 9, TaskAction::Unarchived));
        let times = card_times(&back, &history, at(10));
        assert_eq!(lane_secs(&times, "done"), 3 * H);
        assert_eq!(times.in_lane(at(10)), Some(H));
        assert_eq!(
            times
                .lanes
                .iter()
                .find(|l| l.lane == "done")
                .map(|l| l.stays),
            Some(2)
        );
    }

    /// The board's numbers: cards done, the median pull to done over the
    /// pulled ones, the median finished time in Review over the cards
    /// that left it, and each holder's own, most done first.
    #[test]
    fn a_boards_numbers_are_medians_over_the_range() {
        let board = board();
        let mut done = Vec::new();
        for (n, hours) in [(1, 2), (2, 4), (3, 12)] {
            let mut task = card("done", Column::Done, 20);
            task.assignee = Some(AgentId::from(if n == 3 { "agent-2" } else { "agent-1" }));
            let history = vec![
                created(&task, "ready"),
                pulled(&task, 1, "ready", "in_progress"),
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
            pulled(&open, 1, "ready", "in_progress"),
            moved(&open, 2, "in_progress", "review"),
            moved(&open, 6, "review", "in_progress"),
        ];
        // One done by hand, never pulled: counted done, no pull to done.
        let hand = card("done", Column::Done, 3);
        let hand_history = vec![created(&hand, "ready"), moved(&hand, 3, "ready", "done")];
        let now = at(30);
        let mut folded: Vec<(Task, CardTimes)> = done
            .iter()
            .map(|(t, h)| (t.clone(), card_times(t, h, now)))
            .collect();
        folded.push((open.clone(), card_times(&open, &open_history, now)));
        folded.push((hand.clone(), card_times(&hand, &hand_history, now)));
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

    /// The wire form: lanes by slug with their seconds, stays and whether
    /// the card is there, an open stay without `to`.
    #[test]
    fn the_wire_form_is_lanes_and_stays() {
        let task = card("in_progress", Column::InProgress, 2);
        let history = vec![
            created(&task, "ready"),
            pulled(&task, 2, "ready", "in_progress"),
        ];
        let times = card_times(&task, &history, at(3));
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
