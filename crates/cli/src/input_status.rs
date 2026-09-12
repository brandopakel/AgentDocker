//! Report provider evidence before allowing its existing explicit queue ACK.
use agentdocker_core::{InputReport, Request, Response};
use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Utc};

use crate::client::Backend;

pub async fn report<B: Backend>(
    backend: &B,
    agent: &str,
    process_started_at: Option<DateTime<Utc>>,
    report: InputReport,
) -> Result<()> {
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        backend.call(Request::ReportInput {
            agent: agent.into(),
            process_started_at: process_started_at
                .context("provider process generation is unavailable")?,
            observed_at: Utc::now(),
            report,
        }),
    )
    .await
    .context("input status report timed out")??;
    ensure!(
        matches!(response, Response::Ok),
        "input status report refused: {response:?}"
    );
    Ok(())
}
