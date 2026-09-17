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
    /// Where copies of chosen events are posted, signed. Nothing is
    /// posted anywhere unless the person writes a sink here.
    #[serde(default)]
    pub webhooks: Vec<WebhookConfig>,
}

/// At most this many sinks; each carries its own bounded queue and its
/// own connection, so the count is a bound on the daemon's outbound work.
pub const WEBHOOKS: usize = 8;
/// A sink listens to at most this many event kinds.
pub const WEBHOOK_EVENTS: usize = 64;

/// One sink: an `https` address (plain `http` only on this machine), a
/// file holding the shared secret, the event kinds to post, and the
/// shape of the body.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebhookConfig {
    /// How the sink is named in logs and events, so its address never is.
    pub name: String,
    pub url: String,
    /// A private file (a regular file of this user, mode 0600) whose
    /// trimmed content is the HMAC secret.
    pub secret_file: std::path::PathBuf,
    /// Event kinds to post, by their wire names (`question_asked`,
    /// `agent_exited`, `lease_deadlock`…). Empty posts nothing.
    #[serde(default)]
    pub events: Vec<String>,
    /// Only events of this project (id, root or unique prefix), when set.
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub format: WebhookFormat,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebhookFormat {
    /// The projected event as JSON.
    #[default]
    Json,
    /// `{"text": "…"}`, one line, for Slack-shaped receivers.
    Slack,
}

impl WebhookConfig {
    /// The shape a sink must have: a name that can be said, an address
    /// that goes nowhere surprising, kinds that can be kinds. The address
    /// keeps no credentials in it and no fragment, and plain `http` is
    /// allowed only to this machine.
    pub fn check(&self) -> Result<(), String> {
        let name = &self.name;
        if name.is_empty()
            || name.len() > 40
            || !name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err("a webhook name is 1–40 lowercase letters, digits or hyphens".into());
        }
        let (scheme, rest) = self
            .url
            .split_once("://")
            .ok_or_else(|| format!("webhook {name}: the url needs a scheme"))?;
        if rest.contains('#') {
            return Err(format!("webhook {name}: the url has a fragment"));
        }
        let authority = rest.split(['/', '?']).next().unwrap_or("");
        if authority.contains('@') {
            return Err(format!(
                "webhook {name}: the url carries credentials; put a secret in secret_file"
            ));
        }
        let host = authority
            .rsplit_once(':')
            .filter(|(_, port)| port.bytes().all(|b| b.is_ascii_digit()))
            .map_or(authority, |(host, _)| host)
            .trim_matches(['[', ']']);
        if host.is_empty() {
            return Err(format!("webhook {name}: the url has no host"));
        }
        let local = matches!(host, "127.0.0.1" | "::1" | "localhost");
        match scheme {
            "https" => {}
            "http" if local => {}
            "http" => {
                return Err(format!(
                    "webhook {name}: plain http only reaches this machine (127.0.0.1, ::1 or localhost); use https"
                ));
            }
            other => return Err(format!("webhook {name}: {other}:// is not http or https")),
        }
        if self.secret_file.as_os_str().is_empty() || !self.secret_file.is_absolute() {
            return Err(format!("webhook {name}: secret_file is an absolute path"));
        }
        if self.events.len() > WEBHOOK_EVENTS {
            return Err(format!(
                "webhook {name}: at most {WEBHOOK_EVENTS} event kinds"
            ));
        }
        for kind in &self.events {
            if kind.is_empty()
                || kind.len() > 64
                || !kind
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b == b'_' || b.is_ascii_digit())
            {
                return Err(format!("webhook {name}: `{kind}` is not an event kind"));
            }
            if kind.starts_with("webhook_") {
                return Err(format!(
                    "webhook {name}: `{kind}` is a webhook's own event and is never posted"
                ));
            }
        }
        Ok(())
    }
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

    /// The sinks, each checked, their names distinct, no more than
    /// [`WEBHOOKS`] of them.
    pub fn webhooks(&self) -> Result<&[WebhookConfig], String> {
        if self.webhooks.len() > WEBHOOKS {
            return Err(format!("at most {WEBHOOKS} webhooks"));
        }
        let mut names = std::collections::BTreeSet::new();
        for sink in &self.webhooks {
            sink.check()?;
            if !names.insert(sink.name.as_str()) {
                return Err(format!("webhook {} is named twice", sink.name));
            }
        }
        Ok(&self.webhooks)
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

    /// A sink is checked for the shape that keeps it safe: a sayable
    /// name, https (or http only to this machine), no credentials or
    /// fragment in the address, an absolute private secret path, kinds
    /// that are kinds and never a webhook's own, distinct names, a bound
    /// on the count.
    #[test]
    fn webhook_sinks_are_checked_for_shape() {
        let good = |url: &str| WebhookConfig {
            name: "team-slack".into(),
            url: url.into(),
            secret_file: "/Users/me/.config/agentdocker/slack.secret".into(),
            events: vec!["question_asked".into(), "lease_deadlock".into()],
            project: None,
            format: WebhookFormat::Slack,
        };
        assert!(
            good("https://hooks.slack.com/services/T/B/x")
                .check()
                .is_ok()
        );
        assert!(good("http://127.0.0.1:8080/hook").check().is_ok());
        assert!(good("http://localhost/hook").check().is_ok());
        assert!(good("http://[::1]:9/hook").check().is_ok());
        for bad in [
            "http://example.com/hook",
            "ftp://example.com/hook",
            "https://user:pw@example.com/hook",
            "https://example.com/hook#frag",
            "https:///nohost",
            "nothing",
        ] {
            assert!(good(bad).check().is_err(), "{bad}");
        }
        let mut sink = good("https://example.com/hook");
        sink.name = "Team Slack".into();
        assert!(sink.check().is_err(), "the name");
        let mut sink = good("https://example.com/hook");
        sink.secret_file = "relative.secret".into();
        assert!(sink.check().is_err(), "the secret path");
        let mut sink = good("https://example.com/hook");
        sink.events = vec!["webhook_failed".into()];
        assert!(sink.check().is_err(), "its own event");
        let mut sink = good("https://example.com/hook");
        sink.events = vec!["Question Asked".into()];
        assert!(sink.check().is_err(), "not a kind");
        let text = r#"
[[webhooks]]
name = "a"
url = "https://example.com/a"
secret_file = "/tmp/a.secret"
events = ["agent_exited"]

[[webhooks]]
name = "a"
url = "https://example.com/b"
secret_file = "/tmp/b.secret"
"#;
        let config: DaemonConfig = toml::from_str(text).unwrap();
        assert!(config.webhooks().unwrap_err().contains("named twice"));
        let many: Vec<WebhookConfig> = (0..=WEBHOOKS)
            .map(|i| {
                let mut sink = good("https://example.com/hook");
                sink.name = format!("sink-{i}");
                sink
            })
            .collect();
        let config = DaemonConfig {
            webhooks: many,
            ..DaemonConfig::default()
        };
        assert!(config.webhooks().is_err());
        let none: DaemonConfig = toml::from_str("").unwrap();
        assert!(none.webhooks().unwrap().is_empty());
    }

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
