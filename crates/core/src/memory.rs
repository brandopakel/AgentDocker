//! A project's memory: short facts its agents and the person keep for each
//! other, kept on this machine. A memory is added on purpose, never taken
//! from a journal, a message or a transcript, so nothing reaches it that
//! nobody chose to keep. One that is wrong is superseded by a newer one,
//! or forgotten for good.
//!
//! The shape follows Supermemory's (add, search, profile, forget, scoped to
//! one project), so a mirror to it can come later without a second model.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{Link, ProjectId};

/// Characters a memory holds at most: a fact, not a document.
pub const MEMORY_CHARS: usize = 2048;
/// Memories a project keeps at most.
pub const MEMORIES_PER_PROJECT: usize = 1000;
/// Links a memory carries at most.
pub const MEMORY_LINKS: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Memory {
    pub id: String,
    pub project: ProjectId,
    /// The agent or person that kept it.
    pub author: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<Link>,
    /// The memory this one replaces: that one is kept, but no longer said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<String>,
    pub created_at: DateTime<Utc>,
    /// Said first, before the recent ones, for as long as it stands: a
    /// convention or a decision rather than news.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub standing: bool,
    /// After this moment it is no longer said, as if forgotten; it is kept
    /// until somebody forgets it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forget_after: Option<DateTime<Utc>>,
}

impl Memory {
    pub fn new(
        project: ProjectId,
        author: &str,
        text: &str,
        links: Vec<Link>,
        supersedes: Option<String>,
        now: DateTime<Utc>,
    ) -> Result<Self, &'static str> {
        let memory = Self {
            id: uuid::Uuid::new_v4().simple().to_string()[..12].to_owned(),
            project,
            author: author.to_owned(),
            text: text.trim().to_owned(),
            links,
            supersedes,
            created_at: now,
            standing: false,
            forget_after: None,
        };
        memory.check()?;
        Ok(memory)
    }

    /// Said first in the profile while it stands.
    pub fn standing(mut self, standing: bool) -> Self {
        self.standing = standing;
        self
    }

    /// No longer said after `when`, which must come after it was kept.
    pub fn forget_after(mut self, when: Option<DateTime<Utc>>) -> Result<Self, &'static str> {
        if when.is_some_and(|when| when <= self.created_at) {
            return Err("a memory is forgotten after it is kept, not before");
        }
        self.forget_after = when;
        Ok(self)
    }

    /// Whether it is still said at `now`: not past its `forget_after`.
    pub fn said_at(&self, now: DateTime<Utc>) -> bool {
        self.forget_after.is_none_or(|when| now < when)
    }

    pub fn check(&self) -> Result<(), &'static str> {
        if self.text.is_empty() || self.text.chars().count() > MEMORY_CHARS {
            return Err("a memory is one to 2048 characters");
        }
        if self.links.len() > MEMORY_LINKS {
            return Err("a memory carries at most 8 links");
        }
        for link in &self.links {
            link.check()?;
        }
        // Every word that is kept is checked, a link's too.
        if std::iter::once(self.text.as_str())
            .chain(
                self.links.iter().flat_map(|link| {
                    std::iter::once(link.target.as_str()).chain(link.note.as_deref())
                }),
            )
            .any(secret_like)
        {
            return Err(
                "that reads like a secret (a key, a token, a private key); a memory is kept in plain text, so keep secrets out of it",
            );
        }
        Ok(())
    }
}

/// Whether `text` holds what looks like a credential: a private key, a
/// well-known token shape, or a long run of mixed letters and digits.
/// A guard, not a guarantee: it catches the common shapes only.
pub fn secret_like(text: &str) -> bool {
    if text.contains("-----BEGIN") && text.contains("PRIVATE KEY") {
        return true;
    }
    // Words as written, so a base64 key with `+ / =` is one; and the same
    // text cut at every character a token never holds, so one inside
    // `token=…`, a URL or a path is a word of its own.
    let written = text.split(|c: char| {
        c.is_whitespace() || matches!(c, '"' | '\'' | '`' | ',' | ';' | '(' | ')')
    });
    let cut = text.split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-')));
    written.chain(cut).any(|word| {
        let word = word.trim_matches(|c: char| !c.is_ascii_alphanumeric());
        let token = |prefix: &str, min: usize| {
            word.strip_prefix(prefix).is_some_and(|rest| {
                rest.len() >= min
                    && rest
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
            })
        };
        token("sk-", 20)
            || token("sk_live_", 16)
            || token("ghp_", 30)
            || token("gho_", 30)
            || token("github_pat_", 30)
            || token("xoxb-", 20)
            || token("xoxp-", 20)
            || token("glpat-", 20)
            || (word.len() == 20
                && word.starts_with("AKIA")
                && word
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()))
            || high_entropy(word)
    })
}

/// A run of 32 or more letters and digits that mixes upper case, lower
/// case and digits, as generated keys do and words never do.
fn high_entropy(word: &str) -> bool {
    word.len() >= 32
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+' | '/' | '='))
        && word.chars().any(|c| c.is_ascii_uppercase())
        && word.chars().any(|c| c.is_ascii_lowercase())
        && word.chars().filter(|c| c.is_ascii_digit()).count() >= 4
}

/// The memories still said at `now`: none another memory supersedes, none
/// past its `forget_after`; newest first.
pub fn current(memories: &[Memory], now: DateTime<Utc>) -> Vec<&Memory> {
    let replaced: std::collections::HashSet<&str> = memories
        .iter()
        .filter_map(|m| m.supersedes.as_deref())
        .collect();
    let mut kept: Vec<&Memory> = memories
        .iter()
        .filter(|m| !replaced.contains(m.id.as_str()) && m.said_at(now))
        .collect();
    kept.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(a.id.cmp(&b.id)));
    kept
}

/// The words of a text as search reads them: lower case, split at
/// anything but a letter, a digit or `_`, so an identifier such as
/// `task_id` is one word, and `latest` holds no `test`.
fn words(text: &str) -> std::collections::HashSet<String> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// The current memories that answer `query`, the best first. A memory
/// answers a word it holds whole (`test` is not in `latest`), and a word
/// few memories hold counts for more than one most of them hold; a newer
/// memory wins a tie, and between two kept at the same moment the lower
/// id comes first, so equal evidence always comes back in one order. An
/// empty query answers the newest.
pub fn search<'a>(
    memories: &'a [Memory],
    query: &str,
    limit: usize,
    now: DateTime<Utc>,
) -> Vec<&'a Memory> {
    let asked = words(query);
    let said = current(memories, now);
    let held: Vec<std::collections::HashSet<String>> =
        said.iter().map(|m| words(&m.text)).collect();
    let total = said.len() as f64;
    // Inverse document frequency, as BM25 has it: never negative, highest
    // for a word one memory holds.
    let weight = |word: &String| {
        let holding = held.iter().filter(|words| words.contains(word)).count() as f64;
        (1.0 + (total - holding + 0.5) / (holding + 0.5)).ln()
    };
    let weights: Vec<(&String, f64)> = asked.iter().map(|w| (w, weight(w))).collect();
    let mut scored: Vec<(f64, &Memory)> = said
        .into_iter()
        .zip(&held)
        .map(|(memory, words)| {
            // Summed in one fixed order, so equal evidence is an equal
            // score and the newer memory wins the tie.
            let mut held: Vec<f64> = weights
                .iter()
                .filter(|(word, _)| words.contains(*word))
                .map(|(_, weight)| *weight)
                .collect();
            held.sort_by(f64::total_cmp);
            (held.into_iter().sum::<f64>(), memory)
        })
        .filter(|(score, _)| asked.is_empty() || *score > 0.0)
        .collect();
    scored.sort_by(|a, b| {
        b.0.total_cmp(&a.0)
            .then(b.1.created_at.cmp(&a.1.created_at))
            .then(a.1.id.cmp(&b.1.id))
    });
    scored.into_iter().take(limit).map(|(_, m)| m).collect()
}

/// What a project's agents should know first, as many as fit in
/// `max_chars`: its standing memories, the oldest first, then the others,
/// newest first.
pub fn profile(memories: &[Memory], max_chars: usize, now: DateTime<Utc>) -> Vec<&Memory> {
    let said = current(memories, now);
    let (mut standing, recent): (Vec<&Memory>, Vec<&Memory>) =
        said.into_iter().partition(|m| m.standing);
    // Oldest first; two kept at the same moment by id, as everywhere else.
    standing.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
    let mut used = 0;
    standing
        .into_iter()
        .chain(recent)
        // One too long for what is left is passed over, not the end: the
        // shorter ones after it still fit.
        .filter(|memory| {
            let size = memory.text.chars().count() + 1;
            let fits = used + size <= max_chars;
            if fits {
                used += size;
            }
            fits
        })
        .collect()
}

/// What a copy of a project's memory elsewhere must do to hold exactly its
/// current memories, given the ids it holds: the memories to add, and the
/// ids to take out — forgotten, past their `forget_after`, or superseded
/// by a newer one. A memory
/// never changes once kept, so its id says everything.
pub fn mirror_plan<'a>(
    memories: &'a [Memory],
    mirrored: &std::collections::BTreeSet<String>,
    now: DateTime<Utc>,
) -> (Vec<&'a Memory>, Vec<String>) {
    let said = current(memories, now);
    let wanted: std::collections::BTreeSet<&str> = said.iter().map(|m| m.id.as_str()).collect();
    let add = said
        .into_iter()
        .filter(|m| !mirrored.contains(&m.id))
        .collect();
    let remove = mirrored
        .iter()
        .filter(|id| !wanted.contains(id.as_str()))
        .cloned()
        .collect();
    (add, remove)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn memory(id: &str, text: &str, minutes: i64, supersedes: Option<&str>) -> Memory {
        Memory {
            id: id.into(),
            project: ProjectId::from("p"),
            author: "a".into(),
            text: text.into(),
            links: vec![],
            supersedes: supersedes.map(str::to_owned),
            created_at: DateTime::from_timestamp(1_700_000_000, 0).unwrap()
                + Duration::minutes(minutes),
            standing: false,
            forget_after: None,
        }
    }

    /// A moment after every memory these tests keep.
    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).unwrap() + Duration::days(1)
    }

    #[test]
    fn a_memory_is_a_short_fact_and_never_a_secret() {
        let now = Utc::now();
        let project = ProjectId::from("p");
        assert!(
            Memory::new(
                project.clone(),
                "a",
                "  Tests run with nextest  ",
                vec![],
                None,
                now
            )
            .is_ok()
        );
        assert!(Memory::new(project.clone(), "a", "   ", vec![], None, now).is_err());
        assert!(Memory::new(project.clone(), "a", &"x".repeat(2049), vec![], None, now).is_err());
        for secret in [
            "the key is sk-abcdefghijklmnopqrstuvwx",
            "token ghp_abcdefghijklmnopqrstuvwxyz0123456789",
            "AKIAABCDEFGHIJKLMNOP is the access key",
            "-----BEGIN OPENSSH PRIVATE KEY-----\nabc",
            "x9Fk2LmQ7vR3tY8wZ1aB4cD6eG0hJ5nP",
        ] {
            assert!(
                Memory::new(project.clone(), "a", secret, vec![], None, now).is_err(),
                "{secret}"
            );
        }
        assert!(!secret_like(
            "Use the reviewer role for every PR; Codex reviews, Claude implements"
        ));
        assert!(!secret_like(
            "the build cache lives in target/debug and is 4 GiB"
        ));
    }

    #[test]
    fn a_secret_inside_an_assignment_a_url_or_a_link_is_refused() {
        let now = Utc::now();
        let project = ProjectId::from("p");
        for text in [
            "token=ghp_abcdefghijklmnopqrstuvwxyz0123456789",
            "see https://example.invalid/?token=ghp_abcdefghijklmnopqrstuvwxyz0123456789",
            "path /keys/sk-abcdefghijklmnopqrstuvwx/x",
        ] {
            assert!(
                Memory::new(project.clone(), "a", text, vec![], None, now).is_err(),
                "{text}"
            );
        }
        let mut link = Link::new(crate::LinkKind::Memory, "sk-abcdefghijklmnopqrstuvwx");
        assert!(
            Memory::new(project.clone(), "a", "fine", vec![link.clone()], None, now).is_err(),
            "a link's target"
        );
        link.target = "notes".into();
        link.note = Some("ghp_abcdefghijklmnopqrstuvwxyz0123456789".into());
        assert!(
            Memory::new(project.clone(), "a", "fine", vec![link], None, now).is_err(),
            "a link's note"
        );
        assert!(!secret_like(
            "set RUST_LOG=debug and see https://docs.rs/tokio"
        ));
    }

    #[test]
    fn a_superseded_memory_is_kept_but_no_longer_said() {
        let all = vec![
            memory("old", "deploys go out on Fridays", 0, None),
            memory("new", "deploys go out on Mondays", 5, Some("old")),
            memory("other", "the reviewer is Codex", 3, None),
        ];
        let ids: Vec<&str> = current(&all, now()).iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["new", "other"], "newest first, the replaced one gone");
    }

    #[test]
    fn search_ranks_by_words_held_then_by_recency() {
        let all = vec![
            memory("a", "the reviewer reviews every PR", 0, None),
            memory("b", "the release reviewer signs the build", 5, None),
            memory("c", "nothing about it", 9, None),
        ];
        let ids = |q: &str| -> Vec<String> {
            search(&all, q, 10, now())
                .iter()
                .map(|m| m.id.clone())
                .collect()
        };
        assert_eq!(ids("release reviewer"), ["b", "a"]);
        assert_eq!(ids("REVIEWER"), ["b", "a"], "case is not a difference");
        assert_eq!(ids("absent"), Vec::<String>::new());
        assert_eq!(ids(""), ["c", "b", "a"], "no words, the newest");
        assert_eq!(search(&all, "", 1, now()).len(), 1);
    }

    #[test]
    fn a_mirror_adds_what_it_lacks_and_drops_what_is_no_longer_said() {
        let all = vec![
            memory("old", "deploys go out on Fridays", 0, None),
            memory("new", "deploys go out on Mondays", 5, Some("old")),
            memory("kept", "the reviewer is a role", 3, None),
        ];
        let mirrored: std::collections::BTreeSet<String> =
            ["old", "kept", "forgotten"].map(str::to_owned).into();
        let (add, remove) = mirror_plan(&all, &mirrored, now());
        let added: Vec<&str> = add.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(added, ["new"]);
        assert_eq!(remove, ["forgotten", "old"]);
        let (add, remove) = mirror_plan(&all, &["new", "kept"].map(str::to_owned).into(), now());
        assert!(add.is_empty() && remove.is_empty(), "in step");
    }

    #[test]
    fn a_profile_is_the_newest_that_fit() {
        let all = vec![
            memory("a", &"a".repeat(10), 0, None),
            memory("b", &"b".repeat(10), 1, None),
            memory("c", &"c".repeat(10), 2, None),
        ];
        let ids: Vec<&str> = profile(&all, 22, now())
            .iter()
            .map(|m| m.id.as_str())
            .collect();
        assert_eq!(ids, ["c", "b"]);
        assert!(profile(&all, 5, now()).is_empty());
        let mut long = all.clone();
        long.push(memory("big", &"z".repeat(900), 3, None));
        let ids: Vec<&str> = profile(&long, 22, now())
            .iter()
            .map(|m| m.id.as_str())
            .collect();
        assert_eq!(
            ids,
            ["c", "b"],
            "a long newest one is passed over, not the end"
        );
    }

    #[test]
    fn search_matches_whole_words_and_weighs_the_rare_ones() {
        let all = vec![
            memory("both", "alpha beta", 0, None),
            memory("rare", "gamma beta", 1, None),
            memory("older", "alpha", 2, None),
            memory("newer", "alpha", 3, None),
            memory("latest", "the latest build", 4, None),
        ];
        let ids = |q: &str| -> Vec<String> {
            search(&all, q, 10, now())
                .iter()
                .map(|m| m.id.clone())
                .collect()
        };
        assert_eq!(
            ids("alpha beta"),
            ["both", "rare", "newer", "older"],
            "beta, which fewer hold, outweighs alpha; a tie goes to the newer"
        );
        assert_eq!(ids("test"), Vec::<String>::new(), "not inside another word");
        assert_eq!(ids("latest"), ["latest"]);
        assert_eq!(
            ids("Beta, ALPHA!"),
            ids("alpha beta"),
            "case and marks aside"
        );
    }

    #[test]
    fn equal_evidence_is_an_equal_score_and_the_newer_wins() {
        let all = vec![
            memory("older", "a b common", 0, None),
            memory("newer", "c d common", 1, None),
            memory("x", "unrelated one", 2, None),
            memory("y", "unrelated two", 3, None),
        ];
        // Whatever order the query's words are held in, the sums match.
        for query in ["a b c d common", "common d c b a", "b common a d c"] {
            for _ in 0..20 {
                let ids: Vec<&str> = search(&all, query, 10, now())
                    .iter()
                    .map(|m| m.id.as_str())
                    .collect();
                assert_eq!(ids, ["newer", "older"], "{query}");
            }
        }
    }

    #[test]
    fn standing_memories_come_first_the_oldest_of_them_first() {
        let mut all = vec![
            memory("rule", "reviews go through the gate", 0, None),
            memory("news", "the build is slow today", 1, None),
            memory("later rule", "commits carry no trailers", 2, None),
            memory("latest", "the cache was cleared", 3, None),
        ];
        all[0].standing = true;
        all[2].standing = true;
        let ids: Vec<&str> = profile(&all, 4000, now())
            .iter()
            .map(|m| m.id.as_str())
            .collect();
        assert_eq!(ids, ["rule", "later rule", "latest", "news"]);
        let ids: Vec<&str> = profile(&all, 30, now())
            .iter()
            .map(|m| m.id.as_str())
            .collect();
        assert_eq!(ids, ["rule"], "the standing ones claim the room first");
    }

    #[test]
    fn a_word_is_cut_at_marks_but_an_identifier_stays_whole() {
        let all = vec![
            memory("field", "the task_id field names the card", 0, None),
            memory("cargo", "hold task:local-cargo-campaign first", 1, None),
            memory("tests", "testing happens in CI", 2, None),
            memory("accent", "L'ÉTÉ est long", 3, None),
        ];
        let ids = |q: &str| -> Vec<String> {
            search(&all, q, 10, now())
                .iter()
                .map(|m| m.id.clone())
                .collect()
        };
        assert_eq!(ids("task"), ["cargo"], "`:` and `-` cut, `_` does not");
        assert_eq!(ids("id"), Vec::<String>::new(), "not inside `task_id`");
        assert_eq!(ids("task_id"), ["field"], "an identifier is one word");
        assert_eq!(ids("TASK_ID"), ["field"]);
        assert_eq!(ids("campaign"), ["cargo"]);
        assert_eq!(ids("test"), Vec::<String>::new(), "not inside `testing`");
        assert_eq!(ids("ask"), Vec::<String>::new(), "not inside `task`");
        assert_eq!(ids("été"), ["accent"], "case folds beyond ASCII");
    }

    #[test]
    fn equal_scores_kept_at_the_same_moment_go_by_id() {
        let same = |id: &str, text: &str| memory(id, text, 7, None);
        let forward = vec![
            same("b2", "deploy on monday"),
            same("a1", "deploy on monday"),
            same("c3", "deploy on monday"),
            memory("newer", "deploy on friday", 8, None),
            memory("other", "unrelated", 0, None),
        ];
        let mut backward = forward.clone();
        backward.reverse();
        for all in [&forward, &backward] {
            let ids: Vec<&str> = search(all, "deploy", 10, now())
                .iter()
                .map(|m| m.id.as_str())
                .collect();
            assert_eq!(ids, ["newer", "a1", "b2", "c3"], "newer, then by id");
            let ids: Vec<&str> = search(all, "monday deploy", 10, now())
                .iter()
                .map(|m| m.id.as_str())
                .collect();
            assert_eq!(ids, ["a1", "b2", "c3", "newer"], "score, then by id");
        }
    }

    #[test]
    fn a_profile_says_standing_then_recent_in_one_order_whatever_was_stored() {
        let mut all = vec![
            memory("s-b", "standing kept with another", 2, None),
            memory("r-old", "older news", 1, None),
            memory("s-a", "standing kept with another", 2, None),
            memory("s-first", "the first convention", 0, None),
            memory("r-b", "news at one moment", 5, None),
            memory("r-a", "news at one moment", 5, None),
        ];
        for m in all.iter_mut().filter(|m| m.id.starts_with("s-")) {
            m.standing = true;
        }
        let mut backward = all.clone();
        backward.reverse();
        for stored in [&all, &backward] {
            let ids: Vec<&str> = profile(stored, 4000, now())
                .iter()
                .map(|m| m.id.as_str())
                .collect();
            assert_eq!(ids, ["s-first", "s-a", "s-b", "r-a", "r-b", "r-old"]);
        }
    }

    #[test]
    fn a_memory_past_its_forget_after_is_no_longer_said() {
        let mut all = vec![
            memory("kept", "the reviewer is a role", 0, None),
            memory("brief", "the main branch is frozen", 1, None),
        ];
        all[1].forget_after = Some(now() - Duration::minutes(1));
        let said: Vec<&str> = current(&all, now()).iter().map(|m| m.id.as_str()).collect();
        assert_eq!(said, ["kept"]);
        assert!(search(&all, "frozen", 10, now()).is_empty());
        assert_eq!(profile(&all, 4000, now()).len(), 1);
        let (_, remove) = mirror_plan(&all, &["kept", "brief"].map(str::to_owned).into(), now());
        assert_eq!(remove, ["brief"], "a mirror lets it go too");
        let before = now() - Duration::minutes(2);
        assert_eq!(current(&all, before).len(), 2, "said until then");

        let kept = Memory::new(ProjectId::from("p"), "a", "frozen", vec![], None, now()).unwrap();
        assert!(
            kept.clone().forget_after(Some(now())).is_err(),
            "not when it is kept"
        );
        let later = kept.forget_after(Some(now() + Duration::hours(1))).unwrap();
        assert!(later.said_at(now()) && !later.said_at(now() + Duration::hours(1)));
    }
}
