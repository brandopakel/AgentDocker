//! Typed review routes share the ordinary inbox. A response is retained before
//! writing it to Codex and is acknowledged only after that request resolves.
use agentdocker_core::{
    Destination, Envelope, EventKind, MessageId, QuestionOption, QuestionPresentation,
};
use anyhow::{Context, Result, bail, ensure};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashSet;

pub(super) const MAX_QUESTIONS: usize = 8;
pub(super) const RETAINED: usize = 8;
const MAX_TEXT: usize = 16_000;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Kind {
    Command,
    UserInput,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum Closure {
    #[default]
    Open,
    Answered {
        message: MessageId,
    },
    Cancelled,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Question {
    field: String,
    pub text: String,
    pub message: Option<MessageId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presentation: Option<QuestionPresentation>,
    answer: Option<Envelope>,
    #[serde(default)]
    closure: Closure,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Pending {
    pub id: Value,
    pub thread: String,
    pub turn: String,
    pub human: String,
    kind: Kind,
    pub expires_at: DateTime<Utc>,
    pub questions: Vec<Question>,
    // Persisted before attempting the write. A pipe write alone is not a
    // provider receipt, and this value is never automatically replayed.
    pub response: Option<Value>,
    pub extra_answers: Vec<Envelope>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Outcome {
    Resolved,
    Cancelled,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Closed {
    pub request: Pending,
    pub outcome: Outcome,
    pub acknowledged: bool,
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control)
}

pub(super) fn valid_request_id(value: &Value) -> bool {
    value.as_str().is_some_and(valid_id) || value.as_i64().is_some() || value.as_u64().is_some()
}

fn text(value: &Value) -> Result<&str> {
    let text = value.as_str().context("Codex supplied no review text")?;
    ensure!(
        !text.trim().is_empty() && text.len() <= MAX_TEXT,
        "unsupported review text size"
    );
    Ok(text)
}

fn answer_text(answer: &Envelope) -> Result<&str> {
    let text = answer.payload["text"]
        .as_str()
        .context("question response has no text")?;
    ensure!(
        text.len() <= MAX_TEXT,
        "question response exceeds 16000 bytes"
    );
    Ok(text)
}

impl Pending {
    pub fn plan(
        event: &Value,
        thread: &str,
        turn: Option<&str>,
        human: &str,
        now: DateTime<Utc>,
    ) -> Result<Self> {
        ensure!(
            valid_request_id(&event["id"]),
            "Codex request has no valid ID"
        );
        let turn = turn.context("provider request has no active input turn")?;
        let params = &event["params"];
        ensure!(
            params["threadId"].as_str() == Some(thread) && params["turnId"].as_str() == Some(turn),
            "provider request does not belong to the active input turn"
        );
        let method = event["method"]
            .as_str()
            .context("Codex request has no method")?;
        let mut questions = Vec::new();
        let kind = match method {
            "item/commandExecution/requestApproval" => {
                ensure!(
                    params["kind"].as_str().unwrap_or("command") == "command",
                    "this Codex approval action requires a richer review UI"
                );
                let command = text(&params["command"])?;
                let cwd = text(&params["cwd"])?;
                let reason = params["reason"].as_str().unwrap_or("Requested by Codex");
                let presentation = QuestionPresentation::CodexCommand {
                    command: command.into(),
                    cwd: cwd.into(),
                    reason: reason.into(),
                };
                let prompt = presentation.text();
                ensure!(
                    presentation.valid_for(&prompt),
                    "provider request is too large to review as a question"
                );
                questions.push(Question {
                    field: "command".into(),
                    text: prompt,
                    presentation: Some(presentation),
                    message: None,
                    answer: None,
                    closure: Closure::Open,
                });
                Kind::Command
            }
            "item/tool/requestUserInput" => {
                let values = params["questions"]
                    .as_array()
                    .context("Codex supplied no questions")?;
                ensure!(
                    !values.is_empty() && values.len() <= MAX_QUESTIONS,
                    "unsupported question count"
                );
                let mut fields = HashSet::new();
                for value in values {
                    ensure!(
                        value["isSecret"] != true,
                        "secret input cannot be stored in AgentDocker questions"
                    );
                    let field = value["id"].as_str().context("Codex question has no ID")?;
                    ensure!(
                        valid_id(field) && fields.insert(field),
                        "invalid or repeated question ID"
                    );
                    let mut prompt = text(&value["question"])?.to_owned();
                    let mut presentation = None;
                    if let Some(options) = value["options"].as_array() {
                        ensure!(options.len() <= 16, "too many provider question choices");
                        if !options.is_empty() {
                            let options = options
                                .iter()
                                .map(|option| {
                                    Ok(QuestionOption {
                                        label: text(&option["label"])?.into(),
                                        description: option["description"]
                                            .as_str()
                                            .unwrap_or_default()
                                            .into(),
                                    })
                                })
                                .collect::<Result<Vec<_>>>()?;
                            let choices = QuestionPresentation::Choices {
                                question: prompt,
                                options,
                            };
                            prompt = choices.text();
                            ensure!(
                                choices.valid_for(&prompt),
                                "unsupported or oversized provider choices"
                            );
                            presentation = Some(choices);
                        }
                    }
                    questions.push(Question {
                        field: field.into(),
                        text: prompt,
                        presentation,
                        message: None,
                        answer: None,
                        closure: Closure::Open,
                    });
                }
                Kind::UserInput
            }
            _ => bail!("Codex callback {method} is not yet supported by the input controller"),
        };
        Ok(Self {
            id: event["id"].clone(),
            thread: thread.into(),
            turn: turn.into(),
            human: human.into(),
            kind,
            expires_at: now + Duration::seconds(300),
            questions,
            response: None,
            extra_answers: Vec::new(),
        })
    }

    pub fn key(&self) -> String {
        self.id.to_string()
    }

    pub fn owns(&self, message: &Envelope, agent: &str) -> bool {
        message.from == self.human
            && matches!(&message.to, Destination::Agent(id) if id.as_str() == agent)
            && message.reply_to.as_ref().is_some_and(|id| {
                self.questions
                    .iter()
                    .any(|q| q.message.as_ref() == Some(id))
            })
    }

    pub fn answers(&self) -> impl Iterator<Item = &Envelope> {
        self.questions
            .iter()
            .filter_map(|q| q.answer.as_ref())
            .chain(self.extra_answers.iter())
    }

    pub fn observe(&mut self, event: &EventKind, owner: &str) -> Result<bool> {
        let (id, next) = match event {
            EventKind::QuestionClosed { question, answer } => (
                question,
                match answer {
                    Some(message) => Closure::Answered {
                        message: message.clone(),
                    },
                    None => Closure::Cancelled,
                },
            ),
            EventKind::QuestionCancelled { question, agent } if agent.as_str() == owner => {
                (question, Closure::Cancelled)
            }
            _ => return Ok(false),
        };
        let Some(question) = self
            .questions
            .iter_mut()
            .find(|q| q.message.as_ref() == Some(id))
        else {
            return Ok(false);
        };
        ensure!(
            question.closure == Closure::Open || question.closure == next,
            "conflicting daemon question closure"
        );
        let changed = question.closure != next;
        question.closure = next;
        Ok(changed)
    }

    // The daemon accepts one addressed Answer. A generic human Send can also
    // carry reply_to; retain later copies as unapplied responses, never turns.
    pub fn capture(&mut self, queue: &[Envelope], agent: &str, closed: bool) -> Result<bool> {
        let mut changed = false;
        for message in queue {
            if !self.owns(message, agent) || self.answers().any(|a| a.id == message.id) {
                continue;
            }
            answer_text(message)?;
            let question = self
                .questions
                .iter_mut()
                .find(|q| q.message.as_ref() == message.reply_to.as_ref())
                .context("question response lost its route")?;
            if !closed && question.closure == Closure::Open {
                // Its accepted answer may be in the queue before the closure
                // event reaches this controller. Wait for that exact ID.
                continue;
            }
            if !closed
                && question.answer.is_none()
                && self.response.is_none()
                && matches!(&question.closure, Closure::Answered { message: id } if id == &message.id)
                && message.sent_at <= self.expires_at
            {
                question.answer = Some(message.clone());
            } else {
                ensure!(
                    self.extra_answers.len() < MAX_QUESTIONS,
                    "too many later responses to a provider question; retained input needs review"
                );
                self.extra_answers.push(message.clone());
            }
            changed = true;
        }
        Ok(changed)
    }

    pub fn reply(&self, now: DateTime<Utc>) -> Result<Option<Value>> {
        if self.response.is_some() {
            return Ok(None);
        }
        if self
            .questions
            .iter()
            .any(|q| q.closure == Closure::Cancelled)
        {
            return Ok(Some(
                json!({"id":self.id,"error":{"code":-32000,"message":"The human question was cancelled or expired."}}),
            ));
        }
        if self.questions.iter().any(|q| q.answer.is_none()) {
            return Ok((now >= self.expires_at).then(|| json!({"id":self.id,"error":{"code":-32000,"message":"The human question expired before a complete response."}})));
        }
        ensure!(self.questions.iter().all(|q| matches!(&q.closure, Closure::Answered { message } if q.answer.as_ref().is_some_and(|a| &a.id == message))), "provider response has no exact daemon answer receipt");
        let result = match self.kind {
            Kind::Command => {
                let answer = answer_text(
                    self.questions[0]
                        .answer
                        .as_ref()
                        .context("command has no answer")?,
                )?;
                json!({"decision":if answer.trim().eq_ignore_ascii_case("allow") { "accept" } else { "decline" }})
            }
            Kind::UserInput => {
                let mut answers = serde_json::Map::new();
                for question in &self.questions {
                    answers.insert(question.field.clone(), json!({"answers":[answer_text(question.answer.as_ref().context("question has no answer")?)?]}));
                }
                json!({"answers":answers})
            }
        };
        Ok(Some(json!({"id":self.id,"result":result})))
    }

    pub fn validate(&self, thread: Option<&str>, agent: &str) -> Result<()> {
        ensure!(
            valid_request_id(&self.id)
                && thread == Some(self.thread.as_str())
                && valid_id(&self.turn)
                && valid_id(&self.human),
            "invalid retained provider question identity"
        );
        ensure!(
            !self.questions.is_empty()
                && self.questions.len() <= MAX_QUESTIONS
                && self.extra_answers.len() <= MAX_QUESTIONS,
            "invalid retained provider question count"
        );
        ensure!(
            !matches!(self.kind, Kind::Command) || self.questions.len() == 1,
            "command review has multiple questions"
        );
        let mut fields = HashSet::new();
        let mut routes = HashSet::new();
        for question in &self.questions {
            ensure!(
                question
                    .presentation
                    .as_ref()
                    .is_none_or(|p| p.valid_for(&question.text)),
                "stored question controls differ from the review text"
            );
            ensure!(
                valid_id(&question.field)
                    && fields.insert(&question.field)
                    && !question.text.is_empty()
                    && question.text.len() <= MAX_TEXT,
                "invalid retained review question"
            );
            if let Some(message) = &question.message {
                ensure!(
                    valid_id(message.as_str()) && routes.insert(message),
                    "invalid or repeated review route"
                );
            }
            if let Some(answer) = &question.answer {
                ensure!(
                    question.message.as_ref() == answer.reply_to.as_ref(),
                    "answer has another review route"
                );
            }
            if let Closure::Answered { message } = &question.closure {
                ensure!(
                    question.message.is_some() && valid_id(message.as_str()),
                    "invalid daemon question receipt"
                );
                ensure!(
                    question
                        .answer
                        .as_ref()
                        .is_none_or(|answer| &answer.id == message),
                    "stored response differs from the daemon answer receipt"
                );
            }
        }
        let mut answers = HashSet::new();
        for answer in self.answers() {
            ensure!(
                self.owns(answer, agent)
                    && valid_id(answer.id.as_str())
                    && answers.insert(&answer.id),
                "invalid or repeated retained human answer"
            );
            answer_text(answer)?;
        }
        if let Some(response) = &self.response {
            ensure!(
                response["id"] == self.id
                    && (response.get("result").is_some() ^ response.get("error").is_some())
                    && self.questions.iter().all(|q| q.message.is_some()),
                "invalid retained provider response"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event() -> Value {
        json!({"id":7,"method":"item/commandExecution/requestApproval","params":{"threadId":"thread","turnId":"turn","cwd":"/owned","command":"echo trial","kind":"command"}})
    }
    fn pending() -> Pending {
        let mut request =
            Pending::plan(&event(), "thread", Some("turn"), "human", Utc::now()).unwrap();
        request.questions[0].message = Some("question".to_owned().into());
        request
    }
    fn answer(from: &str, text: &str) -> Envelope {
        Envelope::new(
            from,
            Destination::Agent("owner".into()),
            "answer",
            json!({"text":text}),
            Some("question".to_owned().into()),
            Utc::now(),
        )
    }
    #[test]
    fn only_the_addressed_human_can_answer_and_later_copies_do_not_change_the_decision() {
        let mut request = pending();
        assert!(
            !request
                .capture(&[answer("peer", "Allow")], "owner", false)
                .unwrap()
        );
        assert!(request.reply(Utc::now()).unwrap().is_none());
        let first = answer("human", "Deny");
        let later = answer("human", "Allow");
        request
            .observe(
                &EventKind::QuestionClosed {
                    question: "question".to_owned().into(),
                    answer: Some(first.id.clone()),
                },
                "owner",
            )
            .unwrap();
        assert!(
            request
                .capture(&[first.clone(), later.clone()], "owner", false)
                .unwrap()
        );
        assert_eq!(
            request.reply(Utc::now()).unwrap().unwrap()["result"]["decision"],
            "decline"
        );
        assert_eq!(
            request.answers().map(|a| &a.id).collect::<Vec<_>>(),
            vec![&first.id, &later.id]
        );
        assert!(!request.capture(&[first, later], "owner", false).unwrap());
        request.validate(Some("thread"), "owner").unwrap();
    }
    #[test]
    fn queued_reply_waits_for_daemon_closure_and_cancellation_never_authorizes_it() {
        let mut request = pending();
        let late = answer("human", "Allow");
        assert!(
            !request
                .capture(std::slice::from_ref(&late), "owner", false)
                .unwrap()
        );
        assert!(request.reply(Utc::now()).unwrap().is_none());
        let cancelled = EventKind::QuestionCancelled {
            question: "question".to_owned().into(),
            agent: "owner".into(),
        };
        assert!(request.observe(&cancelled, "owner").unwrap());
        assert!(!request.observe(&cancelled, "owner").unwrap());
        assert!(
            request
                .capture(std::slice::from_ref(&late), "owner", false)
                .unwrap()
        );
        assert!(request.questions[0].answer.is_none());
        assert_eq!(request.extra_answers[0].id, late.id);
        assert!(
            request
                .reply(Utc::now())
                .unwrap()
                .unwrap()
                .get("error")
                .is_some()
        );
        assert!(
            request
                .observe(
                    &EventKind::QuestionClosed {
                        question: "question".to_owned().into(),
                        answer: Some(late.id)
                    },
                    "owner"
                )
                .is_err()
        );
    }

    #[test]
    fn a_late_answer_is_retained_without_granting_an_expired_request() {
        let mut request = pending();
        let mut late = answer("human", "Allow");
        late.sent_at = request.expires_at + Duration::seconds(1);
        request
            .observe(
                &EventKind::QuestionClosed {
                    question: "question".to_owned().into(),
                    answer: Some(late.id.clone()),
                },
                "owner",
            )
            .unwrap();
        assert!(
            request
                .capture(std::slice::from_ref(&late), "owner", false)
                .unwrap()
        );
        assert!(request.questions[0].answer.is_none());
        assert_eq!(request.extra_answers[0].id, late.id);
        assert!(
            request
                .reply(late.sent_at)
                .unwrap()
                .unwrap()
                .get("error")
                .is_some()
        );
    }

    #[test]
    fn every_question_is_validated_before_any_route_is_opened() {
        let mut input = json!({"id":"ask","method":"item/tool/requestUserInput","params":{"threadId":"thread","turnId":"turn","questions":[{"id":"first","question":"Choose?"},{"id":"secret","question":"Password?","isSecret":true}]}});
        assert!(Pending::plan(&input, "thread", Some("turn"), "human", Utc::now()).is_err());
        input["params"]["questions"][1]["isSecret"] = json!(false);
        input["params"]["questions"][1]["id"] = json!("first");
        assert!(Pending::plan(&input, "thread", Some("turn"), "human", Utc::now()).is_err());
        assert!(
            Pending::plan(
                &event(),
                "another-thread",
                Some("turn"),
                "human",
                Utc::now()
            )
            .is_err()
        );
        assert!(Pending::plan(&event(), "thread", None, "human", Utc::now()).is_err());
    }
    #[test]
    fn an_expired_incomplete_bundle_returns_an_error_and_never_infers_approval() {
        let request = pending();
        let response = request.reply(request.expires_at).unwrap().unwrap();
        assert_eq!(response["id"], 7);
        assert!(response.get("result").is_none());
        assert!(response.get("error").is_some());
    }
}
