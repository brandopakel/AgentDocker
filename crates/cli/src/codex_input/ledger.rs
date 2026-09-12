//! One durable provider input attempt. Queue acknowledgement follows an exact
//! provider receipt; a prepared attempt can never be submitted automatically again.
use super::review::{self, Closed, Pending};
use agentdocker_core::{Envelope, MessageId};
use agentdocker_host::{dirs, lock};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
};

const VERSION: u32 = 4;
const MAX_STATE_BYTES: usize = 8 * 1024 * 1024;
const MAX_INPUT_BYTES: usize = 1024 * 1024;
const RETAINED_RECEIPTS: usize = 128;
const MAX_RETIRED_QUESTIONS: usize = 10_000;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Binding {
    pub agent: String,
    pub socket: PathBuf,
    pub cwd: PathBuf,
    pub provider_home: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::Destination;
    fn binding(home: &Path) -> Binding {
        Binding {
            agent: "owned-agent".into(),
            socket: home.join("agentd.sock"),
            cwd: home.into(),
            provider_home: home.into(),
        }
    }
    fn message() -> Envelope {
        Envelope::new(
            "peer",
            Destination::parse("owned-agent"),
            "chat",
            serde_json::json!({"text":"complete input"}),
            None,
            chrono::Utc::now(),
        )
    }
    fn receipt() -> Receipt {
        Receipt {
            thread: "thread".into(),
            turn: "turn".into(),
            item: "item".into(),
        }
    }

    #[test]
    fn prepared_input_survives_restart_and_cannot_be_repeated_or_acknowledged_without_proof() {
        let home = tempfile::tempdir().unwrap();
        let binding = binding(home.path());
        let message = message();
        let mut ledger = Ledger::open(home.path(), binding.clone()).unwrap();
        ledger.bind_thread("thread".into()).unwrap();
        let input = ledger.prepare(&message).unwrap();
        let path = ledger.path.clone();
        let before = std::fs::read(&path).unwrap();
        assert!(ledger.acknowledge(message.id.as_str()).is_err());
        assert!(ledger.prepare(&message).is_err());
        assert!(ledger.accept("only part of the input", receipt()).is_err());
        let mut wrong = receipt();
        wrong.thread = "another-thread".into();
        assert!(ledger.accept(&input, wrong).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        drop(ledger);
        let mut ledger = Ledger::open(home.path(), binding).unwrap();
        assert_eq!(ledger.record().attempt.as_ref().unwrap().input, input);
        assert!(ledger.prepare(&message).is_err());
        assert!(ledger.finish("turn").is_err());
        ledger.accept(&input, receipt()).unwrap();
        assert!(ledger.finish("turn").is_err());
        ledger.acknowledge(message.id.as_str()).unwrap();
        ledger.acknowledge(message.id.as_str()).unwrap();
        assert!(ledger.finish("another-turn").is_err());
        ledger.finish("turn").unwrap();
        assert!(ledger.record().attempt.is_none());
        assert!(ledger.prepare(&message).is_err());
    }

    #[test]
    fn owner_lock_binding_and_private_state_are_enforced() {
        let home = tempfile::tempdir().unwrap();
        let binding = binding(home.path());
        let mut ledger = Ledger::open(home.path(), binding.clone()).unwrap();
        assert!(Ledger::open(home.path(), binding.clone()).is_err());
        ledger.bind_thread("thread".into()).unwrap();
        let path = ledger.path.clone();
        drop(ledger);
        let mut other = binding.clone();
        other.provider_home = home.path().join("different");
        assert!(Ledger::open(home.path(), other).is_err());
        std::fs::write(&path, b"corrupted").unwrap();
        assert!(Ledger::open(home.path(), binding.clone()).is_err());
        std::fs::remove_file(&path).unwrap();
        #[cfg(unix)]
        {
            let target = home.path().join("untouched");
            std::fs::write(&target, b"private").unwrap();
            std::os::unix::fs::symlink(&target, &path).unwrap();
            assert!(Ledger::open(home.path(), binding).is_err());
            assert_eq!(std::fs::read(&target).unwrap(), b"private");
        }
    }

    #[test]
    fn only_a_never_used_conversation_can_be_replaced_after_restart() {
        let home = tempfile::tempdir().unwrap();
        let mut ledger = Ledger::open(home.path(), binding(home.path())).unwrap();
        ledger
            .bind_thread("not-persisted-by-provider".into())
            .unwrap();
        ledger.discard_unused_thread().unwrap();
        ledger.bind_thread("thread".into()).unwrap();
        let message = message();
        let input = ledger.prepare(&message).unwrap();
        let before = std::fs::read(&ledger.path).unwrap();
        assert!(ledger.discard_unused_thread().is_err());
        assert_eq!(std::fs::read(&ledger.path).unwrap(), before);
        ledger.accept(&input, receipt()).unwrap();
        ledger.acknowledge(message.id.as_str()).unwrap();
        ledger.finish("turn").unwrap();
        assert!(ledger.discard_unused_thread().is_err());
        assert_eq!(ledger.record().thread.as_deref(), Some("thread"));
    }

    #[test]
    fn completed_receipts_remain_bounded_and_conflicting_receipts_do_not_advance_state() {
        let home = tempfile::tempdir().unwrap();
        let mut ledger = Ledger::open(home.path(), binding(home.path())).unwrap();
        ledger.bind_thread("thread".into()).unwrap();
        for index in 0..RETAINED_RECEIPTS + 3 {
            let message = message();
            let input = ledger.prepare(&message).unwrap();
            let receipt = Receipt {
                thread: "thread".into(),
                turn: format!("turn-{index}"),
                item: format!("item-{index}"),
            };
            ledger.accept(&input, receipt.clone()).unwrap();
            let before = std::fs::read(&ledger.path).unwrap();
            let mut wrong = receipt.clone();
            wrong.item.push('x');
            assert!(ledger.accept(&input, wrong).is_err());
            assert_eq!(std::fs::read(&ledger.path).unwrap(), before);
            ledger.acknowledge(message.id.as_str()).unwrap();
            ledger.finish(&receipt.turn).unwrap();
        }
        assert_eq!(ledger.record().completed.len(), RETAINED_RECEIPTS);
        assert_eq!(ledger.record().completed[0].receipt.turn, "turn-3");
    }

    #[test]
    fn legacy_delivery_state_upgrades_without_replacing_prepared_input() {
        for version in [1, 2] {
            let home = tempfile::tempdir().unwrap();
            let binding = binding(home.path());
            let mut ledger = Ledger::open(home.path(), binding.clone()).unwrap();
            ledger.bind_thread("thread".into()).unwrap();
            let message = message();
            let input = ledger.prepare(&message).unwrap();
            let path = ledger.path.clone();
            drop(ledger);
            let mut legacy: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            legacy["version"] = serde_json::json!(version);
            legacy.as_object_mut().unwrap().remove("reviews");
            legacy.as_object_mut().unwrap().remove("closed_reviews");
            let bytes = serde_json::to_vec(&legacy).unwrap();
            std::fs::write(&path, &bytes).unwrap();
            let mut ledger = Ledger::open(home.path(), binding).unwrap();
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            assert_eq!(ledger.record().attempt.as_ref().unwrap().input, input);
            assert!(ledger.prepare(&message).is_err());
            ledger.accept(&input, receipt()).unwrap();
            let saved: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            assert_eq!(saved["version"], VERSION);
            assert_eq!(saved["attempt"]["input"], input);
        }
    }

    #[test]
    fn legacy_question_history_without_retired_routes_is_refused_without_rewriting_it() {
        use crate::codex_input::review::{Closed, Outcome, Pending};
        let home = tempfile::tempdir().unwrap();
        let binding = binding(home.path());
        let mut ledger = Ledger::open(home.path(), binding.clone()).unwrap();
        ledger.bind_thread("thread".into()).unwrap();
        let event = serde_json::json!({"id":9,"method":"item/commandExecution/requestApproval","params":{"threadId":"thread","turnId":"turn","command":"echo trial","cwd":"/owned"}});
        let mut request =
            Pending::plan(&event, "thread", Some("turn"), "human", chrono::Utc::now()).unwrap();
        request.questions[0].message = Some("recent-question".to_owned().into());
        ledger
            .update_reviews(|_, closed| {
                closed.push_back(Closed {
                    request,
                    outcome: Outcome::Cancelled,
                    acknowledged: true,
                });
                Ok(true)
            })
            .unwrap();
        let path = ledger.path.clone();
        drop(ledger);
        let mut legacy: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        legacy["version"] = serde_json::json!(2);
        legacy.as_object_mut().unwrap().remove("retired_questions");
        let bytes = serde_json::to_vec(&legacy).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        assert!(
            Ledger::open(home.path(), binding).is_err(),
            "version 2 cannot prove that older question IDs were never evicted"
        );
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }

    #[test]
    fn version_three_populated_question_history_upgrades_without_losing_routes() {
        use crate::codex_input::review::{Closed, Outcome, Pending};
        let home = tempfile::tempdir().unwrap();
        let binding = binding(home.path());
        let mut ledger = Ledger::open(home.path(), binding.clone()).unwrap();
        ledger.bind_thread("thread".into()).unwrap();
        let event = serde_json::json!({"id":9,"method":"item/commandExecution/requestApproval","params":{"threadId":"thread","turnId":"turn","command":"echo trial","cwd":"/owned"}});
        let mut request =
            Pending::plan(&event, "thread", Some("turn"), "human", chrono::Utc::now()).unwrap();
        request.questions[0].message = Some("recent-question".to_owned().into());
        ledger
            .update_reviews(|_, closed| {
                closed.push_back(Closed {
                    request,
                    outcome: Outcome::Cancelled,
                    acknowledged: true,
                });
                Ok(true)
            })
            .unwrap();
        let path = ledger.path.clone();
        drop(ledger);
        let mut legacy: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        legacy["version"] = serde_json::json!(3);
        legacy["closed_reviews"][0]["request"]["questions"][0]
            .as_object_mut()
            .unwrap()
            .remove("presentation");
        let bytes = serde_json::to_vec(&legacy).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        let mut ledger = Ledger::open(home.path(), binding).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(
            ledger.record().closed_reviews[0].request.questions[0]
                .message
                .as_ref()
                .unwrap()
                .as_str(),
            "recent-question"
        );
        assert!(
            ledger.record().closed_reviews[0].request.questions[0]
                .presentation
                .is_none()
        );
        ledger.bind_thread("thread".into()).unwrap();
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(saved["version"], VERSION);
        assert_eq!(saved["closed_reviews"], legacy["closed_reviews"]);
        assert_eq!(saved["retired_questions"], legacy["retired_questions"]);
    }

    #[test]
    fn pending_questions_and_prepared_responses_survive_restart_without_becoming_input() {
        use crate::codex_input::review::{Closed, Outcome, Pending};
        let home = tempfile::tempdir().unwrap();
        let binding = binding(home.path());
        let mut ledger = Ledger::open(home.path(), binding.clone()).unwrap();
        ledger.bind_thread("thread".into()).unwrap();
        let input = ledger.prepare(&message()).unwrap();
        ledger.accept(&input, receipt()).unwrap();
        let original = ledger.record().attempt.as_ref().unwrap().message.clone();
        ledger.acknowledge(&original).unwrap();
        let event = serde_json::json!({"id":9,"method":"item/commandExecution/requestApproval","params":{"threadId":"thread","turnId":"turn","command":"echo trial","cwd":"/owned"}});
        let pending =
            Pending::plan(&event, "thread", Some("turn"), "human", chrono::Utc::now()).unwrap();
        ledger
            .update_reviews(|reviews, _| {
                reviews.push(pending);
                Ok(true)
            })
            .unwrap();
        drop(ledger);
        let mut ledger = Ledger::open(home.path(), binding.clone()).unwrap();
        assert_eq!(ledger.record().reviews.len(), 1);
        assert!(ledger.record().reviews[0].questions[0].message.is_none());
        assert!(ledger.finish("turn").is_err());
        assert!(ledger.prepare(&message()).is_err());
        let answer = Envelope::new(
            "human",
            Destination::Agent("owned-agent".into()),
            "answer",
            serde_json::json!({"text":"Allow"}),
            Some("question".to_owned().into()),
            chrono::Utc::now(),
        );
        ledger
            .update_reviews(|reviews, _| {
                reviews[0].questions[0].message = Some("question".to_owned().into());
                reviews[0].observe(
                    &agentdocker_core::EventKind::QuestionClosed {
                        question: "question".to_owned().into(),
                        answer: Some(answer.id.clone()),
                    },
                    "owned-agent",
                )?;
                reviews[0].capture(std::slice::from_ref(&answer), "owned-agent", false)?;
                reviews[0].response = reviews[0].reply(chrono::Utc::now())?;
                Ok(true)
            })
            .unwrap();
        drop(ledger);
        let mut ledger = Ledger::open(home.path(), binding.clone()).unwrap();
        let request = &ledger.record().reviews[0];
        assert_eq!(request.answers().next().unwrap().id, answer.id);
        assert_eq!(
            request.response.as_ref().unwrap()["result"]["decision"],
            "accept"
        );
        assert!(
            request.reply(chrono::Utc::now()).unwrap().is_none(),
            "prepared responses must never be replayed"
        );
        assert!(ledger.finish("turn").is_err());
        ledger
            .update_reviews(|reviews, closed| {
                closed.push_back(Closed {
                    request: reviews.remove(0),
                    outcome: Outcome::Resolved,
                    acknowledged: false,
                });
                Ok(true)
            })
            .unwrap();
        drop(ledger);
        let mut ledger = Ledger::open(home.path(), binding.clone()).unwrap();
        assert!(ledger.record().reviews.is_empty());
        assert!(!ledger.record().closed_reviews[0].acknowledged);
        assert_eq!(
            ledger.record().closed_reviews[0]
                .request
                .answers()
                .next()
                .unwrap()
                .id,
            answer.id
        );
        ledger.finish("turn").unwrap();
        assert_eq!(
            ledger.record().completed.len(),
            1,
            "only the original input is a model turn"
        );
        ledger
            .update_reviews(|_, closed| {
                closed.front_mut().unwrap().acknowledged = true;
                Ok(true)
            })
            .unwrap();
        ledger
            .update_reviews(|_, closed| {
                closed.pop_front();
                Ok(true)
            })
            .unwrap();
        drop(ledger);
        let mut ledger = Ledger::open(home.path(), binding).unwrap();
        let before = std::fs::read(&ledger.path).unwrap();
        assert!(
            ledger.prepare(&answer).is_err(),
            "retired question replies cannot become new input"
        );
        assert_eq!(std::fs::read(&ledger.path).unwrap(), before);
        assert!(ledger.prepare(&message()).is_ok());
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Receipt {
    pub thread: String,
    pub turn: String,
    pub item: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Attempt {
    pub message: String,
    pub input: String,
    pub receipt: Option<Receipt>,
    pub acknowledged: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Completed {
    pub message: String,
    pub receipt: Receipt,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Record {
    version: u32,
    pub binding: Binding,
    pub thread: Option<String>,
    pub attempt: Option<Attempt>,
    pub completed: VecDeque<Completed>,
    #[serde(default)]
    pub reviews: Vec<Pending>,
    #[serde(default)]
    pub closed_reviews: VecDeque<Closed>,
    #[serde(default)]
    retired_questions: Vec<MessageId>,
}

pub(super) struct Ledger {
    _owner: lock::Lock,
    path: PathBuf,
    record: Record,
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control)
}

impl Record {
    fn validate(&self, binding: &Binding) -> Result<()> {
        ensure!(
            self.version == VERSION
                || self.version == 3
                || (matches!(self.version, 1 | 2)
                    && self.reviews.is_empty()
                    && self.closed_reviews.is_empty()
                    && self.retired_questions.is_empty()),
            "unsupported Codex delivery record version"
        );
        ensure!(
            &self.binding == binding,
            "Codex delivery record belongs to another agent, workspace or provider profile"
        );
        ensure!(
            self.thread.as_deref().is_none_or(valid_id),
            "invalid retained Codex thread"
        );
        ensure!(
            self.completed.len() <= RETAINED_RECEIPTS,
            "too many retained Codex receipts"
        );
        for completed in &self.completed {
            ensure!(valid_id(&completed.message), "invalid retained message ID");
            self.validate_receipt(&completed.receipt)?;
        }
        ensure!(
            self.reviews.len() <= review::MAX_QUESTIONS
                && self.closed_reviews.len() <= review::RETAINED
                && self
                    .reviews
                    .iter()
                    .map(|r| r.questions.len())
                    .sum::<usize>()
                    <= review::MAX_QUESTIONS,
            "too many retained provider questions"
        );
        ensure!(
            self.reviews.is_empty() || self.attempt.is_some(),
            "provider question has no active input"
        );
        let mut requests = std::collections::HashSet::new();
        for request in &self.reviews {
            request.validate(self.thread.as_deref(), &self.binding.agent)?;
            ensure!(
                requests.insert(request.key()),
                "duplicate pending provider request"
            );
            if let Some(receipt) = self.attempt.as_ref().and_then(|a| a.receipt.as_ref()) {
                ensure!(
                    request.turn == receipt.turn,
                    "provider question belongs to another input turn"
                );
            }
        }
        let mut routes = std::collections::HashSet::new();
        ensure!(
            self.retired_questions.len() <= MAX_RETIRED_QUESTIONS,
            "provider question route history is full; delivery must pause"
        );
        for id in &self.retired_questions {
            ensure!(
                valid_id(id.as_str()) && routes.insert(id),
                "invalid or repeated retired provider question"
            );
        }
        for request in self
            .reviews
            .iter()
            .chain(self.closed_reviews.iter().map(|r| &r.request))
        {
            request.validate(self.thread.as_deref(), &self.binding.agent)?;
            for question in &request.questions {
                if let Some(id) = &question.message {
                    ensure!(
                        routes.insert(id),
                        "provider requests share a question route"
                    );
                }
            }
        }
        for closed in &self.closed_reviews {
            ensure!(
                closed.outcome != review::Outcome::Resolved || closed.request.response.is_some(),
                "resolved provider request has no prepared response"
            );
        }
        if let Some(attempt) = &self.attempt {
            ensure!(
                self.thread.is_some(),
                "delivery attempt has no retained thread"
            );
            ensure!(
                valid_id(&attempt.message)
                    && !attempt.input.is_empty()
                    && attempt.input.len() <= MAX_INPUT_BYTES,
                "invalid retained Codex input"
            );
            ensure!(
                !attempt.acknowledged || attempt.receipt.is_some(),
                "acknowledged input has no provider receipt"
            );
            if let Some(receipt) = &attempt.receipt {
                self.validate_receipt(receipt)?;
            }
            ensure!(
                !self
                    .completed
                    .iter()
                    .any(|done| done.message == attempt.message),
                "pending input duplicates a completed receipt"
            );
        }
        Ok(())
    }

    fn validate_receipt(&self, receipt: &Receipt) -> Result<()> {
        ensure!(
            self.thread.as_ref() == Some(&receipt.thread)
                && valid_id(&receipt.turn)
                && valid_id(&receipt.item),
            "provider receipt has the wrong thread or invalid IDs"
        );
        Ok(())
    }
}

impl Ledger {
    pub fn open(home: &Path, binding: Binding) -> Result<Self> {
        ensure!(
            !binding.agent.is_empty()
                && binding.agent.len() <= 128
                && binding
                    .agent
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
            "invalid agent ID for Codex input ownership"
        );
        ensure!(
            binding.socket.is_absolute()
                && binding.cwd.is_absolute()
                && binding.provider_home.is_absolute(),
            "Codex input paths must be absolute"
        );
        let parent = home.join("codex-input");
        dirs::secure_state_dir(&parent)?;
        let directory = parent.join(&binding.agent);
        dirs::secure_state_dir(&directory)?;
        let lock_path = directory.join("owner.lock");
        dirs::private_file(&lock_path, true, false)?;
        let owner = lock::try_exclusive_existing(&lock_path)?
            .context("another Codex input bridge already owns this agent")?;
        let path = directory.join("delivery.json");
        let mut record = match dirs::read_private_file(&path) {
            Ok(file) => {
                let mut data = Vec::new();
                file.take((MAX_STATE_BYTES + 1) as u64)
                    .read_to_end(&mut data)?;
                ensure!(
                    data.len() <= MAX_STATE_BYTES,
                    "Codex delivery record exceeds its size limit"
                );
                serde_json::from_slice::<Record>(&data)
                    .context("cannot read the retained Codex delivery record")?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Record {
                version: VERSION,
                binding: binding.clone(),
                thread: None,
                attempt: None,
                completed: VecDeque::new(),
                reviews: Vec::new(),
                closed_reviews: VecDeque::new(),
                retired_questions: Vec::new(),
            },
            Err(error) => return Err(error.into()),
        };
        record.validate(&binding)?;
        record.version = VERSION;
        Ok(Self {
            _owner: owner,
            path,
            record,
        })
    }

    pub fn record(&self) -> &Record {
        &self.record
    }

    pub fn update_reviews(
        &mut self,
        edit: impl FnOnce(&mut Vec<Pending>, &mut VecDeque<Closed>) -> Result<bool>,
    ) -> Result<()> {
        let mut next = self.record.clone();
        if edit(&mut next.reviews, &mut next.closed_reviews)? {
            // Detailed receipts rotate, but forgetting their routing identity
            // would turn a later answer into a new ordinary model prompt.
            for old in &self.record.closed_reviews {
                for id in old
                    .request
                    .questions
                    .iter()
                    .filter_map(|q| q.message.as_ref())
                {
                    if !next.closed_reviews.iter().any(|r| {
                        r.request
                            .questions
                            .iter()
                            .any(|q| q.message.as_ref() == Some(id))
                    }) {
                        ensure!(
                            old.acknowledged,
                            "cannot retire unacknowledged provider answers"
                        );
                        next.retired_questions.push(id.clone());
                    }
                }
            }
            self.save(next)?;
        }
        Ok(())
    }

    fn save(&mut self, next: Record) -> Result<()> {
        next.validate(&self.record.binding)?;
        let bytes = serde_json::to_vec(&next)?;
        ensure!(
            bytes.len() <= MAX_STATE_BYTES,
            "Codex delivery record exceeds its size limit"
        );
        // Validate an existing destination before replacing it. The private
        // directory and lifetime ownership lock are shared by all bridge writers.
        match dirs::read_private_file(&self.path) {
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
        let directory = self
            .path
            .parent()
            .context("delivery record has no directory")?;
        let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
        temporary.write_all(&bytes)?;
        temporary.as_file().sync_all()?;
        temporary.persist(&self.path)?;
        File::open(directory)?.sync_all()?;
        // Failed persistence never advances the in-memory submission state.
        self.record = next;
        Ok(())
    }

    pub fn bind_thread(&mut self, thread: String) -> Result<()> {
        ensure!(valid_id(&thread), "invalid Codex thread ID");
        ensure!(
            self.record.thread.as_ref().is_none_or(|old| old == &thread),
            "cannot replace the retained Codex conversation"
        );
        let mut next = self.record.clone();
        next.thread = Some(thread);
        self.save(next)
    }

    pub fn discard_unused_thread(&mut self) -> Result<()> {
        ensure!(
            self.record.attempt.is_none()
                && self.record.completed.is_empty()
                && self.record.reviews.is_empty()
                && self.record.closed_reviews.is_empty(),
            "a conversation with prepared or accepted input cannot be discarded"
        );
        if self.record.thread.is_some() {
            let mut next = self.record.clone();
            next.thread = None;
            self.save(next)?;
        }
        Ok(())
    }

    pub fn prepare(&mut self, envelope: &Envelope) -> Result<String> {
        ensure!(
            envelope
                .reply_to
                .as_ref()
                .is_none_or(|id| !self.record.retired_questions.contains(id)),
            "this reply names a retired provider question; it remains queued for review and cannot become new input"
        );
        ensure!(
            self.record.thread.is_some(),
            "Codex conversation is not ready"
        );
        ensure!(
            self.record.attempt.is_none(),
            "an earlier Codex input still needs receipt recovery; automatic resubmission is refused"
        );
        ensure!(
            self.record.reviews.is_empty(),
            "a provider question still needs response recovery"
        );
        let message = envelope.id.to_string();
        ensure!(
            !self
                .record
                .completed
                .iter()
                .any(|done| done.message == message),
            "this input already has a completed provider receipt"
        );
        let input = serde_json::to_string(&serde_json::json!({"agentdocker_message": envelope}))?;
        ensure!(
            input.len() <= MAX_INPUT_BYTES,
            "queued input is too large for the Codex bridge"
        );
        let mut next = self.record.clone();
        next.attempt = Some(Attempt {
            message,
            input: input.clone(),
            receipt: None,
            acknowledged: false,
        });
        self.save(next)?;
        Ok(input)
    }

    pub fn accept(&mut self, input: &str, receipt: Receipt) -> Result<()> {
        self.record.validate_receipt(&receipt)?;
        let mut next = self.record.clone();
        let attempt = next
            .attempt
            .as_mut()
            .context("provider receipt has no prepared input")?;
        ensure!(
            attempt.input == input,
            "provider receipt does not match the complete submitted input"
        );
        ensure!(
            attempt.receipt.as_ref().is_none_or(|old| old == &receipt),
            "conflicting provider receipt"
        );
        attempt.receipt = Some(receipt);
        self.save(next)
    }

    pub fn acknowledge(&mut self, message: &str) -> Result<()> {
        let mut next = self.record.clone();
        let attempt = next
            .attempt
            .as_mut()
            .context("queue acknowledgement has no prepared input")?;
        ensure!(
            attempt.message == message && attempt.receipt.is_some(),
            "queue acknowledgement requires the exact provider receipt"
        );
        attempt.acknowledged = true;
        self.save(next)
    }

    pub fn finish(&mut self, turn: &str) -> Result<()> {
        ensure!(
            self.record.reviews.is_empty(),
            "provider turn still has unresolved questions"
        );
        let mut next = self.record.clone();
        let attempt = next
            .attempt
            .take()
            .context("completed turn has no prepared input")?;
        let receipt = attempt
            .receipt
            .context("completed turn has no provider receipt")?;
        ensure!(
            attempt.acknowledged && receipt.turn == turn,
            "completed turn does not match the acknowledged input"
        );
        next.completed.push_back(Completed {
            message: attempt.message,
            receipt,
        });
        while next.completed.len() > RETAINED_RECEIPTS {
            next.completed.pop_front();
        }
        self.save(next)
    }
}
