//! The daemon's own configuration: `agentd.toml` in its home.
//!
//! This is distinct from admission policy (`policy.toml`), which decides what
//! agents may do. `agentd.toml` decides how the daemon keeps house for itself.
//! Reading the file is host work; what it means is decided here.

use chrono::Duration;
use serde::Deserialize;

/// The file's name inside the daemon's home.
pub const FILE_NAME: &str = "agentd.toml";

/// How many journal rows one retention tick deletes per project, so the
/// once-a-minute tick stays short however far behind retention has fallen.
pub const RETENTION_BATCH: usize = 1_000;

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonConfig {
    #[serde(default)]
    pub journal: JournalConfig,
    #[serde(default)]
    pub messages: MessagesConfig,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessagesConfig {
    /// How long archived messages are kept, as `30m`, `12h`, `180d` or
    /// plain seconds. Absent means the per-conversation cap alone bounds
    /// the archive.
    #[serde(default)]
    pub retention: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalConfig {
    /// How long journal entries are kept, as `30m`, `12h`, `180d` or plain
    /// seconds. Absent means forever: the journal is the audit trail.
    #[serde(default)]
    pub retention: Option<String>,
}

impl DaemonConfig {
    /// The journal retention window, when one is configured. A window that
    /// cannot be read is a broken file, not a file that keeps everything:
    /// callers treat the error as "ignore the file and say so".
    pub fn journal_retention(&self) -> Result<Option<Duration>, String> {
        self.journal
            .retention
            .as_deref()
            .map(|text| parse_duration(text).map_err(|error| format!("journal.retention: {error}")))
            .transpose()
    }

    /// The message archive's retention window, when one is configured.
    pub fn messages_retention(&self) -> Result<Option<Duration>, String> {
        self.messages
            .retention
            .as_deref()
            .map(|text| {
                parse_duration(text).map_err(|error| format!("messages.retention: {error}"))
            })
            .transpose()
    }
}

/// `45s`, `30m`, `12h`, `7d`, or a plain number of seconds. Whitespace around
/// the value is tolerated; anything else, including zero, is refused so an
/// empty or negative window is never applied by accident.
pub fn parse_duration(text: &str) -> Result<Duration, String> {
    let text = text.trim();
    let (digits, unit) = match text.char_indices().find(|(_, c)| !c.is_ascii_digit()) {
        Some((at, _)) => text.split_at(at),
        None => (text, "s"),
    };
    let amount: i64 = digits
        .parse()
        .map_err(|_| format!("`{text}` is not a duration such as 30m, 12h or 180d"))?;
    if amount == 0 {
        return Err("a duration must be positive".to_owned());
    }
    let seconds = match unit.trim() {
        "s" | "sec" | "secs" | "second" | "seconds" => Some(amount),
        "m" | "min" | "mins" | "minute" | "minutes" => amount.checked_mul(60),
        "h" | "hr" | "hrs" | "hour" | "hours" => amount.checked_mul(3_600),
        "d" | "day" | "days" => amount.checked_mul(86_400),
        _ => {
            return Err(format!(
                "`{text}` is not a duration such as 30m, 12h or 180d"
            ));
        }
    }
    .ok_or_else(|| format!("`{text}` is too long a duration"))?;
    Duration::try_seconds(seconds).ok_or_else(|| format!("`{text}` is too long a duration"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_parse_with_units_and_plain_seconds() {
        assert_eq!(parse_duration("45").unwrap(), Duration::seconds(45));
        assert_eq!(parse_duration("45s").unwrap(), Duration::seconds(45));
        assert_eq!(parse_duration("30m").unwrap(), Duration::minutes(30));
        assert_eq!(parse_duration(" 12h ").unwrap(), Duration::hours(12));
        assert_eq!(parse_duration("180d").unwrap(), Duration::days(180));
        assert_eq!(parse_duration("2 days").unwrap(), Duration::days(2));
    }

    #[test]
    fn durations_refuse_zero_negative_junk_and_overflow() {
        for bad in [
            "",
            "0",
            "0d",
            "-1d",
            "d",
            "1w",
            "1.5h",
            "soon",
            "99999999999999999d",
            // Fits an i64 of seconds, not a chrono duration: an error, not a panic.
            "9223372036854775807",
            "9223372036854775807s",
        ] {
            assert!(parse_duration(bad).is_err(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn config_parses_retention_and_refuses_unknown_keys() {
        let parse = |text: &str| toml::from_str::<DaemonConfig>(text);
        let config = parse("[journal]\nretention = \"180d\"\n").unwrap();
        assert_eq!(
            config.journal_retention().unwrap(),
            Some(Duration::days(180))
        );
        assert_eq!(parse("").unwrap(), DaemonConfig::default());
        let unreadable = parse("[journal]\nretention = \"forever\"\n").unwrap();
        assert!(unreadable.journal_retention().is_err());
        assert!(parse("[journal]\nretain = \"1d\"\n").is_err());
        assert!(parse("[jurnal]\nretention = \"1d\"\n").is_err());
    }
}
