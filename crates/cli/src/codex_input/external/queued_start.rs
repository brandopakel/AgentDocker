//! Explicitly start an existing native queue entry after a reviewed interruption.
//! The provider owns scheduling; no timer or historical idle state enters here.
use super::{
    Client, Ledger, Provider, call, identity, ledger::Attempt, queue, receipts, verify_provider,
};
use agentdocker_core::{ProcessIdentity, Request, Response};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

fn live_idle(value: &Value, thread: &str) -> Result<()> {
    ensure!(
        value["thread"]["id"].as_str() == Some(thread)
            && value["thread"]["status"]["type"] == "idle",
        "the owned native thread is not live and idle; queued input is unchanged"
    );
    Ok(())
}

fn exact_head(value: &Value, attempt: &Attempt) -> Result<String> {
    let items = value["data"]
        .as_array()
        .context("native queue has no data")?;
    ensure!(
        items.len() == 1,
        "native queue did not return one pending head"
    );
    let id = receipts::queued_id(&items[0], attempt)?.context(
        "another or edited input is ahead of this message; leave native ordering intact",
    )?;
    ensure!(
        attempt.queued.as_deref() == Some(&id),
        "native queue identity changed; review it again"
    );
    Ok(id)
}

pub(super) async fn start(
    client: &Client,
    provider: &mut Provider,
    ledger: &mut Ledger,
    message: &str,
    confirmation: &str,
    note: &str,
    operator: ProcessIdentity,
) -> Result<Value> {
    let binding = ledger.record().binding.clone();
    ensure!(
        binding.remote.is_some(),
        "explicit start requires the launcher-owned shared native server"
    );
    let attempt = ledger
        .record()
        .attempt
        .clone()
        .context("no retained native input to start")?;
    ensure!(
        attempt.message == message,
        "retained input changed; review it again"
    );
    if let Some(start) = &attempt.start {
        ensure!(
            start.confirmation == confirmation,
            "this entry already has another start intent"
        );
        // A retry only reads the durable disposition, even if its journal or
        // provider reply was lost. Ordinary receipt recovery remains in charge.
        return Ok(json!({"queued_start":start.id,"message":message,
            "turn":start.turn,"already_attempted":true,
            "notice":"The original start intent is retained. This call did not start or acknowledge input again; await its exact provider receipt."}));
    }
    ensure!(
        ledger.start_confirmation()? == confirmation,
        "queued input or provider generation changed; review it again"
    );
    let current = identity(client, &binding).await?;
    ensure!(
        current.project.is_some(),
        "queued start requires a project journal"
    );
    verify_provider(provider, &binding).await?;
    ensure!(
        receipts::find(provider, &binding.provider.session, &attempt, &binding)
            .await?
            .is_none(),
        "a provider receipt already exists; let the receiver reconcile it"
    );
    ensure!(
        receipts::queued(provider, &binding.provider.session, &attempt)
            .await?
            .as_deref()
            == attempt.queued.as_deref(),
        "the original native queue entry is unavailable"
    );
    let state = provider
        .request(
            "thread/read",
            json!({"threadId":binding.provider.session,"includeTurns":false}),
        )
        .await?;
    live_idle(&state, &binding.provider.session)?;
    let head = provider
        .request(
            "thread/queue/list",
            json!({"threadId":binding.provider.session,"limit":1}),
        )
        .await?;
    let queued = exact_head(&head, &attempt)?;
    // The daemon checks exact receiver ownership, project pause and shared
    // provider blocks here. A provider-side interruption is a separate fence.
    let (messages, uncertain, _) = queue(client, ledger, Vec::new()).await?;
    ensure!(
        messages.first().is_some_and(|m| m.id.as_str() == message)
            && !uncertain.iter().any(|id| id.as_str() == message),
        "the daemon is holding this input or its delivery is uncertain; start refused"
    );
    identity(client, &binding).await?;
    let start = ledger.begin_queued_start(message, confirmation, note, operator)?;
    ensure!(matches!(call(client, Request::JournalAdd {
        agent: binding.agent.clone(),
        summary: format!("Explicit native queue start {} requested: message {}, queued entry {}, provider pid {} born {} / thread {}, operator pid {} born {}. {}. Intent persisted before provider transmission; this is not a delivery receipt.",
            start.id, message, queued, binding.provider.process.pid, binding.provider.process.started_at,
            binding.provider.session, start.operator.pid, start.operator.started_at, start.note),
    }).await?, Response::JournalEntry { .. }), "queued start journal write unconfirmed; intent retained without provider transmission");
    // The provider atomically refuses a newly active/pending turn. Never add,
    // delete, reorder or repeat this queued submission to recover a lost reply.
    let value = provider
        .request(
            "thread/queue/start",
            json!({
                "threadId":binding.provider.session,"queuedSubmissionId":queued
            }),
        )
        .await
        .context("queued start unconfirmed; its original intent is retained without retry")?;
    let turn = value["turn"]["id"]
        .as_str()
        .context("queued start reply lacks a turn; intent retained")?;
    ledger.queued_start_replied(&start.id, turn)?;
    Ok(
        json!({"queued_start":start.id,"message":message,"turn":turn,
        "already_attempted":false,"notice":"Started the existing queue entry. Delivery remains pending until its exact provider receipt; no acknowledgement was inferred from this reply."}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_start_requires_the_exact_unedited_queue_head_and_live_thread() {
        let attempt = Attempt {
            message: "message".into(),
            input: "original".into(),
            queued: Some("entry".into()),
            anchor: None,
            receipt: None,
            hook: None,
            start: None,
        };
        let value = json!({"data":[{"id":"entry","clientUserMessageId":"message",
            "input":[{"type":"text","text":"original","text_elements":[]}]}]});
        assert_eq!(exact_head(&value, &attempt).unwrap(), "entry");
        for changed in [
            json!({"data":[]}),
            json!({"data":[value["data"][0],value["data"][0]]}),
        ] {
            assert!(exact_head(&changed, &attempt).is_err());
        }
        for (field, replacement) in [
            ("id", "other-entry"),
            ("clientUserMessageId", "other-message"),
        ] {
            let mut changed = value.clone();
            changed["data"][0][field] = json!(replacement);
            assert!(exact_head(&changed, &attempt).is_err());
        }
        let mut edited = value;
        edited["data"][0]["input"][0]["text"] = json!("edited");
        assert!(exact_head(&edited, &attempt).is_err());
        assert!(
            live_idle(
                &json!({"thread":{"id":"thread","status":{"type":"idle"}}}),
                "thread"
            )
            .is_ok()
        );
        for state in ["active", "notLoaded", "interrupted"] {
            assert!(
                live_idle(
                    &json!({"thread":{"id":"thread","status":{"type":state}}}),
                    "thread"
                )
                .is_err()
            );
        }
        assert!(
            live_idle(
                &json!({"thread":{"id":"other","status":{"type":"idle"}}}),
                "thread"
            )
            .is_err()
        );
    }
}
