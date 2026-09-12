//! Bounded history reconciliation. Absence of proof never means "send again".
use super::{Client, Ledger, Provider, acknowledge, ledger::Receipt};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{collections::HashSet, time::Duration};

const MAX_PAGES: usize = 100;

pub(super) fn terminal(status: &str) -> bool {
    matches!(status, "completed" | "interrupted" | "failed")
}

pub(super) fn receipt(
    thread: &str,
    turn: &str,
    item: &Value,
    input: &str,
) -> Result<Option<Receipt>> {
    if item["type"] != "userMessage" {
        return Ok(None);
    }
    let content = item["content"]
        .as_array()
        .context("Codex user item has no complete content")?;
    if content.len() != 1
        || content[0]["type"] != "text"
        || content[0]["text"].as_str() != Some(input)
    {
        return Ok(None);
    }
    // The controller submits plain text only. A mixed/annotated item is not an
    // exact receipt, even if flattening it would produce the same visible text.
    ensure!(
        content[0]
            .get("text_elements")
            .is_none_or(|v| v.as_array().is_some_and(Vec::is_empty)),
        "Codex input receipt contains unexpected text attachments"
    );
    ensure!(!turn.is_empty(), "Codex input receipt has no turn");
    Ok(Some(Receipt {
        thread: thread.into(),
        turn: turn.into(),
        item: item["id"]
            .as_str()
            .context("Codex input receipt has no item ID")?
            .into(),
    }))
}

fn page<'a>(
    response: &'a Value,
    cursors: &mut HashSet<String>,
) -> Result<(&'a Vec<Value>, Option<String>)> {
    let data = response["data"]
        .as_array()
        .context("Codex history page has no items")?;
    ensure!(
        data.len() <= 50,
        "Codex history exceeded the requested page size"
    );
    let cursor = match response.get("nextCursor") {
        None | Some(Value::Null) => None,
        Some(Value::String(cursor)) if !cursor.is_empty() && cursor.len() <= 4096 => {
            Some(cursor.clone())
        }
        _ => bail!("Codex history returned an invalid cursor"),
    };
    if let Some(cursor) = &cursor {
        ensure!(
            cursors.insert(cursor.clone()),
            "Codex history cursor repeated"
        );
    }
    Ok((data, cursor))
}

pub(super) async fn find_receipt(provider: &mut Provider, ledger: &mut Ledger) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(60), async {
        let attempt = ledger.record().attempt.as_ref().context("no retained Codex input")?;
        let input = attempt.input.clone();
        let thread = ledger.record().thread.clone().context("no retained Codex conversation")?;
        let mut cursor: Option<String> = None;
        let mut cursors = HashSet::new();
        let mut found = None;
        for _ in 0..MAX_PAGES {
            let response = provider.request("thread/items/list", json!({"threadId":thread,
                "limit":50,"sortDirection":"desc","cursor":cursor})).await?;
            let (items, next) = page(&response, &mut cursors)?;
            for entry in items {
                if let Some(receipt) = receipt(&thread, entry["turnId"].as_str().unwrap_or_default(), &entry["item"], &input)? {
                    ensure!(found.is_none(), "more than one Codex item matches the input; recovery requires inspection");
                    found = Some(receipt);
                }
            }
            cursor = next;
            if cursor.is_none() {
                let receipt = found.context("no exact provider receipt exists for the prepared input; automatic resubmission is refused")?;
                return ledger.accept(&input, receipt);
            }
        }
        bail!("Codex history exceeds the recovery page limit; retained input requires inspection")
    }).await.context("Codex receipt recovery exceeded one minute")?
}

async fn turn_status(
    provider: &mut Provider,
    thread: &str,
    wanted: Option<&str>,
) -> Result<String> {
    let mut cursor: Option<String> = None;
    let mut cursors = HashSet::new();
    for _ in 0..MAX_PAGES {
        let response = provider
            .request(
                "thread/turns/list",
                json!({"threadId":thread,
            "limit":50,"itemsView":"notLoaded","sortDirection":"desc","cursor":cursor}),
            )
            .await?;
        let (turns, next) = page(&response, &mut cursors)?;
        for turn in turns {
            if wanted.is_none_or(|id| turn["id"].as_str() == Some(id)) {
                return Ok(turn["status"]
                    .as_str()
                    .context("retained Codex turn has no status")?
                    .into());
            }
        }
        cursor = next;
        if cursor.is_none() {
            ensure!(
                wanted.is_none(),
                "retained Codex receipt names a missing turn"
            );
            return Ok("completed".into()); // An empty, previously bound thread.
        }
    }
    bail!("Codex turn history exceeds the recovery page limit")
}

pub(super) async fn recover(
    provider: &mut Provider,
    client: &Client,
    ledger: &mut Ledger,
) -> Result<()> {
    if ledger.record().attempt.is_some() {
        find_receipt(provider, ledger).await?;
        let receipt = ledger
            .record()
            .attempt
            .as_ref()
            .and_then(|a| a.receipt.clone())
            .context("receipt recovery did not find the input")?;
        acknowledge(client, ledger).await?;
        let status = tokio::time::timeout(
            Duration::from_secs(60),
            turn_status(provider, &receipt.thread, Some(&receipt.turn)),
        )
        .await
        .context("Codex turn recovery timed out")??;
        ensure!(
            terminal(&status),
            "the retained Codex turn has no terminal outcome; input remains paused"
        );
        ledger.finish(&receipt.turn)?;
        println!("Recovered the accepted Codex input ({status}); it was not submitted again.");
    }
    let thread = ledger
        .record()
        .thread
        .as_deref()
        .context("no Codex thread")?;
    let status = tokio::time::timeout(Duration::from_secs(60), turn_status(provider, thread, None))
        .await??;
    ensure!(
        terminal(&status),
        "Codex already has an active turn; refusing to steer it with a queued message"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn receipts_require_complete_plain_input_and_valid_pagination() {
        let item = json!({"type":"userMessage","id":"item","content":[{"type":"text","text":"exact","text_elements":[]}]});
        assert_eq!(
            receipt("thread", "turn", &item, "exact")
                .unwrap()
                .unwrap()
                .item,
            "item"
        );
        assert!(
            receipt("thread", "turn", &item, "different")
                .unwrap()
                .is_none()
        );
        let mut mixed = item.clone();
        mixed["content"]
            .as_array_mut()
            .unwrap()
            .push(json!({"type":"image","url":"x"}));
        assert!(
            receipt("thread", "turn", &mixed, "exact")
                .unwrap()
                .is_none()
        );
        let mut annotated = item;
        annotated["content"][0]["text_elements"] = json!([{"start":0}]);
        assert!(receipt("thread", "turn", &annotated, "exact").is_err());
        let mut seen = HashSet::new();
        let response = json!({"data":[],"nextCursor":"again"});
        assert!(page(&response, &mut seen).is_ok());
        assert!(page(&response, &mut seen).is_err());
        assert!(page(&json!({"data":[],"nextCursor":true}), &mut seen).is_err());
    }
}
