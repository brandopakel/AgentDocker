//! `agentdocker verdict` and `agentdocker gate`: a reviewer's word on a
//! branch at the commit it read, and whether a branch may land now. The
//! gate is the daemon's; this side turns a pull request or a branch into
//! the branch and the full commit it means, and can set the answer as a
//! commit status on GitHub, where any later commit starts without one.
use std::path::PathBuf;
use std::time::Duration;

use crate::client::Client;
use crate::sender;
use agentdocker_core::gate::{CodeVerdict, Findings, Gate};
use agentdocker_core::{HUMAN, Request, Response};
use anyhow::{Context, Result, bail, ensure};

/// How long a `git` or `gh` call may take.
const TOOL_TIMEOUT: Duration = Duration::from_secs(30);
/// The commit status context the gate sets.
pub const STATUS_CONTEXT: &str = "agentdocker/review";

#[derive(clap::Args, Debug)]
pub struct VerdictArgs {
    /// Reviewer; defaults to this agent's identity, or `user` in a human terminal.
    #[arg(long = "as", env = "AGENTDOCKER_AGENT_ID")]
    agent: Option<String>,
    /// The branch reviewed.
    branch: String,
    /// The commit you read: a full id, or anything `git rev-parse` turns into one here.
    #[arg(long)]
    head: String,
    /// The commit an earlier review read, when you read only what came after it.
    #[arg(long)]
    from: Option<String>,
    #[arg(long, group = "word", required = true)]
    /// Good to land at this commit.
    approve: bool,
    #[arg(long, group = "word")]
    /// Not yet; say what to change in the note.
    changes: bool,
    #[arg(long, group = "word")]
    /// Neither approves nor blocks.
    comment: bool,
    /// Findings by priority, as `P1,P2,P3` counts (`0,1,0`).
    #[arg(long, value_parser = parse_findings, default_value = "0,0,0")]
    findings: Findings,
    /// Project id, root or unique prefix (default: the reviewer's, or this checkout's).
    #[arg(long, value_name = "ID|PATH")]
    project: Option<String>,
    /// What you checked and what you found.
    note: Option<String>,
}

#[derive(clap::Args, Debug)]
pub struct GateArgs {
    /// Asker; defaults to this agent's identity, or `user` in a human terminal.
    #[arg(long = "as", env = "AGENTDOCKER_AGENT_ID")]
    agent: Option<String>,
    /// A pull request (`#402` or `402`, read with `gh`) or a branch.
    target: String,
    /// The commit to ask about (default: the pull request's head, or the branch's on `origin`, else here).
    #[arg(long)]
    head: Option<String>,
    /// Whose approvals count, besides the person's (default: `reviewer`).
    #[arg(long)]
    role: Option<String>,
    /// Project id, root or unique prefix (default: the asker's, or this checkout's).
    #[arg(long, value_name = "ID|PATH")]
    project: Option<String>,
    /// Also set the answer as the `agentdocker/review` commit status on GitHub.
    #[arg(long)]
    publish: bool,
    /// Print the answer as JSON.
    #[arg(long)]
    json: bool,
}

fn parse_findings(text: &str) -> Result<Findings, String> {
    let counts: Vec<u32> = text
        .split(',')
        .map(|n| n.trim().parse::<u32>())
        .collect::<Result<_, _>>()
        .map_err(|_| "findings are three counts, P1,P2,P3 (`0,1,0`)".to_owned())?;
    match counts.as_slice() {
        [p1, p2, p3] => Ok(Findings {
            p1: *p1,
            p2: *p2,
            p3: *p3,
        }),
        _ => Err("findings are three counts, P1,P2,P3 (`0,1,0`)".to_owned()),
    }
}

fn tool(argv: &[&str]) -> Result<String> {
    let argv: Vec<String> = argv.iter().map(|&a| a.to_owned()).collect();
    let output = agentdocker_host::command::run(&std::env::current_dir()?, &argv, TOOL_TIMEOUT)
        .with_context(|| format!("cannot run {}", argv[0]))?;
    ensure!(
        output.success,
        "`{}` failed: {}",
        argv.join(" "),
        output.text.trim()
    );
    Ok(output.stdout.trim().to_owned())
}

/// The full id of the commit `reference` names here.
fn commit(reference: &str) -> Result<String> {
    tool(&[
        "git",
        "rev-parse",
        "--verify",
        "--end-of-options",
        &format!("{reference}^{{commit}}"),
    ])
    .with_context(|| format!("{reference} is not a commit this checkout knows"))
}

/// This checkout's root, for a project when none is named.
fn checkout_root() -> Result<PathBuf> {
    Ok(PathBuf::from(tool(&[
        "git",
        "rev-parse",
        "--show-toplevel",
    ])?))
}

/// The project to send: the one named, else none when an agent asks (the
/// daemon takes its own), else this checkout's.
fn project(named: Option<String>, agent: &str) -> Result<Option<String>> {
    if named.is_some() || agent != HUMAN {
        return Ok(named);
    }
    Ok(Some(checkout_root()?.display().to_string()))
}

/// A pull request number, from `#402` or `402`.
fn pull_request(target: &str) -> Option<u64> {
    target
        .strip_prefix('#')
        .unwrap_or(target)
        .parse()
        .ok()
        .filter(|n| *n > 0)
}

/// The branch and commit a gate is asked about.
fn branch_and_head(target: &str, head: Option<&str>) -> Result<(String, String)> {
    if let Some(number) = pull_request(target) {
        let found = tool(&[
            "gh",
            "pr",
            "view",
            &number.to_string(),
            "--json",
            "headRefName,headRefOid",
        ])?;
        let found: serde_json::Value =
            serde_json::from_str(&found).context("`gh pr view` gave no pull request")?;
        let branch = found["headRefName"]
            .as_str()
            .context("the pull request names no branch")?
            .to_owned();
        let head = match head {
            Some(head) => commit(head)?,
            None => found["headRefOid"]
                .as_str()
                .context("the pull request names no head commit")?
                .to_owned(),
        };
        return Ok((branch, head));
    }
    let head = match head {
        Some(head) => commit(head)?,
        None => commit(&format!("refs/remotes/origin/{target}"))
            .or_else(|_| commit(&format!("refs/heads/{target}")))?,
    };
    Ok((target.to_owned(), head))
}

fn short(commit: &str) -> &str {
    &commit[..commit.len().min(8)]
}

/// One line saying what the gate said.
fn said(gate: &Gate, branch: &str, head: &str) -> String {
    let who = |v: &CodeVerdict| {
        let Findings { p1, p2, p3 } = v.findings;
        format!("{} (P1 {p1}, P2 {p2}, P3 {p3})", v.by_name)
    };
    match gate {
        Gate::Approved { verdict } => {
            format!("approved: {branch} at {} by {}", short(head), who(verdict))
        }
        Gate::Changes { verdict } => format!(
            "changes asked: {branch} by {} at {}",
            who(verdict),
            short(&verdict.head)
        ),
        Gate::Stale { verdict } => format!(
            "stale: {branch} was approved at {} by {}; {}..{} is unreviewed",
            short(&verdict.head),
            verdict.by_name,
            short(&verdict.head),
            short(head)
        ),
        Gate::Missing => format!("unreviewed: nobody who counts has approved {branch}"),
    }
}

/// The commit status that says the same: GitHub keeps it on this commit
/// only, so a later one starts without it.
fn status(gate: &Gate) -> (&'static str, String) {
    let description = match gate {
        Gate::Approved { verdict } => format!("approved by {}", verdict.by_name),
        Gate::Changes { verdict } => format!("{} asked for changes", verdict.by_name),
        Gate::Stale { verdict } => format!(
            "approved at {}; later commits unreviewed",
            short(&verdict.head)
        ),
        Gate::Missing => "not reviewed".to_owned(),
    };
    let state = match gate {
        Gate::Approved { .. } => "success",
        Gate::Changes { .. } => "failure",
        Gate::Stale { .. } | Gate::Missing => "pending",
    };
    (state, description.chars().take(140).collect())
}

fn publish(gate: &Gate, head: &str) -> Result<()> {
    let (state, description) = status(gate);
    tool(&[
        "gh",
        "api",
        "--method",
        "POST",
        &format!("repos/{{owner}}/{{repo}}/statuses/{head}"),
        "-f",
        &format!("state={state}"),
        "-f",
        &format!("context={STATUS_CONTEXT}"),
        "-f",
        &format!("description={description}"),
    ])
    .context("cannot set the commit status on GitHub")?;
    Ok(())
}

pub async fn verdict(client: &Client, args: VerdictArgs) -> Result<()> {
    let agent = sender::resolve(client, args.agent)
        .await?
        .unwrap_or_else(|| HUMAN.into());
    let word = if args.approve {
        "approve"
    } else if args.changes {
        "changes"
    } else {
        "comment"
    };
    let request = Request::CodeVerdict {
        project: project(args.project, &agent)?,
        agent,
        branch: args.branch,
        head: commit(&args.head)?,
        from: args.from.as_deref().map(commit).transpose()?,
        verdict: word.to_owned(),
        findings: args.findings,
        note: args.note,
    };
    let Response::CodeVerdict { verdict } = client.call(&request).await? else {
        bail!("the daemon did not record the verdict");
    };
    println!(
        "{} {} at {}",
        verdict.verdict,
        verdict.branch,
        short(&verdict.head)
    );
    Ok(())
}

/// Ask the gate. `Ok(true)` when the branch may land at that commit.
pub async fn gate(client: &Client, args: GateArgs) -> Result<bool> {
    let agent = sender::resolve(client, args.agent).await?;
    let (branch, head) = branch_and_head(&args.target, args.head.as_deref())?;
    let request = Request::ReviewGate {
        project: project(args.project, agent.as_deref().unwrap_or(HUMAN))?,
        agent,
        branch: branch.clone(),
        head: head.clone(),
        role: args.role,
    };
    let Response::ReviewGate { gate } = client.call(&request).await? else {
        bail!("the daemon did not answer for the gate");
    };
    if args.publish {
        publish(&gate, &head)?;
    }
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "branch": branch, "head": head, "gate": gate,
            }))?
        );
    } else {
        println!("{}", said(&gate, &branch, &head));
    }
    Ok(gate.approved())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::{AgentId, ProjectId, Verdict};

    fn verdict(head: &str) -> Box<CodeVerdict> {
        Box::new(CodeVerdict {
            id: "1".into(),
            project: ProjectId::from("p"),
            branch: "b".into(),
            head: head.into(),
            from: None,
            by: AgentId::from("r"),
            by_name: "reviewer-1".into(),
            human: false,
            roles: vec!["reviewer".into()],
            verdict: Verdict::Approve,
            findings: Findings {
                p1: 0,
                p2: 1,
                p3: 0,
            },
            note: String::new(),
            at: chrono::Utc::now(),
        })
    }

    #[test]
    fn findings_are_three_counts() {
        assert_eq!(
            parse_findings("0, 2,1").unwrap(),
            Findings {
                p1: 0,
                p2: 2,
                p3: 1
            }
        );
        for bad in ["", "1,2", "1,2,3,4", "a,b,c", "-1,0,0"] {
            assert!(parse_findings(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_pull_request_is_a_number_and_anything_else_a_branch() {
        assert_eq!(pull_request("#402"), Some(402));
        assert_eq!(pull_request("402"), Some(402));
        assert_eq!(pull_request("claude/review-gate"), None);
        assert_eq!(pull_request("#0"), None);
    }

    #[test]
    fn the_status_says_what_the_gate_said_and_only_an_approval_passes() {
        let head = "a".repeat(40);
        let other = "b".repeat(40);
        let approved = Gate::Approved {
            verdict: verdict(&head),
        };
        assert_eq!(status(&approved).0, "success");
        assert!(said(&approved, "b", &head).starts_with("approved: b at aaaaaaaa"));
        let stale = Gate::Stale {
            verdict: verdict(&head),
        };
        assert_eq!(status(&stale).0, "pending");
        assert!(said(&stale, "b", &other).contains("aaaaaaaa..bbbbbbbb is unreviewed"));
        let mut changes = verdict(&head);
        changes.verdict = Verdict::Changes;
        assert_eq!(status(&Gate::Changes { verdict: changes }).0, "failure");
        assert_eq!(status(&Gate::Missing).0, "pending");
        assert!(status(&approved).1.chars().count() <= 140);
    }
}
