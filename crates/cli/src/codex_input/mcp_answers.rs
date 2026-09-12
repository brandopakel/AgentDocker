//! A tool result is a separate provider receipt, never another user input.
use super::{Client, Ledger, Provider, ledger::Receipt, queue, recovery};
use agentdocker_core::{Destination, Envelope};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::HashSet, time::Duration};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Origin {
    pub human: String,
    pub servers: Vec<String>,
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control)
}

impl Origin {
    pub fn from_overrides(human: String, overrides: &Value) -> Result<Self> {
        // These leaves were constructed by config::overrides, which replaces
        // the executable, arguments and daemon identity for exactly these MCPs.
        let servers = overrides
            .as_object()
            .context("missing MCP overrides")?
            .keys()
            .filter_map(|key| {
                key.strip_prefix("mcp_servers.")?
                    .strip_suffix(".command")
                    .map(str::to_owned)
            })
            .collect();
        let origin = Self { human, servers };
        origin.validate()?;
        Ok(origin)
    }

    pub fn validate(&self) -> Result<()> {
        let mut names = HashSet::new();
        ensure!(
            valid_id(&self.human) && self.servers.len() <= 128,
            "invalid retained MCP origin"
        );
        for name in &self.servers {
            ensure!(
                valid_id(name)
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                    && names.insert(name),
                "invalid or repeated MCP server binding"
            );
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Answer {
    pub receipt: Receipt,
    pub server: String,
    pub answer: Envelope,
    pub acknowledged: bool,
}

impl Answer {
    pub fn validate(&self, agent: &str) -> Result<()> {
        ensure!(
            valid_id(&self.server)
                && valid_id(self.answer.id.as_str())
                && valid_id(self.answer.from.as_str())
                && self.answer.kind == "answer"
                && self.answer.to == Destination::Agent(agent.into())
                && self
                    .answer
                    .reply_to
                    .as_ref()
                    .is_some_and(|id| valid_id(id.as_str()))
                && self.answer.payload["text"].is_string(),
            "invalid retained MCP answer"
        );
        Ok(())
    }

    pub fn same_proof(&self, other: &Self) -> bool {
        self.receipt == other.receipt && self.server == other.server && self.answer == other.answer
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReturnedAnswer {
    answered: bool,
    message_id: String,
    from: String,
    text: String,
}

fn proof(
    origin: &Origin,
    thread: &str,
    turn: &str,
    item: &Value,
    messages: &[Envelope],
    agent: &str,
) -> Result<Option<Answer>> {
    if item["type"] != "mcpToolCall"
        || item["tool"] != "ask_human"
        || item["status"] != "completed"
        || !item["error"].is_null()
        || !origin
            .servers
            .iter()
            .any(|s| item["server"].as_str() == Some(s))
    {
        return Ok(None);
    }
    let Some(content) = item["result"]["content"]
        .as_array()
        .filter(|c| c.len() == 1)
    else {
        return Ok(None);
    };
    if content[0]["type"] != "text" {
        return Ok(None);
    }
    let Some(text) = content[0]["text"].as_str() else {
        return Ok(None);
    };
    // Failed/timed-out MCP calls can contain ordinary error text. They prove no
    // answer consumption; an unmatched queued human answer will pause input.
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return Ok(None);
    };
    if value["answered"] != true {
        return Ok(None);
    }
    let returned: ReturnedAnswer =
        serde_json::from_value(value).context("malformed MCP answer receipt")?;
    ensure!(
        returned.answered && returned.from == origin.human && valid_id(thread) && valid_id(turn),
        "MCP answer receipt has another human or invalid conversation"
    );
    let answer = messages
        .iter()
        .find(|m| m.id.as_str() == returned.message_id)
        .context("MCP answer receipt has no exact retained queue message")?;
    ensure!(
        answer.from.as_str() == returned.from && answer.payload == json!({"text":returned.text}),
        "MCP answer receipt differs from the complete queued answer"
    );
    let proof = Answer {
        receipt: Receipt {
            thread: thread.into(),
            turn: turn.into(),
            item: item["id"]
                .as_str()
                .filter(|id| valid_id(id))
                .context("MCP answer receipt has no item ID")?
                .into(),
        },
        server: item["server"]
            .as_str()
            .context("MCP answer has no server")?
            .into(),
        answer: answer.clone(),
        acknowledged: false,
    };
    proof.validate(agent)?;
    Ok(Some(proof))
}

pub(super) async fn acknowledge(client: &Client, ledger: &mut Ledger) -> Result<()> {
    let ids: Vec<_> = ledger
        .record()
        .mcp_answers
        .iter()
        .filter(|a| !a.acknowledged)
        .map(|a| a.answer.id.clone())
        .collect();
    if ids.is_empty() {
        return Ok(());
    }
    queue(client, ledger, ids.clone()).await?;
    for id in ids {
        ledger.acknowledge_mcp_answer(&id)?;
    }
    Ok(())
}

pub(super) async fn observe(
    client: &Client,
    ledger: &mut Ledger,
    thread: &str,
    turn: &str,
    item: &Value,
) -> Result<()> {
    let Some(origin) = ledger
        .record()
        .attempt
        .as_ref()
        .and_then(|a| a.mcp_origin.clone())
    else {
        return Ok(());
    };
    // Include durable receipts when their messages have already left the inbox.
    let mut messages = queue(client, ledger, Vec::new()).await?;
    for saved in &ledger.record().mcp_answers {
        if !messages.iter().any(|m| m.id == saved.answer.id) {
            messages.push(saved.answer.clone());
        }
    }
    if let Some(answer) = proof(
        &origin,
        thread,
        turn,
        item,
        &messages,
        &ledger.record().binding.agent,
    )? {
        ledger.capture_mcp_answer(answer)?;
        acknowledge(client, ledger).await?;
    }
    Ok(())
}

pub(super) async fn reconcile(
    provider: &mut Provider,
    client: &Client,
    ledger: &mut Ledger,
) -> Result<()> {
    let attempt = ledger
        .record()
        .attempt
        .as_ref()
        .context("no retained input for MCP recovery")?;
    // Legacy input has no persisted proof of which MCP executable was bound.
    // Current configuration cannot establish a historical tool's origin.
    if attempt.mcp_origin.is_none() {
        return acknowledge(client, ledger).await;
    }
    let receipt = attempt
        .receipt
        .clone()
        .context("MCP recovery has no exact input turn")?;
    tokio::time::timeout(Duration::from_secs(60), async {
        let mut cursor: Option<String> = None;
        let mut cursors = HashSet::new();
        for _ in 0..100 {
            let response = provider.request("thread/items/list", json!({"threadId":receipt.thread,"turnId":receipt.turn,"limit":50,"sortDirection":"asc","cursor":cursor})).await?;
            let (items, next) = recovery::page(&response, &mut cursors)?;
            for entry in items {
                ensure!(entry["turnId"].as_str() == Some(&receipt.turn), "MCP history returned another turn");
                if entry["item"]["type"] == "mcpToolCall" && entry["item"]["tool"] == "ask_human" {
                    observe(client, ledger, &receipt.thread, &receipt.turn, &entry["item"]).await?;
                }
            }
            cursor = next;
            if cursor.is_none() { return Ok(()); }
        }
        anyhow::bail!("MCP history exceeded its recovery bound")
    }).await.context("MCP answer recovery exceeded one minute")?
}

#[cfg(test)]
mod tests {
    use super::super::ledger::Binding;
    use super::*;
    fn origin() -> Origin {
        Origin {
            human: "human".into(),
            servers: vec!["coordination".into()],
        }
    }
    fn answer() -> Envelope {
        Envelope::new(
            "human",
            Destination::Agent("owner".into()),
            "answer",
            json!({"text":"Blue"}),
            Some("question".to_owned().into()),
            chrono::Utc::now(),
        )
    }
    fn item(answer: &Envelope) -> Value {
        json!({"type":"mcpToolCall","id":"mcp-item","server":"coordination","tool":"ask_human","status":"completed","error":null,
            "result":{"content":[{"type":"text","text":json!({"answered":true,"message_id":answer.id,"from":answer.from,"text":answer.payload["text"]}).to_string()}]}})
    }
    fn fixture() -> (tempfile::TempDir, Ledger, Envelope) {
        let home = tempfile::tempdir().unwrap();
        let binding = Binding {
            agent: "owner".into(),
            socket: home.path().join("sock"),
            cwd: home.path().into(),
            provider_home: home.path().into(),
        };
        let mut ledger = Ledger::open(home.path(), binding).unwrap();
        ledger.bind_thread("thread".into()).unwrap();
        let input = Envelope::new(
            "peer",
            Destination::Agent("owner".into()),
            "chat",
            json!({"text":"do work"}),
            None,
            chrono::Utc::now(),
        );
        let text = ledger.prepare_bound(&input, Some(origin())).unwrap();
        ledger
            .accept(
                &text,
                Receipt {
                    thread: "thread".into(),
                    turn: "turn".into(),
                    item: "input".into(),
                },
            )
            .unwrap();
        ledger.acknowledge(input.id.as_str()).unwrap();
        (home, ledger, answer())
    }
    #[test]
    fn only_the_bound_completed_tool_and_exact_queued_human_answer_prove_consumption() {
        let answer = answer();
        let item = item(&answer);
        let messages = vec![answer.clone()];
        assert!(
            proof(&origin(), "thread", "turn", &item, &messages, "owner")
                .unwrap()
                .is_some()
        );
        for (field, value) in [
            ("server", "foreign"),
            ("tool", "send_message"),
            ("status", "inProgress"),
            ("status", "failed"),
        ] {
            let mut wrong = item.clone();
            wrong[field] = json!(value);
            assert!(
                proof(&origin(), "thread", "turn", &wrong, &messages, "owner")
                    .unwrap()
                    .is_none()
            );
        }
        assert!(proof(&origin(), "thread", "turn", &item, &[], "owner").is_err());
        for changed in 0..5 {
            let mut wrong = answer.clone();
            match changed {
                0 => wrong.from = "peer".into(),
                1 => wrong.to = Destination::Agent("other".into()),
                2 => wrong.payload = json!({"text":"Green"}),
                3 => wrong.reply_to = None,
                _ => wrong.kind = "chat".into(),
            }
            assert!(proof(&origin(), "thread", "turn", &item, &[wrong], "owner").is_err());
        }
        let mut malformed = item;
        malformed["result"]["content"][0]["text"] = json!("{\"answered\":true}");
        assert!(proof(&origin(), "thread", "turn", &malformed, &messages, "owner").is_err());
        malformed["result"]["content"][0]["text"] = json!("Question timed out");
        assert!(
            proof(&origin(), "thread", "turn", &malformed, &messages, "owner")
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn exact_receipts_survive_restart_and_conflicts_or_unmatched_answers_never_become_input() {
        let (home, mut ledger, answer) = fixture();
        let binding = ledger.record().binding.clone();
        let receipt = proof(
            &origin(),
            "thread",
            "turn",
            &item(&answer),
            std::slice::from_ref(&answer),
            "owner",
        )
        .unwrap()
        .unwrap();
        let mut other = receipt.clone();
        other.receipt.turn = "another-turn".into();
        assert!(ledger.capture_mcp_answer(other).is_err());
        ledger.capture_mcp_answer(receipt.clone()).unwrap();
        assert!(ledger.finish("turn").is_err());
        drop(ledger);
        let mut ledger = Ledger::open(home.path(), binding.clone()).unwrap();
        assert_eq!(
            ledger
                .record()
                .attempt
                .as_ref()
                .unwrap()
                .mcp_origin
                .as_ref()
                .unwrap()
                .servers,
            vec!["coordination"]
        );
        assert!(!ledger.record().mcp_answers[0].acknowledged);
        ledger.capture_mcp_answer(receipt.clone()).unwrap();
        assert_eq!(ledger.record().mcp_answers.len(), 1);
        let mut conflict = receipt;
        conflict.answer.payload = json!({"text":"Green"});
        assert!(ledger.capture_mcp_answer(conflict).is_err());
        ledger.acknowledge_mcp_answer(&answer.id).unwrap();
        ledger.finish("turn").unwrap();
        drop(ledger);
        let mut ledger = Ledger::open(home.path(), binding).unwrap();
        assert!(ledger.prepare_bound(&answer, Some(origin())).is_err());
        let mut late = answer.clone();
        late.id = agentdocker_core::MessageId::generate();
        late.from = "peer".into();
        assert!(
            ledger.prepare_bound(&late, Some(origin())).is_err(),
            "a retired question is never a new input, including a peer reply"
        );
        late.reply_to = Some("unknown-question".to_owned().into());
        late.from = "human".into();
        assert!(ledger.prepare_bound(&late, Some(origin())).is_err());
        late.kind = "chat".into();
        late.reply_to = None;
        assert!(
            ledger.prepare_bound(&late, Some(origin())).is_ok(),
            "ordinary human submissions keep the shared queue path"
        );
    }
    #[test]
    fn legacy_prepared_inputs_have_no_invented_historical_mcp_binding() {
        let (home, ledger, _) = fixture();
        let binding = ledger.record().binding.clone();
        let path = home.path().join("codex-input/owner/delivery.json");
        drop(ledger);
        let mut old: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        old["version"] = json!(4);
        old["attempt"].as_object_mut().unwrap().remove("mcp_origin");
        old.as_object_mut().unwrap().remove("mcp_answers");
        let bytes = serde_json::to_vec(&old).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        let ledger = Ledger::open(home.path(), binding).unwrap();
        assert!(
            ledger
                .record()
                .attempt
                .as_ref()
                .unwrap()
                .mcp_origin
                .is_none()
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
    #[tokio::test]
    #[cfg(unix)]
    async fn recovery_acknowledges_only_the_durable_answer_and_keeps_proof_after_lost_ack_reply() {
        use agentdocker_core::{Request, Response};
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let (home, mut ledger, answer) = fixture();
        let binding = ledger.record().binding.clone();
        let receipt = proof(
            &origin(),
            "thread",
            "turn",
            &item(&answer),
            std::slice::from_ref(&answer),
            "owner",
        )
        .unwrap()
        .unwrap();
        ledger.capture_mcp_answer(receipt).unwrap();
        drop(ledger);
        for lost_reply in [true, false] {
            let mut ledger = Ledger::open(home.path(), binding.clone()).unwrap();
            let listener = tokio::net::UnixListener::bind(&binding.socket).unwrap();
            let serving = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).await.unwrap();
                let request: Request = serde_json::from_str(&line).unwrap();
                if !lost_reply {
                    reader
                        .get_mut()
                        .write_all(
                            (serde_json::to_string(&Response::Messages { messages: vec![] })
                                .unwrap()
                                + "\n")
                                .as_bytes(),
                        )
                        .await
                        .unwrap();
                }
                request
            });
            let client = Client::new(Some(binding.socket.clone())).with_start_timeout(None);
            assert_eq!(acknowledge(&client, &mut ledger).await.is_err(), lost_reply);
            assert!(
                matches!(serving.await.unwrap(), Request::ProviderInbox {agent, acknowledge} if agent == "owner" && acknowledge == vec![answer.id.clone()])
            );
            assert_eq!(ledger.record().mcp_answers[0].acknowledged, !lost_reply);
            std::fs::remove_file(&binding.socket).unwrap();
        }
        let mut ledger = Ledger::open(home.path(), binding.clone()).unwrap();
        let client = Client::new(Some(binding.socket)).with_start_timeout(None);
        acknowledge(&client, &mut ledger).await.unwrap();
        ledger.finish("turn").unwrap();
    }
    #[test]
    fn receipt_rotation_preserves_old_question_routes_and_refuses_overfull_active_turns() {
        let (home, mut ledger, mut answer) = fixture();
        let binding = ledger.record().binding.clone();
        for index in 0..128 {
            answer.id = agentdocker_core::MessageId::generate();
            answer.reply_to = Some(format!("question-{index}").into());
            let mut event = item(&answer);
            event["id"] = json!(format!("item-{index}"));
            let receipt = proof(
                &origin(),
                "thread",
                "turn",
                &event,
                std::slice::from_ref(&answer),
                "owner",
            )
            .unwrap()
            .unwrap();
            ledger.capture_mcp_answer(receipt).unwrap();
            ledger.acknowledge_mcp_answer(&answer.id).unwrap();
        }
        let mut extra = answer.clone();
        extra.id = agentdocker_core::MessageId::generate();
        extra.reply_to = Some("extra-question".to_owned().into());
        let rejected = proof(
            &origin(),
            "thread",
            "turn",
            &item(&extra),
            std::slice::from_ref(&extra),
            "owner",
        )
        .unwrap()
        .unwrap();
        assert!(ledger.capture_mcp_answer(rejected).is_err());
        assert_eq!(ledger.record().mcp_answers.len(), 128);
        ledger.finish("turn").unwrap();
        let input = Envelope::new(
            "peer",
            Destination::Agent("owner".into()),
            "chat",
            json!({"text":"next"}),
            None,
            chrono::Utc::now(),
        );
        let text = ledger.prepare_bound(&input, Some(origin())).unwrap();
        ledger
            .accept(
                &text,
                Receipt {
                    thread: "thread".into(),
                    turn: "next".into(),
                    item: "next-input".into(),
                },
            )
            .unwrap();
        ledger.acknowledge(input.id.as_str()).unwrap();
        let receipt = proof(
            &origin(),
            "thread",
            "next",
            &item(&extra),
            std::slice::from_ref(&extra),
            "owner",
        )
        .unwrap()
        .unwrap();
        ledger.capture_mcp_answer(receipt).unwrap();
        ledger.acknowledge_mcp_answer(&extra.id).unwrap();
        ledger.finish("next").unwrap();
        assert_eq!(ledger.record().mcp_answers.len(), 128);
        drop(ledger);
        let mut ledger = Ledger::open(home.path(), binding).unwrap();
        answer.id = agentdocker_core::MessageId::generate();
        answer.reply_to = Some("question-0".to_owned().into());
        answer.from = "peer".into();
        assert!(ledger.prepare_bound(&answer, Some(origin())).is_err());
    }
}
