//! Normalized provider signals: never parse terminal prose or retain secrets.
use crate::client::{Backend, Client};
use agentdocker_core::{
    AgentRecord, ProviderIssue, ProviderIssueKind as Kind, ProviderReport, Request, Response,
};
use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Utc};
use clap::{Args, Subcommand};
use serde_json::Value;

#[derive(Args)]
pub struct ProviderArgs {
    #[command(subcommand)]
    command: ProviderCommand,
}

#[derive(Subcommand)]
enum ProviderCommand {
    /// Report a provider limit or interruption for the current process.
    Report {
        agent: String,
        #[arg(long)]
        kind: String,
        /// Only a reset time explicitly supplied by the provider, with offset.
        #[arg(long)]
        reset_at: Option<DateTime<Utc>>,
        #[arg(long)]
        quota_group: Option<String>,
        #[arg(long)]
        model: Option<String>,
    },
    /// Resume queued delivery after checking provider availability. Received
    /// input is not replayed, and existing uncertain-input protection remains.
    Resume { agent: String },
}

pub async fn run(client: &Client, args: ProviderArgs) -> Result<()> {
    let name = match &args.command {
        ProviderCommand::Report { agent, .. } | ProviderCommand::Resume { agent } => agent,
    };
    let Response::Agent { agent } = client
        .call(&Request::Inspect {
            agent: name.clone(),
        })
        .await?
    else {
        anyhow::bail!("provider session was not found");
    };
    match args.command {
        ProviderCommand::Report {
            kind,
            reset_at,
            quota_group,
            model,
            ..
        } => {
            let kind = serde_json::from_value(Value::String(kind)).context("kind must be usage, rate, budget, billing, concurrency, context, authentication, transport or unknown")?;
            report(
                client,
                &agent,
                ProviderReport::Blocked {
                    issue: ProviderIssue {
                        kind,
                        reset_at,
                        quota_group,
                        model,
                    },
                },
            )
            .await?;
            println!("{}; accepted messages remain queued.", kind.label());
        }
        ProviderCommand::Resume { .. } => {
            let blocked = agent
                .provider_availability
                .as_ref()
                .filter(|s| s.issue.is_some())
                .context("this session has no recorded provider block")?;
            let response = client
                .call(&Request::ResumeProvider {
                    agent: agent.id.to_string(),
                    blocked_at: blocked.observed_at,
                })
                .await?;
            ensure!(
                matches!(response, Response::Ok),
                "provider resumption refused: {response:?}"
            );
            println!(
                "Queued delivery resumed; uncertain or previously received input is not replayed."
            );
        }
    }
    Ok(())
}

pub async fn report<B: Backend>(
    backend: &B,
    agent: &AgentRecord,
    report: ProviderReport,
) -> Result<()> {
    report_bound(backend, agent.id.as_str(), agent.process_started_at, report).await
}

pub async fn report_bound<B: Backend>(
    backend: &B,
    agent: &str,
    generation: Option<DateTime<Utc>>,
    report: ProviderReport,
) -> Result<()> {
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        backend.call(Request::ReportProvider {
            agent: agent.into(),
            process_started_at: generation.context("provider process identity is unavailable")?,
            observed_at: Utc::now(),
            report,
        }),
    )
    .await
    .context("provider status report timed out")??;
    ensure!(
        matches!(response, Response::Ok),
        "provider status report refused: {response:?}"
    );
    Ok(())
}

pub async fn recovered<B: Backend>(backend: &B, agent: &AgentRecord) -> Result<()> {
    if let Some(blocked) = agent
        .provider_availability
        .as_ref()
        .filter(|s| s.issue.is_some())
    {
        report(
            backend,
            agent,
            ProviderReport::Recovered {
                blocked_at: blocked.observed_at,
            },
        )
        .await?;
    }
    Ok(())
}

/// Claude Code StopFailure supplies a typed failure code. Its current contract
/// supplies no reset timestamp. Do not guess one from a human-facing message.
pub fn claude(error: Option<&str>) -> ProviderIssue {
    ProviderIssue::local(match error {
        Some("rate_limit") => Kind::Rate,
        Some("billing_error") => Kind::Billing,
        Some("authentication_failed" | "oauth_org_not_allowed" | "cloud_credential_error") => {
            Kind::Authentication
        }
        Some("server_error" | "overloaded") => Kind::Transport,
        _ => Kind::Unknown,
    })
}

/// Codex 0.154 app-server's structured CodexErrorInfo, including providers
/// selected behind Codex. Unrecognized failures keep their uncertainty.
pub fn codex(error: &Value) -> ProviderIssue {
    let info = error
        .get("codexErrorInfo")
        .or_else(|| error.get("data").and_then(|d| d.get("codexErrorInfo")));
    ProviderIssue::local(match info.and_then(Value::as_str) {
        Some("contextWindowExceeded") => Kind::Context,
        Some("sessionBudgetExceeded") => Kind::Budget,
        Some("usageLimitExceeded") => Kind::Usage,
        Some("rateLimitExceeded") => Kind::Rate,
        Some("serverOverloaded") => Kind::Transport,
        Some("unauthorized") => Kind::Authentication,
        _ if info.is_some_and(|v| v.is_object()) => {
            let status = info
                .and_then(Value::as_object)
                .and_then(|m| m.values().next())
                .and_then(|v| v.get("httpStatusCode"))
                .and_then(Value::as_u64);
            match status {
                Some(429) => Kind::Rate,
                Some(401 | 403) => Kind::Authentication,
                Some(402) => Kind::Billing,
                Some(500..=599) => Kind::Transport,
                _ => Kind::Unknown,
            }
        }
        _ => Kind::Unknown,
    })
}

#[derive(Debug)]
pub struct Failure(pub ProviderIssue);
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0.kind.label())
    }
}
impl std::error::Error for Failure {}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn provider_codes_are_normalized_without_inferring_scope_or_reset_from_text() {
        for (code, kind) in [
            ("usageLimitExceeded", Kind::Usage),
            ("rateLimitExceeded", Kind::Rate),
            ("sessionBudgetExceeded", Kind::Budget),
            ("contextWindowExceeded", Kind::Context),
            ("unauthorized", Kind::Authentication),
            ("newProviderError", Kind::Unknown),
        ] {
            let issue = codex(
                &serde_json::json!({"codexErrorInfo":code,"message":"private account; resets tomorrow"}),
            );
            assert_eq!(issue, ProviderIssue::local(kind));
        }
        assert_eq!(
            codex(
                &serde_json::json!({"codexErrorInfo":{"httpConnectionFailed":{"httpStatusCode":429}}})
            ),
            ProviderIssue::local(Kind::Rate)
        );
        assert_eq!(claude(Some("rate_limit")), ProviderIssue::local(Kind::Rate));
        assert_eq!(claude(None), ProviderIssue::local(Kind::Unknown));
    }
}
