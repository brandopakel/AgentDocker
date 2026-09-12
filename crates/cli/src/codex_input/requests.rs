//! Provider callbacks route to the registered human, never the peer inbox.
use super::{Client, HUMAN};
use agentdocker_core::{Request, Response};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::time::Duration;

async fn ask(client: &Client, agent: &str, human: &str, question: String) -> Result<String> {
    ensure!(
        question.len() <= 16_000,
        "provider request is too large to review as a question"
    );
    let response = tokio::time::timeout(
        Duration::from_secs(305),
        client.call(&Request::Ask {
            from: agent.into(),
            to: HUMAN.into(),
            question,
            timeout_secs: 300,
        }),
    )
    .await
    .context("human response timed out")??;
    human_answer(response, human)
}

fn human_answer(response: Response, human: &str) -> Result<String> {
    match response {
        Response::Answer { from, text, .. } if from == human => Ok(text),
        _ => bail!("provider requests require an answer from the registered human"),
    }
}

async fn resolve(
    client: &Client,
    agent: &str,
    human: &str,
    thread: &str,
    turn: Option<&str>,
    event: &Value,
) -> Result<Value> {
    let method = event["method"]
        .as_str()
        .context("provider request has no method")?;
    let params = &event["params"];
    ensure!(
        params["threadId"].as_str() == Some(thread)
            && turn.is_some()
            && params["turnId"].as_str() == turn,
        "provider request does not belong to the active input turn"
    );
    match method {
        "item/commandExecution/requestApproval" => {
            // Approve this single callback only. Never accept a session-wide
            // permission or a proposed policy amendment from a textual answer.
            let command = params["command"]
                .as_str()
                .context("Codex did not supply the command to review")?;
            let cwd = params["cwd"]
                .as_str()
                .context("Codex did not supply the command directory")?;
            let kind = params["kind"].as_str().unwrap_or("command");
            ensure!(
                kind == "command",
                "this Codex approval action requires a richer review UI"
            );
            let question = format!(
                "Allow Codex to run this command once?\n\nDirectory: {cwd}\nCommand:\n{command}\n\nReason: {}\n\nReply Allow or Deny.",
                params["reason"].as_str().unwrap_or("Requested by Codex")
            );
            let answer = ask(client, agent, human, question).await?;
            Ok(
                json!({"decision": if answer.trim().eq_ignore_ascii_case("allow") { "accept" } else { "decline" }}),
            )
        }
        "item/tool/requestUserInput" => {
            let questions = params["questions"]
                .as_array()
                .context("Codex supplied no questions")?;
            ensure!(
                !questions.is_empty() && questions.len() <= 8,
                "unsupported question count"
            );
            let mut answers = serde_json::Map::new();
            for question in questions {
                ensure!(
                    question["isSecret"] != true,
                    "secret input cannot be stored in AgentDocker questions"
                );
                let id = question["id"]
                    .as_str()
                    .context("Codex question has no ID")?;
                ensure!(
                    !id.is_empty() && id.len() <= 256 && !answers.contains_key(id),
                    "invalid or repeated question ID"
                );
                let mut prompt = question["question"]
                    .as_str()
                    .context("Codex question has no text")?
                    .to_owned();
                if let Some(options) = question["options"].as_array() {
                    for option in options {
                        prompt.push_str(&format!(
                            "\n- {}: {}",
                            option["label"].as_str().unwrap_or_default(),
                            option["description"].as_str().unwrap_or_default()
                        ));
                    }
                }
                let answer = ask(client, agent, human, prompt).await?;
                answers.insert(id.into(), json!({"answers":[answer]}));
            }
            Ok(json!({"answers":answers}))
        }
        // A file/permission approval without the complete diff or permission
        // review is not actionable. Refuse explicitly; do not infer consent.
        _ => bail!("Codex callback {method} is not yet supported by the input controller"),
    }
}

pub(super) async fn answer(
    client: Client,
    agent: String,
    human: String,
    thread: String,
    turn: Option<String>,
    event: Value,
) -> Value {
    let id = event["id"].clone();
    match resolve(&client, &agent, &human, &thread, turn.as_deref(), &event).await {
        Ok(result) => json!({"id":id,"result":result}),
        Err(error) => {
            eprintln!("Codex request could not be completed: {error:#}");
            json!({"id":id,"error":{"code":-32000,"message":error.to_string()}})
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn peer_answers_cannot_authorize_a_provider_request() {
        let response = |from: &str| Response::Answer {
            message: "question".to_owned().into(),
            from: from.into(),
            text: "Allow".into(),
        };
        assert!(human_answer(response("peer-agent"), "human-id").is_err());
        assert!(human_answer(response("user"), "human-id").is_err());
        assert_eq!(
            human_answer(response("human-id"), "human-id").unwrap(),
            "Allow"
        );
        assert!(human_answer(Response::Ok, "human-id").is_err());
    }
}
