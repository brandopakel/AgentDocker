//! Explicitly start an existing native queue entry after a reviewed interruption.
//! The provider owns scheduling; no timer or historical idle state enters here.
use super::{Client, Ledger, Provider, call, identity, ledger::Attempt, queue, receipts, remote};
use agentdocker_core::{ProcessIdentity, ProjectId, Request, Response};
use anyhow::{Context, Result, bail, ensure};
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

fn unpaused_project(response: Response, project: &ProjectId) -> Result<()> {
    let Response::Pauses { pauses } = response else {
        bail!("project pause state is unavailable; queued start refused");
    };
    ensure!(
        !pauses.iter().any(|pause| &pause.project == project),
        "the project is paused; queued start refused"
    );
    Ok(())
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
    // This is the service's initialized connection, not a newly spawned
    // observer. Recheck its ownership without another initialize request.
    remote::reverify(binding.remote.as_ref().expect("checked remote"), &binding)?;
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
    ensure!(
        state["thread"]["cwd"]
            .as_str()
            .and_then(|p| std::path::Path::new(p).canonicalize().ok())
            .as_ref()
            == Some(&binding.cwd),
        "native thread checkout changed; queued input is unchanged"
    );
    let head = provider
        .request(
            "thread/queue/list",
            json!({"threadId":binding.provider.session,"limit":1}),
        )
        .await?;
    let queued = exact_head(&head, &attempt)?;
    // The daemon checks exact receiver ownership and shared provider blocks.
    // Its inbox remains readable during a project pause so lifecycle notices
    // can arrive; queue visibility is not permission to start paused work.
    let (messages, uncertain, _) = queue(client, ledger, Vec::new()).await?;
    ensure!(
        messages.first().is_some_and(|m| m.id.as_str() == message)
            && !uncertain.iter().any(|id| id.as_str() == message),
        "the daemon is holding this input or its delivery is uncertain; start refused"
    );
    let current = identity(client, &binding).await?;
    let project = current
        .project
        .as_ref()
        .context("queued start requires a project journal")?
        .id();
    unpaused_project(call(client, Request::Pauses).await?, &project)?;
    let start = ledger.begin_queued_start(message, confirmation, note, operator)?;
    ensure!(matches!(call(client, Request::JournalAdd {
        agent: binding.agent.clone(),
        summary: format!("Explicit native queue start {} requested: message {}, queued entry {}, provider pid {} born {} / thread {}, operator pid {} born {}. {}. Intent persisted before provider transmission; this is not a delivery receipt.",
            start.id, message, queued, binding.provider.process.pid, binding.provider.process.started_at,
            binding.provider.session, start.operator.pid, start.operator.started_at, start.note),
    }).await?, Response::JournalEntry { .. }), "queued start journal write unconfirmed; intent retained without provider transmission");
    // A pause may have arrived while the journal call was pending. Refuse it
    // here too, retaining the already persisted intent without blind retries.
    // This is a snapshot before transmission, not an atomic provider lock.
    unpaused_project(call(client, Request::Pauses).await?, &project)?;
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
    fn queued_start_requires_known_unpaused_project_even_with_visible_inbox() {
        let project = ProjectId::from("owned-project");
        let unrelated = agentdocker_core::Pause {
            project: ProjectId::from("other-project"),
            by: "user".into(),
            reason: "finish the current step".into(),
            at: chrono::Utc::now(),
        };
        assert!(unpaused_project(Response::Pauses { pauses: vec![] }, &project).is_ok());
        assert!(
            unpaused_project(
                Response::Pauses {
                    pauses: vec![unrelated.clone()],
                },
                &project,
            )
            .is_ok()
        );
        let held = agentdocker_core::Pause {
            project: project.clone(),
            ..unrelated.clone()
        };
        assert!(
            unpaused_project(
                Response::Pauses {
                    pauses: vec![unrelated, held],
                },
                &project,
            )
            .unwrap_err()
            .to_string()
            .contains("project is paused")
        );
        assert!(unpaused_project(Response::Ok, &project).is_err());
    }

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
