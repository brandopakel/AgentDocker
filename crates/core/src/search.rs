//! Finding cards by words: a key answers exactly; otherwise every word
//! asked must match a word of the card — whole, as a prefix, inside, or
//! one or two typing slips away — and the card's title counts most.
use crate::task::Task;

/// Words shorter than this match only whole or as a prefix.
const SLIP_MIN: usize = 4;
/// Words at least this long may be two slips away.
const TWO_SLIPS_MIN: usize = 8;

/// How well a card answers `query`, or `None` when some word of the
/// query matches nothing on it. A key (`AD-12`) answers only its card.
pub fn score(query: &str, task: &Task) -> Option<u32> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return None;
    }
    if !task.key.is_empty() && task.key.to_lowercase() == query {
        return Some(10_000);
    }
    if task.id.as_str() == query {
        return Some(10_000);
    }
    // Words shaped like a key ask for that card, not for one a slip away.
    if key_shaped(&query) {
        return None;
    }
    let fields: [(&str, u32); 5] = [
        (&task.title, 6),
        (&task.description, 1),
        (&task.acceptance, 1),
        (&task.external_blocker, 1),
        (&task.key, 4),
    ];
    let words: Vec<(String, u32)> = fields
        .iter()
        .flat_map(|(text, weight)| words(text).map(move |w| (w, *weight)))
        .chain(task.tags.iter().map(|t| (t.to_lowercase(), 3)))
        .collect();
    let mut total = 0;
    for asked in words_of(&query) {
        let best = words
            .iter()
            .filter_map(|(word, weight)| matched(&asked, word).map(|m| m * weight))
            .max()?;
        total += best;
    }
    Some(total)
}

/// The cards that answer `query`, best first; among equals, open cards
/// before ended ones, then the most recently changed. At most `limit`.
pub fn rank<'t>(query: &str, tasks: &'t [Task], limit: usize) -> Vec<&'t Task> {
    let mut found: Vec<(u32, &Task)> = tasks
        .iter()
        .filter_map(|t| score(query, t).map(|s| (s, t)))
        .collect();
    found.sort_by(|(a, x), (b, y)| {
        b.cmp(a)
            .then(x.column.terminal().cmp(&y.column.terminal()))
            .then(y.updated_at.cmp(&x.updated_at))
            .then(x.id.cmp(&y.id))
    });
    found.into_iter().take(limit).map(|(_, t)| t).collect()
}

/// `ad-12`: letters, a dash, digits.
fn key_shaped(query: &str) -> bool {
    query.split_once('-').is_some_and(|(prefix, number)| {
        !prefix.is_empty()
            && prefix.chars().all(|c| c.is_ascii_alphabetic())
            && !number.is_empty()
            && number.chars().all(|c| c.is_ascii_digit())
    })
}

fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_'))
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
}

fn words_of(query: &str) -> Vec<String> {
    words(query).collect()
}

/// How well one asked word matches one card word: whole 10, prefix 7,
/// inside 4, a slip 5, two slips 3.
fn matched(asked: &str, word: &str) -> Option<u32> {
    if asked == word {
        return Some(10);
    }
    if word.starts_with(asked) && asked.chars().count() >= 2 {
        return Some(7);
    }
    let asked_len = asked.chars().count();
    if asked_len >= SLIP_MIN {
        let slips = distance(asked, word, 2);
        if slips == 1 {
            return Some(5);
        }
        if slips == 2 && asked_len >= TWO_SLIPS_MIN {
            return Some(3);
        }
        if asked_len >= 3 && word.contains(asked) {
            return Some(4);
        }
    } else if asked_len >= 3 && word.contains(asked) {
        return Some(4);
    }
    None
}

/// Edit distance between two words, giving up past `bound` (a swap of
/// neighbours counts once).
fn distance(a: &str, b: &str, bound: usize) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.len().abs_diff(b.len()) > bound {
        return bound + 1;
    }
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    let mut before: Vec<usize> = vec![0; b.len() + 1];
    for i in 1..=a.len() {
        let mut current = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut best = (previous[j] + 1)
                .min(current[j - 1] + 1)
                .min(previous[j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(before[j - 2] + 1);
            }
            current[j] = best;
        }
        if current.iter().min().is_some_and(|m| *m > bound) {
            return bound + 1;
        }
        before = std::mem::replace(&mut previous, current);
    }
    previous[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProjectId;
    use crate::task::NewTask;
    use chrono::Utc;

    fn card(key: &str, title: &str, description: &str) -> Task {
        let mut task = Task::file(
            ProjectId::from("p"),
            NewTask {
                title: title.into(),
                description: description.into(),
                ..Default::default()
            },
            "user",
            Utc::now(),
        )
        .unwrap();
        task.key = key.into();
        task
    }

    #[test]
    fn a_key_answers_exactly_and_titles_count_most() {
        let cards = vec![
            card("AD-1", "Fix the login page", ""),
            card("AD-2", "Write notes", "the login flow"),
            card("AD-12", "Ship the parser", ""),
        ];
        let found = rank("ad-12", &cards, 10);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].key, "AD-12");
        let found = rank("login", &cards, 10);
        assert_eq!(
            found.iter().map(|t| t.key.as_str()).collect::<Vec<_>>(),
            ["AD-1", "AD-2"],
            "a title match before a description match"
        );
    }

    #[test]
    fn slips_prefixes_and_every_word() {
        let cards = vec![card("AD-1", "Refactor the authentication middleware", "")];
        assert!(score("authentcation", &cards[0]).is_some(), "one slip");
        assert!(
            score("athuentication", &cards[0]).is_some(),
            "two slips in a long word"
        );
        assert!(score("middle", &cards[0]).is_some(), "a prefix");
        assert!(score("refactor auth", &cards[0]).is_some());
        assert!(score("refactor billing", &cards[0]).is_none(), "every word");
        assert!(score("ab", &cards[0]).is_none());
        assert_eq!(distance("kitten", "sitting", 3), 3);
        assert_eq!(distance("abcd", "abdc", 2), 1);
    }
}
