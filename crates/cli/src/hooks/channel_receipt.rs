//! Recover a forgotten model ACK from provider-owned transcript evidence.
//! A queued attachment is not evidence of model input. Require the complete
//! channel user record and a real assistant continuation in its parent chain.
//! Unknown/truncated formats fail closed; the explicit MCP receipt remains.
use super::{Backend, HookInput};
use agentdocker_core::{
    AgentRecord, Envelope, InputReceipt, InputReport, ReceivedInput, Request, Response,
};
use anyhow::{Result, ensure};
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

pub(super) mod deferred;
mod history;

const TAIL_BYTES: u64 = 2 * 1024 * 1024;
const MAX_RECORDS: usize = 4096;

pub(super) async fn recover<B: Backend>(
    backend: &B,
    input: &HookInput,
    agent: &AgentRecord,
    home: &std::path::Path,
) -> Result<bool> {
    let Some(started) = agent.process_started_at else {
        return Ok(false);
    };
    if input.agent_id.is_some()
        || input.session_id.is_empty()
        || agent.spec.runtime != "claude-code"
        || agent.spec.labels.get("session_id") != Some(&input.session_id)
        || !agent.status.is_live()
    {
        return Ok(false);
    }
    let Some(path) = input.transcript_path.as_deref() else {
        return Ok(false);
    };
    let Response::Messages { messages } = backend
        .call(Request::DeliveryQueue {
            agent: agent.id.to_string(),
        })
        .await?
    else {
        return Ok(false);
    };
    // The transport offers one head at a time. Never acknowledge another
    // envelope merely because its ID occurs somewhere in the transcript.
    let Some(message) = messages.first() else {
        return Ok(false);
    };
    let tail = history::tail(path)?;
    let now = Utc::now();
    if !tail
        .as_deref()
        .is_some_and(|tail| consumed(tail, &input.session_id, started, now, message))
        && !history::find(home, path, agent, message, |window| {
            consumed(window, &input.session_id, started, now, message)
        })?
    {
        return Ok(false);
    }
    crate::input_status::report(
        backend,
        agent.id.as_str(),
        Some(started),
        InputReport::Received {
            input: ReceivedInput {
                messages: vec![message.id.clone()],
                receipt: InputReceipt::ClaudeChannel,
            },
        },
    )
    .await?;
    // Commit the receipt before ACK. A refused report/timeout leaves the head
    // untouched; a refused ACK can retry the same evidence without redispatch.
    ensure!(
        matches!(
            backend
                .call(Request::AckInbox {
                    agent: agent.id.to_string(),
                    messages: vec![message.id.clone()],
                })
                .await?,
            Response::Ok
        ),
        "channel transcript receipt ACK refused"
    );
    Ok(true)
}

fn consumed(
    tail: &str,
    session: &str,
    started: DateTime<Utc>,
    now: DateTime<Utc>,
    message: &Envelope,
) -> bool {
    // Unrelated historical records, including compaction copies, are not in
    // the proof chain. Start at the first candidate envelope in this window.
    let Some(candidate) = tail.find(message.id.as_str()) else {
        return false;
    };
    let start = tail[..candidate].rfind('\n').map_or(0, |i| i + 1);
    let mut lineage: HashMap<String, DateTime<Utc>> = HashMap::new();
    let mut seen = HashSet::new();
    for (index, line) in tail[start..].lines().enumerate() {
        if index >= MAX_RECORDS {
            return false;
        }
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(uuid) = record["uuid"].as_str().filter(|s| !s.is_empty()) else {
            continue;
        };
        if !seen.insert(uuid.to_owned()) {
            return false;
        }
        let Some(at) = record["timestamp"]
            .as_str()
            .and_then(|s| s.parse::<DateTime<Utc>>().ok())
        else {
            continue;
        };
        if record["sessionId"] != session
            || record["isSidechain"] != false
            || at < started
            || at > now
        {
            continue;
        }
        if record["type"] == "user" {
            // Do not infer receipt from an unrelated human turn, a pasted
            // channel tag, a tool result, or a compacted historical summary.
            lineage.clear();
            if record["origin"]["kind"] == "channel"
                && record["origin"]["server"] == "agentdocker"
                && record["isMeta"] == true
                && record["message"]["role"] == "user"
                && record["message"]["content"]
                    .as_str()
                    .is_some_and(|text| matches_message(text, message))
            {
                lineage.insert(uuid.to_owned(), at);
            }
            continue;
        }
        // Busy delivery is a provider queued_command attachment rather than
        // a user record. The attachment alone still proves nothing: it must
        // have the full channel origin/body and a subsequent model response.
        let attachment = &record["attachment"];
        if record["type"] == "attachment"
            && attachment["type"] == "queued_command"
            && attachment["commandMode"] == "prompt"
            && attachment["origin"]["kind"] == "channel"
            && attachment["origin"]["server"] == "agentdocker"
            && attachment["isMeta"] == true
            && attachment["prompt"]
                .as_str()
                .is_some_and(|text| matches_message(text, message))
        {
            lineage.clear();
            lineage.insert(uuid.to_owned(), at);
            continue;
        }
        let Some(parent_at) = record["parentUuid"]
            .as_str()
            .and_then(|parent| lineage.get(parent))
        else {
            continue;
        };
        if at < *parent_at {
            continue;
        }
        if record["type"] == "assistant" {
            return record["isApiErrorMessage"] != true
                && record["message"]["role"] == "assistant"
                && record["requestId"].as_str().is_some_and(|s| !s.is_empty())
                && record["message"]["id"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty())
                && record["message"]["model"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty() && !s.starts_with('<'))
                && record["message"]["content"]
                    .as_array()
                    .is_some_and(|parts| {
                        parts.iter().any(|part| {
                            matches!(
                                part["type"].as_str(),
                                Some("text" | "thinking" | "tool_use")
                            )
                        })
                    });
        }
        if record["type"] == "attachment" {
            lineage.insert(uuid.to_owned(), at);
        }
    }
    false
}

fn matches_message(text: &str, message: &Envelope) -> bool {
    let Some((header, body)) = text.split_once('\n') else {
        return false;
    };
    let Some(body) = body.strip_suffix("\n</channel>") else {
        return false;
    };
    let Some(attributes) = attributes(header) else {
        return false;
    };
    attributes.get("source").is_some_and(|v| v == "agentdocker")
        && attributes
            .get("message_id")
            .is_some_and(|v| v == message.id.as_str())
        && attributes
            .get("from_agent")
            .is_some_and(|v| v == message.from.as_str())
        && attributes.get("kind").is_some_and(|v| v == &message.kind)
        && attributes
            .get("sent_at")
            .and_then(|v| v.parse::<DateTime<Utc>>().ok())
            == Some(message.sent_at)
        && attributes
            .get("destination")
            .and_then(|v| serde_json::from_str::<agentdocker_core::Destination>(v).ok())
            .as_ref()
            == Some(&message.to)
        && attributes.get("reply_to").map(String::as_str)
            == message.reply_to.as_ref().map(|id| id.as_str())
        && serde_json::from_str::<Value>(body).is_ok_and(|payload| payload == message.payload)
}

// The provider's channel wrapper uses double-quoted XML-escaped metadata.
// Accept that grammar only; duplicate/unknown entities cannot create receipts.
fn attributes(header: &str) -> Option<HashMap<String, String>> {
    let mut rest = header.strip_prefix("<channel ")?.strip_suffix('>')?;
    let mut result = HashMap::new();
    while !rest.is_empty() {
        let (name, value) = rest.split_once("=\"")?;
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return None;
        }
        let (value, after) = value.split_once('"')?;
        let mut decoded = String::new();
        let mut value = value;
        while let Some((before, entity)) = value.split_once('&') {
            decoded.push_str(before);
            let (entity, after) = entity.split_once(';')?;
            decoded.push(match entity {
                "amp" => '&',
                "quot" => '"',
                "apos" => '\'',
                "lt" => '<',
                "gt" => '>',
                _ => return None,
            });
            value = after;
        }
        decoded.push_str(value);
        if result.insert(name.to_owned(), decoded).is_some() {
            return None;
        }
        rest = if after.is_empty() {
            after
        } else {
            after.strip_prefix(' ')?
        };
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::{AgentSpec, AgentStatus, Destination};
    use serde_json::json;
    use std::cell::RefCell;

    fn fixture() -> (Envelope, Vec<Value>, DateTime<Utc>) {
        let now: DateTime<Utc> = "2026-09-18T06:45:00Z".parse().unwrap();
        let envelope = Envelope::new(
            "human",
            Destination::Project("project".into()),
            "message",
            json!({"text":"Pause & keep the draft — please"}),
            None,
            now - chrono::Duration::minutes(2),
        );
        let escape = |s: String| s.replace('&', "&amp;").replace('"', "&quot;");
        let content = format!(
            "<channel source=\"agentdocker\" message_id=\"{}\" from_agent=\"human\" kind=\"message\" sent_at=\"{}\" destination=\"{}\">\n{}\n</channel>",
            envelope.id,
            envelope.sent_at.to_rfc3339(),
            escape(serde_json::to_string(&envelope.to).unwrap()),
            envelope.payload
        );
        let mut user = json!({"uuid":"input", "parentUuid":"earlier", "type":"user", "isMeta":true,
            "origin":{"kind":"channel","server":"agentdocker"}, "message":{"role":"user","content":content}});
        let mut attachment =
            json!({"uuid":"attachment", "parentUuid":"input", "type":"attachment"});
        let mut assistant = json!({"uuid":"response", "parentUuid":"attachment", "type":"assistant", "requestId":"request",
            "message":{"id":"response-id","role":"assistant","model":"claude-model","content":[{"type":"tool_use","name":"Bash"}]}});
        for value in [&mut user, &mut attachment, &mut assistant] {
            value["sessionId"] = json!("session");
            value["isSidechain"] = json!(false);
            value["timestamp"] = json!(now - chrono::Duration::seconds(1));
        }
        (envelope, vec![user, attachment, assistant], now)
    }

    fn text(records: &[Value]) -> String {
        records
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn full_channel_input_and_assistant_lineage_confirm_receipt_without_a_model_ack_tool() {
        let (message, records, now) = fixture();
        assert!(consumed(
            &text(&records),
            "session",
            now - chrono::Duration::minutes(1),
            now,
            &message
        ));
        assert!(!consumed(
            &text(&records[..2]),
            "session",
            now - chrono::Duration::minutes(1),
            now,
            &message
        ));
    }

    #[test]
    fn queued_attachments_pasted_tags_errors_and_unrelated_turns_are_not_receipts() {
        let (message, records, now) = fixture();
        let changes = [
            (0, "/origin/kind", json!("human")),
            (0, "/origin/server", json!("different-server")),
            (0, "/type", json!("attachment")),
            (0, "/isMeta", json!(false)),
            (0, "/isSidechain", json!(true)),
            (0, "/sessionId", json!("different-session")),
            (0, "/timestamp", json!(now - chrono::Duration::hours(1))),
            (0, "/timestamp", json!(now + chrono::Duration::hours(1))),
            (1, "/parentUuid", json!("unrelated")),
            (1, "/type", json!("user")),
            (1, "/isSidechain", json!(true)),
            (2, "/message/model", json!("<synthetic>")),
            (2, "/requestId", json!(null)),
            (2, "/message/content", json!([])),
        ];
        for (index, path, replacement) in changes {
            let mut changed = records.clone();
            *changed[index].pointer_mut(path).unwrap() = replacement;
            assert!(
                !consumed(
                    &text(&changed),
                    "session",
                    now - chrono::Duration::minutes(1),
                    now,
                    &message
                ),
                "{index} {path}"
            );
        }
        let mut error = records.clone();
        error[2]["isApiErrorMessage"] = json!(true);
        assert!(!consumed(
            &text(&error),
            "session",
            now - chrono::Duration::minutes(1),
            now,
            &message
        ));
        let mut duplicate = records.clone();
        duplicate.insert(2, records[1].clone());
        assert!(!consumed(
            &text(&duplicate),
            "session",
            now - chrono::Duration::minutes(1),
            now,
            &message
        ));
    }

    #[test]
    fn busy_channel_attachment_needs_an_actual_assistant_continuation() {
        let (message, mut records, now) = fixture();
        let content = records[0]["message"]["content"].clone();
        records[0]["type"] = json!("attachment");
        records[0].as_object_mut().unwrap().remove("origin");
        records[0].as_object_mut().unwrap().remove("message");
        records[0]["attachment"] = json!({"type":"queued_command", "commandMode":"prompt", "origin":{"kind":"channel","server":"agentdocker"}, "isMeta":true, "prompt":content});
        let start = now - chrono::Duration::minutes(1);
        assert!(!consumed(
            &text(&records[..2]),
            "session",
            start,
            now,
            &message
        ));
        assert!(consumed(&text(&records), "session", start, now, &message));
        records[0]["attachment"]["origin"]["kind"] = json!("human");
        assert!(!consumed(&text(&records), "session", start, now, &message));
    }

    #[test]
    fn receipt_needs_exact_body_sender_destination_and_reply_metadata() {
        let (message, records, now) = fixture();
        let original = records[0]["message"]["content"].as_str().unwrap();
        for altered in [
            original.replace("Pause &", "Changed &"),
            original.replace("human", "other"),
            original.replace("project", "other"),
            original.replace("message_id=", "missing_id="),
            original.replace("source=", "source=\"other\" source="),
            original.replace("&quot;", "&unknown;"),
            original.replace("</channel>", "</truncated>"),
            original.replace("kind=", "reply_to=\"unexpected\" kind="),
        ] {
            let mut changed = records.clone();
            changed[0]["message"]["content"] = json!(altered);
            assert!(!consumed(
                &text(&changed),
                "session",
                now - chrono::Duration::minutes(1),
                now,
                &message
            ));
        }
    }

    struct BackendFixture {
        queue: RefCell<Vec<Envelope>>,
        calls: RefCell<Vec<&'static str>>,
        refuse_report: bool,
        refuse_ack: bool,
    }
    impl Backend for BackendFixture {
        async fn call(&self, request: Request) -> Result<Response> {
            match request {
                Request::DeliveryQueue { .. } => Ok(Response::Messages {
                    messages: self.queue.borrow().clone(),
                }),
                Request::ReportInput {
                    report: InputReport::Received { input },
                    ..
                } => {
                    assert_eq!(input.messages, vec![self.queue.borrow()[0].id.clone()]);
                    self.calls.borrow_mut().push("receipt");
                    if self.refuse_report {
                        anyhow::bail!("unavailable")
                    }
                    Ok(Response::Ok)
                }
                Request::AckInbox { messages, .. } => {
                    self.calls.borrow_mut().push("ack");
                    if self.refuse_ack {
                        anyhow::bail!("unavailable")
                    }
                    self.queue
                        .borrow_mut()
                        .retain(|message| !messages.contains(&message.id));
                    Ok(Response::Ok)
                }
                other => panic!("unexpected {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn recovered_receipt_commits_before_ack_and_does_not_consume_the_next_pause() {
        let (message, mut records, _) = fixture();
        let now = Utc::now();
        for record in &mut records {
            record["timestamp"] = json!(now);
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        std::fs::write(&path, format!("{}\n", text(&records))).unwrap();
        let mut agent = AgentRecord::new(
            AgentSpec {
                runtime: "claude-code".into(),
                ..AgentSpec::default()
            },
            false,
            now,
        );
        agent.status = AgentStatus::Running;
        agent
            .spec
            .labels
            .insert("session_id".into(), "session".into());
        agent.process_started_at = Some(now - chrono::Duration::minutes(1));
        let event = HookInput {
            session_id: "session".into(),
            transcript_path: Some(path),
            ..HookInput::default()
        };
        let mut next = message.clone();
        next.id = agentdocker_core::MessageId::generate();
        let child_backend = BackendFixture {
            queue: RefCell::new(vec![message.clone()]),
            calls: RefCell::new(vec![]),
            refuse_report: false,
            refuse_ack: false,
        };
        let mut child = event.clone();
        child.agent_id = Some("child".into());
        assert!(
            !recover(&child_backend, &child, &agent, directory.path())
                .await
                .unwrap()
        );
        assert!(child_backend.calls.borrow().is_empty());
        assert_eq!(child_backend.queue.borrow().len(), 1);
        for (refuse_report, refuse_ack, expected_calls) in [
            (true, false, vec!["receipt"]),
            (false, true, vec!["receipt", "ack"]),
            (false, false, vec!["receipt", "ack"]),
        ] {
            let backend = BackendFixture {
                queue: RefCell::new(vec![message.clone(), next.clone()]),
                calls: RefCell::new(vec![]),
                refuse_report,
                refuse_ack,
            };
            assert_eq!(
                recover(&backend, &event, &agent, directory.path())
                    .await
                    .is_err(),
                refuse_report || refuse_ack
            );
            assert_eq!(*backend.calls.borrow(), expected_calls);
            assert_eq!(
                backend.queue.borrow().len(),
                if refuse_report || refuse_ack { 2 } else { 1 }
            );
            if !refuse_report && !refuse_ack {
                recover(&backend, &event, &agent, directory.path())
                    .await
                    .unwrap();
                assert_eq!(*backend.calls.borrow(), expected_calls);
                assert_eq!(backend.queue.borrow()[0].id, next.id);
            }
        }
    }

    #[test]
    fn oversized_truncated_and_special_transcripts_fail_closed() {
        let (message, records, now) = fixture();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        std::fs::write(
            &path,
            format!("{}\n{}", text(&records), "x".repeat(TAIL_BYTES as usize)),
        )
        .unwrap();
        let tail = history::tail(&path).unwrap().unwrap_or_default();
        assert!(!consumed(
            &tail,
            "session",
            now - chrono::Duration::minutes(1),
            now,
            &message
        ));
        let link = directory.path().join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(history::tail(&link).is_err());
        assert!(history::tail(directory.path()).is_err());
    }
}
