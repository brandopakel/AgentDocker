//! A one-time managed Codex response whose values never enter the delivery ledger.
//! Only the request identity and uncertain-write fence survive a restart.
use super::{Client, Ledger, Provider, call, review};
use agentdocker_core::{
    AgentRecord, ProcessIdentity, QuestionOption, QuestionPresentation, Request, Response,
    secret::{SecretAnswers, SecretField, SecretReply, SecretReviewSpec, SecretText},
};
use anyhow::{Context, Result, anyhow, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Fence {
    pub id: Value,
    pub thread: String,
    pub turn: String,
    pub response_attempted: bool,
}

impl Fence {
    pub fn valid(&self, thread: Option<&str>, turn: Option<&str>) -> bool {
        review::valid_request_id(&self.id)
            && thread == Some(self.thread.as_str())
            && turn == Some(self.turn.as_str())
            && !self.thread.is_empty()
            && !self.turn.is_empty()
    }
}

/// Includes the whole mixed bundle; an ordinary field must not escape into
/// persisted question/answer messages when another field is marked secret.
pub(super) fn contains_secret(event: &Value) -> bool {
    event["method"] == "item/tool/requestUserInput"
        && event["params"]["questions"]
            .as_array()
            .is_some_and(|q| q.iter().any(|f| f["isSecret"] == true))
}

fn plan(event: &Value, thread: &str, turn: Option<&str>) -> Result<(Fence, SecretReviewSpec)> {
    ensure!(
        review::valid_request_id(&event["id"]),
        "invalid temporary request identity"
    );
    let p = &event["params"];
    let turn = turn.context("temporary input requires the active turn")?;
    ensure!(
        p["threadId"].as_str() == Some(thread) && p["turnId"].as_str() == Some(turn),
        "temporary input has another conversation or turn"
    );
    let questions = p["questions"]
        .as_array()
        .context("missing temporary questions")?;
    ensure!(
        questions.len() <= agentdocker_core::secret::MAX_FIELDS,
        "too many temporary questions"
    );
    let mut fields = Vec::new();
    for q in questions {
        let is_secret = q
            .get("isSecret")
            .map(|v| v.as_bool().context("invalid secret flag"))
            .transpose()?
            .unwrap_or(false);
        let id = q["id"]
            .as_str()
            .context("missing temporary field identity")?
            .to_owned();
        let mut question = q["question"]
            .as_str()
            .context("missing temporary question")?
            .to_owned();
        if let Some(header) = q.get("header") {
            let header = header
                .as_str()
                .context("invalid temporary question heading")?;
            ensure!(header.len() <= 256, "temporary heading exceeds its bound");
            if !header.is_empty() {
                question = format!("{header}\n\n{question}");
            }
        }
        if !q["options"].is_null() {
            let options = q["options"]
                .as_array()
                .context("invalid temporary choices")?;
            ensure!(options.len() <= 16, "too many temporary choices");
            if !options.is_empty() {
                let options = options
                    .iter()
                    .map(|v| {
                        Ok(QuestionOption {
                            label: v["label"]
                                .as_str()
                                .context("invalid temporary choice")?
                                .into(),
                            description: v["description"]
                                .as_str()
                                .context("invalid temporary choice description")?
                                .into(),
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                let view = QuestionPresentation::Choices { question, options };
                question = view.text();
                ensure!(
                    view.valid_for(&question),
                    "temporary choices cannot be completely reviewed"
                );
            }
        }
        fields.push(SecretField {
            id,
            question,
            is_secret,
        });
    }
    let spec = SecretReviewSpec {
        thread: thread.into(),
        turn: turn.into(),
        fields,
    };
    ensure!(
        spec.valid(),
        "invalid or oversized temporary question bundle"
    );
    Ok((
        Fence {
            id: event["id"].clone(),
            thread: thread.into(),
            turn: turn.into(),
            response_attempted: false,
        },
        spec,
    ))
}

// Refuse only the new request. Never replace, persist or cancel the review
// already holding the input queue, and never reflect provider text in errors.
pub(super) fn overlap_response(id: &Value) -> Value {
    json!({"id":id,"error":{"code":-32000,
        "message":"another input review is still pending"}})
}

struct Live {
    review: String,
    token: SecretText,
}

pub(super) struct Session {
    owner: ProcessIdentity,
    live: Option<Live>,
}

impl Session {
    pub fn new(agent: &AgentRecord) -> Result<Self> {
        Ok(Self {
            owner: ProcessIdentity {
                pid: agent
                    .pid
                    .context("temporary input has no process identity")?,
                started_at: agent
                    .process_started_at
                    .context("temporary input has no process birth")?,
            },
            live: None,
        })
    }

    pub async fn open(
        &mut self,
        client: &Client,
        ledger: &mut Ledger,
        human: &str,
        thread: &str,
        turn: Option<&str>,
        event: &Value,
    ) -> Result<Option<Value>> {
        let planned = plan(event, thread, turn);
        let Ok((fence, request)) = planned else {
            // Do not print provider fields or reflect malformed content in errors.
            return Ok(Some(json!({"id":event["id"],"error":{"code":-32000,
                "message":"temporary input cannot be completely reviewed"}})));
        };
        if self.live.is_some()
            || ledger.record().secret_review.is_some()
            || !ledger.record().reviews.is_empty()
        {
            return Ok(Some(overlap_response(&event["id"])));
        }
        // Durable identity first, before even opening an in-memory route. If the
        // IPC response is lost there is no reopen/resubmit attempt on restart.
        ledger.open_secret_review(fence)?;
        let token = SecretText::new(format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        ))
        .map_err(anyhow::Error::msg)?;
        let response = call(
            client,
            Request::OpenSecretReview {
                agent: ledger.record().binding.agent.clone(),
                owner: self.owner.clone(),
                recipient: human.into(),
                request,
                token: token.clone(),
            },
        )
        .await
        .map_err(|_| anyhow!("temporary input route was not confirmed; input remains paused"))?;
        let Response::SecretReview { review } = response else {
            return Err(anyhow!(
                "daemon did not confirm the temporary input route; input remains paused"
            ));
        };
        self.live = Some(Live {
            review: review.id,
            token,
        });
        eprintln!(
            "Codex is waiting for temporary input in AgentDocker. This terminal discards new typing; after the request closes, press Enter once to resume your saved draft."
        );
        Ok(None)
    }

    pub async fn poll(
        &mut self,
        client: &Client,
        provider: &mut Provider,
        ledger: &mut Ledger,
    ) -> Result<()> {
        let Some(fence) = ledger.record().secret_review.as_ref() else {
            return Ok(());
        };
        if fence.response_attempted {
            return Ok(());
        }
        let live = self
            .live
            .as_ref()
            .context("temporary input lost its live owner; automatic replay is refused")?;
        let reply = call(
            client,
            Request::PollSecretReview {
                agent: ledger.record().binding.agent.clone(),
                owner: self.owner.clone(),
                review: live.review.clone(),
                token: live.token.clone(),
                close: false,
            },
        )
        .await
        .map_err(|_| {
            anyhow!("temporary answer handoff was not confirmed; automatic replay is refused")
        })?;
        let response = match reply {
            Response::SecretReply {
                reply: SecretReply::Waiting,
            } => return Ok(()),
            Response::SecretReply {
                reply: SecretReply::Answered { answers },
            } => answer(&fence.id, answers),
            Response::SecretReply {
                reply: SecretReply::Closed,
            } => json!({"id":fence.id,
                "error":{"code":-32000,"message":"human question cancelled or expired"}}),
            _ => {
                return Err(anyhow!(
                    "temporary answer handoff returned an unsupported response"
                ));
            }
        };
        // The answer itself remains only in this stack frame. A crash before or
        // after either boundary leaves a fence and can never replay the value.
        ledger.secret_response_attempted()?;
        self.live = None;
        provider.send(&response).await.map_err(|_| {
            anyhow!("temporary provider response is unconfirmed; automatic replay is refused")
        })
    }

    async fn close_route(&mut self, client: &Client, ledger: &Ledger) -> Result<()> {
        let Some(live) = self.live.take() else {
            return Ok(());
        };
        ensure!(
            matches!(
                call(
                    client,
                    Request::PollSecretReview {
                        agent: ledger.record().binding.agent.clone(),
                        owner: self.owner.clone(),
                        review: live.review,
                        token: live.token,
                        close: true,
                    }
                )
                .await?,
                Response::SecretReply {
                    reply: SecretReply::Closed
                }
            ),
            "temporary route closure was not confirmed"
        );
        Ok(())
    }

    pub async fn resolved(
        &mut self,
        client: &Client,
        ledger: &mut Ledger,
        params: &Value,
    ) -> Result<()> {
        let Some(fence) = ledger.record().secret_review.as_ref() else {
            return Ok(());
        };
        if params["requestId"] != fence.id || params["threadId"].as_str() != Some(&fence.thread) {
            return Ok(());
        }
        self.close_route(client, ledger).await?;
        ledger.close_secret_review()?;
        eprintln!(
            "Codex resolved the temporary input request. Press Enter in the managed terminal once to resume its saved draft."
        );
        Ok(())
    }

    pub async fn turn_ended(&mut self, client: &Client, ledger: &mut Ledger) -> Result<()> {
        if ledger.record().secret_review.is_none() {
            return Ok(());
        }
        self.close_route(client, ledger).await?;
        // A terminal turn is not proof that a submitted secret was accepted.
        ensure!(
            !ledger
                .record()
                .secret_review
                .as_ref()
                .is_some_and(|f| f.response_attempted),
            "Codex ended without confirming temporary input; retained metadata requires inspection"
        );
        ledger.close_secret_review()
    }
}

fn answer(id: &Value, values: SecretAnswers) -> Value {
    let answers: serde_json::Map<String, Value> = values
        .into_values()
        .into_iter()
        .map(|(id, value)| (id, json!({"answers":[value.expose()]})))
        .collect();
    json!({"id":id,"result":{"answers":answers}})
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event() -> Value {
        json!({"id":17,"method":"item/tool/requestUserInput","params":{"threadId":"thread","turnId":"turn",
            "questions":[{"id":"ordinary","header":"Label","question":"Name?","isSecret":false},
            {"id":"private","header":"Credential","question":"Temporary value?","isSecret":true,
             "options":[{"label":"Supply","description":"One time"},{"label":"Decline","description":"Cancel"}]}]}})
    }
    #[test]
    fn whole_secret_bundle_is_separate_and_its_fence_never_contains_prompts_or_answers() {
        let event = event();
        assert!(contains_secret(&event));
        let (fence, spec) = plan(&event, "thread", Some("turn")).unwrap();
        assert_eq!(spec.fields.len(), 2);
        assert!(spec.fields[1].question.contains("One time"));
        let saved = serde_json::to_string(&fence).unwrap();
        for excluded in ["Temporary value", "Credential", "Name?", "Supply"] {
            assert!(!saved.contains(excluded));
        }
        let bundle = SecretAnswers::new(std::collections::BTreeMap::from([
            (
                "private".into(),
                SecretText::new("invented-private-value".into()).unwrap(),
            ),
            (
                "ordinary".into(),
                SecretText::new("also-temporary".into()).unwrap(),
            ),
        ]))
        .unwrap();
        assert!(bundle.matches(&spec.fields));
        let wire = answer(&fence.id, bundle);
        assert_eq!(
            wire["result"]["answers"]["private"]["answers"][0],
            "invented-private-value"
        );
        assert_eq!(
            wire["result"]["answers"]["ordinary"]["answers"][0],
            "also-temporary"
        );
        assert!(plan(&event, "other", Some("turn")).is_err());
        assert!(plan(&event, "thread", None).is_err());
    }
    #[tokio::test]
    async fn overlapping_reviews_refuse_only_the_new_request_without_writing_or_ipc() {
        use super::super::{
            file_changes,
            ledger::{Binding, Receipt},
            requests,
        };
        use agentdocker_core::{Destination, Envelope};
        for order in 0..5 {
            let home = tempfile::tempdir().unwrap();
            let binding = Binding {
                agent: "owner".into(),
                socket: home.path().join("absent.sock"),
                cwd: home.path().into(),
                provider_home: home.path().into(),
            };
            let client = Client::new(Some(binding.socket.clone())).with_start_timeout(None);
            let mut ledger = Ledger::open(home.path(), binding.clone()).unwrap();
            ledger.bind_thread("thread".into()).unwrap();
            let message = Envelope::new(
                "peer",
                Destination::Agent("owner".into()),
                "chat",
                json!({"text":"original input"}),
                None,
                chrono::Utc::now(),
            );
            let input = ledger.prepare(&message).unwrap();
            ledger
                .accept(
                    &input,
                    Receipt {
                        thread: "thread".into(),
                        turn: "turn".into(),
                        item: "item".into(),
                    },
                )
                .unwrap();
            ledger.acknowledge(message.id.as_str()).unwrap();
            let ordinary = json!({"id":31,"method":"item/commandExecution/requestApproval",
                "params":{"threadId":"thread","turnId":"turn","command":"original command","cwd":"/owned"}});
            let mut session = Session {
                owner: ProcessIdentity {
                    pid: std::process::id(),
                    started_at: chrono::Utc::now(),
                },
                live: None,
            };
            if order == 0 {
                let pending = review::Pending::plan(
                    &ordinary,
                    "thread",
                    Some("turn"),
                    "human",
                    chrono::Utc::now(),
                )
                .unwrap();
                ledger
                    .update_reviews(|reviews, _| {
                        reviews.push(pending);
                        Ok(true)
                    })
                    .unwrap();
            } else if order != 3 {
                ledger
                    .open_secret_review(plan(&event(), "thread", Some("turn")).unwrap().0)
                    .unwrap();
            }
            // The live-only case independently covers a route awaiting teardown.
            if order == 2 || order == 3 {
                session.live = Some(Live {
                    review: "first-route".into(),
                    token: SecretText::new("private-capability".into()).unwrap(),
                });
            }
            let before = serde_json::to_value(ledger.record()).unwrap();
            let mut incoming = if order == 1 { ordinary } else { event() };
            incoming["id"] = json!(99);
            let response = if order == 1 {
                requests::open(
                    &client,
                    &mut ledger,
                    "human",
                    "thread",
                    Some("turn"),
                    incoming,
                    &mut file_changes::Reviews::default(),
                )
                .await
            } else {
                session
                    .open(
                        &client,
                        &mut ledger,
                        "human",
                        "thread",
                        Some("turn"),
                        &incoming,
                    )
                    .await
            }
            .unwrap()
            .unwrap();
            assert_eq!(response["id"], 99);
            assert_eq!(response["error"]["code"], -32000);
            assert_eq!(
                response["error"]["message"],
                "another input review is still pending"
            );
            assert_eq!(serde_json::to_value(ledger.record()).unwrap(), before);
            if order == 2 || order == 3 {
                assert_eq!(session.live.as_ref().unwrap().review, "first-route");
            }
            drop(ledger);
            let ledger = Ledger::open(home.path(), binding).unwrap();
            assert_eq!(serde_json::to_value(ledger.record()).unwrap(), before);
            let saved = serde_json::to_string(ledger.record()).unwrap();
            assert!(!saved.contains("Temporary value") && !saved.contains("private-capability"));
        }
    }

    #[test]
    fn invalid_mixed_bundles_never_downgrade_to_ordinary_questions() {
        for mutation in 0..5 {
            let mut e = event();
            match mutation {
                0 => e["params"]["questions"][0]["isSecret"] = json!("false"),
                1 => e["params"]["questions"][0]["id"] = json!("private"),
                2 => e["params"]["questions"][1]["options"] = json!(false),
                3 => e["params"]["questions"][1]["question"] = json!("x".repeat(16_001)),
                _ => e["params"]["turnId"] = json!("wrong"),
            }
            assert!(contains_secret(&e));
            assert!(plan(&e, "thread", Some("turn")).is_err());
            assert!(
                review::Pending::plan(&e, "thread", Some("turn"), "human", chrono::Utc::now())
                    .is_err()
            );
        }
    }
}
