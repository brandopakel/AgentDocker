//! One durable provider input attempt. Queue acknowledgement follows an exact
//! provider receipt; a prepared attempt can never be submitted automatically again.
use super::{
    durable::Durable,
    mcp_answers::{Answer, Origin},
    review::{self, Closed, Pending},
};
use agentdocker_core::{Envelope, MessageId};
use agentdocker_host::dirs;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
};

const VERSION: u32 = 15;
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
    use std::io::Read;
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
    fn steering_survives_restart_and_keeps_its_receipt_separate_from_the_starting_input() {
        let home = tempfile::tempdir().unwrap();
        let binding = binding(home.path());
        let mut ledger = Ledger::open(home.path(), binding.clone()).unwrap();
        ledger.bind_thread("thread".into()).unwrap();
        let first = message();
        let input = ledger.prepare(&first).unwrap();
        let next = message();
        assert!(ledger.prepare_steering(&next, "turn").is_err());
        ledger.accept(&input, receipt()).unwrap();
        ledger.acknowledge(first.id.as_str()).unwrap();
        assert!(ledger.prepare_steering(&first, "turn").is_err());
        assert!(ledger.prepare_steering(&next, "wrong").is_err());
        let steering = ledger.prepare_steering(&next, "turn").unwrap();
        let path = ledger.file.path().to_path_buf();
        let prepared = std::fs::read(&path).unwrap();
        drop(ledger);
        let mut ledger = Ledger::open(home.path(), binding).unwrap();
        assert_eq!(ledger.record().steering.as_ref().unwrap().input, steering);
        assert!(ledger.prepare_steering(&next, "turn").is_err());
        assert!(ledger.finish("turn").is_err());
        assert!(ledger.acknowledge(next.id.as_str()).is_err());
        let mut wrong = receipt();
        wrong.turn = "another-turn".into();
        wrong.item = "steering".into();
        assert!(ledger.accept(&steering, wrong).is_err());
        assert!(ledger.accept(&steering, receipt()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), prepared);
        let mut accepted = receipt();
        accepted.item = "steering".into();
        ledger.accept(&steering, accepted.clone()).unwrap();
        assert!(ledger.reject_steering().is_err());
        ledger.acknowledge(next.id.as_str()).unwrap();
        ledger.finish_steering().unwrap();
        assert_eq!(
            ledger.record().attempt.as_ref().unwrap().message,
            first.id.as_str()
        );
        assert_eq!(ledger.record().completed.back().unwrap().receipt, accepted);
        assert!(ledger.prepare_steering(&next, "turn").is_err());
        let third = message();
        let third_input = ledger.prepare_steering(&third, "turn").unwrap();
        let prepared = std::fs::read(&path).unwrap();
        assert!(ledger.accept(&third_input, accepted.clone()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), prepared);
        ledger.reject_steering().unwrap();
        ledger.finish("turn").unwrap();
        assert_eq!(ledger.record().completed.len(), 2);
    }

    #[test]
    fn a_rejected_prepared_input_leaves_the_message_queued_and_an_accepted_one_stays() {
        let home = tempfile::tempdir().unwrap();
        let mut ledger = Ledger::open(home.path(), binding(home.path())).unwrap();
        ledger.bind_thread("thread".into()).unwrap();
        assert!(ledger.reject_prepared().is_err(), "nothing is prepared yet");
        let message = message();
        let input = ledger.prepare(&message).unwrap();
        ledger.reject_prepared().unwrap();
        assert!(ledger.record().attempt.is_none());
        let again = ledger.prepare(&message).unwrap();
        assert_eq!(input, again, "the same message can be prepared again later");
        ledger.accept(&again, receipt()).unwrap();
        assert!(
            ledger.reject_prepared().is_err(),
            "a receipt is the provider's word that the input was taken"
        );
        drop(ledger);
        let reopened = Ledger::open(home.path(), binding(home.path())).unwrap();
        assert!(
            reopened
                .record()
                .attempt
                .as_ref()
                .unwrap()
                .receipt
                .is_some()
        );
    }

    #[test]
    fn rejected_steering_leaves_the_message_eligible_for_a_later_ordinary_turn() {
        let home = tempfile::tempdir().unwrap();
        let mut ledger = Ledger::open(home.path(), binding(home.path())).unwrap();
        ledger.bind_thread("thread".into()).unwrap();
        let first = message();
        let input = ledger.prepare(&first).unwrap();
        ledger.accept(&input, receipt()).unwrap();
        ledger.acknowledge(first.id.as_str()).unwrap();
        let next = message();
        let input = ledger.prepare_steering(&next, "turn").unwrap();
        ledger.reject_steering().unwrap();
        assert!(ledger.record().completed.is_empty());
        ledger.finish("turn").unwrap();
        assert_eq!(ledger.prepare(&next).unwrap(), input);
    }

    #[test]
    fn version_nine_upgrades_without_rewriting_pending_input_or_accepting_new_receipt_semantics() {
        let home = tempfile::tempdir().unwrap();
        let binding = binding(home.path());
        let mut ledger = Ledger::open(home.path(), binding.clone()).unwrap();
        ledger.bind_thread("thread".into()).unwrap();
        let first = message();
        let input = ledger.prepare(&first).unwrap();
        let path = ledger.file.path().to_path_buf();
        drop(ledger);
        let mut old: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        old["version"] = serde_json::json!(9);
        old.as_object_mut().unwrap().remove("steering");
        let bytes = serde_json::to_vec(&old).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        let mut ledger = Ledger::open(home.path(), binding.clone()).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert!(ledger.prepare(&first).is_err());
        ledger.accept(&input, receipt()).unwrap();
        ledger.acknowledge(first.id.as_str()).unwrap();
        ledger.prepare_steering(&message(), "turn").unwrap();
        drop(ledger);
        let mut incompatible: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        incompatible["version"] = serde_json::json!(9);
        let bytes = serde_json::to_vec(&incompatible).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        assert!(Ledger::open(home.path(), binding).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }

    #[test]
    fn prepared_input_survives_restart_and_cannot_be_repeated_or_acknowledged_without_proof() {
        let home = tempfile::tempdir().unwrap();
        let binding = binding(home.path());
        let message = message();
        let mut ledger = Ledger::open(home.path(), binding.clone()).unwrap();
        ledger.bind_thread("thread".into()).unwrap();
        let input = ledger.prepare(&message).unwrap();
        let path = ledger.file.path().to_path_buf();
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
        let path = ledger.file.path().to_path_buf();
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
    fn prepared_input_replaces_an_open_snapshot_and_reopens_without_replay() {
        let home = tempfile::tempdir().unwrap();
        let binding = binding(home.path());
        let mut ledger = Ledger::open(home.path(), binding.clone()).unwrap();
        ledger.bind_thread("thread".into()).unwrap();
        let before = std::fs::read(ledger.file.path()).unwrap();
        let mut reader = agentdocker_host::files::open_regular(ledger.file.path()).unwrap();
        let message = message();
        let input = ledger.prepare(&message).unwrap();
        let mut after = Vec::new();
        dirs::read_private_file(ledger.file.path())
            .unwrap()
            .read_to_end(&mut after)
            .unwrap();
        assert_ne!(after, before);
        let mut retained = Vec::new();
        reader.read_to_end(&mut retained).unwrap();
        assert_eq!(retained, before);
        drop(reader);
        drop(ledger);
        let mut reopened = Ledger::open(home.path(), binding).unwrap();
        let attempt = reopened.record().attempt.as_ref().unwrap();
        assert_eq!(attempt.message, message.id.as_str());
        assert_eq!(attempt.input, input);
        assert!(reopened.prepare(&message).is_err());
        assert_eq!(std::fs::read(reopened.file.path()).unwrap(), after);
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
        let before = std::fs::read(ledger.file.path()).unwrap();
        assert!(ledger.discard_unused_thread().is_err());
        assert_eq!(std::fs::read(ledger.file.path()).unwrap(), before);
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
            let before = std::fs::read(ledger.file.path()).unwrap();
            let mut wrong = receipt.clone();
            wrong.item.push('x');
            assert!(ledger.accept(&input, wrong).is_err());
            assert_eq!(std::fs::read(ledger.file.path()).unwrap(), before);
            ledger.acknowledge(message.id.as_str()).unwrap();
            ledger.finish(&receipt.turn).unwrap();
        }
        assert_eq!(ledger.record().completed.len(), RETAINED_RECEIPTS);
        assert_eq!(ledger.record().completed[0].receipt.turn, "turn-3");
    }

    #[test]
    fn legacy_delivery_state_upgrades_without_replacing_prepared_input() {
        for version in 1..=13 {
            let home = tempfile::tempdir().unwrap();
            let binding = binding(home.path());
            let mut ledger = Ledger::open(home.path(), binding.clone()).unwrap();
            ledger.bind_thread("thread".into()).unwrap();
            let message = message();
            let input = ledger.prepare(&message).unwrap();
            let path = ledger.file.path().to_path_buf();
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
    fn file_review_history_requires_version_six_and_round_trips_without_losing_the_diff() {
        use crate::codex_input::review::Pending;
        use agentdocker_core::{QuestionFileChange, QuestionFileChangeKind, QuestionPresentation};
        let home = tempfile::tempdir().unwrap();
        let binding = binding(home.path());
        let mut ledger = Ledger::open(home.path(), binding.clone()).unwrap();
        ledger.bind_thread("thread".into()).unwrap();
        ledger.prepare(&message()).unwrap();
        let presentation = QuestionPresentation::CodexFiles {
            cwd: "/owned".into(),
            reason: "Fixture".into(),
            changes: vec![QuestionFileChange {
                path: "/owned/a".into(),
                kind: QuestionFileChangeKind::Add,
                diff: "+new\n".into(),
            }],
        };
        let event = serde_json::json!({"id":10,"method":"item/fileChange/requestApproval","params":{"threadId":"thread","turnId":"turn","itemId":"patch"}});
        let pending = Pending::plan_with_files(
            &event,
            "thread",
            Some("turn"),
            "human",
            chrono::Utc::now(),
            Some(presentation.clone()),
        )
        .unwrap();
        ledger
            .update_reviews(|reviews, _| {
                reviews.push(pending);
                Ok(true)
            })
            .unwrap();
        let path = ledger.file.path().to_path_buf();
        drop(ledger);
        let original = std::fs::read(&path).unwrap();
        let mut old: serde_json::Value = serde_json::from_slice(&original).unwrap();
        old["version"] = serde_json::json!(5);
        let old = serde_json::to_vec(&old).unwrap();
        std::fs::write(&path, &old).unwrap();
        assert!(
            Ledger::open(home.path(), binding.clone())
                .err()
                .unwrap()
                .to_string()
                .contains("legacy input cannot supply file-change review receipts")
        );
        assert_eq!(std::fs::read(&path).unwrap(), old);
        let mut version_six: serde_json::Value = serde_json::from_slice(&original).unwrap();
        version_six["version"] = serde_json::json!(6);
        let version_six = serde_json::to_vec(&version_six).unwrap();
        std::fs::write(&path, &version_six).unwrap();
        let old_files = Ledger::open(home.path(), binding.clone()).unwrap();
        assert_eq!(
            old_files.record().reviews[0].questions[0].presentation,
            Some(presentation.clone())
        );
        assert_eq!(std::fs::read(&path).unwrap(), version_six);
        drop(old_files);
        std::fs::write(&path, &original).unwrap();
        let reopened = Ledger::open(home.path(), binding).unwrap();
        assert_eq!(
            reopened.record().reviews[0].questions[0].presentation,
            Some(presentation)
        );
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }

    #[test]
    fn command_cancellation_requires_version_eight_without_rewriting_older_records() {
        let home = tempfile::tempdir().unwrap();
        let binding = binding(home.path());
        let mut ledger = Ledger::open(home.path(), binding.clone()).unwrap();
        ledger.bind_thread("thread".into()).unwrap();
        ledger.prepare(&message()).unwrap();
        let event = serde_json::json!({"id":12,"method":"item/commandExecution/requestApproval","params":{"threadId":"thread","turnId":"turn","cwd":"/owned","command":"printf fixture","availableDecisions":["accept","cancel"]}});
        let pending = super::super::review::Pending::plan(
            &event,
            "thread",
            Some("turn"),
            "human",
            chrono::Utc::now(),
        )
        .unwrap();
        assert!(pending.has_command_cancellation());
        ledger
            .update_reviews(|reviews, _| {
                reviews.push(pending);
                Ok(true)
            })
            .unwrap();
        let path = ledger.file.path().to_path_buf();
        drop(ledger);
        let original = std::fs::read(&path).unwrap();
        let mut legacy: serde_json::Value = serde_json::from_slice(&original).unwrap();
        legacy["version"] = serde_json::json!(7);
        let legacy = serde_json::to_vec(&legacy).unwrap();
        std::fs::write(&path, &legacy).unwrap();
        assert!(
            Ledger::open(home.path(), binding.clone())
                .err()
                .unwrap()
                .to_string()
                .contains("legacy input cannot supply command cancellation review receipts")
        );
        assert_eq!(std::fs::read(&path).unwrap(), legacy);
        std::fs::write(&path, &original).unwrap();
        let reopened = Ledger::open(home.path(), binding).unwrap();
        assert!(reopened.record().reviews[0].has_command_cancellation());
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }

    #[test]
    fn network_review_requires_version_nine_and_preserves_older_ledger_bytes() {
        let home = tempfile::tempdir().unwrap();
        let binding = binding(home.path());
        let mut ledger = Ledger::open(home.path(), binding.clone()).unwrap();
        ledger.bind_thread("thread".into()).unwrap();
        ledger.prepare(&message()).unwrap();
        let event = serde_json::json!({"id":15,"method":"item/commandExecution/requestApproval","params":{
            "threadId":"thread","turnId":"turn","networkApprovalContext":{"host":"example.com","protocol":"https"},"availableDecisions":["accept","decline"]
        }});
        let pending = super::super::review::Pending::plan(
            &event,
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
        let path = ledger.file.path().to_path_buf();
        drop(ledger);
        let original = std::fs::read(&path).unwrap();
        let mut legacy: serde_json::Value = serde_json::from_slice(&original).unwrap();
        legacy["version"] = serde_json::json!(8);
        let legacy = serde_json::to_vec(&legacy).unwrap();
        std::fs::write(&path, &legacy).unwrap();
        assert!(
            Ledger::open(home.path(), binding.clone())
                .err()
                .unwrap()
                .to_string()
                .contains("legacy input cannot supply network-only review receipts")
        );
        assert_eq!(std::fs::read(&path).unwrap(), legacy);
        std::fs::write(&path, &original).unwrap();
        let reopened = Ledger::open(home.path(), binding).unwrap();
        assert!(reopened.record().reviews[0].is_network_review());
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }

    #[test]
    fn stdin_reviews_require_version_eleven_in_open_and_closed_history_without_rewriting() {
        for closed in [false, true] {
            let home = tempfile::tempdir().unwrap();
            let binding = binding(home.path());
            let mut ledger = Ledger::open(home.path(), binding.clone()).unwrap();
            ledger.bind_thread("thread".into()).unwrap();
            ledger.prepare(&message()).unwrap();
            let event = serde_json::json!({"id":"stdin","method":"item/commandExecution/requestApproval","params":{
                "threadId":"thread","turnId":"turn","itemId":"command","approvalId":"input",
                "kind":"writeStdin","command":"write_stdin --session-id 123 input","cwd":"/owned",
                "availableDecisions":["accept","cancel"]
            }});
            let pending =
                review::Pending::plan(&event, "thread", Some("turn"), "human", chrono::Utc::now())
                    .unwrap();
            ledger
                .update_reviews(|reviews, history| {
                    if closed {
                        history.push_back(review::Closed {
                            request: pending,
                            outcome: review::Outcome::Cancelled,
                            acknowledged: true,
                        });
                    } else {
                        reviews.push(pending);
                    }
                    Ok(true)
                })
                .unwrap();
            let path = ledger.file.path().to_path_buf();
            drop(ledger);
            let original = std::fs::read(&path).unwrap();
            let mut legacy: serde_json::Value = serde_json::from_slice(&original).unwrap();
            legacy["version"] = serde_json::json!(10);
            let bytes = serde_json::to_vec(&legacy).unwrap();
            std::fs::write(&path, &bytes).unwrap();
            assert!(
                Ledger::open(home.path(), binding.clone())
                    .err()
                    .unwrap()
                    .to_string()
                    .contains("legacy input cannot supply terminal input review receipts")
            );
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            std::fs::write(&path, &original).unwrap();
            let reopened = Ledger::open(home.path(), binding).unwrap();
            assert_eq!(reopened.record().version, VERSION);
            assert_eq!(std::fs::read(&path).unwrap(), original);
        }
    }

    #[test]
    fn mcp_url_reviews_require_version_twelve_in_open_and_closed_history() {
        for closed in [false, true] {
            let home = tempfile::tempdir().unwrap();
            let binding = binding(home.path());
            let mut ledger = Ledger::open(home.path(), binding.clone()).unwrap();
            ledger.bind_thread("thread".into()).unwrap();
            ledger.prepare(&message()).unwrap();
            let event = serde_json::json!({"id":"url","method":"mcpServer/elicitation/request","params":{
                "threadId":"thread","turnId":null,"serverName":"fixture","mode":"url",
                "elicitationId":"one","message":"Connect the fixture.","url":"https://example.com/consent"
            }});
            let pending =
                review::Pending::plan(&event, "thread", Some("turn"), "human", chrono::Utc::now())
                    .unwrap();
            ledger
                .update_reviews(|reviews, history| {
                    if closed {
                        history.push_back(review::Closed {
                            request: pending,
                            outcome: review::Outcome::Cancelled,
                            acknowledged: true,
                        });
                    } else {
                        reviews.push(pending);
                    }
                    Ok(true)
                })
                .unwrap();
            let path = ledger.file.path().to_path_buf();
            drop(ledger);
            let original = std::fs::read(&path).unwrap();
            let mut legacy: serde_json::Value = serde_json::from_slice(&original).unwrap();
            legacy["version"] = serde_json::json!(11);
            let bytes = serde_json::to_vec(&legacy).unwrap();
            std::fs::write(&path, &bytes).unwrap();
            assert!(
                Ledger::open(home.path(), binding.clone())
                    .err()
                    .unwrap()
                    .to_string()
                    .contains("legacy input cannot supply MCP URL review receipts")
            );
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            std::fs::write(&path, &original).unwrap();
            let reopened = Ledger::open(home.path(), binding).unwrap();
            assert_eq!(reopened.record().version, VERSION);
            assert_eq!(std::fs::read(&path).unwrap(), original);
        }
    }

    #[test]
    fn idle_mcp_reviews_block_input_and_require_version_fourteen_in_both_histories() {
        for form in [false, true] {
            for closed in [false, true] {
                let home = tempfile::tempdir().unwrap();
                let binding = binding(home.path());
                let mut ledger = Ledger::open(home.path(), binding.clone()).unwrap();
                ledger.bind_thread("thread".into()).unwrap();
                let mut event = serde_json::json!({"id":"idle","method":"mcpServer/elicitation/request",
                    "params":{"threadId":"thread","serverName":"fixture","mode":"url","message":"Review.",
                    "elicitationId":"request","url":"https://example.com/consent"}});
                if form {
                    event["params"] = serde_json::json!({"threadId":"thread","serverName":"fixture","mode":"form",
                        "message":"Choose.","requestedSchema":{"type":"object","properties":{"enabled":{"type":"boolean"}}}});
                }
                let pending =
                    review::Pending::plan(&event, "thread", None, "human", chrono::Utc::now())
                        .unwrap();
                ledger
                    .update_reviews(|reviews, history| {
                        if closed {
                            history.push_back(review::Closed {
                                request: pending,
                                outcome: review::Outcome::Cancelled,
                                acknowledged: true,
                            });
                        } else {
                            reviews.push(pending);
                        }
                        Ok(true)
                    })
                    .unwrap();
                let path = ledger.file.path().to_path_buf();
                let original = std::fs::read(&path).unwrap();
                if !closed {
                    assert!(ledger.prepare(&message()).is_err());
                    assert_eq!(std::fs::read(&path).unwrap(), original);
                    let mut overlapping = ledger.record().clone();
                    overlapping.attempt = Some(Attempt {
                        message: message().id.to_string(),
                        input: serde_json::to_string(&message()).unwrap(),
                        mcp_origin: None,
                        receipt: None,
                        acknowledged: false,
                    });
                    assert!(
                        overlapping
                            .validate(&binding)
                            .unwrap_err()
                            .to_string()
                            .contains("idle MCP review cannot overlap")
                    );
                }
                drop(ledger);
                let mut legacy: serde_json::Value = serde_json::from_slice(&original).unwrap();
                legacy["version"] = serde_json::json!(13);
                let bytes = serde_json::to_vec(&legacy).unwrap();
                std::fs::write(&path, &bytes).unwrap();
                assert!(
                    Ledger::open(home.path(), binding.clone())
                        .err()
                        .unwrap()
                        .to_string()
                        .contains("legacy input cannot supply idle MCP review receipts")
                );
                assert_eq!(std::fs::read(&path).unwrap(), bytes);
                std::fs::write(&path, &original).unwrap();
                let mut reopened = Ledger::open(home.path(), binding).unwrap();
                assert_eq!(std::fs::read(&path).unwrap(), original);
                assert_eq!(reopened.record().version, VERSION);
                if closed {
                    reopened.prepare(&message()).unwrap();
                } else {
                    assert!(reopened.prepare(&message()).is_err());
                }
            }
        }
    }

    #[test]
    fn mcp_form_reviews_require_version_thirteen_in_open_and_closed_history() {
        for closed in [false, true] {
            let home = tempfile::tempdir().unwrap();
            let binding = binding(home.path());
            let mut ledger = Ledger::open(home.path(), binding.clone()).unwrap();
            ledger.bind_thread("thread".into()).unwrap();
            ledger.prepare(&message()).unwrap();
            let event = serde_json::json!({"id":"form","method":"mcpServer/elicitation/request","params":{
                "threadId":"thread","turnId":null,"serverName":"fixture","mode":"form","message":"Choose.",
                "requestedSchema":{"type":"object","properties":{"value":{"type":"boolean"}}}
            }});
            let pending =
                review::Pending::plan(&event, "thread", Some("turn"), "human", chrono::Utc::now())
                    .unwrap();
            ledger
                .update_reviews(|reviews, history| {
                    if closed {
                        history.push_back(review::Closed {
                            request: pending,
                            outcome: review::Outcome::Cancelled,
                            acknowledged: true,
                        });
                    } else {
                        reviews.push(pending);
                    }
                    Ok(true)
                })
                .unwrap();
            let path = ledger.file.path().to_path_buf();
            drop(ledger);
            let original = std::fs::read(&path).unwrap();
            let mut legacy: serde_json::Value = serde_json::from_slice(&original).unwrap();
            legacy["version"] = serde_json::json!(12);
            let bytes = serde_json::to_vec(&legacy).unwrap();
            std::fs::write(&path, &bytes).unwrap();
            assert!(
                Ledger::open(home.path(), binding.clone())
                    .err()
                    .unwrap()
                    .to_string()
                    .contains("legacy input cannot supply MCP form review receipts")
            );
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            std::fs::write(&path, &original).unwrap();
            let reopened = Ledger::open(home.path(), binding).unwrap();
            assert_eq!(reopened.record().version, VERSION);
            assert_eq!(std::fs::read(&path).unwrap(), original);
        }
    }

    #[test]
    fn permission_review_history_requires_version_seven_without_rewriting_legacy_state() {
        let home = tempfile::tempdir().unwrap();
        let binding = binding(home.path());
        let mut ledger = Ledger::open(home.path(), binding.clone()).unwrap();
        ledger.bind_thread("thread".into()).unwrap();
        ledger.prepare(&message()).unwrap();
        let event = serde_json::json!({"id":11,"method":"item/permissions/requestApproval","params":{"threadId":"thread","turnId":"turn","itemId":"permissions","cwd":"/owned","permissions":{"fileSystem":{"write":["/owned/output"]}}}});
        let pending = super::super::review::Pending::plan(
            &event,
            "thread",
            Some("turn"),
            "human",
            chrono::Utc::now(),
        )
        .unwrap();
        let presentation = pending.questions[0].presentation.clone();
        ledger
            .update_reviews(|reviews, _| {
                reviews.push(pending);
                Ok(true)
            })
            .unwrap();
        let path = ledger.file.path().to_path_buf();
        drop(ledger);
        let original = std::fs::read(&path).unwrap();
        let mut legacy: serde_json::Value = serde_json::from_slice(&original).unwrap();
        legacy["version"] = serde_json::json!(6);
        let legacy = serde_json::to_vec(&legacy).unwrap();
        std::fs::write(&path, &legacy).unwrap();
        assert!(
            Ledger::open(home.path(), binding.clone())
                .err()
                .unwrap()
                .to_string()
                .contains("legacy input cannot supply permission review receipts")
        );
        assert_eq!(std::fs::read(&path).unwrap(), legacy);
        std::fs::write(&path, &original).unwrap();
        let reopened = Ledger::open(home.path(), binding).unwrap();
        assert_eq!(
            reopened.record().reviews[0].questions[0].presentation,
            presentation
        );
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }

    #[test]
    fn legacy_question_history_without_retired_routes_is_refused_without_rewriting_it() {
        use crate::codex_input::review::{Closed, Outcome, Pending};
        let home = tempfile::tempdir().unwrap();
        let binding = binding(home.path());
        let mut ledger = Ledger::open(home.path(), binding.clone()).unwrap();
        ledger.bind_thread("thread".into()).unwrap();
        let event = serde_json::json!({"id":9,"method":"item/commandExecution/requestApproval","params":{"threadId":"thread","turnId":"turn","command":"echo trial","cwd":"/owned","availableDecisions":["accept","decline"]}});
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
        let path = ledger.file.path().to_path_buf();
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
        let event = serde_json::json!({"id":9,"method":"item/commandExecution/requestApproval","params":{"threadId":"thread","turnId":"turn","command":"echo trial","cwd":"/owned","availableDecisions":["accept","decline"]}});
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
        let path = ledger.file.path().to_path_buf();
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
        let event = serde_json::json!({"id":9,"method":"item/commandExecution/requestApproval","params":{"threadId":"thread","turnId":"turn","command":"echo trial","cwd":"/owned","availableDecisions":["accept","decline"]}});
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
        let before = std::fs::read(ledger.file.path()).unwrap();
        assert!(
            ledger.prepare(&answer).is_err(),
            "retired question replies cannot become new input"
        );
        assert_eq!(std::fs::read(ledger.file.path()).unwrap(), before);
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
    #[serde(default)]
    pub mcp_origin: Option<Origin>,
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
    /// One additional input being steered into the accepted active turn.
    /// Its receipt is independent of the input that started that turn.
    #[serde(default)]
    pub steering: Option<Attempt>,
    pub completed: VecDeque<Completed>,
    #[serde(default)]
    pub reviews: Vec<Pending>,
    #[serde(default)]
    pub closed_reviews: VecDeque<Closed>,
    #[serde(default)]
    pub secret_review: Option<super::secret_requests::Fence>,
    #[serde(default)]
    retired_questions: Vec<MessageId>,
    #[serde(default)]
    pub mcp_answers: VecDeque<Answer>,
}

pub(super) struct Ledger {
    file: Durable,
    record: Record,
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control)
}

impl Record {
    fn validate(&self, binding: &Binding) -> Result<()> {
        ensure!(
            self.version >= 15 || self.secret_review.is_none(),
            "legacy input cannot supply a temporary response fence"
        );
        if let Some(fence) = &self.secret_review {
            ensure!(
                self.reviews.is_empty()
                    && self.attempt.as_ref().is_some_and(|a| a.acknowledged)
                    && fence.valid(
                        self.thread.as_deref(),
                        self.attempt
                            .as_ref()
                            .and_then(|a| a.receipt.as_ref())
                            .map(|r| r.turn.as_str())
                    ),
                "temporary response fence has another input or overlaps ordinary questions"
            );
        }
        ensure!(
            self.version >= 14
                || self
                    .reviews
                    .iter()
                    .chain(self.closed_reviews.iter().map(|r| &r.request))
                    .all(|r| !r.is_idle_mcp_review()),
            "legacy input cannot supply idle MCP review receipts"
        );
        ensure!(
            self.version >= 13
                || (self.reviews.iter().all(|r| !r.is_mcp_form_review())
                    && self
                        .closed_reviews
                        .iter()
                        .all(|r| !r.request.is_mcp_form_review())),
            "legacy input cannot supply MCP form review receipts"
        );
        ensure!(
            self.version >= 12
                || (self.reviews.iter().all(|r| !r.is_mcp_url_review())
                    && self
                        .closed_reviews
                        .iter()
                        .all(|r| !r.request.is_mcp_url_review())),
            "legacy input cannot supply MCP URL review receipts"
        );
        ensure!(
            self.version >= 11
                || (self.reviews.iter().all(|r| !r.is_stdin_review())
                    && self
                        .closed_reviews
                        .iter()
                        .all(|r| !r.request.is_stdin_review())),
            "legacy input cannot supply terminal input review receipts"
        );
        ensure!(
            self.version >= 10 || self.steering.is_none(),
            "legacy input cannot supply active-turn steering receipts"
        );
        ensure!(
            self.version >= 9
                || (self.reviews.iter().all(|r| !r.is_network_review())
                    && self
                        .closed_reviews
                        .iter()
                        .all(|r| !r.request.is_network_review())),
            "legacy input cannot supply network-only review receipts"
        );
        ensure!(
            self.version >= 8
                || (self.reviews.iter().all(|r| !r.has_command_cancellation())
                    && self
                        .closed_reviews
                        .iter()
                        .all(|r| !r.request.has_command_cancellation())),
            "legacy input cannot supply command cancellation review receipts"
        );
        ensure!(
            self.version >= 7
                || (self.reviews.iter().all(|r| !r.is_permission_review())
                    && self
                        .closed_reviews
                        .iter()
                        .all(|r| !r.request.is_permission_review())),
            "legacy input cannot supply permission review receipts"
        );
        ensure!(
            self.version >= 6
                || (self.reviews.iter().all(|r| !r.is_file_review())
                    && self
                        .closed_reviews
                        .iter()
                        .all(|r| !r.request.is_file_review())),
            "legacy input cannot supply file-change review receipts"
        );
        ensure!(
            self.version >= 5
                || (self.mcp_answers.is_empty()
                    && self.attempt.as_ref().is_none_or(|a| a.mcp_origin.is_none())),
            "legacy input cannot supply MCP origin or answer receipts"
        );
        ensure!(
            self.version == VERSION
                || matches!(self.version, 3..=14)
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
        let mut input_receipts = std::collections::HashSet::new();
        for completed in &self.completed {
            ensure!(valid_id(&completed.message), "invalid retained message ID");
            self.validate_receipt(&completed.receipt)?;
            ensure!(
                input_receipts.insert((&completed.receipt.turn, &completed.receipt.item)),
                "multiple inputs share a provider receipt"
            );
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
        let mut requests = std::collections::HashSet::new();
        for request in &self.reviews {
            request.validate(self.thread.as_deref(), &self.binding.agent)?;
            if request.is_idle_mcp_review() {
                ensure!(
                    self.attempt.is_none() && self.steering.is_none(),
                    "idle MCP review cannot overlap active or uncertain input"
                );
            } else {
                ensure!(
                    self.attempt.is_some(),
                    "provider question has no active input"
                );
            }
            ensure!(
                requests.insert(request.key()),
                "duplicate pending provider request"
            );
            if let Some(receipt) = self.attempt.as_ref().and_then(|a| a.receipt.as_ref()) {
                ensure!(
                    request.turn.as_deref() == Some(receipt.turn.as_str()),
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
        ensure!(
            self.mcp_answers.len() <= RETAINED_RECEIPTS,
            "too many retained MCP answer receipts"
        );
        let mut answer_ids = std::collections::HashSet::new();
        let mut item_ids = std::collections::HashSet::new();
        for answer in &self.mcp_answers {
            self.validate_receipt(&answer.receipt)?;
            answer.validate(&self.binding.agent)?;
            ensure!(
                answer_ids.insert(&answer.answer.id)
                    && item_ids.insert((&answer.receipt.turn, &answer.receipt.item)),
                "duplicate MCP answer receipt"
            );
            ensure!(
                answer
                    .answer
                    .reply_to
                    .as_ref()
                    .is_some_and(|id| self.retired_questions.contains(id)),
                "MCP answer has no retained question route"
            );
        }
        for attempt in self.attempt.iter().chain(self.steering.iter()) {
            if let Some(origin) = &attempt.mcp_origin {
                origin.validate()?;
            }
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
                ensure!(
                    input_receipts.insert((&receipt.turn, &receipt.item)),
                    "multiple inputs share a provider receipt"
                );
            }
            ensure!(
                !self
                    .completed
                    .iter()
                    .any(|done| done.message == attempt.message),
                "pending input duplicates a completed receipt"
            );
        }
        if let Some(steering) = &self.steering {
            let active = self
                .attempt
                .as_ref()
                .context("steering has no active input")?;
            let receipt = active
                .receipt
                .as_ref()
                .context("steering has no accepted turn")?;
            ensure!(
                active.acknowledged,
                "steering precedes the starting input acknowledgement"
            );
            ensure!(
                steering.message != active.message,
                "steering repeats the starting input"
            );
            ensure!(
                steering.mcp_origin == active.mcp_origin,
                "steering changes the MCP binding"
            );
            ensure!(
                steering
                    .receipt
                    .as_ref()
                    .is_none_or(|r| r.turn == receipt.turn && r.item != receipt.item),
                "steering receipt belongs to another turn or repeats its first item"
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
        let file = Durable::open(
            &directory,
            "delivery.json",
            MAX_STATE_BYTES,
            "Codex delivery record",
            "another Codex input bridge already owns this agent",
        )?;
        let mut record = match file.read::<Record>()? {
            Some(record) => record,
            None => Record {
                version: VERSION,
                binding: binding.clone(),
                thread: None,
                attempt: None,
                steering: None,
                completed: VecDeque::new(),
                reviews: Vec::new(),
                closed_reviews: VecDeque::new(),
                secret_review: None,
                retired_questions: Vec::new(),
                mcp_answers: VecDeque::new(),
            },
        };
        record.validate(&binding)?;
        record.version = VERSION;
        Ok(Self { file, record })
    }

    pub fn record(&self) -> &Record {
        &self.record
    }

    pub fn open_secret_review(&mut self, fence: super::secret_requests::Fence) -> Result<()> {
        ensure!(
            self.record.secret_review.is_none(),
            "another temporary request is unresolved"
        );
        let mut next = self.record.clone();
        next.secret_review = Some(fence);
        self.save(next)
    }

    pub fn secret_response_attempted(&mut self) -> Result<()> {
        let mut next = self.record.clone();
        let fence = next
            .secret_review
            .as_mut()
            .context("temporary response fence is absent")?;
        ensure!(
            !fence.response_attempted,
            "temporary response cannot be sent twice"
        );
        fence.response_attempted = true;
        self.save(next)
    }

    pub fn close_secret_review(&mut self) -> Result<()> {
        let mut next = self.record.clone();
        next.secret_review = None;
        self.save(next)
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

    pub fn capture_mcp_answer(&mut self, answer: Answer) -> Result<()> {
        let mut next = self.record.clone();
        if let Some(old) = next
            .mcp_answers
            .iter()
            .find(|old| old.answer.id == answer.answer.id || old.receipt == answer.receipt)
        {
            ensure!(old.same_proof(&answer), "conflicting MCP answer receipt");
            return Ok(());
        }
        let active = next
            .attempt
            .as_ref()
            .and_then(|a| a.receipt.as_ref())
            .context("MCP answer has no accepted input turn")?;
        ensure!(
            active.thread == answer.receipt.thread && active.turn == answer.receipt.turn,
            "MCP answer belongs to another input turn"
        );
        let origin = next
            .attempt
            .as_ref()
            .and_then(|a| a.mcp_origin.as_ref())
            .context("retained input has no proven MCP binding")?;
        ensure!(
            origin.human == answer.answer.from.as_str() && origin.servers.contains(&answer.server),
            "MCP answer has another provider binding"
        );
        if next.mcp_answers.len() == RETAINED_RECEIPTS {
            ensure!(
                next.mcp_answers
                    .front()
                    .is_some_and(|old| old.acknowledged && old.receipt.turn != active.turn),
                "MCP answer receipt history is full; delivery must pause"
            );
            next.mcp_answers.pop_front();
        }
        let question = answer
            .answer
            .reply_to
            .clone()
            .context("MCP answer has no question")?;
        ensure!(
            !next.retired_questions.contains(&question),
            "MCP question already has another answer receipt"
        );
        next.retired_questions.push(question);
        next.mcp_answers.push_back(answer);
        self.save(next)
    }

    pub fn acknowledge_mcp_answer(&mut self, message: &MessageId) -> Result<()> {
        let mut next = self.record.clone();
        let answer = next
            .mcp_answers
            .iter_mut()
            .find(|a| &a.answer.id == message)
            .context("MCP acknowledgement has no durable receipt")?;
        answer.acknowledged = true;
        self.save(next)
    }

    fn save(&mut self, next: Record) -> Result<()> {
        next.validate(&self.record.binding)?;
        self.file.publish(&next)?;
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
                && self.record.secret_review.is_none()
                && self.record.closed_reviews.is_empty()
                && self.record.mcp_answers.is_empty()
                && self.record.retired_questions.is_empty(),
            "a conversation with prepared or accepted input cannot be discarded"
        );
        if self.record.thread.is_some() {
            let mut next = self.record.clone();
            next.thread = None;
            self.save(next)?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub fn prepare(&mut self, envelope: &Envelope) -> Result<String> {
        self.prepare_bound(envelope, None)
    }

    pub fn prepare_bound(&mut self, envelope: &Envelope, origin: Option<Origin>) -> Result<String> {
        ensure!(
            self.record.attempt.is_none() && self.record.steering.is_none(),
            "an earlier Codex input still needs receipt recovery; automatic resubmission is refused"
        );
        let attempt = self.planned_input(envelope, origin)?;
        let input = attempt.input.clone();
        let mut next = self.record.clone();
        next.attempt = Some(attempt);
        self.save(next)?;
        Ok(input)
    }

    pub fn prepare_steering(&mut self, envelope: &Envelope, turn: &str) -> Result<String> {
        let active = self
            .record
            .attempt
            .as_ref()
            .context("no active Codex input")?;
        ensure!(
            active.acknowledged && active.receipt.as_ref().is_some_and(|r| r.turn == turn),
            "steering requires the exact acknowledged active turn"
        );
        ensure!(
            self.record.steering.is_none(),
            "earlier steering still needs receipt recovery"
        );
        ensure!(
            active.message != envelope.id.as_str(),
            "cannot steer the starting input again"
        );
        let attempt = self.planned_input(envelope, active.mcp_origin.clone())?;
        let input = attempt.input.clone();
        let mut next = self.record.clone();
        next.steering = Some(attempt);
        self.save(next)?;
        Ok(input)
    }

    fn planned_input(&self, envelope: &Envelope, origin: Option<Origin>) -> Result<Attempt> {
        ensure!(
            !origin
                .as_ref()
                .is_some_and(|o| envelope.kind == "answer" && envelope.from.as_str() == o.human),
            "this human answer has no reconciled tool receipt; it remains queued and cannot become new input"
        );
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
            self.record.reviews.is_empty() && self.record.secret_review.is_none(),
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
        Ok(Attempt {
            message,
            input: input.clone(),
            mcp_origin: origin,
            receipt: None,
            acknowledged: false,
        })
    }

    pub fn accept(&mut self, input: &str, receipt: Receipt) -> Result<()> {
        self.record.validate_receipt(&receipt)?;
        let mut next = self.record.clone();
        let attempt = next
            .attempt
            .iter_mut()
            .chain(next.steering.iter_mut())
            .find(|attempt| attempt.input == input)
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
            .iter_mut()
            .chain(next.steering.iter_mut())
            .find(|attempt| attempt.message == message)
            .context("queue acknowledgement has no prepared input")?;
        ensure!(
            attempt.message == message && attempt.receipt.is_some(),
            "queue acknowledgement requires the exact provider receipt"
        );
        attempt.acknowledged = true;
        self.save(next)
    }

    pub fn finish_steering(&mut self) -> Result<()> {
        let mut next = self.record.clone();
        let attempt = next
            .steering
            .take()
            .context("no steering input to finish")?;
        ensure!(
            attempt.acknowledged,
            "steering input still needs acknowledgement"
        );
        let receipt = attempt
            .receipt
            .context("steering input has no provider receipt")?;
        next.completed.push_back(Completed {
            message: attempt.message,
            receipt,
        });
        while next.completed.len() > RETAINED_RECEIPTS {
            next.completed.pop_front();
        }
        self.save(next)
    }

    /// Called only for a definitive precondition rejection of turn/steer.
    /// Transport failures and unknown provider errors retain the attempt.
    /// The server's word that the prepared input never became a turn: the
    /// attempt is dropped and the message stays queued for a later turn.
    /// An attempt with a receipt, or one acknowledged, is not prepared.
    pub fn reject_prepared(&mut self) -> Result<()> {
        let pending = self
            .record
            .attempt
            .as_ref()
            .context("no prepared input to reject")?;
        ensure!(
            pending.receipt.is_none() && !pending.acknowledged,
            "accepted input cannot be rejected"
        );
        let mut next = self.record.clone();
        next.attempt = None;
        self.save(next)
    }

    pub fn reject_steering(&mut self) -> Result<()> {
        let pending = self
            .record
            .steering
            .as_ref()
            .context("no steering input to reject")?;
        ensure!(
            pending.receipt.is_none() && !pending.acknowledged,
            "accepted steering cannot be rejected"
        );
        let mut next = self.record.clone();
        next.steering = None;
        self.save(next)
    }

    pub fn finish(&mut self, turn: &str) -> Result<()> {
        ensure!(
            self.record.steering.is_none(),
            "steering input still needs receipt recovery"
        );
        ensure!(
            self.record.mcp_answers.iter().all(|a| a.acknowledged),
            "MCP answer still needs queue acknowledgement"
        );
        ensure!(
            self.record.reviews.is_empty() && self.record.secret_review.is_none(),
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

#[cfg(test)]
mod secret_fence_tests {
    use super::*;
    use crate::codex_input::secret_requests::Fence;
    use agentdocker_core::Destination;
    use serde_json::json;

    #[test]
    fn temporary_input_survives_reopen_without_values_or_permission_to_replay() {
        let home = tempfile::tempdir().unwrap();
        let binding = Binding {
            agent: "owned".into(),
            socket: home.path().join("sock"),
            cwd: home.path().into(),
            provider_home: home.path().into(),
        };
        let mut ledger = Ledger::open(home.path(), binding.clone()).unwrap();
        ledger.bind_thread("thread".into()).unwrap();
        let input = Envelope::new(
            "peer",
            Destination::Agent("owned".into()),
            "chat",
            json!({"text":"start"}),
            None,
            chrono::Utc::now(),
        );
        let text = ledger.prepare(&input).unwrap();
        ledger
            .accept(
                &text,
                Receipt {
                    thread: "thread".into(),
                    turn: "turn".into(),
                    item: "item".into(),
                },
            )
            .unwrap();
        ledger.acknowledge(input.id.as_str()).unwrap();
        let fence = Fence {
            id: json!(17),
            thread: "thread".into(),
            turn: "turn".into(),
            response_attempted: false,
        };
        let mut other = fence.clone();
        other.turn = "other".into();
        assert!(ledger.open_secret_review(other).is_err());
        ledger.open_secret_review(fence.clone()).unwrap();
        let path = ledger.file.path().to_path_buf();
        let queued = Envelope::new(
            "peer",
            Destination::Agent("owned".into()),
            "chat",
            json!({"text":"later"}),
            None,
            chrono::Utc::now(),
        );
        assert!(ledger.open_secret_review(fence.clone()).is_err());
        assert!(ledger.prepare_steering(&queued, "turn").is_err());
        assert!(ledger.finish("turn").is_err());
        drop(ledger);
        let mut ledger = Ledger::open(home.path(), binding.clone()).unwrap();
        assert!(ledger.record().secret_review.is_some());
        assert!(ledger.prepare(&queued).is_err());
        ledger.secret_response_attempted().unwrap();
        assert!(ledger.secret_response_attempted().is_err());
        drop(ledger);
        let bytes = std::fs::read(&path).unwrap();
        let mut old: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        old["version"] = json!(14);
        std::fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
        assert!(Ledger::open(home.path(), binding.clone()).is_err());
        std::fs::write(&path, &bytes).unwrap();
        let mut ledger = Ledger::open(home.path(), binding.clone()).unwrap();
        assert!(
            ledger
                .record()
                .secret_review
                .as_ref()
                .unwrap()
                .response_attempted
        );
        assert!(ledger.finish("turn").is_err());
        ledger.close_secret_review().unwrap();
        ledger.finish("turn").unwrap();
        assert!(ledger.prepare(&queued).is_ok());
        drop(ledger);
        let mut old: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        old["version"] = json!(14);
        old.as_object_mut().unwrap().remove("secret_review");
        let before = serde_json::to_vec(&old).unwrap();
        std::fs::write(&path, &before).unwrap();
        let ledger = Ledger::open(home.path(), binding).unwrap();
        assert!(ledger.record().secret_review.is_none());
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "legacy open does not rewrite a retained attempt"
        );
    }
}
