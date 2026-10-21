//! What the clients of a grant look like. Every client of a vendor account
//! — its browser side panel, its web and desktop apps, a terminal Codex or
//! Claude Code reaching the account's connectors — presents the same grant
//! from the same vendor addresses, so nothing proves which one is calling.
//! What each says about its own software is kept per grant, bounded, for
//! the person to read: never a value that could identify a person. Pure:
//! the caller reads the request and keeps the store.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Distinct fingerprints kept per grant; past this, the one seen longest
/// ago gives way.
pub const MAX_SEEN: usize = 8;
/// A `User-Agent` kept is cut to this many characters.
const MAX_AGENT: usize = 160;
/// A client name, version, protocol version or `_meta` key is cut to this.
const MAX_WORD: usize = 64;
/// `_meta` key names kept per fingerprint, in sorted order.
const MAX_KEYS: usize = 16;

/// What one request said about the software that made it: the HTTP
/// `User-Agent`, the MCP `clientInfo` name and version and the protocol
/// version (from `initialize`, or the `MCP-Protocol-Version` header of a
/// later request), and the sorted names of the `_meta` keys it sent —
/// never their values, which may carry a person's locale, location or ids.
/// All of it is the client's own claim.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Fingerprint {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol_version: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub meta_keys: Vec<String>,
}

/// One fingerprint and when it was first and last seen on its grant.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Seen {
    #[serde(flatten)]
    pub fingerprint: Fingerprint,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
}

impl Fingerprint {
    /// What one JSON-RPC message says about its client, with the request's
    /// `User-Agent` and `MCP-Protocol-Version` headers: an `initialize`
    /// (where a client names itself) or a `tools/call` (where `_meta`
    /// travels). Anything else — a ping, a listing, a notification — adds
    /// nothing a fingerprint would keep, and is not one.
    pub fn of(
        user_agent: Option<&str>,
        protocol_header: Option<&str>,
        message: &Value,
    ) -> Option<Self> {
        let params = &message["params"];
        let (client, protocol) = match message["method"].as_str() {
            Some("initialize") => (
                Some(&params["clientInfo"]),
                params["protocolVersion"].as_str(),
            ),
            Some("tools/call") => (None, protocol_header),
            _ => return None,
        };
        let mut meta_keys: Vec<String> = match params.get("_meta") {
            Some(Value::Object(meta)) => meta
                .keys()
                .filter_map(|key| bounded(key, MAX_WORD))
                .collect(),
            _ => Vec::new(),
        };
        meta_keys.sort_unstable();
        meta_keys.dedup();
        meta_keys.truncate(MAX_KEYS);
        Some(Self {
            user_agent: user_agent.and_then(|agent| bounded(agent, MAX_AGENT)),
            client_name: client
                .and_then(|c| c["name"].as_str())
                .and_then(|name| bounded(name, MAX_WORD)),
            client_version: client
                .and_then(|c| c["version"].as_str())
                .and_then(|version| bounded(version, MAX_WORD)),
            protocol_version: protocol.and_then(|v| bounded(v, MAX_WORD)),
            meta_keys,
        })
    }

    /// One line for `connector grants`.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if let Some(agent) = &self.user_agent {
            parts.push(format!("user-agent \"{agent}\""));
        }
        match (&self.client_name, &self.client_version) {
            (Some(name), Some(version)) => parts.push(format!("client {name} {version}")),
            (Some(name), None) => parts.push(format!("client {name}")),
            (None, Some(version)) => parts.push(format!("client version {version}")),
            (None, None) => {}
        }
        if let Some(protocol) = &self.protocol_version {
            parts.push(format!("protocol {protocol}"));
        }
        if !self.meta_keys.is_empty() {
            parts.push(format!("_meta keys {}", self.meta_keys.join(", ")));
        }
        if parts.is_empty() {
            "nothing said about itself".to_owned()
        } else {
            parts.join(" · ")
        }
    }
}

/// Note `fingerprint` among a grant's `seen`: a known one is seen again,
/// a new one is kept and, past [`MAX_SEEN`], the one seen longest ago
/// gives way. Whether it was new, which is when the store is worth saving.
pub fn observe(seen: &mut Vec<Seen>, fingerprint: Fingerprint, now: DateTime<Utc>) -> bool {
    if let Some(known) = seen.iter_mut().find(|s| s.fingerprint == fingerprint) {
        known.last_seen = known.last_seen.max(now);
        return false;
    }
    seen.push(Seen {
        fingerprint,
        first_seen: now,
        last_seen: now,
    });
    while seen.len() > MAX_SEEN {
        let Some(oldest) = seen
            .iter()
            .enumerate()
            .min_by_key(|(_, s)| s.last_seen)
            .map(|(i, _)| i)
        else {
            break;
        };
        seen.remove(oldest);
    }
    true
}

/// `text` without control characters, trimmed and cut to `max` characters;
/// none when nothing is left.
fn bounded(text: &str, max: usize) -> Option<String> {
    let kept: String = text
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .chars()
        .take(max)
        .collect();
    (!kept.is_empty()).then_some(kept)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn at(minutes: i64) -> DateTime<Utc> {
        "2026-09-17T22:00:00Z".parse::<DateTime<Utc>>().unwrap()
            + chrono::Duration::minutes(minutes)
    }

    /// An `initialize` names its client and protocol; a tool call carries
    /// the header's protocol and its `_meta` key names, sorted, never their
    /// values; anything else is no fingerprint.
    #[test]
    fn a_fingerprint_keeps_what_software_says_and_no_value_of_a_person() {
        let init = Fingerprint::of(
            Some("openai-mcp/1.0.0"),
            None,
            &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
                "protocolVersion": "2025-06-18",
                "clientInfo": {"name": "openai-mcp", "version": "1.0.0", "title": "ignored"},
            }}),
        )
        .unwrap();
        assert_eq!(
            init,
            Fingerprint {
                user_agent: Some("openai-mcp/1.0.0".into()),
                client_name: Some("openai-mcp".into()),
                client_version: Some("1.0.0".into()),
                protocol_version: Some("2025-06-18".into()),
                meta_keys: vec![],
            }
        );
        let call = Fingerprint::of(
            Some("openai-mcp/1.0.0"),
            Some("2025-06-18"),
            &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
                "name": "whoami",
                "_meta": {
                    "openai/userLocation": {"city": "Lyon"},
                    "openai/locale": "fr-FR",
                    "openai/subject": "person-7f3a",
                },
            }}),
        )
        .unwrap();
        assert_eq!(
            call.meta_keys,
            ["openai/locale", "openai/subject", "openai/userLocation"]
        );
        assert_eq!(call.client_name, None);
        assert_eq!(call.protocol_version.as_deref(), Some("2025-06-18"));
        let kept = serde_json::to_string(&call).unwrap();
        for value in ["Lyon", "fr-FR", "person-7f3a"] {
            assert!(!kept.contains(value), "{kept}");
        }
        assert_eq!(
            call.describe(),
            "user-agent \"openai-mcp/1.0.0\" · protocol 2025-06-18 · _meta keys openai/locale, openai/subject, openai/userLocation"
        );
        assert_eq!(
            init.describe(),
            "user-agent \"openai-mcp/1.0.0\" · client openai-mcp 1.0.0 · protocol 2025-06-18"
        );
        for other in ["ping", "tools/list", "notifications/initialized"] {
            assert!(
                Fingerprint::of(Some("x"), None, &json!({"method": other})).is_none(),
                "{other}"
            );
        }
        // Bounded, and without control characters.
        let long = Fingerprint::of(
            Some(&format!("agent\n{}", "a".repeat(500))),
            None,
            &json!({"method": "tools/call", "params": {"_meta": (0..40)
                .map(|i| (format!("k{i:02}"), json!(i)))
                .collect::<serde_json::Map<String, Value>>()}}),
        )
        .unwrap();
        let agent = long.user_agent.unwrap();
        assert_eq!(agent.chars().count(), MAX_AGENT);
        assert!(!agent.contains('\n'));
        assert_eq!(long.meta_keys.len(), MAX_KEYS);
        assert_eq!(long.meta_keys[0], "k00");
        assert_eq!(
            Fingerprint::default().describe(),
            "nothing said about itself"
        );
    }

    /// A known fingerprint is seen again; a new one is kept, and past the
    /// bound the one seen longest ago gives way.
    #[test]
    fn a_grant_keeps_the_last_distinct_fingerprints() {
        let shape = |n: usize| Fingerprint {
            user_agent: Some(format!("client/{n}")),
            ..Fingerprint::default()
        };
        let mut seen = Vec::new();
        assert!(observe(&mut seen, shape(0), at(0)));
        assert!(!observe(&mut seen, shape(0), at(5)), "seen again");
        assert_eq!(seen.len(), 1);
        assert_eq!((seen[0].first_seen, seen[0].last_seen), (at(0), at(5)));
        for n in 1..MAX_SEEN {
            assert!(observe(&mut seen, shape(n), at(n as i64)));
        }
        assert_eq!(seen.len(), MAX_SEEN);
        // The first was seen again at 5, so the one from minute 1 is the oldest.
        assert!(observe(&mut seen, shape(99), at(60)));
        assert_eq!(seen.len(), MAX_SEEN);
        assert!(!seen.iter().any(|s| s.fingerprint == shape(1)));
        assert!(seen.iter().any(|s| s.fingerprint == shape(0)));
        assert!(seen.iter().any(|s| s.fingerprint == shape(99)));
        let round = serde_json::to_value(&seen[0]).unwrap();
        assert_eq!(round["user_agent"], "client/0");
        assert_eq!(serde_json::from_value::<Seen>(round).unwrap(), seen[0]);
    }
}
