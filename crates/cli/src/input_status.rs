//! Report provider evidence before allowing its existing explicit queue ACK.
use agentdocker_core::{InputReport, Request, Response};
use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Utc};

use crate::client::Backend;

/// Preserve the outer error context, without embedding a full provider error
/// chain or terminal control sequences in the durable session summary.
pub fn paused(error: &anyhow::Error) -> InputReport {
    let mut reason = String::new();
    for character in error.to_string().chars() {
        let character = if character.is_control() {
            ' '
        } else {
            character
        };
        if reason.len() + character.len_utf8() > 2048 {
            break;
        }
        reason.push(character);
    }
    if reason.trim().is_empty() {
        reason = "Input delivery stopped before completion. Review the session before restarting."
            .into();
    }
    InputReport::Paused { reason }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pause_summary_keeps_outer_context_and_bounds_multibyte_and_control_text() {
        for message in [
            "日本語\n".repeat(1000),
            "\n\t".into(),
            "outer context".into(),
        ] {
            let error = anyhow::anyhow!("private inner provider details").context(message);
            let InputReport::Paused { reason } = paused(&error) else {
                panic!("pause summary")
            };
            assert!(!reason.trim().is_empty() && reason.len() <= 2048);
            assert!(!reason.chars().any(char::is_control));
            assert!(!reason.contains("private inner"));
        }
    }
}
