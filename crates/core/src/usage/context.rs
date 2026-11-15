//! How much of its model's context window a session's latest request took,
//! as the provider's own record counted it. An estimate, never a measurement
//! of what the provider will do next: the window comes from the record where
//! it states one, otherwise from the table below by the model's exact name,
//! and a model the table does not list gives no figure at all.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// One provider record's account of a session's latest request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Context {
    /// Tokens the latest request carried, cached or not, as the provider
    /// counted them: what the model was given.
    pub used_tokens: u64,
    /// The model's window, as the record stated it or the table lists it.
    pub window_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Which record said so: its runtime and format, the record's own
    /// identity within the session, and when the provider wrote it.
    pub runtime: String,
    pub format: String,
    pub record: String,
    pub at: DateTime<Utc>,
}

impl Context {
    /// The fraction used, rounded to the nearest whole percent; nothing for
    /// a window of zero, which says no size. Over 100 when the counts
    /// exceed the window: the counts and the window the record stated or
    /// the table assumed then disagree, and the figure says so rather than
    /// hiding it at 100.
    pub fn percent(&self) -> Option<u64> {
        let window = self.window_tokens;
        (window > 0).then(|| {
            (self
                .used_tokens
                .saturating_mul(100)
                .saturating_add(window / 2))
                / window
        })
    }

    /// `Context 62 %`, as the row's hint and Details say it.
    pub fn label(&self) -> Option<String> {
        self.percent().map(|percent| format!("Context {percent} %"))
    }
}

/// What a session's newest record that speaks for its own context says:
/// a figure, or none (the model's window is unknown, or the record counts
/// no input). A newer record that says none clears an older figure, so
/// what is shown is always the newest record's word.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reading {
    pub runtime: String,
    /// When the provider wrote the record.
    pub at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<Context>,
}

impl Reading {
    /// Whether `other` takes this one's place: a record no older than it
    /// does, so of two written at the same moment the later one read wins
    /// (the rule the store keeps across batches).
    pub fn replaced_by(&self, other: &Reading) -> bool {
        other.at >= self.at
    }
}

const WINDOW_200K: u64 = 200_000;
const WINDOW_1M: u64 = 1_000_000;

/// The models this table knows, by the exact name a Claude Code transcript
/// keeps for a response (the model id the API answered with), and each
/// one's context window as Anthropic's models overview documents it. Only
/// models whose name says their window: one served at a single window,
/// with no smaller default or larger opt-in under the same name. Claude
/// Opus 4.6, Claude Sonnet 4.6 and Claude Sonnet 4.5 are not listed:
/// each was served at 200,000 tokens or at one million under the same id,
/// so the name a transcript keeps does not say which a session had.
const WINDOWS: &[(&str, u64)] = &[
    ("claude-fable-5-2", WINDOW_1M),
    ("claude-fable-5-1", WINDOW_1M),
    ("claude-fable-5", WINDOW_1M),
    ("claude-mythos-5-2", WINDOW_1M),
    ("claude-mythos-5-1", WINDOW_1M),
    ("claude-mythos-5", WINDOW_1M),
    ("claude-opus-5-5", WINDOW_1M),
    ("claude-opus-5", WINDOW_1M),
    ("claude-opus-4-8", WINDOW_1M),
    ("claude-opus-4-7", WINDOW_1M),
    ("claude-sonnet-5-5", WINDOW_1M),
    ("claude-sonnet-5", WINDOW_1M),
    ("claude-haiku-5-5", WINDOW_1M),
    ("claude-opus-4-5", WINDOW_200K),
    ("claude-opus-4-5-20251101", WINDOW_200K),
    ("claude-haiku-4-5", WINDOW_200K),
    ("claude-haiku-4-5-20251001", WINDOW_200K),
];

/// The window of a model the table lists by its exact name, or nothing
/// for any other name: no family, version or suffix is read into one. A
/// Codex rollout states its own window and needs no table.
pub fn window_tokens(model: &str) -> Option<u64> {
    WINDOWS
        .iter()
        .find(|(name, _)| *name == model)
        .map(|(_, window)| *window)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_knows_listed_names_exactly_and_nothing_else() {
        for (model, window) in WINDOWS {
            assert_eq!(window_tokens(model), Some(*window), "{model}");
        }
        assert_eq!(window_tokens("claude-opus-5-5"), Some(WINDOW_1M));
        assert_eq!(
            window_tokens("claude-haiku-4-5-20251001"),
            Some(WINDOW_200K)
        );
        for model in [
            // Served at two windows under one name.
            "claude-opus-4-6",
            "claude-sonnet-4-6",
            "claude-sonnet-4-5",
            "claude-sonnet-4-5-20250929",
            // A suffix or a date is not read into a listed name.
            "claude-opus-4-6[1m]",
            "claude-opus-5-5[1m]",
            "claude-opus-0[1m]",
            "claude-opus-5-5-20260101",
            // Names nobody verified, however they read.
            "claude-sonnet-999-999",
            "claude-haiku-4-999",
            "claude-fable-6",
            "claude-opus-5-6",
            "CLAUDE-OPUS-5-5",
            " claude-opus-5-5",
            "claude-3-opus-20240229",
            "fixture-model",
            "opus",
            "",
        ] {
            assert_eq!(window_tokens(model), None, "{model}");
        }
    }

    fn context(used: u64, window: u64) -> Context {
        Context {
            used_tokens: used,
            window_tokens: window,
            model: None,
            runtime: "claude-code".into(),
            format: "fixture".into(),
            record: "fixture".into(),
            at: DateTime::<Utc>::UNIX_EPOCH,
        }
    }

    #[test]
    fn the_percent_rounds_to_the_nearest_and_says_more_than_the_window_when_it_is() {
        assert_eq!(context(124_000, 200_000).percent(), Some(62));
        assert_eq!(context(124_999, 200_000).percent(), Some(62));
        assert_eq!(context(125_000, 200_000).percent(), Some(63));
        assert_eq!(context(1, 200_000).percent(), Some(0));
        assert_eq!(context(200_000, 200_000).percent(), Some(100));
        assert_eq!(context(250_000, 200_000).percent(), Some(125));
        assert_eq!(context(5, 0).percent(), None, "a zero window says no size");
        assert_eq!(context(5, 0).label(), None);
        assert_eq!(
            context(124_000, 200_000).label().as_deref(),
            Some("Context 62 %")
        );
    }

    #[test]
    fn a_newer_reading_replaces_an_older_and_a_tie_goes_to_the_later_read() {
        let at = |seconds| DateTime::<Utc>::UNIX_EPOCH + chrono::Duration::seconds(seconds);
        let reading = |seconds, figure: Option<Context>| Reading {
            runtime: "claude-code".into(),
            at: at(seconds),
            context: figure,
        };
        let kept = reading(10, Some(context(1, 10)));
        assert!(kept.replaced_by(&reading(20, None)));
        assert!(kept.replaced_by(&reading(10, None)));
        assert!(!kept.replaced_by(&reading(5, Some(context(2, 10)))));
    }
}
