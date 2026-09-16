//! Provider completion and limits come from typed turn state, never terminal text.
use super::{
    super::{Client, transport::Provider},
    ledger::Ledger,
    report,
};
use agentdocker_core::{AgentRecord, InputReport, ProviderReport};
use anyhow::{Context, Result, ensure};
use serde_json::json;

/// Wait when history still reports the last delivered input as active.
/// This sidecar does not own the TUI, so terminal historical turn states do
/// not prove live idleness. The native queue schedules input in the original TUI.
/// A completed provider receipt is not task completion or permission to flood
/// a session that just reached a usage/context/authentication limit.
pub(super) async fn check(
    client: &Client,
    provider: &mut Provider,
    ledger: &mut Ledger,
    agent: &AgentRecord,
) -> Result<bool> {
    let thread = &ledger.record().binding.provider.session;
    let value = provider
        .request(
            "thread/turns/list",
            json!({"threadId":thread,"limit":50,"itemsView":"notLoaded","sortDirection":"desc"}),
        )
        .await?;
    let turns = value["data"]
        .as_array()
        .context("Codex turn state has no data")?;
    ensure!(
        turns.len() <= 50,
        "Codex turn state exceeds requested bound"
    );
    if let Some(turn) = turns.first()
        && turn["status"] == "failed"
        && let Some(id) = turn["id"].as_str()
        && ledger.record().failed_turn.as_deref() != Some(id)
    {
        let issue = crate::provider_status::codex(&turn["error"]);
        crate::provider_status::report(client, agent, ProviderReport::Blocked { issue }).await?;
        ledger.failed_turn(id)?;
        report(client, ledger, InputReport::Paused { reason: "The provider stopped this conversation. Check its limit or sign-in status before resuming queued input.".into() }).await?;
    }
    if let Some(receipt) = ledger.latest_receipt()
        && let Some(turn) = turns
            .iter()
            .find(|t| t["id"].as_str() == Some(&receipt.turn))
    {
        return Ok(matches!(
            turn["status"].as_str(),
            Some("completed" | "interrupted" | "failed")
        ));
    }
    // Missing historical state does not establish live activity either. The
    // original TUI's native queue remains responsible for scheduling turns.
    Ok(true)
}
