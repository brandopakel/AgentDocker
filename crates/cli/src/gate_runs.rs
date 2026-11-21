//! `gate-run record`, which `scripts/verify.sh` uses to record a run, and
//! `gate-runs`, which lists the runs recorded.

use crate::client::Client;
use agentdocker_core::verification::{self, GateRun, GateRunReport, StepState};
use agentdocker_core::{Request, Response};
use anyhow::{Context, Result, bail};
use std::path::PathBuf;

#[derive(clap::Subcommand)]
pub enum GateRunCommand {
    /// Record a local gate run from the report scripts/verify.sh wrote. Its exit says only whether the daemon kept it.
    Record {
        /// The caller the run was started for, as verify.sh captured it; absent, the run's caller is unstated.
        #[arg(long = "as")]
        agent: Option<String>,
        /// The temporary agent verify.sh registered to hold the build slot, kept apart from the caller.
        #[arg(long)]
        runner: Option<String>,
        /// With no --as, infer the caller from this process's ancestry as a claim does; unstated when that finds nobody.
        #[arg(long, conflicts_with = "agent")]
        infer_caller: bool,
        /// The run's report, as JSON.
        #[arg(long)]
        file: PathBuf,
    },
}

#[derive(clap::Args)]
pub struct GateRunsArgs {
    /// Asker; defaults to this agent's identity, or `user` in a human terminal.
    #[arg(long = "as", env = "AGENTDOCKER_AGENT_ID")]
    agent: Option<String>,
    /// Only the runs on this branch.
    branch: Option<String>,
    /// Only the runs that started at this commit.
    #[arg(long)]
    head: Option<String>,
    /// Project id, root or unique prefix (default: the asker's, or this checkout's).
    #[arg(long, value_name = "ID|PATH")]
    project: Option<String>,
    /// At most this many, newest first.
    #[arg(long)]
    limit: Option<usize>,
    /// Print the runs as JSON.
    #[arg(long)]
    json: bool,
}

pub async fn record(client: &Client, command: GateRunCommand) -> Result<()> {
    let GateRunCommand::Record {
        agent,
        runner,
        infer_caller,
        file,
    } = command;
    // An inferred caller is one exact live agent, never the person; a
    // failed inference leaves the caller unstated rather than refusing.
    let agent = match agent {
        Some(agent) => Some(agent),
        None if infer_caller => crate::sender::resolve(client, None).await.ok().flatten(),
        None => None,
    };
    let text = std::fs::read_to_string(&file)
        .with_context(|| format!("cannot read {}", file.display()))?;
    let run: GateRunReport = serde_json::from_str(&text)
        .with_context(|| format!("{} is not a run's report", file.display()))?;
    let Response::GateRun { run } = client
        .call(&Request::RecordGateRun { agent, runner, run })
        .await?
    else {
        bail!("the daemon did not record the run");
    };
    println!("{}", run.id);
    Ok(())
}

pub async fn list(client: &Client, args: GateRunsArgs) -> Result<()> {
    let agent = crate::sender::resolve(client, args.agent).await?;
    let project = crate::gate::project(
        client,
        args.project,
        agent.as_deref().unwrap_or(agentdocker_core::HUMAN),
    )
    .await?;
    let head = args.head.as_deref().map(crate::gate::commit).transpose()?;
    let Response::GateRuns { runs, retained } = client
        .call(&Request::GateRuns {
            agent,
            project,
            branch: args.branch,
            head,
            limit: args.limit,
        })
        .await?
    else {
        bail!("the daemon did not list the runs");
    };
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(
                &serde_json::json!({ "runs": runs, "retained": retained })
            )?
        );
        return Ok(());
    }
    if runs.is_empty() {
        eprintln!("no gate run recorded here (a project keeps its newest {retained})");
        return Ok(());
    }
    for run in &runs {
        println!("{}", line(run));
    }
    eprintln!("(a project keeps its newest {retained} runs; each is its runner's report)");
    Ok(())
}

/// One run: when, the mode, the outcome, the tree, the branch and head, who
/// asked, and what the counts established.
pub fn line(run: &GateRun) -> String {
    let report = &run.report;
    let head = report
        .start
        .head
        .as_deref()
        .map(|head| head.chars().take(8).collect::<String>())
        .unwrap_or_else(|| "unknown".to_owned());
    let tree = match verification::tree(&report.start, &report.end) {
        verification::Tree::CleanBoundaries => "clean boundaries",
        verification::Tree::Dirty => "dirty",
        verification::Tree::Changed => "changed during the run",
        verification::Tree::Unknown => "tree unknown",
    };
    let by = match (&run.by_name, &run.runner) {
        (Some(name), _) => name.clone(),
        (None, Some(_)) => "unstated (a temporary runner)".to_owned(),
        (None, None) => "unstated".to_owned(),
    };
    let mut said = format!(
        "{}  {:<5} {:<10} {}@{} ({tree}) by {by}",
        run.received_at.format("%Y-%m-%d %H:%M"),
        report.mode,
        verification::outcome(report).word(),
        report.branch.as_deref().unwrap_or("(no branch)"),
        head,
    );
    for step in verification::steps(report) {
        match (&step.counts, step.state) {
            (Some(counts), _) => {
                let mut parts = Vec::new();
                for (word, count) in [
                    ("run", counts.run),
                    ("passed", counts.passed),
                    ("failed", counts.failed),
                    ("skipped", counts.skipped),
                ] {
                    if let Some(count) = count {
                        parts.push(format!("{count} {word}"));
                    }
                }
                said.push_str(&format!("; {} {}", step.name, parts.join(", ")));
            }
            (
                None,
                StepState::Failed
                | StepState::Lost
                | StepState::Incomplete
                | StepState::NotReached
                | StepState::Unknown,
            ) => {
                let state = match step.state {
                    StepState::Failed => "failed",
                    StepState::Lost => "stopped",
                    StepState::Incomplete => "never ended",
                    StepState::Unknown => "not run, and not known to be required",
                    _ => "not reached",
                };
                said.push_str(&format!("; {} {state}", step.name));
            }
            _ => {}
        }
    }
    said
}
