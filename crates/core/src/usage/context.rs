//! How much of its model's context window a session's latest request took,
//! as the provider's own record counted it. An estimate, never a measurement
//! of what the provider will do next: the window comes from the record where
//! it states one, otherwise from the small table below by model name, and a
//! model the table does not know gives no figure at all.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// One provider record's account of a session's latest request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Context {
    /// Tokens the latest request carried, cached or not, as the provider
    /// counted them: what the model was given.
    pub used_tokens: u64,
    /// The model's window, as the record stated it or the table knows it.
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
    /// The fraction used, rounded to the nearest whole percent. Over 100
    /// when the counts exceed the window the table assumed, which is the
    /// honest figure: the table, not the counts, is then wrong.
    pub fn percent(&self) -> u64 {
        let window = self.window_tokens.max(1);
        (self.used_tokens.saturating_mul(100).saturating_add(window / 2)) / window
    }

    /// `Context 62 %`, as the row's hint and Details say it.
    pub fn label(&self) -> String {
        format!("Context {} %", self.percent())
    }
}

const WINDOW_200K: u64 = 200_000;
const WINDOW_1M: u64 = 1_000_000;

/// The window of a model named the way a Claude Code transcript names it
/// (`claude-<family>-<major>-<minor>[-<date>]`, the older
/// `claude-<major>-<minor>-<family>[-<date>]`, either with a `[1m]` suffix
/// for the one-million-token variant), or nothing for a name this table
/// does not know. A Codex rollout states its own window and needs no table.
pub fn window_tokens(model: &str) -> Option<u64> {
    let (name, one_million) = match model.strip_suffix("[1m]") {
        Some(name) => (name, true),
        None => (model, false),
    };
    let rest = name.strip_prefix("claude-")?;
    let mut family = None;
    let mut generation = Vec::new();
    for part in rest.split('-') {
        match part {
            "opus" | "sonnet" | "haiku" | "fable" | "mythos" => {
                if family.replace(part).is_some() {
                    return None;
                }
            }
            // A release date, not a version.
            digits if digits.len() == 8 && digits.bytes().all(|b| b.is_ascii_digit()) => {}
            digits if generation.len() < 2 => generation.push(digits.parse::<u32>().ok()?),
            _ => return None,
        }
    }
    let family = family?;
    let major = *generation.first()?;
    let minor = generation.get(1).copied().unwrap_or(0);
    Some(match (family, major, minor) {
        _ if one_million => WINDOW_1M,
        ("fable" | "mythos", _, _) => WINDOW_1M,
        (_, 5.., _) => WINDOW_1M,
        ("opus", 4, 7..) => WINDOW_1M,
        (_, 3 | 4, _) => WINDOW_200K,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_knows_transcript_model_names_and_nothing_else() {
        for (model, window) in [
            ("claude-opus-4-6", Some(WINDOW_200K)),
            ("claude-opus-4-6[1m]", Some(WINDOW_1M)),
            ("claude-sonnet-4-6", Some(WINDOW_200K)),
            ("claude-sonnet-4-6[1m]", Some(WINDOW_1M)),
            ("claude-opus-4-7", Some(WINDOW_1M)),
            ("claude-opus-4-8", Some(WINDOW_1M)),
            ("claude-opus-4-5-20251101", Some(WINDOW_200K)),
            ("claude-opus-4-1-20250805", Some(WINDOW_200K)),
            ("claude-opus-4-20250514", Some(WINDOW_200K)),
            ("claude-sonnet-4-5-20250929", Some(WINDOW_200K)),
            ("claude-sonnet-4-5-20250929[1m]", Some(WINDOW_1M)),
            ("claude-haiku-4-5-20251001", Some(WINDOW_200K)),
            ("claude-haiku-5-5", Some(WINDOW_1M)),
            ("claude-sonnet-5", Some(WINDOW_1M)),
            ("claude-sonnet-5-5", Some(WINDOW_1M)),
            ("claude-opus-5-5", Some(WINDOW_1M)),
            ("claude-fable-5-1", Some(WINDOW_1M)),
            ("claude-mythos-5", Some(WINDOW_1M)),
            ("claude-3-5-sonnet-20241022", Some(WINDOW_200K)),
            ("claude-3-7-sonnet-20250219", Some(WINDOW_200K)),
            ("claude-3-opus-20240229", Some(WINDOW_200K)),
            ("fixture-model", None),
            ("claude-2.1", None),
            ("claude-instant-1.2", None),
            ("claude-opus", None),
            ("claude-opus-sonnet-4-6", None),
            ("claude-opus-4-6-1-2", None),
            ("opus", None),
            ("", None),
        ] {
            assert_eq!(window_tokens(model), window, "{model}");
        }
    }

    #[test]
    fn the_percent_rounds_to_the_nearest_and_says_more_than_the_window_when_it_is() {
        let context = |used, window| Context {
            used_tokens: used,
            window_tokens: window,
            model: None,
            runtime: "claude-code".into(),
            format: "fixture".into(),
            record: "fixture".into(),
            at: DateTime::<Utc>::UNIX_EPOCH,
        };
        assert_eq!(context(124_000, 200_000).percent(), 62);
        assert_eq!(context(124_999, 200_000).percent(), 62);
        assert_eq!(context(125_000, 200_000).percent(), 63);
        assert_eq!(context(1, 200_000).percent(), 0);
        assert_eq!(context(200_000, 200_000).percent(), 100);
        assert_eq!(context(250_000, 200_000).percent(), 125);
        assert_eq!(context(5, 0).percent(), 500, "a zero window divides by one");
        assert_eq!(context(124_000, 200_000).label(), "Context 62 %");
    }
}
