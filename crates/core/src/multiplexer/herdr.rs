//! herdr's socket API, as far as AgentDocker reads it.
//!
//! herdr marks the agent in each of its panes working, blocked, done or
//! idle, and says so over its socket: newline-delimited JSON, one request
//! per connection, or an `events.subscribe` that stays open and pushes a
//! line per event. For an agent that reports nothing of its own — no
//! lifecycle hooks in its process — that is the best evidence there is of
//! what it is doing, so the daemon reads it.
//!
//! Only reads: `pane.list` and `events.subscribe`. Nothing here writes to
//! herdr. A pane is matched to an agent by the provider session herdr
//! says it holds (`agent_session.value` against the agent's `session_id`),
//! never by the agent's or the pane's display name, and only when exactly
//! one pane holds it.
//!
//! Pure: building requests and reading replies. The connection is the
//! daemon's.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use crate::{ActivityObservation, ActivitySource, ReportedActivity, WaitReason};

/// What herdr says of the agent in a pane (`agent_status`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Idle,
    Working,
    /// Stopped for the person: a prompt or a question in its terminal.
    Blocked,
    /// Idle, with finished work nobody has looked at yet.
    Done,
    /// herdr cannot tell, or no agent is detected in the pane; also any
    /// status this build does not know.
    Unknown,
}

impl Status {
    pub fn parse(text: &str) -> Self {
        match text {
            "idle" => Self::Idle,
            "working" => Self::Working,
            "blocked" => Self::Blocked,
            "done" => Self::Done,
            _ => Self::Unknown,
        }
    }

    /// The activity it shows, observed at `now` by herdr: working is
    /// working; blocked is waiting for the person, for a reason herdr does
    /// not name; done and idle are idle. Unknown says nothing.
    pub fn observation(self, now: DateTime<Utc>) -> Option<ActivityObservation> {
        let (activity, reason) = match self {
            Self::Working => (ReportedActivity::Working, None),
            Self::Blocked => (ReportedActivity::Waiting, Some(WaitReason::Other)),
            Self::Done | Self::Idle => (ReportedActivity::Idle, None),
            Self::Unknown => return None,
        };
        Some(ActivityObservation {
            activity,
            observed_at: now,
            reason,
            source: Some(ActivitySource::Herdr),
        })
    }
}

/// One pane, as `pane.list` describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pane {
    pub pane: String,
    /// The provider session herdr says the pane's agent holds.
    pub session: Option<String>,
    pub status: Status,
}

/// The one pane holding a provider session, and its agent's status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Held {
    pub pane: String,
    pub status: Status,
}

/// The id every request carries; herdr echoes it, and names it again when
/// it ends a subscription.
pub const REQUEST_ID: &str = "agentdocker-activity";

/// `pane.list`: every pane in the session, with its agent's status and
/// the provider session it holds.
pub fn list_request() -> Value {
    json!({"id": REQUEST_ID, "method": "pane.list", "params": {}})
}

/// The panes in a `pane.list` reply, or why there are none: herdr's own
/// refusal, or a reply that is not one.
pub fn panes(reply: &str) -> Result<Vec<Pane>, String> {
    let value: Value = serde_json::from_str(reply.trim())
        .map_err(|_| "herdr gave a reply AgentDocker cannot read".to_owned())?;
    if let Some(error) = value.get("error") {
        return Err(refusal(error));
    }
    let panes = value["result"]["panes"]
        .as_array()
        .ok_or_else(|| "herdr's reply lists no panes".to_owned())?;
    Ok(panes
        .iter()
        .filter_map(|pane| {
            Some(Pane {
                pane: pane["pane_id"].as_str()?.to_owned(),
                session: pane["agent_session"]["value"]
                    .as_str()
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned),
                status: pane["agent_status"]
                    .as_str()
                    .map_or(Status::Unknown, Status::parse),
            })
        })
        .collect())
}

/// Which of `sessions` exactly one pane holds, and where. A session no
/// pane holds, or more than one does, is left out: there is no one pane
/// to read it from.
pub fn holding<'a>(
    panes: &[Pane],
    sessions: impl IntoIterator<Item = &'a str>,
) -> BTreeMap<String, Held> {
    sessions
        .into_iter()
        .filter_map(|session| {
            let mut holders = panes
                .iter()
                .filter(|pane| pane.session.as_deref() == Some(session));
            match (holders.next(), holders.next()) {
                (Some(pane), None) => Some((
                    session.to_owned(),
                    Held {
                        pane: pane.pane.clone(),
                        status: pane.status,
                    },
                )),
                _ => None,
            }
        })
        .collect()
}

/// `events.subscribe` for an agent appearing in or leaving any pane — the
/// pane holding a session may have changed — and for status changes in
/// each of `panes`.
pub fn subscribe_request<'a>(panes: impl IntoIterator<Item = &'a str>) -> Value {
    let mut subscriptions = vec![json!({"type": "pane.agent_detected"})];
    subscriptions.extend(
        panes
            .into_iter()
            .map(|pane| json!({"type": "pane.agent_status_changed", "pane_id": pane})),
    );
    json!({
        "id": REQUEST_ID,
        "method": "events.subscribe",
        "params": {"subscriptions": subscriptions},
    })
}

/// One line on a subscription.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Line {
    /// The subscription is open.
    Started,
    /// The agent in a pane changed status.
    Status { pane: String, status: Status },
    /// An agent was detected in a pane, or released from one: which pane
    /// holds a session may have changed.
    Detected { pane: String },
    /// herdr refused the subscription or ended it (`events_lost` when the
    /// reader fell behind, `pane_not_found` for a pane gone meanwhile).
    Refused(String),
    /// Anything else, which is not read.
    Other,
}

pub fn line(text: &str) -> Line {
    let Ok(value) = serde_json::from_str::<Value>(text.trim()) else {
        return Line::Other;
    };
    if let Some(error) = value.get("error") {
        return Line::Refused(refusal(error));
    }
    if value["result"]["type"] == "subscription_started" {
        return Line::Started;
    }
    let pane = || value["data"]["pane_id"].as_str().map(str::to_owned);
    // A subscription names its events with a dot; herdr's event envelope
    // names the same ones with an underscore. Both are read.
    match value["event"].as_str() {
        Some("pane.agent_status_changed" | "pane_agent_status_changed") => match pane() {
            Some(pane) => Line::Status {
                pane,
                status: value["data"]["agent_status"]
                    .as_str()
                    .map_or(Status::Unknown, Status::parse),
            },
            None => Line::Other,
        },
        Some("pane.agent_detected" | "pane_agent_detected") => {
            pane().map_or(Line::Other, |pane| Line::Detected { pane })
        }
        _ => Line::Other,
    }
}

fn refusal(error: &Value) -> String {
    format!(
        "herdr refused: {} ({})",
        error["message"].as_str().unwrap_or("no reason given"),
        error["code"].as_str().unwrap_or("no code")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).unwrap()
    }

    /// herdr's four states become the three a person acts on; what herdr
    /// cannot tell says nothing rather than a guess.
    #[test]
    fn herdr_states_map_to_working_waiting_and_idle() {
        let seen = |text: &str| {
            Status::parse(text)
                .observation(now())
                .map(|o| (o.activity, o.reason, o.source))
        };
        let herdr = Some(ActivitySource::Herdr);
        assert_eq!(
            seen("working"),
            Some((ReportedActivity::Working, None, herdr))
        );
        assert_eq!(
            seen("blocked"),
            Some((ReportedActivity::Waiting, Some(WaitReason::Other), herdr))
        );
        assert_eq!(seen("done"), Some((ReportedActivity::Idle, None, herdr)));
        assert_eq!(seen("idle"), Some((ReportedActivity::Idle, None, herdr)));
        assert_eq!(seen("unknown"), None);
        assert_eq!(seen("thinking"), None, "a newer state is not guessed at");
        assert_eq!(
            Status::Working.observation(now()).unwrap().observed_at,
            now()
        );
    }

    /// `pane.list` as herdr 0.9 answers it: a session is read from the
    /// one pane that holds it, never from a display name.
    #[test]
    fn a_session_is_read_from_the_one_pane_holding_it() {
        let reply = json!({"id": REQUEST_ID, "result": {"type": "pane_list", "panes": [
            {"pane_id": "w1:p1", "agent": "codex", "agent_status": "working",
             "agent_session": {"source": "herdr:codex", "agent": "codex", "kind": "id", "value": "thread-1"}},
            {"pane_id": "w1:p2", "agent": "codex", "agent_status": "blocked",
             "agent_session": {"source": "herdr:codex", "agent": "codex", "kind": "id", "value": "thread-2"}},
            {"pane_id": "w2:p1", "agent": "codex", "agent_status": "done",
             "agent_session": {"source": "herdr:codex", "agent": "codex", "kind": "id", "value": "thread-2"}},
            {"pane_id": "w2:p2", "agent": "thread-3", "title": "thread-3", "agent_status": "idle"},
            {"pane_id": "w3:p1", "agent_status": "idle", "agent_session": null},
        ]}})
        .to_string();
        let panes = panes(&reply).unwrap();
        assert_eq!(panes.len(), 5);
        let held = holding(&panes, ["thread-1", "thread-2", "thread-3", "gone"]);
        assert_eq!(
            held,
            BTreeMap::from([(
                "thread-1".to_owned(),
                Held {
                    pane: "w1:p1".into(),
                    status: Status::Working
                }
            )]),
            "two panes holding one session is none to choose; a name is not a session"
        );
    }

    #[test]
    fn a_refused_or_unreadable_list_is_a_reason() {
        let refused =
            panes(r#"{"id":"agentdocker-activity","error":{"code":"internal","message":"busy"}}"#)
                .unwrap_err();
        assert_eq!(refused, "herdr refused: busy (internal)");
        assert!(panes("not json").is_err());
        assert!(panes(r#"{"id":"x","result":{"type":"ok"}}"#).is_err());
    }

    /// One subscription: agents appearing anywhere, and status changes in
    /// the panes found holding a session.
    #[test]
    fn the_subscription_asks_for_detections_and_each_held_panes_status() {
        assert_eq!(
            subscribe_request(["w1:p1", "w2:p4"]),
            json!({"id": REQUEST_ID, "method": "events.subscribe", "params": {"subscriptions": [
                {"type": "pane.agent_detected"},
                {"type": "pane.agent_status_changed", "pane_id": "w1:p1"},
                {"type": "pane.agent_status_changed", "pane_id": "w2:p4"},
            ]}})
        );
        assert_eq!(
            subscribe_request([]),
            json!({"id": REQUEST_ID, "method": "events.subscribe", "params": {"subscriptions": [
                {"type": "pane.agent_detected"},
            ]}})
        );
        assert_eq!(list_request()["method"], "pane.list");
    }

    /// The lines herdr 0.9.3 sent on a live subscription, verbatim.
    #[test]
    fn subscription_lines_are_read_as_herdr_sends_them() {
        assert_eq!(
            line(r#"{"id":"agentdocker-activity","result":{"type":"subscription_started"}}"#),
            Line::Started
        );
        assert_eq!(
            line(
                r#"{"data":{"agent":"gemini","agent_status":"blocked","pane_id":"w1:p1","workspace_id":"w1"},"event":"pane.agent_status_changed"}"#
            ),
            Line::Status {
                pane: "w1:p1".into(),
                status: Status::Blocked
            }
        );
        assert_eq!(
            line(
                r#"{"data":{"agent":"gemini","pane_id":"w1:p1","type":"pane_agent_detected","workspace_id":"w1"},"event":"pane_agent_detected"}"#
            ),
            Line::Detected {
                pane: "w1:p1".into()
            }
        );
        assert_eq!(
            line(
                r#"{"data":{"agent":"gemini","final_status":"unknown","pane_id":"w1:p1","released":true,"type":"pane_agent_detected","workspace_id":"w1"},"event":"pane_agent_detected"}"#
            ),
            Line::Detected {
                pane: "w1:p1".into()
            },
            "a release is a detection too: the pane no longer holds the agent"
        );
        assert_eq!(
            line(
                r#"{"id":"agentdocker-activity","error":{"code":"events_lost","message":"subscriber fell behind"}}"#
            ),
            Line::Refused("herdr refused: subscriber fell behind (events_lost)".into())
        );
        assert_eq!(
            line(r#"{"data":{"pane_id":"w1:p1"},"event":"pane.scroll_changed"}"#),
            Line::Other
        );
        assert_eq!(
            line(r#"{"data":{},"event":"pane.agent_status_changed"}"#),
            Line::Other
        );
        assert_eq!(line("garbage"), Line::Other);
    }
}
