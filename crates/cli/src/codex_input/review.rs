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
const CANCEL_REASON: &str = "\n\nDeny cancels this Codex request.";
const NETWORK_REVIEW: &str = "Allow network access for this request?";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Kind {
    Command,
    Network,
    Files,
    Permissions,
    UserInput,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum CommandDenial {
    #[default]
    Decline,
    Cancel,
}

impl CommandDenial {
    fn is_decline(&self) -> bool {
        *self == Self::Decline
    }

    fn wire(self) -> &'static str {
        match self {
            Self::Decline => "decline",
            Self::Cancel => "cancel",
        }
    }
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
    #[serde(default, skip_serializing_if = "CommandDenial::is_decline")]
    command_denial: CommandDenial,
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

fn network_options(denial: CommandDenial) -> Vec<QuestionOption> {
    vec![
        QuestionOption {
            label: "Allow".into(),
            description: "Allow the pending network request".into(),
        },
        QuestionOption {
            label: "Deny".into(),
            description: match denial {
                CommandDenial::Decline => "Decline the pending network request",
                CommandDenial::Cancel => "Cancel this Codex request",
            }
            .into(),
        },
    ]
}

/// Build the complete local command review before publishing a human route.
/// `accept` never selects either proposed policy amendment or session scope.
fn command_presentation(params: &Value) -> Result<(QuestionPresentation, CommandDenial)> {
    ensure!(
        params["kind"].is_null() || params["kind"] == "command",
        "this Codex approval action requires a richer review UI"
    );
    ensure!(
        params["environmentId"].is_null() || params["environmentId"] == "local",
        "remote command requests need a separate review flow"
    );
    // Codex 0.154.0's legacy decision fallback offers accept/cancel when
    // this optional field is absent. Persisted pre-v8 requests still retain
    // their original decline semantics via CommandDenial's serde default.
    let mut denial = CommandDenial::Cancel;
    if !params["availableDecisions"].is_null() {
        let decisions = params["availableDecisions"]
            .as_array()
            .context("unsupported command approval decisions")?;
        ensure!(
            decisions.len() <= 16
                && decisions.iter().any(|decision| decision == "accept")
                && decisions
                    .iter()
                    .any(|decision| decision == "decline" || decision == "cancel"),
            "this command does not offer a one-time approval and a negative response"
        );
        if decisions.iter().any(|decision| decision == "decline") {
            denial = CommandDenial::Decline;
        }
    }
    let mut reason = match &params["reason"] {
        Value::Null => "Requested by Codex".to_owned(),
        value => value
            .as_str()
            .context("unsupported command reason")?
            .to_owned(),
    };
    // Provider prose cannot insert lines or change the visual direction of
    // the access details below it. Reject it rather than changing the text
    // the provider supplied; only our own formatting adds section breaks.
    ensure!(
        !reason.chars().any(|c| c.is_control()
            || matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}')),
        "command reason contains unsupported display controls"
    );
    if !params["networkApprovalContext"].is_null() {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Network {
            host: String,
            protocol: String,
        }
        let network: Network = serde_json::from_value(params["networkApprovalContext"].clone())
            .context("unsupported network approval context")?;
        // Keep the exact requested host. No URL parsing or normalization may
        // turn a different destination into the one the human reviewed.
        ensure!(
            !network.host.is_empty()
                && network.host.len() <= 253
                && network.host.is_ascii()
                && network
                    .host
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b".-:[]".contains(&b))
                && matches!(
                    network.protocol.as_str(),
                    "http" | "https" | "socks5Tcp" | "socks5Udp"
                ),
            "network destination cannot be completely reviewed"
        );
        reason.push_str(&format!(
            "\n\nRequested connection: {} ({})",
            network.host, network.protocol
        ));
    }
    if !params["additionalPermissions"].is_null() {
        let permissions: agentdocker_core::QuestionPermissions =
            serde_json::from_value(params["additionalPermissions"].clone())
                .context("unsupported command permission selectors")?;
        ensure!(
            permissions.valid(),
            "command permissions cannot be completely reviewed"
        );
        reason.push_str("\n\nAdditional access for this command:\n");
        reason.push_str(&permissions.lines().join("\n"));
    }
    let presentation = if !params["networkApprovalContext"].is_null() {
        // Codex groups pending connections to a destination. Network context
        // determines this route even when optional command metadata is present;
        // accept does not select a command policy amendment or session grant.
        let mut context = String::new();
        if !params["cwd"].is_null() {
            context.push_str(&format!("Directory: {}\n", text(&params["cwd"])?));
        }
        if !params["command"].is_null() {
            context.push_str(&format!(
                "Provider context:\n{}\n\n",
                text(&params["command"])?
            ));
        }
        reason.push_str(
            "\n\nThis approval may cover multiple pending connections to this destination.",
        );
        if denial == CommandDenial::Cancel {
            reason.push_str(CANCEL_REASON);
        }
        QuestionPresentation::Choices {
            question: format!("{NETWORK_REVIEW}\n\n{context}Reason: {reason}"),
            options: network_options(denial),
        }
    } else {
        if denial == CommandDenial::Cancel {
            reason.push_str(CANCEL_REASON);
        }
        QuestionPresentation::CodexCommand {
            command: text(&params["command"])?.into(),
            cwd: text(&params["cwd"])?.into(),
            reason,
        }
    };
    ensure!(
        presentation.valid_for(&presentation.text()),
        "provider request is too large to review as a question"
    );
    Ok((presentation, denial))
}

impl Pending {
    pub fn plan(
        event: &Value,
        thread: &str,
        turn: Option<&str>,
        human: &str,
        now: DateTime<Utc>,
    ) -> Result<Self> {
        Self::plan_with_files(event, thread, turn, human, now, None)
    }

    pub fn plan_with_files(
        event: &Value,
        thread: &str,
        turn: Option<&str>,
        human: &str,
        now: DateTime<Utc>,
        files: Option<QuestionPresentation>,
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
        let mut command_denial = CommandDenial::Decline;
        let kind = match method {
            "item/permissions/requestApproval" => {
                ensure!(
                    params["environmentId"].is_null()
                        || params["environmentId"].as_str() == Some("local"),
                    "remote permission requests need a separate review flow"
                );
                ensure!(
                    params["itemId"].as_str().is_some_and(valid_id),
                    "permission request has no valid item ID"
                );
                let presentation = QuestionPresentation::CodexPermissions {
                    cwd: text(&params["cwd"])?.into(),
                    reason: params["reason"]
                        .as_str()
                        .unwrap_or("Requested by Codex")
                        .into(),
                    permissions: serde_json::from_value(params["permissions"].clone())
                        .context("unsupported permission selectors")?,
                };
                ensure!(
                    presentation.valid_for(&presentation.text()),
                    "permission request cannot be completely reviewed"
                );
                questions.push(Question {
                    field: "permissions".into(),
                    text: presentation.text(),
                    presentation: Some(presentation),
                    message: None,
                    answer: None,
                    closure: Closure::Open,
                });
                Kind::Permissions
            }
            "item/fileChange/requestApproval" => {
                ensure!(
                    params["grantRoot"].is_null(),
                    "session-wide file access needs a separate review flow"
                );
                let presentation =
                    files.context("file approval requires a complete correlated review")?;
                ensure!(
                    matches!(presentation, QuestionPresentation::CodexFiles { .. })
                        && presentation.valid_for(&presentation.text()),
                    "file approval has no complete file presentation"
                );
                questions.push(Question {
                    field: "files".into(),
                    text: presentation.text(),
                    presentation: Some(presentation),
                    message: None,
                    answer: None,
                    closure: Closure::Open,
                });
                Kind::Files
            }
            "item/commandExecution/requestApproval" => {
                let (presentation, denial) = command_presentation(params)?;
                let network = matches!(presentation, QuestionPresentation::Choices { .. });
                command_denial = denial;
                let prompt = presentation.text();
                questions.push(Question {
                    field: "command".into(),
                    text: prompt,
                    presentation: Some(presentation),
                    message: None,
                    answer: None,
                    closure: Closure::Open,
                });
                if network {
                    Kind::Network
                } else {
                    Kind::Command
                }
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
            command_denial,
            expires_at: now + Duration::seconds(300),
            questions,
            response: None,
            extra_answers: Vec::new(),
        })
    }

    pub fn key(&self) -> String {
        self.id.to_string()
    }

    pub fn has_command_cancellation(&self) -> bool {
        self.command_denial == CommandDenial::Cancel
    }

    pub fn is_network_review(&self) -> bool {
        matches!(self.kind, Kind::Network)
    }

    pub fn is_file_review(&self) -> bool {
        matches!(self.kind, Kind::Files)
            || self.questions.iter().any(|q| {
                matches!(
                    q.presentation,
                    Some(QuestionPresentation::CodexFiles { .. })
                )
            })
    }

    pub fn is_permission_review(&self) -> bool {
        matches!(self.kind, Kind::Permissions)
            || self.questions.iter().any(|q| {
                matches!(
                    q.presentation,
                    Some(QuestionPresentation::CodexPermissions { .. })
                )
            })
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
            Kind::Permissions => {
                let question = &self.questions[0];
                let Some(QuestionPresentation::CodexPermissions { permissions, .. }) =
                    &question.presentation
                else {
                    bail!("permission approval has no complete presentation");
                };
                let answer = answer_text(
                    question
                        .answer
                        .as_ref()
                        .context("permission approval has no answer")?,
                )?;
                // An exact human decision grants only the reviewed subset for
                // this turn. Do not override the provider's auto-review policy.
                let granted = if answer == "Allow" {
                    serde_json::to_value(permissions)?
                } else {
                    json!({})
                };
                json!({"permissions":granted,"scope":"turn"})
            }
            Kind::Command | Kind::Network | Kind::Files => {
                let answer = answer_text(
                    self.questions[0]
                        .answer
                        .as_ref()
                        .context("approval has no answer")?,
                )?;
                json!({"decision":if answer == "Allow" { "accept" } else { self.command_denial.wire() }})
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
            self.command_denial == CommandDenial::Decline
                || matches!(self.kind, Kind::Command | Kind::Network),
            "a non-command review cannot supply command cancellation semantics"
        );
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
            !matches!(
                self.kind,
                Kind::Command | Kind::Network | Kind::Files | Kind::Permissions
            ) || self.questions.len() == 1,
            "approval review has multiple questions"
        );
        let mut fields = HashSet::new();
        let mut routes = HashSet::new();
        for question in &self.questions {
            ensure!(
                !self.has_command_cancellation()
                    || matches!(&question.presentation, Some(QuestionPresentation::CodexCommand { reason, .. }) if reason.ends_with(CANCEL_REASON))
                    || (self.is_network_review()
                        && matches!(&question.presentation, Some(QuestionPresentation::Choices { question, .. }) if question.ends_with(CANCEL_REASON))),
                "command cancellation has no matching human review"
            );
            ensure!(
                !self.is_network_review()
                    || matches!(&question.presentation, Some(QuestionPresentation::Choices { question, options })
                        if question.starts_with(NETWORK_REVIEW) && options == &network_options(self.command_denial)),
                "network approval has no complete one-time choice presentation"
            );
            ensure!(
                matches!(self.kind, Kind::Permissions)
                    == matches!(
                        question.presentation,
                        Some(QuestionPresentation::CodexPermissions { .. })
                    ),
                "permission review kind and presentation disagree"
            );
            ensure!(
                matches!(self.kind, Kind::Files)
                    == matches!(
                        question.presentation,
                        Some(QuestionPresentation::CodexFiles { .. })
                    ),
                "file review kind and presentation disagree"
            );
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

    fn network_event() -> Value {
        json!({"id":"network-callback","method":"item/commandExecution/requestApproval","params":{
            "threadId":"thread","turnId":"turn","itemId":"shared-item","approvalId":"specific-connection",
            "networkApprovalContext":{"host":"api.example.com:443","protocol":"https"},
            "availableDecisions":["accept","decline"]
        }})
    }

    #[test]
    fn network_only_reviews_show_exact_destination_and_require_correlated_human_receipts() {
        for denial in ["decline", "cancel"] {
            for decision in ["Allow", "Deny", "Allow for session"] {
                let mut event = network_event();
                event["params"]["availableDecisions"] = json!(["accept", denial]);
                let mut request =
                    Pending::plan(&event, "thread", Some("turn"), "human", Utc::now()).unwrap();
                assert!(request.is_network_review());
                let question = &request.questions[0];
                assert!(question.text.contains("api.example.com:443 (https)"));
                assert!(!question.text.contains("Directory:"));
                assert!(!question.text.contains("Command:"));
                assert!(
                    question
                        .presentation
                        .as_ref()
                        .unwrap()
                        .valid_for(&question.text)
                );
                request.questions[0].message = Some("question".into());
                assert!(
                    !request
                        .capture(&[answer("peer", "Allow")], "owner", false)
                        .unwrap()
                );
                let response = answer("human", decision);
                assert!(
                    !request
                        .capture(std::slice::from_ref(&response), "owner", false)
                        .unwrap()
                );
                assert!(request.reply(Utc::now()).unwrap().is_none());
                request
                    .observe(
                        &EventKind::QuestionClosed {
                            question: "question".into(),
                            answer: Some(response.id.clone()),
                        },
                        "owner",
                    )
                    .unwrap();
                request.capture(&[response], "owner", false).unwrap();
                request.response = request.reply(Utc::now()).unwrap();
                assert_eq!(
                    request.response,
                    Some(
                        json!({"id":"network-callback","result":{"decision":if decision == "Allow" {"accept"} else {denial}}})
                    )
                );
                let restored: Pending =
                    serde_json::from_slice(&serde_json::to_vec(&request).unwrap()).unwrap();
                restored.validate(Some("thread"), "owner").unwrap();
                assert_eq!(restored.response, request.response);
                assert!(
                    restored.reply(Utc::now()).unwrap().is_none(),
                    "uncertain provider writes must not replay"
                );
            }
        }
        let mut event = network_event();
        event["params"]
            .as_object_mut()
            .unwrap()
            .remove("availableDecisions");
        let pending = Pending::plan(&event, "thread", Some("turn"), "human", Utc::now()).unwrap();
        assert!(pending.has_command_cancellation());
        pending.validate(Some("thread"), "owner").unwrap();
    }

    #[test]
    fn network_context_selects_network_review_with_or_without_command_metadata() {
        for command in [Value::Null, json!("provider connection context")] {
            for cwd in [Value::Null, json!("/owned")] {
                let mut event = access_event();
                event["params"]["command"] = command.clone();
                event["params"]["cwd"] = cwd.clone();
                event["params"]["commandActions"] = json!([]);
                let request =
                    Pending::plan(&event, "thread", Some("turn"), "human", Utc::now()).unwrap();
                assert!(request.is_network_review());
                let shown = &request.questions[0].text;
                assert!(shown.starts_with(NETWORK_REVIEW));
                assert!(shown.contains("multiple pending connections to this destination"));
                assert!(shown.contains("Requested connection: example.com (https)"));
                assert!(shown.contains("Read: /owned/input"));
                assert!(shown.contains("Write: /owned/output"));
                assert!(shown.contains("Exclude: /owned/private"));
                assert_eq!(shown.contains("Provider context:"), !command.is_null());
                assert_eq!(shown.contains("Directory:"), !cwd.is_null());
                request.validate(Some("thread"), "owner").unwrap();
            }
        }
        for field in ["command", "cwd"] {
            let mut event = event();
            event["params"][field] = Value::Null;
            assert!(Pending::plan(&event, "thread", Some("turn"), "human", Utc::now()).is_err());
        }
    }

    #[test]
    fn network_review_refuses_missing_or_hidden_access_and_wrong_identity() {
        for (key, value) in [
            ("networkApprovalContext", Value::Null),
            (
                "networkApprovalContext",
                json!({"host":"x\nAllow everything","protocol":"https"}),
            ),
            (
                "networkApprovalContext",
                json!({"host":"example.com","protocol":"https","hidden":true}),
            ),
            ("availableDecisions", json!(["acceptForSession", "decline"])),
            ("environmentId", json!("remote")),
            ("kind", json!("writeStdin")),
            ("threadId", json!("another")),
            ("turnId", json!("another")),
        ] {
            let mut event = network_event();
            event["params"][key] = value;
            assert!(
                Pending::plan(&event, "thread", Some("turn"), "human", Utc::now()).is_err(),
                "accepted {key}"
            );
        }
        let mut request = Pending::plan(
            &network_event(),
            "thread",
            Some("turn"),
            "human",
            Utc::now(),
        )
        .unwrap();
        let Some(QuestionPresentation::Choices { options, .. }) =
            &mut request.questions[0].presentation
        else {
            panic!()
        };
        options[0].label = "Allow for session".into();
        request.questions[0].text = request.questions[0].presentation.as_ref().unwrap().text();
        assert!(request.validate(Some("thread"), "owner").is_err());
    }

    fn permission_event() -> Value {
        json!({"id":11,"method":"item/permissions/requestApproval","params":{"threadId":"thread","turnId":"turn","itemId":"permissions","cwd":"/owned","permissions":{"network":{"enabled":true},"fileSystem":{"write":["/owned/output"]}}}})
    }

    #[test]
    fn permission_grants_require_exact_human_receipts_and_are_limited_to_the_current_turn() {
        let event = permission_event();
        for decision in ["Allow", "Deny", "Allow for session", "allow"] {
            let mut request =
                Pending::plan(&event, "thread", Some("turn"), "human", Utc::now()).unwrap();
            request.questions[0].message = Some("question".to_owned().into());
            assert!(
                !request
                    .capture(&[answer("peer", "Allow")], "owner", false)
                    .unwrap()
            );
            let response = answer("human", decision);
            assert!(
                !request
                    .capture(std::slice::from_ref(&response), "owner", false)
                    .unwrap()
            );
            assert!(request.reply(Utc::now()).unwrap().is_none());
            request
                .observe(
                    &EventKind::QuestionClosed {
                        question: "question".to_owned().into(),
                        answer: Some(response.id.clone()),
                    },
                    "owner",
                )
                .unwrap();
            request.capture(&[response], "owner", false).unwrap();
            let granted = if decision == "Allow" {
                event["params"]["permissions"].clone()
            } else {
                json!({})
            };
            let expected = json!({"id":11,"result":{"permissions":granted,"scope":"turn"}});
            assert_eq!(request.reply(Utc::now()).unwrap(), Some(expected.clone()));
            let bytes = serde_json::to_vec(&request).unwrap();
            let decoded: Pending = serde_json::from_slice(&bytes).unwrap();
            decoded.validate(Some("thread"), "owner").unwrap();
            assert_eq!(decoded.reply(Utc::now()).unwrap(), Some(expected));
            let mut mismatched = serde_json::to_value(&decoded).unwrap();
            mismatched["kind"] = json!("command");
            assert!(
                serde_json::from_value::<Pending>(mismatched)
                    .unwrap()
                    .validate(Some("thread"), "owner")
                    .is_err()
            );
        }
    }

    #[test]
    fn permission_requests_refuse_wrong_turns_remote_environments_and_incomplete_presentations() {
        let mut local = permission_event();
        local["params"]["environmentId"] = json!("local");
        assert!(Pending::plan(&local, "thread", Some("turn"), "human", Utc::now()).is_ok());
        for (key, value) in [
            ("turnId", json!("other")),
            ("threadId", json!("other")),
            ("itemId", json!("")),
            ("environmentId", json!("remote")),
            ("permissions", json!({"fileSystem":{"read":["relative"]}})),
            (
                "permissions",
                json!({"network":{"enabled":true,"unknown":true}}),
            ),
            ("reason", json!("x".repeat(16_000))),
        ] {
            let mut event = permission_event();
            event["params"][key] = value;
            assert!(
                Pending::plan(&event, "thread", Some("turn"), "human", Utc::now()).is_err(),
                "accepted {key}"
            );
        }
        let request = Pending::plan(
            &permission_event(),
            "thread",
            Some("turn"),
            "human",
            Utc::now(),
        )
        .unwrap();
        let presentation = request.questions[0].presentation.as_ref().unwrap();
        assert!(!presentation.valid_for("Allow something else?"));
        assert!(presentation.permits_choice("Allow"));
        assert!(!presentation.permits_choice("Allow for session"));
    }

    fn event() -> Value {
        json!({"id":7,"method":"item/commandExecution/requestApproval","params":{"threadId":"thread","turnId":"turn","cwd":"/owned","command":"echo trial","kind":"command","availableDecisions":["accept","decline"]}})
    }
    fn access_event() -> Value {
        let mut event = event();
        event["params"]["environmentId"] = json!("local");
        event["params"]["availableDecisions"] = json!(["accept", "acceptForSession", "decline"]);
        event["params"]["networkApprovalContext"] =
            json!({"host":"example.com", "protocol":"https"});
        event["params"]["additionalPermissions"] = json!({"network":{"enabled":true}, "fileSystem":{"read":["/owned/input"],"write":["/owned/output"],"entries":[{"access":"deny","path":{"type":"path","path":"/owned/private"}}]}});
        event
    }
    #[test]
    fn command_reason_refuses_display_controls_and_preserves_unicode_prose() {
        for control in [
            '\0', '\t', '\n', '\r', '\u{001b}', '\u{007f}', '\u{0085}', '\u{061c}', '\u{200e}',
            '\u{200f}', '\u{2028}', '\u{2029}', '\u{202a}', '\u{202b}', '\u{202c}', '\u{202d}',
            '\u{202e}', '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}',
        ] {
            let mut event = access_event();
            event["params"]["reason"] = json!(format!("Run the check{control}Exclude: /private"));
            assert!(
                Pending::plan(&event, "thread", Some("turn"), "human", Utc::now()).is_err(),
                "accepted display control {control:?}"
            );
        }
        for reason in [Value::Null, json!("変更を確認する — check the change.")] {
            let mut event = access_event();
            event["params"]["reason"] = reason.clone();
            let planned =
                Pending::plan(&event, "thread", Some("turn"), "human", Utc::now()).unwrap();
            let shown = &planned.questions[0].text;
            assert!(shown.contains(reason.as_str().unwrap_or("Requested by Codex")));
            assert!(shown.contains("\n\nAdditional access for this command:\n"));
        }
    }
    #[test]
    fn command_access_is_complete_in_native_and_fallback_reviews_and_retained_receipts() {
        for (decisions, negative) in [
            (Some(json!(["accept", "decline"])), "decline"),
            (Some(json!(["accept", "cancel"])), "cancel"),
            (Some(Value::Null), "cancel"),
            (None, "cancel"),
        ] {
            for decision in ["Allow", "Deny", "allow", " Allow", "Allow for session"] {
                let mut event = access_event();
                match &decisions {
                    Some(value) => event["params"]["availableDecisions"] = value.clone(),
                    None => {
                        event["params"]
                            .as_object_mut()
                            .unwrap()
                            .remove("availableDecisions");
                    }
                }
                let mut request =
                    Pending::plan(&event, "thread", Some("turn"), "human", Utc::now()).unwrap();
                let question = &request.questions[0];
                let presentation = question.presentation.as_ref().unwrap();
                assert!(presentation.valid_for(&question.text));
                for detail in [
                    "echo trial",
                    "Directory: /owned",
                    "Requested connection: example.com (https)",
                    "Allow network access",
                    "Read: /owned/input",
                    "Write: /owned/output",
                    "Exclude: /owned/private",
                ] {
                    assert!(question.text.contains(detail), "missing {detail}");
                }
                request.questions[0].message = Some("question".to_owned().into());
                assert!(
                    !request
                        .capture(&[answer("peer", "Allow")], "owner", false)
                        .unwrap()
                );
                let response = answer("human", decision);
                assert!(
                    !request
                        .capture(std::slice::from_ref(&response), "owner", false)
                        .unwrap()
                );
                assert!(request.reply(Utc::now()).unwrap().is_none());
                request
                    .observe(
                        &EventKind::QuestionClosed {
                            question: "question".to_owned().into(),
                            answer: Some(response.id.clone()),
                        },
                        "owner",
                    )
                    .unwrap();
                request.capture(&[response], "owner", false).unwrap();
                let expected = json!({"id":7,"result":{"decision":if decision == "Allow" { "accept" } else { negative }}});
                request.response = request.reply(Utc::now()).unwrap();
                assert_eq!(request.response, Some(expected.clone()));
                let restored: Pending =
                    serde_json::from_slice(&serde_json::to_vec(&request).unwrap()).unwrap();
                restored.validate(Some("thread"), "owner").unwrap();
                assert_eq!(restored.questions[0].text, request.questions[0].text);
                assert_eq!(restored.response, Some(expected));
                // A retained write intent must survive recovery without being
                // emitted again: only the provider's resolution proves receipt.
                assert!(restored.reply(Utc::now()).unwrap().is_none());
            }
        }
    }
    #[test]
    fn missing_command_decisions_keep_codex_default_cancellation_visible() {
        for mut event in [event(), access_event()] {
            for decisions in [None, Some(Value::Null)] {
                match decisions {
                    None => {
                        event["params"]
                            .as_object_mut()
                            .unwrap()
                            .remove("availableDecisions");
                    }
                    Some(value) => event["params"]["availableDecisions"] = value,
                }
                let request =
                    Pending::plan(&event, "thread", Some("turn"), "human", Utc::now()).unwrap();
                assert!(request.has_command_cancellation());
                assert!(request.questions[0].text.contains(CANCEL_REASON.trim()));
                let restored: Pending =
                    serde_json::from_slice(&serde_json::to_vec(&request).unwrap()).unwrap();
                restored.validate(Some("thread"), "owner").unwrap();
                assert!(restored.has_command_cancellation());
            }
        }
    }
    #[test]
    fn command_access_refuses_hidden_permissions_remote_context_and_unoffered_decisions() {
        for (key, value) in [
            ("kind", json!("writeStdin")),
            ("kind", json!(false)),
            ("environmentId", json!("remote")),
            ("environmentId", json!({})),
            ("availableDecisions", json!(["acceptForSession", "decline"])),
            ("availableDecisions", json!(["accept", "acceptForSession"])),
            ("availableDecisions", json!([])),
            ("availableDecisions", json!({})),
            (
                "additionalPermissions",
                json!({"fileSystem":{"write":["relative"]}}),
            ),
            (
                "additionalPermissions",
                json!({"network":{"enabled":true,"hidden":true}}),
            ),
            (
                "additionalPermissions",
                json!({"fileSystem":{"entries":[{"access":"write","path":{"type":"glob_pattern","pattern":"/**"}}]}}),
            ),
            (
                "networkApprovalContext",
                json!({"host":"example.com", "protocol":"ftp"}),
            ),
            (
                "networkApprovalContext",
                json!({"host":"example.com", "protocol":"https", "hidden":true}),
            ),
            ("reason", json!({})),
            ("reason", json!("x".repeat(MAX_TEXT))),
            ("command", json!(false)),
            ("cwd", json!([])),
        ] {
            let mut event = access_event();
            event["params"][key] = value;
            assert!(
                Pending::plan(&event, "thread", Some("turn"), "human", Utc::now()).is_err(),
                "accepted {key}: {}",
                event["params"][key]
            );
        }
    }
    #[test]
    fn network_review_preserves_supported_hosts_and_refuses_ambiguous_display() {
        for host in ["example.com", "127.0.0.1", "[::1]", "xn--bcher-kva.example"] {
            for protocol in ["http", "https", "socks5Tcp", "socks5Udp"] {
                let mut event = access_event();
                event["params"]["networkApprovalContext"] =
                    json!({"host":host, "protocol":protocol});
                let planned =
                    Pending::plan(&event, "thread", Some("turn"), "human", Utc::now()).unwrap();
                assert!(
                    planned.questions[0]
                        .text
                        .contains(&format!("Requested connection: {host} ({protocol})"))
                );
            }
        }
        for host in [
            "",
            "example.com\nDeny",
            "example.com/another",
            "user@example.com",
            " example.com",
            "example.com\u{202e}",
            "bücher.example",
        ] {
            let mut event = access_event();
            event["params"]["networkApprovalContext"]["host"] = json!(host);
            assert!(
                Pending::plan(&event, "thread", Some("turn"), "human", Utc::now()).is_err(),
                "accepted {host}"
            );
        }
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
    fn file_change_decisions_require_the_exact_human_closure_and_never_grant_a_session() {
        use agentdocker_core::{QuestionFileChange, QuestionFileChangeKind};
        let presentation = QuestionPresentation::CodexFiles {
            cwd: "/owned".into(),
            reason: "Fixture".into(),
            changes: vec![QuestionFileChange {
                path: "/owned/a".into(),
                kind: QuestionFileChangeKind::Add,
                diff: "+new\n".into(),
            }],
        };
        let event = json!({"id":8,"method":"item/fileChange/requestApproval","params":{"threadId":"thread","turnId":"turn","itemId":"patch"}});
        assert!(Pending::plan(&event, "thread", Some("turn"), "human", Utc::now()).is_err());
        for (answer_text, expected) in [
            ("Allow", "accept"),
            ("Deny", "decline"),
            ("Allow for session", "decline"),
        ] {
            let mut request = Pending::plan_with_files(
                &event,
                "thread",
                Some("turn"),
                "human",
                Utc::now(),
                Some(presentation.clone()),
            )
            .unwrap();
            request.questions[0].message = Some("question".to_owned().into());
            assert!(
                !request
                    .capture(&[answer("peer", "Allow")], "owner", false)
                    .unwrap()
            );
            let response = answer("human", answer_text);
            assert!(
                !request
                    .capture(std::slice::from_ref(&response), "owner", false)
                    .unwrap()
            );
            assert!(request.reply(Utc::now()).unwrap().is_none());
            request
                .observe(
                    &EventKind::QuestionClosed {
                        question: "question".to_owned().into(),
                        answer: Some(response.id.clone()),
                    },
                    "owner",
                )
                .unwrap();
            request.capture(&[response], "owner", false).unwrap();
            assert_eq!(
                request.reply(Utc::now()).unwrap().unwrap()["result"],
                json!({"decision":expected})
            );
            let encoded = serde_json::to_vec(&request).unwrap();
            let decoded: Pending = serde_json::from_slice(&encoded).unwrap();
            decoded.validate(Some("thread"), "owner").unwrap();
            assert_eq!(
                decoded.questions[0].presentation,
                Some(presentation.clone())
            );
        }
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
