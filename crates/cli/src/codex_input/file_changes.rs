//! File approval callbacks name an item; the actual diff arrives separately.
//! Keep a bounded snapshot for the active turn and never guess missing changes.
use agentdocker_core::{QuestionFileChange, QuestionPresentation};
use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

const ITEMS: usize = 64;
const ITEM_BYTES: usize = 32_000;

#[derive(Default)]
pub(super) struct Reviews {
    items: HashMap<String, Result<Vec<QuestionFileChange>, String>>,
    overflow: bool,
    bound: Option<(String, String)>,
    presented: HashSet<String>,
    changed_after_review: bool,
}

impl Reviews {
    pub fn observe(&mut self, event: &Value, thread: &str, turn: Option<&str>) {
        let params = &event["params"];
        if turn.is_none()
            || params["threadId"].as_str() != Some(thread)
            || params["turnId"].as_str() != turn
            || params["item"]["type"] != "fileChange"
        {
            return;
        }
        let bound = (thread.to_owned(), turn.expect("checked above").to_owned());
        if self.bound.as_ref() != Some(&bound) {
            *self = Self {
                bound: Some(bound),
                ..Self::default()
            };
        }
        let Some(id) = params["item"]["id"]
            .as_str()
            .filter(|id| !id.is_empty() && id.len() <= 256)
        else {
            return;
        };
        if event["method"] == "item/started" && self.presented.contains(id) {
            self.changed_after_review = true;
        }
        if self.overflow {
            return;
        }
        if self.items.len() == ITEMS && !self.items.contains_key(id) {
            self.items.clear();
            self.overflow = true;
            return;
        }
        match event["method"].as_str() {
            Some("item/started") => {
                let changes = if self.items.contains_key(id) {
                    Err("Codex repeated or changed a file review item".into())
                } else {
                    parse(&params["item"]).map_err(|e| format!("{e:#}"))
                };
                self.items.insert(id.into(), changes);
            }
            Some("item/completed") => {
                self.items.insert(
                    id.into(),
                    Err("the file change item has already completed".into()),
                );
            }
            _ => (),
        }
    }

    pub fn presentation(
        &mut self,
        event: &Value,
        cwd: &std::path::Path,
    ) -> Result<QuestionPresentation> {
        self.check_current()?;
        ensure!(
            !self.overflow,
            "too many file changes to correlate safely in this turn"
        );
        let params = &event["params"];
        ensure!(
            self.bound
                .as_ref()
                .is_some_and(|(thread, turn)| params["threadId"].as_str() == Some(thread)
                    && params["turnId"].as_str() == Some(turn)),
            "file approval does not match the retained thread and turn"
        );
        ensure!(
            params["grantRoot"].is_null(),
            "session-wide file access needs a separate review flow"
        );
        let id = params["itemId"]
            .as_str()
            .context("file approval has no item ID")?;
        let slot = self
            .items
            .get_mut(id)
            .context("Codex supplied no complete file-change item to review")?;
        let changes = match std::mem::replace(
            slot,
            Err("file-change item was already requested for review".into()),
        ) {
            Ok(changes) => changes,
            Err(reason) => bail!(reason),
        };
        let presentation = QuestionPresentation::CodexFiles {
            cwd: cwd
                .to_str()
                .context("file review directory is not Unicode")?
                .into(),
            reason: params["reason"]
                .as_str()
                .unwrap_or("Requested by Codex")
                .into(),
            changes,
        };
        ensure!(
            presentation.valid_for(&presentation.text()),
            "file changes are too large or incomplete to review as a question"
        );
        self.presented.insert(id.into());
        Ok(presentation)
    }

    pub fn check_current(&self) -> Result<()> {
        ensure!(
            !self.changed_after_review,
            "Codex changed or repeated file details after review publication; pending approvals must be cancelled"
        );
        Ok(())
    }
}

fn parse(item: &Value) -> Result<Vec<QuestionFileChange>> {
    ensure!(
        item["status"] == "inProgress",
        "file-change item is not awaiting execution"
    );
    ensure!(
        serde_json::to_vec(item)?.len() <= ITEM_BYTES,
        "file-change item exceeds the review size limit"
    );
    let values = item["changes"]
        .as_array()
        .context("file-change item has no changes")?;
    ensure!(
        !values.is_empty() && values.len() <= 16,
        "unsupported file-change count"
    );
    values
        .iter()
        .map(|v| serde_json::from_value(v.clone()).context("unsupported file-change details"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn started() -> Value {
        json!({"method":"item/started","params":{"threadId":"thread","turnId":"turn","item":{"id":"patch","type":"fileChange","status":"inProgress","changes":[{"path":"/owned/a.txt","kind":{"type":"update","move_path":"/owned/b.txt"},"diff":"@@ -1 +1 @@\n-old\n+new"}]}}})
    }
    fn request() -> Value {
        json!({"id":9,"method":"item/fileChange/requestApproval","params":{"threadId":"thread","turnId":"turn","itemId":"patch","grantRoot":null,"reason":"Update the fixture"}})
    }
    #[test]
    fn file_reviews_keep_the_exact_diff_and_refuse_reuse_foreign_turns_or_session_grants() {
        let mut reviews = Reviews::default();
        reviews.observe(&started(), "thread", Some("turn"));
        let mut request = request();
        request["params"]["threadId"] = json!("other");
        assert!(reviews.presentation(&request, "/owned".as_ref()).is_err());
        request["params"]["threadId"] = json!("thread");
        request["params"]["grantRoot"] = json!("/owned");
        assert!(reviews.presentation(&request, "/owned".as_ref()).is_err());
        request["params"]["grantRoot"] = Value::Null;
        let presentation = reviews.presentation(&request, "/owned".as_ref()).unwrap();
        let text = presentation.text();
        assert!(presentation.valid_for(&text));
        assert!(text.contains("Move /owned/a.txt → /owned/b.txt\n@@ -1 +1 @@\n-old\n+new"));
        assert!(!presentation.valid_for(&text.replace("+new", "+different")));
        assert!(presentation.permits_choice("Allow"));
        assert!(!presentation.permits_choice("Allow for session"));
        reviews.observe(&started(), "thread", Some("turn"));
        assert!(
            reviews.check_current().is_err(),
            "a published diff cannot be silently replaced"
        );
        assert!(reviews.presentation(&request, "/owned".as_ref()).is_err());
    }
    #[test]
    fn file_reviews_refuse_missing_completed_repeated_unknown_and_oversized_details() {
        for case in 0..7 {
            let mut reviews = Reviews::default();
            let mut item = started();
            match case {
                0 => (),
                1 => {
                    item["method"] = json!("item/completed");
                    reviews.observe(&item, "thread", Some("turn"));
                }
                2 => {
                    reviews.observe(&item, "thread", Some("turn"));
                    reviews.observe(&item, "thread", Some("turn"));
                }
                3 => {
                    item["params"]["item"]["changes"][0]["kind"]["unexpectedGrant"] = json!(true);
                    reviews.observe(&item, "thread", Some("turn"));
                }
                4 => {
                    item["params"]["item"]["changes"][0]["diff"] = json!("x".repeat(ITEM_BYTES));
                    reviews.observe(&item, "thread", Some("turn"));
                }
                5 => {
                    item["params"]["item"]["changes"][0]["diff"] = json!("");
                    reviews.observe(&item, "thread", Some("turn"));
                }
                6 => {
                    item["params"]["item"]["changes"][0]["path"] = json!("bad\npath");
                    reviews.observe(&item, "thread", Some("turn"));
                }
                _ => unreachable!(),
            }
            assert!(
                reviews.presentation(&request(), "/owned".as_ref()).is_err(),
                "case {case}"
            );
        }
    }
    #[test]
    fn file_review_item_pressure_refuses_the_turn_and_a_new_turn_discards_old_correlations() {
        let mut reviews = Reviews::default();
        for n in 0..=ITEMS {
            let mut item = started();
            item["params"]["item"]["id"] = json!(n.to_string());
            reviews.observe(&item, "thread", Some("turn"));
        }
        assert!(reviews.overflow && reviews.items.is_empty());
        assert!(reviews.presentation(&request(), "/owned".as_ref()).is_err());
        let mut item = started();
        item["params"]["turnId"] = json!("next");
        reviews.observe(&item, "thread", Some("next"));
        assert!(!reviews.overflow && reviews.items.len() == 1);
        assert!(reviews.presentation(&request(), "/owned".as_ref()).is_err());
        let mut request = request();
        request["params"]["turnId"] = json!("next");
        assert!(reviews.presentation(&request, "/owned".as_ref()).is_ok());
    }
}
