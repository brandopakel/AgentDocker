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
//! So a seat keeps, for each session it was bound to, what that session
//! left: typed evidence, read from its checkout, that outlives its
//! record. A card passes to the successor only when the checks here all
//! hold; anything else is left where it is, named with why, and kept
//! until it is resolved or the person settles it. Nothing unresolved is
//! ever dropped for want of room.
//!
//! Nothing here reads a checkout or moves a card. The daemon reads and
//! commits; these are the rules it commits by.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::AgentId;

/// The most earlier sessions a seat keeps. At the bound the resolved
/// ones are retired first; with every one unresolved, the member's start
/// is refused before another could be added.
pub const EARLIER: usize = 16;

/// A checkout as it was read: where, on what, and what in it is nowhere
/// else.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkout {
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
    /// Commits on its branch that are not in its target.
    pub unmerged: u32,
    /// Changes that are in no commit.
    pub uncommitted: bool,
    /// Another live session works in the same checkout: said, not
    /// assumed away.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shared_with: Vec<AgentId>,
}

impl Checkout {
    /// Work that is nowhere but here.
    pub fn holds_work(&self) -> bool {
        self.unmerged > 0 || self.uncommitted
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
        checkout: Checkout,
    },
    /// The checkout or its git state could not be read.
    Unreadable {
        why: String,
    },
    /// It worked in no checkout of the project.
    None,
}

/// What an earlier session may still owe.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Obligation {
    /// Cards that still name it and are neither done nor archived.
    Cards,
    /// Work in its checkout that is nowhere else, or a checkout that
    /// could not be read.
    Checkout,
}

/// The person's word that what an earlier session left is dealt with. It
/// is their override, not evidence of a merge: it clears only what it
/// names, of the entry as it stood at the revision it names, and gives
/// leave neither to delete a checkout nor to stop anyone.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settlement {
    pub by: String,
    pub at: DateTime<Utc>,
    pub note: String,
    /// The entry's revision this settles. Evidence read since is newer
    /// than the settlement and is not cleared by it.
    pub revision: u64,
    pub cleared: Vec<Obligation>,
}

/// One session a seat was bound to, and what it left.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Earlier {
    pub agent: AgentId,
    pub name: String,
    pub runtime: String,
    pub ended_at: DateTime<Utc>,
    /// How it ended, in the record's own words.
    pub how: String,
    pub observed: Observed,
    /// Counted from one, and raised each time the evidence is read anew.
    pub revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled: Option<Settlement>,
}

impl Earlier {
    /// Whether the person settled `obligation` for the evidence as it
    /// stands now.
    fn cleared(&self, obligation: Obligation) -> bool {
        self.settled.as_ref().is_some_and(|settlement| {
            settlement.revision == self.revision && settlement.cleared.contains(&obligation)
        })
    }

    /// Its checkout still owes: work that is nowhere else, or a state
    /// nobody could read, which is not known to be nothing.
    pub fn owes_checkout(&self) -> bool {
        let unsettled = match &self.observed {
            Observed::Pending | Observed::Unreadable { .. } => true,
            Observed::Read { checkout } => checkout.holds_work(),
            Observed::None => false,
        };
        unsettled && !self.cleared(Obligation::Checkout)
    }

    /// What it still owes. `names_cards` is whether any card that is
    /// neither done nor archived still names it, which the board says.
    pub fn owes(&self, names_cards: bool) -> Vec<Obligation> {
        let mut owed = Vec::new();
        if names_cards && !self.cleared(Obligation::Cards) {
            owed.push(Obligation::Cards);
        }
        if self.owes_checkout() {
            owed.push(Obligation::Checkout);
        }
        owed
    }

    /// The evidence was read anew: a settlement of what was read before
    /// does not cover it.
    pub fn observe(&mut self, observed: Observed) {
        if self.observed != observed {
            self.observed = observed;
            self.revision += 1;
        }
    }
}

/// Why a member cannot be given another session yet.
pub const EARLIER_FULL: &str = "sixteen earlier sessions of this member each still have work unsettled; settle one (`agentdocker team settle`) before it is started again";

/// Make room in a seat's earlier sessions for one more. `resolved` says,
/// for each entry, that it owes nothing: those are retired first, the
/// oldest first, and only as many as are needed. With every entry
/// unresolved at the bound, nothing is retired and the start is refused:
/// nothing unresolved is cut off.
pub fn make_room(earlier: &mut Vec<Earlier>, resolved: &[bool]) -> Result<(), &'static str> {
    let mut excess = (earlier.len() + 1).saturating_sub(EARLIER);
    let retirable = resolved
        .iter()
        .take(earlier.len())
        .filter(|resolved| **resolved)
        .count();
    if retirable < excess {
        return Err(EARLIER_FULL);
    }
    let mut at = 0;
    earlier.retain(|_| {
        let retire = excess > 0 && resolved.get(at).copied().unwrap_or(false);
        at += 1;
        if retire {
            excess -= 1;
        }
        !retire
    });
    Ok(())
}

/// A card that names an earlier session, as it stands when its hand-over
/// is about to be committed. Every fact is read under the lock that
/// commits it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Card {
    /// It is this project's.
    pub here: bool,
    pub archived: bool,
    pub done: bool,
    /// It is in a lane where work is under way, not waiting or blocked.
    pub working: bool,
    /// Its holder is still that earlier session, and no one else.
    pub still_its: bool,
    /// That session's record is ended and has not been resumed since.
    pub ended: bool,
    /// A live lease stands on the card: the whole claim, not only a hold
    /// that lapsed.
    pub leased: bool,
}

/// Where the earlier session's work on a card is, against the checkout
/// its successor will work in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Work {
    /// Its checkout holds nothing that is nowhere else.
    None,
    /// What it holds is in the checkout the successor is given.
    Here,
    /// It is in another checkout.
    Elsewhere {
        path: String,
        branch: Option<String>,
    },
    /// Its checkout has not been read, or could not be.
    Unknown { why: String },
}

/// The work of `earlier`, against the checkout a successor is given.
pub fn work(earlier: &Earlier, successor_checkout: Option<&str>) -> Work {
    if earlier.cleared(Obligation::Checkout) {
        return Work::None;
    }
    match &earlier.observed {
        Observed::None => Work::None,
        Observed::Pending => Work::Unknown {
            why: "its checkout has not been read yet".into(),
        },
        Observed::Unreadable { why } => Work::Unknown { why: why.clone() },
        Observed::Read { checkout } if !checkout.holds_work() => Work::None,
        Observed::Read { checkout } if Some(checkout.path.as_str()) == successor_checkout => {
            Work::Here
        }
        Observed::Read { checkout } => Work::Elsewhere {
            path: checkout.path.clone(),
            branch: checkout.branch.clone(),
        },
    }
}

/// What becomes of a card that names an earlier session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HandOver {
    /// It passes to the successor.
    Hand,
    /// It is not the successor's, for good: done, archived, another
    /// project's, or given to someone else since.
    Exclude(String),
    /// It is left where it is and does not hold the start up: it is
    /// blocked or waiting, or its holder is running again. It becomes a
    /// candidate when that changes.
    Leave(String),
    /// It should pass and cannot yet: a lease in the way, evidence that
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
    if card.done {
        return HandOver::Exclude("it is done".into());
    }
    if !card.still_its {
        return HandOver::Exclude("it was given to someone else since".into());
    }
    if !card.ended {
        return HandOver::Leave("its earlier session is running again and keeps it".into());
    }
    if !card.working {
        return HandOver::Leave(
            "it is not in a lane where work is under way; it is handed over when it returns to one"
                .into(),
        );
    }
    if card.leased {
        return HandOver::Owe("a live lease stands on it".into());
    }
    match work {
        Work::None | Work::Here => HandOver::Hand,
        Work::Elsewhere { path, branch } => HandOver::Owe(match branch {
            Some(branch) => format!(
                "its earlier session's work is in another checkout, {path} on {branch}, and is not merged"
            ),
            None => format!(
                "its earlier session's work is in another checkout, {path}, and is not merged"
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

    fn checkout(path: &str, unmerged: u32, uncommitted: bool) -> Checkout {
        Checkout {
            path: path.into(),
            branch: Some("team/writer".into()),
            head: Some("abc1234".into()),
            target: Some("main".into()),
            merge_base: Some("def5678".into()),
            unmerged,
            uncommitted,
            shared_with: Vec::new(),
        }
    }

    fn earlier(observed: Observed) -> Earlier {
        Earlier {
            agent: AgentId::from("agent-one"),
            name: "writer-member".into(),
            runtime: "claude-code".into(),
            ended_at: at("2026-01-02T03:04:05Z"),
            how: "exited (0)".into(),
            observed,
            revision: 1,
            settled: None,
        }
    }

    fn read(path: &str, unmerged: u32, uncommitted: bool) -> Observed {
        Observed::Read {
            checkout: checkout(path, unmerged, uncommitted),
        }
    }

    fn settled(revision: u64, cleared: &[Obligation]) -> Settlement {
        Settlement {
            by: "user".into(),
            at: at("2026-01-02T04:04:05Z"),
            note: "merged by hand".into(),
            revision,
            cleared: cleared.to_vec(),
        }
    }

    const SOUND: Card = Card {
        here: true,
        archived: false,
        done: false,
        working: true,
        still_its: true,
        ended: true,
        leased: false,
    };

    /// What a checkout owes: work that is nowhere else, and a state
    /// nobody has read or could read, which is never "nothing".
    #[test]
    fn a_checkout_nobody_read_is_not_a_checkout_with_nothing() {
        let table = [
            (Observed::Pending, true),
            (
                Observed::Unreadable {
                    why: "the folder is gone".into(),
                },
                true,
            ),
            (read("/work/app", 0, false), false),
            (read("/work/app", 2, false), true),
            (read("/work/app", 0, true), true),
            (Observed::None, false),
        ];
        for (observed, owes) in table {
            let entry = earlier(observed.clone());
            assert_eq!(entry.owes_checkout(), owes, "{observed:?}");
            assert_eq!(
                entry.owes(false),
                if owes {
                    vec![Obligation::Checkout]
                } else {
                    Vec::new()
                }
            );
            let mut both = entry.owes(true);
            both.sort();
            assert_eq!(both.first(), Some(&Obligation::Cards), "{observed:?}");
        }
    }

    /// The person's settlement clears what it names, of the evidence it
    /// names, and no more: evidence read anew afterwards is not covered,
    /// and settling the checkout does not settle the cards.
    #[test]
    fn a_settlement_clears_what_it_names_and_no_more() {
        let mut entry = earlier(read("/work/app", 2, true));
        assert_eq!(entry.owes(true), [Obligation::Cards, Obligation::Checkout]);
        entry.settled = Some(settled(1, &[Obligation::Checkout]));
        assert_eq!(entry.owes(true), [Obligation::Cards]);
        assert!(!entry.owes_checkout());
        // The same evidence read again changes nothing.
        entry.observe(read("/work/app", 2, true));
        assert_eq!(entry.revision, 1);
        assert!(!entry.owes_checkout());
        // New evidence is newer than the settlement.
        entry.observe(read("/work/app", 3, true));
        assert_eq!(entry.revision, 2);
        assert!(
            entry.owes_checkout(),
            "what was settled was the earlier state"
        );
        // A settlement that names a revision the entry never had clears
        // nothing.
        entry.settled = Some(settled(7, &[Obligation::Cards, Obligation::Checkout]));
        assert_eq!(entry.owes(true), [Obligation::Cards, Obligation::Checkout]);
        entry.settled = Some(settled(2, &[Obligation::Cards, Obligation::Checkout]));
        assert!(entry.owes(true).is_empty());
    }

    /// Room is made by retiring what is resolved, the oldest first and
    /// no more than needed; with sixteen unresolved nothing is retired
    /// and the start is refused.
    #[test]
    fn nothing_unresolved_is_cut_off_for_want_of_room() {
        let entry = |n: usize| {
            let mut entry = earlier(Observed::None);
            entry.agent = AgentId::from(format!("agent-{n}").as_str());
            entry
        };
        // Below the bound: nothing is retired, resolved or not.
        let mut few: Vec<Earlier> = (0..5).map(entry).collect();
        assert_eq!(make_room(&mut few, &[true; 5]), Ok(()));
        assert_eq!(few.len(), 5);
        // At the bound with two resolved: the older of them goes.
        let mut full: Vec<Earlier> = (0..EARLIER).map(entry).collect();
        let mut resolved = [false; EARLIER];
        resolved[3] = true;
        resolved[9] = true;
        assert_eq!(make_room(&mut full, &resolved), Ok(()));
        assert_eq!(full.len(), EARLIER - 1);
        assert!(!full.iter().any(|e| e.agent.as_str() == "agent-3"));
        assert!(full.iter().any(|e| e.agent.as_str() == "agent-9"));
        // At the bound with none resolved: refused, and all sixteen kept.
        let mut stuck: Vec<Earlier> = (0..EARLIER).map(entry).collect();
        assert_eq!(make_room(&mut stuck, &[false; EARLIER]), Err(EARLIER_FULL));
        assert_eq!(stuck.len(), EARLIER);
        // A flag list shorter than the entries reads as unresolved.
        let mut unknown: Vec<Earlier> = (0..EARLIER).map(entry).collect();
        assert_eq!(make_room(&mut unknown, &[]), Err(EARLIER_FULL));
        assert_eq!(unknown.len(), EARLIER);
    }

    /// Where an earlier session's work is, against the checkout its
    /// successor is given: nothing, here, elsewhere, or not known.
    #[test]
    fn where_the_earlier_work_is() {
        let own = "/work/app/.agentdocker/worktrees/writer";
        assert_eq!(work(&earlier(Observed::None), Some(own)), Work::None);
        assert_eq!(
            work(&earlier(read(own, 0, false)), Some("/work/app")),
            Work::None
        );
        assert_eq!(work(&earlier(read(own, 2, false)), Some(own)), Work::Here);
        assert_eq!(
            work(&earlier(read(own, 2, false)), Some("/work/app")),
            Work::Elsewhere {
                path: own.into(),
                branch: Some("team/writer".into())
            }
        );
        // A successor with no checkout of its own is not "here".
        assert!(matches!(
            work(&earlier(read(own, 0, true)), None),
            Work::Elsewhere { .. }
        ));
        assert!(matches!(
            work(&earlier(Observed::Pending), Some(own)),
            Work::Unknown { .. }
        ));
        assert_eq!(
            work(
                &earlier(Observed::Unreadable {
                    why: "the folder is gone".into()
                }),
                Some(own)
            ),
            Work::Unknown {
                why: "the folder is gone".into()
            }
        );
        // Settled by the person: no longer in the way.
        let mut settled_entry = earlier(read(own, 2, false));
        settled_entry.settled = Some(settled(1, &[Obligation::Checkout]));
        assert_eq!(work(&settled_entry, Some("/work/app")), Work::None);
    }

    /// A card passes only when every check holds. Each failure has its
    /// own outcome: excluded for good, left without holding the start
    /// up, or owed and tried again.
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
                    done: true,
                    ..SOUND
                },
                "done",
            ),
            (
                Card {
                    still_its: false,
                    ..SOUND
                },
                "someone else",
            ),
        ];
        for (card, word) in excluded {
            match hand_over(card, &Work::None) {
                HandOver::Exclude(why) => assert!(why.contains(word), "{why}"),
                other => panic!("{card:?}: {other:?}"),
            }
        }
        let left = [
            (
                Card {
                    ended: false,
                    ..SOUND
                },
                "running again",
            ),
            (
                Card {
                    working: false,
                    ..SOUND
                },
                "returns to one",
            ),
        ];
        for (card, word) in left {
            match hand_over(card, &Work::None) {
                HandOver::Leave(why) => assert!(why.contains(word), "{why}"),
                other => panic!("{card:?}: {other:?}"),
            }
        }
        match hand_over(
            Card {
                leased: true,
                ..SOUND
            },
            &Work::None,
        ) {
            HandOver::Owe(why) => assert!(why.contains("lease"), "{why}"),
            other => panic!("{other:?}"),
        }
        let elsewhere = Work::Elsewhere {
            path: "/work/app/.agentdocker/worktrees/writer".into(),
            branch: Some("team/writer".into()),
        };
        assert_eq!(
            hand_over(SOUND, &elsewhere),
            HandOver::Owe(
                "its earlier session's work is in another checkout, /work/app/.agentdocker/worktrees/writer on team/writer, and is not merged"
                    .into()
            )
        );
        match hand_over(
            SOUND,
            &Work::Unknown {
                why: "the folder is gone".into(),
            },
        ) {
            HandOver::Owe(why) => assert!(why.ends_with("the folder is gone"), "{why}"),
            other => panic!("{other:?}"),
        }
        // What excludes a card for good is said before what would only
        // hold it: a done card under a lease is done.
        assert!(matches!(
            hand_over(
                Card {
                    done: true,
                    leased: true,
                    working: false,
                    ..SOUND
                },
                &elsewhere
            ),
            HandOver::Exclude(_)
        ));
    }

    /// An entry as stored: what is unset is not written, and each state
    /// of the checkout has its word.
    #[test]
    fn an_earlier_session_is_stored_and_read_back() {
        let mut entry = earlier(read("/work/app", 2, true));
        entry.settled = Some(settled(1, &[Obligation::Checkout]));
        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(
            serde_json::from_value::<Earlier>(json.clone()).unwrap(),
            entry
        );
        assert_eq!(json["observed"]["state"], "read");
        assert_eq!(json["observed"]["checkout"]["unmerged"], 2);
        assert!(json["observed"]["checkout"].get("shared_with").is_none());
        assert_eq!(json["settled"]["cleared"], serde_json::json!(["checkout"]));
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
        let bare = serde_json::to_value(earlier(Observed::None)).unwrap();
        assert!(bare.get("settled").is_none());
    }
}
