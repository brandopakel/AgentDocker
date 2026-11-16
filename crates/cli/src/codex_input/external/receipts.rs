//! Read-only receipts from the existing provider conversation. Never resume it.
use super::{
    super::{ledger::Receipt, recovery, transport::Provider},
    ledger::Attempt,
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::collections::HashSet;

pub(super) async fn latest_item(provider: &mut Provider, thread: &str) -> Result<Option<String>> {
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        let mut history = super::history::History::new(thread, 1);
        while let Some(items) = history.next(provider).await? {
            if let Some(entry) = items.first() {
                return entry["item"]["id"]
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .map(|id| Some(id.to_owned()))
                    .context("Codex history item has no ID");
            }
        }
        Ok(None)
    })
    .await
    .context("native history anchor lookup exceeded one minute")?
}

pub(super) async fn find(
    provider: &mut Provider,
    thread: &str,
    attempt: &Attempt,
    binding: &super::ledger::Binding,
) -> Result<Option<Receipt>> {
    if let Some(hook) = &attempt.hook {
        let hook = hook.clone();
        let binding = binding.clone();
        if let Some(receipt) =
            tokio::task::spawn_blocking(move || super::hook_receipts::find(&binding, &hook))
                .await
                .context("hook transcript scanner stopped")??
        {
            return Ok(Some(receipt));
        }
    }
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        let mut history = super::history::History::new(thread, 50);
        let mut found = None;
        while let Some(items) = history.next(provider).await? {
            for entry in items {
                if entry["item"]["id"]
                    .as_str()
                    .is_some_and(|id| attempt.anchor.as_deref() == Some(id))
                {
                    return Ok(found);
                }
                if let Some(receipt) = input_receipt(
                    thread,
                    entry["turnId"].as_str().unwrap_or_default(),
                    &entry["item"],
                    attempt,
                )? {
                    // A steered input is received only by the turn it was
                    // steered into; anywhere else is a contradiction to inspect.
                    ensure!(
                        attempt
                            .steer
                            .as_ref()
                            .is_none_or(|steer| steer.turn == receipt.turn),
                        "the steered input appeared in another turn; retained for inspection"
                    );
                    ensure!(
                        found.is_none(),
                        "multiple provider receipts match one native queue input"
                    );
                    found = Some(receipt);
                }
            }
        }
        Ok(found)
    })
    .await
    .context("native queue receipt lookup exceeded one minute")?
}

fn input_receipt(
    thread: &str,
    turn: &str,
    item: &Value,
    attempt: &Attempt,
) -> Result<Option<Receipt>> {
    if let Some(receipt) = recovery::receipt(thread, turn, item, &attempt.input)? {
        return Ok(Some(receipt));
    }
    let Some(hook) = &attempt.hook else {
        return Ok(None);
    };
    if item["type"] != "hookPrompt" {
        return Ok(None);
    }
    let fragments = item["fragments"]
        .as_array()
        .context("hook receipt has no fragments")?;
    let matching: Vec<_> = fragments
        .iter()
        .filter(|f| f["text"].as_str() == Some(&hook.context))
        .collect();
    ensure!(matching.len() <= 1, "repeated native hook receipt");
    let Some(fragment) = matching.first() else {
        return Ok(None);
    };
    ensure!(
        fragment["hookRunId"]
            .as_str()
            .is_some_and(|id| !id.is_empty())
            && !turn.is_empty(),
        "hook receipt lacks provider identity"
    );
    Ok(Some(Receipt {
        thread: thread.into(),
        turn: turn.into(),
        item: item["id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .context("hook receipt lacks item ID")?
            .into(),
    }))
}

pub(super) fn queued_id(value: &Value, attempt: &Attempt) -> Result<Option<String>> {
    let input = value["input"].as_array();
    if value["clientUserMessageId"].as_str() != Some(&attempt.message)
        || !input.is_some_and(|items| {
            items.len() == 1
                && items[0]["type"] == "text"
                && items[0]["text"].as_str() == Some(&attempt.input)
                && items[0]
                    .get("text_elements")
                    .is_none_or(|v| v.as_array().is_some_and(Vec::is_empty))
        })
    {
        return Ok(None);
    }
    let id = value["id"]
        .as_str()
        .context("native queued submission has no ID")?;
    ensure!(
        !id.is_empty() && id.len() <= 128 && !id.chars().any(char::is_control),
        "invalid native queue ID"
    );
    Ok(Some(id.into()))
}

pub(super) async fn queued(
    provider: &mut Provider,
    thread: &str,
    attempt: &Attempt,
) -> Result<Option<String>> {
    scan_queue(provider, thread, attempt, false).await
}

pub(super) async fn queued_for_manual(
    provider: &mut Provider,
    thread: &str,
    attempt: &Attempt,
) -> Result<Option<String>> {
    scan_queue(provider, thread, attempt, true).await
}

/// What one complete look at the live queue found of an input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Scheduled {
    /// The recorded entry is there, its id and its text unchanged, and
    /// nothing else matches.
    Recorded,
    /// Something scheduled matches that is not the recorded entry: its
    /// text was edited, or it carries the message under another queue id.
    Other,
    /// Nothing scheduled matches.
    Absent,
}

/// Read the whole queue once and say what it holds of `attempt`, by the
/// two tests the actions apply and without confusing them: the exact entry
/// (what an explicit start requires) and any entry of the same identity
/// (what a manual readback refuses on).
///
/// It is for the review, which only reports. So it answers only after the
/// last page, and anything short of a whole and consistent reading is an
/// error for the caller to report as not established, never as absence: a
/// read that fails, a queue past the limit, two entries for one input.
/// Nothing is written and nothing found here is recorded.
pub(super) async fn look(
    provider: &mut Provider,
    thread: &str,
    attempt: &Attempt,
) -> Result<Scheduled> {
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        let mut cursor: Option<String> = None;
        let mut cursors = HashSet::new();
        let (mut exact, mut matching) = (None::<String>, 0_usize);
        for _ in 0..100 {
            let value = provider
                .request(
                    "thread/queue/list",
                    json!({"threadId":thread,"limit":50,"cursor":cursor}),
                )
                .await?;
            let (items, next) = recovery::page(&value, &mut cursors)?;
            for item in items {
                if same_queue_identity(item, attempt) {
                    matching += 1;
                }
                if let Some(id) = queued_id(item, attempt)? {
                    ensure!(
                        exact.is_none(),
                        "multiple native queue entries match one input"
                    );
                    exact = Some(id);
                }
            }
            cursor = next;
            if cursor.is_none() {
                ensure!(
                    matching <= 1,
                    "several scheduled native queue entries match one input"
                );
                return Ok(match (exact, matching) {
                    (Some(id), _) if attempt.queued.as_deref() == Some(id.as_str()) => {
                        Scheduled::Recorded
                    }
                    (None, 0) => Scheduled::Absent,
                    _ => Scheduled::Other,
                });
            }
        }
        bail!("native provider queue exceeds the recovery limit")
    })
    .await
    .context("native provider queue lookup exceeded one minute")?
}

fn same_queue_identity(value: &Value, attempt: &Attempt) -> bool {
    value["clientUserMessageId"].as_str() == Some(&attempt.message)
        || attempt
            .queued
            .as_deref()
            .is_some_and(|id| value["id"].as_str() == Some(id))
}

async fn scan_queue(
    provider: &mut Provider,
    thread: &str,
    attempt: &Attempt,
    manual: bool,
) -> Result<Option<String>> {
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        let mut cursor: Option<String> = None;
        let mut cursors = HashSet::new();
        let mut found = None;
        for _ in 0..100 {
            let value = provider
                .request(
                    "thread/queue/list",
                    json!({"threadId":thread,"limit":50,"cursor":cursor}),
                )
                .await?;
            let (items, next) = recovery::page(&value, &mut cursors)?;
            for item in items {
                if manual && same_queue_identity(item, attempt) {
                    // The user may have edited a still-scheduled entry. Changed
                    // text is not proof that the original scheduling is gone.
                    return Ok(Some("matching scheduled input".into()));
                }
                if let Some(id) = queued_id(item, attempt)? {
                    ensure!(
                        found.is_none(),
                        "multiple native queue entries match one input"
                    );
                    found = Some(id);
                }
            }
            cursor = next;
            if cursor.is_none() {
                return Ok(found);
            }
        }
        bail!("native provider queue exceeds the recovery limit")
    })
    .await
    .context("native provider queue lookup exceeded one minute")?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hook_receipt_requires_exact_context_and_provider_ids() {
        let mut attempt = Attempt {
            start: None,
            steer: None,
            message: "message".into(),
            input: "native input".into(),
            queued: None,
            receipt: None,
            anchor: None,
            hook: Some(super::super::ledger::HookOffer {
                request: "request".into(),
                context: "exact context".into(),
                transcript: None,
            }),
        };
        let mut item = json!({"type":"hookPrompt","id":"hook-item","fragments":[{"hookRunId":"actual-provider-run","text":"exact context"}]});
        assert_eq!(
            input_receipt("thread", "turn", &item, &attempt)
                .unwrap()
                .unwrap()
                .item,
            "hook-item"
        );
        item["fragments"][0]["text"] = json!("truncated context");
        assert!(
            input_receipt("thread", "turn", &item, &attempt)
                .unwrap()
                .is_none()
        );
        item["fragments"][0]["text"] = json!("exact context");
        item["fragments"][0]["hookRunId"] = json!("");
        assert!(input_receipt("thread", "turn", &item, &attempt).is_err());
        item["fragments"][0]["hookRunId"] = json!("run");
        assert!(input_receipt("thread", "", &item, &attempt).is_err());
        item["fragments"]
            .as_array_mut()
            .unwrap()
            .push(json!({"hookRunId":"run2","text":"exact context"}));
        assert!(input_receipt("thread", "turn", &item, &attempt).is_err());
        attempt.hook = None;
        assert!(
            input_receipt("thread", "turn", &item, &attempt)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn queue_acceptance_matches_the_entire_input_and_client_id() {
        let attempt = Attempt {
            start: None,
            steer: None,
            message: "original".into(),
            input: "full input".into(),
            queued: None,
            receipt: None,
            anchor: None,
            hook: None,
        };
        let mut value = json!({"id":"provider-queue","clientUserMessageId":"original","input":[{"type":"text","text":"full input","text_elements":[]}]});
        assert_eq!(
            queued_id(&value, &attempt).unwrap().as_deref(),
            Some("provider-queue")
        );
        value["input"][0]["text"] = json!("partial");
        assert!(queued_id(&value, &attempt).unwrap().is_none());
        assert!(same_queue_identity(&value, &attempt));
        value["input"][0]["text"] = json!("full input");
        value["clientUserMessageId"] = json!("another");
        assert!(queued_id(&value, &attempt).unwrap().is_none());
        assert!(!same_queue_identity(&value, &attempt));
        let mut attempt = attempt;
        attempt.queued = Some("provider-queue".into());
        assert!(same_queue_identity(&value, &attempt));
    }

    /// The review's look reads the whole queue before it answers, and
    /// keeps its two tests apart: the recorded entry exactly, and any
    /// entry of the same identity. A read that fails, a second match on a
    /// later page and a queue that never ends are errors, never absence.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_look_answers_only_from_a_whole_and_consistent_reading_of_the_queue() {
        use super::super::testing::{provider, queue_entry};
        let attempt = Attempt {
            start: None,
            steer: None,
            message: "message".into(),
            input: "full input".into(),
            queued: Some("recorded".into()),
            receipt: None,
            anchor: None,
            hook: None,
        };
        let recorded = queue_entry("recorded", "message", "full input");
        let edited = queue_entry("recorded", "message", "edited by the person");
        let requeued = queue_entry("another-id", "message", "full input");
        let stranger = queue_entry("else", "other-message", "other input");

        // The queue's pages in order, and what the look says of them.
        type Pages = Vec<Vec<Value>>;
        type Said = std::result::Result<Scheduled, &'static str>;
        let cases: Vec<(Pages, Said)> = vec![
            (vec![vec![]], Ok(Scheduled::Absent)),
            (vec![vec![stranger.clone()]], Ok(Scheduled::Absent)),
            (
                vec![vec![stranger.clone(), recorded.clone()]],
                Ok(Scheduled::Recorded),
            ),
            // Found on the second page, after one that had nothing of it.
            (
                vec![vec![stranger.clone()], vec![recorded.clone()]],
                Ok(Scheduled::Recorded),
            ),
            (vec![vec![edited.clone()]], Ok(Scheduled::Other)),
            // The same message and text under an id that is not the
            // recorded one: scheduled, and not the entry a start may run.
            (vec![vec![requeued.clone()]], Ok(Scheduled::Other)),
            // An exact match on the first page does not end the reading:
            // a second entry of the same identity on a later page is a
            // contradiction, and nothing is claimed.
            (
                vec![vec![recorded.clone()], vec![edited.clone()]],
                Err("several scheduled native queue entries match one input"),
            ),
            (
                vec![vec![recorded.clone()], vec![requeued.clone()]],
                Err("multiple native queue entries match one input"),
            ),
        ];
        for (pages, expected) in cases {
            let served = pages.clone();
            let mut fake = provider(move |method, params| {
                assert_eq!(method, "thread/queue/list");
                let page = params["cursor"]
                    .as_str()
                    .map_or(0, |cursor| cursor.parse::<usize>().unwrap());
                let next = (page + 1 < served.len()).then(|| (page + 1).to_string());
                Ok(json!({"data": served[page], "nextCursor": next}))
            })
            .await;
            let seen = look(&mut fake.provider, "thread", &attempt).await;
            assert_eq!(
                fake.methods().len(),
                pages.len(),
                "every page is read before anything is said"
            );
            match expected {
                Ok(found) => assert_eq!(seen.unwrap(), found, "{pages:?}"),
                Err(why) => assert_eq!(seen.unwrap_err().to_string(), why, "{pages:?}"),
            }
        }

        // The provider refusing the read is an error, not an empty queue.
        let mut fake =
            provider(|_, _| Err(json!({"code": -32000, "message": "queue unavailable"}))).await;
        assert!(look(&mut fake.provider, "thread", &attempt).await.is_err());

        // Without a recorded id nothing can be the recorded entry.
        let unrecorded = Attempt {
            queued: None,
            ..attempt.clone()
        };
        let entry = queue_entry("some-id", "message", "full input");
        let mut fake = provider(move |_, _| Ok(json!({"data": [entry], "nextCursor": null}))).await;
        assert_eq!(
            look(&mut fake.provider, "thread", &unrecorded)
                .await
                .unwrap(),
            Scheduled::Other
        );
    }
}
