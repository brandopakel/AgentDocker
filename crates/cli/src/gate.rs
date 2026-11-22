//! `agentdocker verdict` and `agentdocker gate`: a reviewer's word on a
//! branch at the commit it read, and whether a branch may land now. The
//! gate is the daemon's; this side turns a pull request or a branch into
//! the branch and the full commit it means, and can set the answer as a
//! commit status on GitHub, where any later commit starts without one.
//! `verdict --supersede <id>` is the person setting a verdict aside.
//! And `agentdocker report-commit`, what the post-commit hook of a worktree
//! AgentDocker created runs, so the daemon keeps who made each commit there
//! and refuses that agent's word on the branch.
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::client::Client;
use crate::sender;
use agentdocker_core::gate::{
    CodeVerdict, Findings, GATE_ROLE, Gate, SupersedeReason, WitnessFile,
};
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
    #[arg(required_unless_present = "supersede")]
    branch: Option<String>,
    /// The commit you read: a full id, or anything `git rev-parse` turns into one here.
    #[arg(long, required_unless_present = "supersede")]
    head: Option<String>,
    /// The commit an earlier review read, when you read only what came after it.
    #[arg(long)]
    from: Option<String>,
    // One word is required unless a verdict is being set aside; the group
    // keeps it to one. (A plain `required` would be waived by the group
    // alone, a `required_unless_present` is not.)
    #[arg(
        long,
        group = "word",
        required_unless_present_any = ["changes", "none", "supersede"],
        visible_alias = "approve"
    )]
    /// Good to land at this commit (no important finding).
    pass: bool,
    #[arg(long, group = "word")]
    /// Not yet; say what to change in the note.
    changes: bool,
    #[arg(long, group = "word", visible_alias = "comment")]
    /// Read, and no verdict given: neither passes nor blocks.
    none: bool,
    /// Findings by severity, as `important,nit,pre-existing` counts (`0,1,0`). Absent, the verdict says its counts were not stated.
    #[arg(long, value_parser = parse_findings)]
    findings: Option<Findings>,
    /// When your review of this branch started (an RFC 3339 time, or a duration ago such as `90m` or `2h`): the window your model evidence is read in. Absent, the window is inferred and said so.
    #[arg(long, value_parser = parse_since)]
    since: Option<chrono::DateTime<chrono::Utc>>,
    /// Project id, root or unique prefix (default: the reviewer's, or this checkout's).
    #[arg(long, value_name = "ID|PATH")]
    project: Option<String>,
    /// What you checked and what you found.
    note: Option<String>,
    /// Set a verdict aside by its id so it counts at no gate (the person only): the way
    /// out when a reviewer asked for changes and is gone.
    #[arg(
        long,
        value_name = "VERDICT",
        conflicts_with_all = ["branch", "head", "from", "pass", "changes", "none", "findings", "since", "note"]
    )]
    supersede: Option<String>,
    /// Why the verdict is set aside (with --supersede).
    // Its own conflicts too: clap waives a `requires` when anything the
    // required argument conflicts with is given.
    #[arg(
        long,
        requires = "supersede",
        conflicts_with_all = ["branch", "head", "from", "pass", "changes", "none", "findings", "since", "note"]
    )]
    reason: Option<String>,
}

#[derive(clap::Args, Debug)]
pub struct GateArgs {
    /// Asker; defaults to this agent's identity, or `user` in a human terminal.
    #[arg(long = "as", env = "AGENTDOCKER_AGENT_ID")]
    agent: Option<String>,
    /// A pull request (`#402` or `402`, read with `gh`) or a branch.
    target: String,
    /// The commit to ask about (default: the pull request's head, or the branch's head on `origin` now).
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

/// A stated start: an RFC 3339 time, or a duration ago (`90m`, `2h`, `1d`).
fn parse_since(text: &str) -> Result<chrono::DateTime<chrono::Utc>, String> {
    if let Ok(at) = chrono::DateTime::parse_from_rfc3339(text) {
        return Ok(at.with_timezone(&chrono::Utc));
    }
    let (number, unit) = text.split_at(text.trim_end_matches(char::is_alphabetic).len());
    let amount = number
        .parse::<u64>()
        .ok()
        .and_then(|amount| i64::try_from(amount).ok())
        .ok_or_else(|| {
            "a start is an RFC 3339 time or a duration ago (`90m`, `2h`, `1d`)".to_owned()
        })?;
    let seconds = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        _ => return Err("a duration ago ends in s, m, h or d".to_owned()),
    };
    amount
        .checked_mul(seconds)
        .and_then(chrono::Duration::try_seconds)
        .and_then(|ago| chrono::Utc::now().checked_sub_signed(ago))
        .ok_or_else(|| "that start is further back than a time can be".to_owned())
}

const FINDINGS_FORM: &str = "findings are three counts, important,nit,pre-existing (`0,1,0`)";

fn parse_findings(text: &str) -> Result<Findings, String> {
    let counts: Vec<u32> = text
        .split(',')
        .map(|n| n.trim().parse::<u32>())
        .collect::<Result<_, _>>()
        .map_err(|_| FINDINGS_FORM.to_owned())?;
    match counts.as_slice() {
        [important, nit, pre_existing] => Ok(Findings {
            important: *important,
            nit: *nit,
            pre_existing: *pre_existing,
        }),
        _ => Err(FINDINGS_FORM.to_owned()),
    }
}

fn tool(argv: &[&str]) -> Result<String> {
    tool_in(&std::env::current_dir()?, argv)
}

fn tool_in(dir: &Path, argv: &[&str]) -> Result<String> {
    let argv: Vec<String> = argv.iter().map(|&a| a.to_owned()).collect();
    let output = agentdocker_host::command::run(dir, &argv, TOOL_TIMEOUT)
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
pub(crate) fn commit(reference: &str) -> Result<String> {
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
/// The project a verdict or a gate names: the one given, else for the
/// person (by the `user` name or by their record's id, as
/// `AGENTDOCKER_AGENT_ID=$(agentdocker me)` sets it) this checkout's;
/// an agent's own the daemon knows.
pub(crate) async fn project(
    client: &Client,
    named: Option<String>,
    agent: &str,
) -> Result<Option<String>> {
    if named.is_some() {
        return Ok(named);
    }
    let person = agent == HUMAN
        || matches!(
            client.call(&Request::Inspect { agent: agent.to_owned() }).await,
            Ok(Response::Agent { agent }) if agent.spec.runtime == agentdocker_core::HUMAN_RUNTIME
        );
    if !person {
        return Ok(None);
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

/// Where a pull request number is read, and the project a read of it is
/// then of. A number names another change in every repository, so the two
/// are one: a project named by its root (an absolute path) is read there;
/// otherwise in this checkout, which must then be the project named.
fn pr_repository(named: Option<&str>) -> Result<(PathBuf, String)> {
    match named {
        Some(root) if Path::new(root).is_absolute() => Ok((PathBuf::from(root), root.to_owned())),
        Some(other) => {
            let here = checkout_root()?;
            let id = agentdocker_host::project::discover(&here).id();
            ensure!(
                id.as_str().starts_with(other),
                "a pull request number is read in this checkout's repository, which is project {id}, not {other}: run this from {other}'s checkout, pass its root as --project, or name the branch"
            );
            Ok((here, other.to_owned()))
        }
        None => {
            let here = checkout_root()?;
            let root = here.display().to_string();
            Ok((here, root))
        }
    }
}

/// The branch pull request `number` is of, as `gh` reads it in `dir`.
fn pr_branch(gh: &str, dir: &Path, number: u64) -> Result<String> {
    let found = tool_in(
        dir,
        &[
            gh,
            "pr",
            "view",
            &number.to_string(),
            "--json",
            "headRefName",
        ],
    )?;
    let found: serde_json::Value =
        serde_json::from_str(&found).context("`gh pr view` gave no pull request")?;
    Ok(found["headRefName"]
        .as_str()
        .context("the pull request names no branch")?
        .to_owned())
}

/// The branch a read names, and its project: a pull request's branch in
/// the repository it is read in, else the branch as given.
async fn read_target(
    client: &Client,
    target: &str,
    named: Option<String>,
    agent: &str,
) -> Result<(String, Option<String>)> {
    match pull_request(target) {
        Some(number) => {
            let (dir, project) = pr_repository(named.as_deref())?;
            Ok((pr_branch("gh", &dir, number)?, Some(project)))
        }
        None => Ok((target.to_owned(), project(client, named, agent).await?)),
    }
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
        None => remote_head(target)?,
    };
    Ok((target.to_owned(), head))
}

/// The branch's head on `origin` as it is now, asked of the remote rather
/// than read from a tracking ref a fetch may not have updated. Not found
/// is an error, never a commit taken from somewhere else: name one with
/// `--head` to ask about a branch `origin` does not have.
fn remote_head(branch: &str) -> Result<String> {
    let listed = tool(&[
        "git",
        "ls-remote",
        "--end-of-options",
        "origin",
        &format!("refs/heads/{branch}"),
    ])
    .context("cannot ask origin for the branch's head; name the commit with --head")?;
    let head = listed
        .lines()
        .find_map(|line| line.split_whitespace().next())
        .filter(|commit| agentdocker_core::gate::full_commit(commit))
        .with_context(|| {
            format!("origin has no branch {branch}; name the commit to ask about with --head")
        })?;
    Ok(head.to_owned())
}

fn short(commit: &str) -> &str {
    &commit[..commit.len().min(8)]
}

/// The reviewer, its runtime and what it found, as a person reads it:
/// `reviewer-1 (codex; important 0, nit 1, pre-existing 0)`.
/// Who gave it, in what, and what they found: the name and the runtime
/// are what a record said, shown with any control or direction mark
/// written out.
fn who(v: &CodeVerdict) -> String {
    use crate::out::text::shown;
    format!(
        "{} ({}; {})",
        shown(&v.by_name),
        shown(v.runtime_said()),
        v.findings.said_as(v.findings_presence)
    )
}

/// One line saying what the gate said.
fn said(gate: &Gate, branch: &str, head: &str) -> String {
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
            "stale: {branch} was passed at {} by {}; {}..{} is unreviewed",
            short(&verdict.head),
            who(verdict),
            short(&verdict.head),
            short(head)
        ),
        Gate::Missing => format!("unreviewed: nobody who counts has passed {branch}"),
    }
}

/// One line for each verdict set aside at `role`'s gate: which it was,
/// whose, and who set it aside.
fn set_aside(superseded: &[CodeVerdict], role: &str) -> Vec<String> {
    superseded
        .iter()
        .filter_map(|verdict| {
            let mark = verdict.superseded_at(role)?;
            let why = match mark.reason {
                SupersedeReason::ReviewerEnded => {
                    format!(
                        "{} has left; {} passed since",
                        verdict.by_name, mark.by_name
                    )
                }
                SupersedeReason::Person => format!("set aside by {}", mark.by_name),
            };
            Some(format!(
                "superseded: {} by {} at {} (verdict {}): {why}",
                verdict.verdict,
                verdict.by_name,
                short(&verdict.head),
                verdict.id
            ))
        })
        .collect()
}

/// The commit status that says the same, with the counts and the
/// reviewer's runtime: GitHub keeps it on this commit only, so a later
/// one starts without it.
fn status(gate: &Gate) -> (&'static str, String) {
    let description = match gate {
        Gate::Approved { verdict } => format!("pass by {}", who(verdict)),
        Gate::Changes { verdict } => format!("changes by {}", who(verdict)),
        Gate::Stale { verdict } => format!(
            "pass at {} by {}; later commits unreviewed",
            short(&verdict.head),
            who(verdict)
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
    if let Some(id) = args.supersede {
        let request = Request::SupersedeVerdict {
            project: project(client, args.project, &agent).await?,
            agent,
            verdict: id,
            reason: args.reason,
        };
        let Response::CodeVerdict { verdict } = client.call(&request).await? else {
            bail!("the daemon did not set the verdict aside");
        };
        println!("{}", verdict.id);
        eprintln!(
            "superseded: {} {} at {} by {}; ask `agentdocker gate {}` for the gate now",
            verdict.verdict,
            verdict.branch,
            short(&verdict.head),
            verdict.by_name,
            verdict.branch
        );
        return Ok(());
    }
    let (Some(branch), Some(head)) = (args.branch, args.head) else {
        bail!("a verdict names the branch and --head it read");
    };
    let word = if args.pass {
        "pass"
    } else if args.changes {
        "changes"
    } else {
        "none"
    };
    let request = Request::CodeVerdict {
        project: project(client, args.project, &agent).await?,
        agent,
        branch,
        head: commit(&head)?,
        from: args.from.as_deref().map(commit).transpose()?,
        verdict: word.to_owned(),
        findings_stated: Some(args.findings.is_some()),
        findings: args.findings.unwrap_or_default(),
        since: args.since,
        note: args.note,
    };
    let Response::CodeVerdict { verdict } = client.call(&request).await? else {
        bail!("the daemon did not record the verdict");
    };
    // Its id, as every command that makes something says it; then the
    // line a job parses, for the person reading.
    println!("{}", verdict.id);
    eprintln!("{} {}", verdict.branch, verdict.line());
    Ok(())
}

#[derive(clap::Args, Debug)]
pub struct VerdictsArgs {
    /// Asker; defaults to this agent's identity, or `user` in a human terminal.
    #[arg(long = "as", env = "AGENTDOCKER_AGENT_ID")]
    agent: Option<String>,
    /// A pull request (`#402` or `402`, read with `gh`) or a branch.
    target: String,
    /// Project id, root or unique prefix (default: the asker's, or this checkout's).
    #[arg(long, value_name = "ID|PATH")]
    project: Option<String>,
    /// Print the verdicts as JSON.
    #[arg(long)]
    json: bool,
}

/// Every retained verdict on a branch, oldest first, each with what it
/// kept of its reviewer's model.
pub async fn verdicts(client: &Client, args: VerdictsArgs) -> Result<()> {
    let agent = sender::resolve(client, args.agent).await?;
    let (branch, project) = read_target(
        client,
        &args.target,
        args.project,
        agent.as_deref().unwrap_or(HUMAN),
    )
    .await?;
    let Response::Verdicts { verdicts, retained } = client
        .call(&Request::Verdicts {
            project,
            agent,
            branch: branch.clone(),
        })
        .await?
    else {
        bail!("the daemon did not list the verdicts");
    };
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "branch": branch, "verdicts": verdicts, "retained": retained,
            }))?
        );
        return Ok(());
    }
    if verdicts.is_empty() {
        eprintln!("no verdict retained on {branch} (a project keeps at most {retained})");
        return Ok(());
    }
    for verdict in &verdicts {
        println!("{}", round(verdict));
    }
    eprintln!(
        "(only retained verdicts are listed: a project keeps at most {retained}, so an absent round proves nothing)"
    );
    Ok(())
}

#[derive(clap::Args, Debug)]
pub struct AuthorshipArgs {
    /// Asker; defaults to this agent's identity, or `user` in a human terminal.
    #[arg(long = "as", env = "AGENTDOCKER_AGENT_ID")]
    agent: Option<String>,
    /// A pull request (`#402` or `402`, read with `gh`) or a branch.
    target: String,
    /// Project id, root or unique prefix (default: the asker's, or this checkout's).
    #[arg(long, value_name = "ID|PATH")]
    project: Option<String>,
    /// Start after this receipt (the `next` a page printed).
    #[arg(long)]
    after: Option<u64>,
    /// At most this many receipts (up to 200).
    #[arg(long)]
    limit: Option<usize>,
    /// Print the page as JSON.
    #[arg(long)]
    json: bool,
}

/// A branch's retained receipts, oldest first, labelled with the project
/// and the full ref, and what they cannot establish.
pub async fn authorship(client: &Client, args: AuthorshipArgs) -> Result<()> {
    let agent = sender::resolve(client, args.agent).await?;
    let (branch, project) = read_target(
        client,
        &args.target,
        args.project,
        agent.as_deref().unwrap_or(HUMAN),
    )
    .await?;
    let response = client
        .call(&Request::Authorship {
            project,
            agent,
            branch,
            after: args.after,
            limit: args.limit,
        })
        .await?;
    let Response::Authorship {
        project,
        branch,
        receipts,
        next,
        coverage,
    } = response
    else {
        bail!("the daemon did not list the branch's receipts");
    };
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "project": project, "branch": branch, "receipts": receipts,
                "next": next, "coverage": coverage,
            }))?
        );
        return Ok(());
    }
    print!(
        "{}",
        receipts_said(&project, &branch, &receipts, next, &coverage)
    );
    Ok(())
}

/// A page of receipts as lines: the project and ref it is of, a line per
/// receipt, and what the page cannot establish.
fn receipts_said(
    project: &agentdocker_core::ProjectId,
    branch: &str,
    receipts: &[agentdocker_core::gate::AuthorshipReceipt],
    next: Option<u64>,
    coverage: &agentdocker_core::gate::AuthorshipCoverage,
) -> String {
    use crate::out::text::shown;
    use agentdocker_core::gate::{Basis, Unreceipted};
    let mut out = format!("{} in project {project}\n", shown(branch));
    if receipts.is_empty() {
        out.push_str("no receipts on this page\n");
    }
    for receipt in receipts {
        let how = match receipt.basis {
            Basis::DaemonCommit => "made by the daemon for it",
            Basis::HookSession => "reported from its own session",
            Basis::HookOwner => {
                "reported unauthenticated; its worktree, not shown to be its commit"
            }
            Basis::Unknown => "basis unknown",
        };
        let who = match &receipt.author {
            Some(author) => {
                let configured = author.configured.as_ref().map_or_else(String::new, |c| {
                    format!(
                        ", configured {}/{}",
                        shown(c.provider.as_deref().unwrap_or("?")),
                        shown(c.model.as_deref().unwrap_or("?"))
                    )
                });
                format!(
                    "{} ({}{configured})",
                    shown(&author.name),
                    shown(&author.runtime)
                )
            }
            None => format!("agent {} (no snapshot kept)", receipt.agent.short()),
        };
        out.push_str(&format!(
            "#{} {} by {who} at {}: {}, {how}\n",
            receipt.seq,
            short(&receipt.commit),
            receipt.at.format("%Y-%m-%d %H:%M:%S UTC"),
            shown(&receipt.witness),
        ));
    }
    if let Some(next) = next {
        out.push_str(&format!("more after #{next} (--after {next})\n"));
    }
    out.push_str(&format!(
        "(a project keeps its newest {} receipts; a ref's receipts do not by themselves identify a change's commits, since a branch reused, force-pushed or named alike in a fork can put others under it; a configured model is what the record asked for, not what wrote the change)\n",
        coverage.retained
    ));
    if let Some(since) = coverage.capture_since {
        out.push_str(&format!(
            "receipts kept since {}; those from before name no basis and no author\n",
            since.format("%Y-%m-%d %H:%M:%S UTC")
        ));
    }
    if let Some(pruned) = coverage.pruned_through {
        out.push_str(&format!(
            "the project's bound has pruned receipts through #{pruned}, on any ref\n"
        ));
    }
    match coverage.oldest_retained {
        Some(oldest) => out.push_str(&format!(
            "the project's oldest retained receipt, on any ref, was kept at {}\n",
            oldest.format("%Y-%m-%d %H:%M:%S UTC")
        )),
        None => out.push_str("the project holds no receipt\n"),
    }
    if coverage.seed_skipped > 0 {
        out.push_str(&format!(
            "{} rows were left out when receipts were first taken, past the bound\n",
            coverage.seed_skipped
        ));
    }
    if coverage.page_may_miss {
        out.push_str("this page may be missing receipts the bound pruned\n");
    }
    match coverage.unreceipted {
        Unreceipted::Counted { count: 0 } => out.push_str(
            "every commit still recorded as witnessed on this ref has a receipt (which does not show capture was continuous)\n",
        ),
        Unreceipted::Counted { count } => out.push_str(&format!(
            "{count} commits recorded as witnessed on this ref have no receipt, for a cause that cannot be told\n"
        )),
        Unreceipted::AtLeast { count } => out.push_str(&format!(
            "at least {count} commits recorded as witnessed on this ref have no receipt, for a cause that cannot be told\n"
        )),
        Unreceipted::Unknown => out.push_str(
            "whether commits recorded as witnessed on this ref lack a receipt is not known\n",
        ),
    }
    out
}

/// One round: its order, word, commit, reviewer, counts and model evidence.
fn round(v: &CodeVerdict) -> String {
    use crate::out::text::shown;
    use agentdocker_core::evidence::{ObservedResult, WindowSource};
    // A provider or model is what a record or a usage source said: shown
    // with its controls and direction marks made visible.
    let pair = |provider: Option<&str>, model: Option<&str>| {
        format!(
            "{}/{}",
            shown(provider.unwrap_or("?")),
            shown(model.unwrap_or("?"))
        )
    };
    let mut said = format!(
        "#{} {} at {} by {}{}",
        v.seq,
        v.verdict,
        short(&v.head),
        who(v),
        if v.superseded.is_empty() {
            ""
        } else {
            " (set aside)"
        }
    );
    let Some(evidence) = &v.evidence else {
        said.push_str(if v.human {
            "; the person's word"
        } else {
            "; no model evidence kept"
        });
        return said;
    };
    if let Some(configured) = &evidence.configured {
        said.push_str(&format!(
            "; configured {}",
            pair(configured.provider.as_deref(), configured.model.as_deref())
        ));
    }
    if let Some(observed) = &evidence.observed {
        let window = match observed.window.as_ref() {
            Some(window) => match (window.source, &window.previous) {
                (WindowSource::Stated, _) => "since the stated start".to_owned(),
                (WindowSource::PreviousVerdict, Some(previous)) => {
                    format!("since its verdict #{} (inferred)", previous.seq)
                }
                (WindowSource::PreviousVerdict, None) => {
                    "since its previous verdict (inferred)".to_owned()
                }
                (WindowSource::RecordStart, _) => "since its record started (inferred)".to_owned(),
                (WindowSource::Unknown, _) => "in a window of a kind not known here".to_owned(),
            },
            None => "with no window".to_owned(),
        };
        let others = |count: u64| match count {
            0 => String::new(),
            1 => "; 1 sample attributed to it from another session".to_owned(),
            n => format!("; {n} samples attributed to it from other sessions"),
        };
        match &observed.result {
            ObservedResult::Found(found) => {
                let pairs: Vec<String> = found
                    .pairs
                    .iter()
                    .map(|p| pair(p.provider.as_deref(), p.model.as_deref()))
                    .collect();
                said.push_str(&format!(
                    "; observed {window}: {}{} in {} samples of retained data{}",
                    pairs.join(", "),
                    if found.pairs_truncated { ", …" } else { "" },
                    found.samples,
                    others(found.other_sessions)
                ));
            }
            ObservedResult::NoneFound { other_sessions } => {
                said.push_str(&format!(
                    "; observed {window}: none of its own session in retained data{}",
                    others(*other_sessions)
                ));
            }
            ObservedResult::Unavailable { why } => {
                said.push_str(&format!("; observed: unavailable ({})", shown(why)));
            }
            ObservedResult::Unknown => {
                said.push_str("; observed: a result of a kind not known here");
            }
        }
        if !observed.incomplete.is_empty() {
            let reasons: Vec<&str> = observed.incomplete.iter().map(|r| r.said()).collect();
            said.push_str(&format!(" (may be incomplete: {})", reasons.join("; ")));
        }
    }
    said
}

/// Ask the gate. `Ok(true)` when the branch may land at that commit.
pub async fn gate(client: &Client, args: GateArgs) -> Result<bool> {
    let agent = sender::resolve(client, args.agent).await?;
    let (branch, head) = branch_and_head(&args.target, args.head.as_deref())?;
    // The role the daemon reads the gate for, as it takes it.
    let role = args
        .role
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .unwrap_or(GATE_ROLE)
        .to_owned();
    let request = Request::ReviewGate {
        project: project(client, args.project, agent.as_deref().unwrap_or(HUMAN)).await?,
        agent,
        branch: branch.clone(),
        head: head.clone(),
        role: args.role,
    };
    let Response::ReviewGate { gate, superseded } = client.call(&request).await? else {
        bail!("the daemon did not answer for the gate");
    };
    if args.publish {
        publish(&gate, &head)?;
    }
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "branch": branch, "head": head, "gate": gate, "superseded": superseded,
            }))?
        );
    } else {
        // What it said for the person, then the line a job gates on.
        println!("{}", said(&gate, &branch, &head));
        println!("{}", gate.line(&branch, &head));
        for line in set_aside(&superseded, &role) {
            println!("{line}");
        }
    }
    Ok(gate.approved())
}

/// How long `agentdocker brief` waits for GitHub's list of open pull
/// requests, and with `--all`, which pages through many more.
const PULLS_TIMEOUT: Duration = Duration::from_secs(10);
const PULLS_TIMEOUT_ALL: Duration = Duration::from_secs(120);
/// How many open pull requests `agentdocker brief` asks GitHub for, and
/// with `--all`. One more is asked for, to know when there are more.
const PULLS_ASKED: usize = 100;
const PULLS_ASKED_ALL: usize = 2000;
/// The least of the command's text the pull requests may have when the
/// brief and they do not both fit: a quarter of it.
const PULLS_SHARE: usize = agentdocker_core::brief::COMMAND_BYTES / 4;

/// An open pull request, as `gh pr list --json` gives it.
#[derive(Debug, PartialEq, serde::Deserialize)]
struct Pull {
    number: u64,
    #[serde(default)]
    title: String,
    #[serde(rename = "headRefName")]
    branch: String,
    #[serde(rename = "headRefOid")]
    head: String,
}

fn parse_pulls(json: &str) -> Result<Vec<Pull>> {
    serde_json::from_str(json).context("GitHub's list of pull requests does not parse")
}

/// The first `asked` of the pull requests GitHub listed, having been
/// asked for one more, and whether it had more than `asked`.
fn first_pulls(json: &str, asked: usize) -> Result<(Vec<Pull>, bool)> {
    let mut pulls = parse_pulls(json)?;
    let more = pulls.len() > asked;
    pulls.truncate(asked);
    Ok((pulls, more))
}

/// GitHub's open pull requests, as `agentdocker brief` tells them.
#[derive(Debug)]
pub struct Pulls {
    /// One line each, with what the gate says at its head.
    pub lines: Vec<String>,
    /// How many GitHub was asked for.
    pub asked: usize,
    /// GitHub had more open than were asked for: they are not here.
    pub more: bool,
    /// The checkout GitHub was asked from.
    pub repository: PathBuf,
}

/// The open pull requests of the repository checked out at `repository`
/// as GitHub lists them now (`gh`, run there), each with what the gate
/// says at its head, as `agentdocker gate` says it, asked of the daemon
/// for the same `project` as the brief. The first [`PULLS_ASKED`], or with
/// `all` the first [`PULLS_ASKED_ALL`], and whether GitHub has more.
/// `Err` says why GitHub could not be asked. A title is quoted, as every
/// record's text in a brief is.
pub async fn open_pulls(
    client: &Client,
    agent: Option<String>,
    project: Option<String>,
    repository: &Path,
    all: bool,
) -> Result<Pulls> {
    let (asked, timeout) = if all {
        (PULLS_ASKED_ALL, PULLS_TIMEOUT_ALL)
    } else {
        (PULLS_ASKED, PULLS_TIMEOUT)
    };
    let limit = (asked + 1).to_string();
    let argv: Vec<String> = [
        "gh",
        "pr",
        "list",
        "--state",
        "open",
        "--limit",
        &limit,
        "--json",
        "number,title,headRefName,headRefOid",
    ]
    .iter()
    .map(|&a| a.to_owned())
    .collect();
    let output = agentdocker_host::command::run(repository, &argv, timeout)
        .with_context(|| format!("cannot run gh in {}", repository.display()))?;
    ensure!(output.success, "gh failed: {}", output.text.trim());
    let (pulls, more) = first_pulls(&output.stdout, asked)?;
    let mut lines = Vec::new();
    for pull in pulls {
        let request = Request::ReviewGate {
            agent: agent.clone(),
            project: project.clone(),
            branch: pull.branch.clone(),
            head: pull.head.clone(),
            role: None,
        };
        let word = match client.call(&request).await {
            Ok(Response::ReviewGate { gate, .. }) => said(&gate, &pull.branch, &pull.head),
            Ok(other) => format!("the gate could not be read ({other:?})"),
            Err(error) => format!("the gate could not be read ({error:#})"),
        };
        let title = agentdocker_core::memory::headline(&pull.title);
        lines.push(format!(
            "- #{} {}: {}",
            pull.number,
            serde_json::to_string(&title).unwrap_or_default(),
            agentdocker_core::memory::headline(&word)
        ));
    }
    Ok(Pulls {
        lines,
        asked,
        more,
        repository: repository.to_owned(),
    })
}

/// `text` as one POSIX shell word meaning exactly it, whole: as it is when
/// it needs no quoting, else in single quotes, inside which the shell
/// expands nothing (`$(…)`, backticks, `$VAR` stay text), a quote in it
/// closed, escaped and reopened. None when it holds a control character
/// or any space but a plain one (a line break among them), which a command
/// told on one line cannot carry as itself.
pub(crate) fn shell_word(text: &str) -> Option<String> {
    if text.is_empty()
        || text
            .chars()
            .any(|c| c.is_control() || (c.is_whitespace() && c != ' '))
    {
        return None;
    }
    if text
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "-_./:@+=,".contains(c))
    {
        return Some(text.to_owned());
    }
    Some(format!("'{}'", text.replace('\'', "'\\''")))
}

/// The line that says which open pull requests are not told, when any are
/// not: `left` for room, and those GitHub was not asked for. The way to
/// the rest names the project (`whole`, [`agentdocker_core::brief::all_command`])
/// or the checkout GitHub was asked from, so it means the same wherever it
/// is run.
fn pulls_left_out(left: usize, pulls: &Pulls, whole: &str, all: bool) -> Option<String> {
    let mut parts = Vec::new();
    if left > 0 {
        parts.push(format!("{left} not shown for room"));
    }
    if pulls.more {
        parts.push(format!(
            "GitHub has more than the {} asked for",
            pulls.asked
        ));
    }
    if parts.is_empty() {
        return None;
    }
    let how = if all {
        let gh = "gh pr list --state open --limit <n>";
        // An absolute path only: `cd` would take a leading `-` for an option.
        let dir = Some(&pulls.repository).filter(|dir| dir.is_absolute());
        match dir.and_then(|dir| dir.to_str()).and_then(shell_word) {
            Some(dir) => format!("`cd {dir} && {gh}` lists the rest, without the gate's word"),
            // No command that would mean another place: the path, as data.
            None => format!(
                "`{gh}`, run in the checkout at {} (a path no shell word here holds exactly), \
                 lists the rest, without the gate's word",
                serde_json::to_string(&pulls.repository.to_string_lossy()).unwrap_or_default()
            ),
        }
    } else {
        format!("`{whole}` lists up to {PULLS_ASKED_ALL}")
    };
    Some(format!(
        "Open pull requests left out: {}; {how}.",
        parts.join("; ")
    ))
}

/// The pull requests' part of `agentdocker brief`'s text, within
/// `max_bytes`: its heading, its lines while they fit, in GitHub's order,
/// and a closing line counting the rest, room for which is kept at its
/// longest, offering `whole` for them. Why GitHub was not asked, when it
/// was not. Nothing when even the heading and closing line do not fit.
pub fn pulls_text(pulls: &Result<Pulls>, whole: &str, all: bool, max_bytes: usize) -> String {
    let pulls = match pulls {
        Err(error) => {
            let text = format!(
                "GitHub was not asked for pull requests: {}",
                serde_json::to_string(&agentdocker_core::memory::headline(&format!("{error:#}")))
                    .unwrap_or_default()
            );
            return if text.len() <= max_bytes {
                text
            } else {
                String::new()
            };
        }
        Ok(pulls) => pulls,
    };
    if pulls.lines.is_empty() && !pulls.more {
        let text = "No open pull requests on GitHub.";
        return if text.len() <= max_bytes {
            text.to_owned()
        } else {
            String::new()
        };
    }
    let reserve = pulls_left_out(pulls.lines.len(), pulls, whole, all).map_or(0, |l| l.len() + 1);
    let mut text = "Open pull requests on GitHub, asked now (titles quoted as written):".to_owned();
    if text.len() + reserve > max_bytes {
        return String::new();
    }
    let room = max_bytes - reserve;
    let mut told = 0;
    for line in &pulls.lines {
        if text.len() + 1 + line.len() > room {
            break;
        }
        text.push('\n');
        text.push_str(line);
        told += 1;
    }
    if let Some(line) = pulls_left_out(pulls.lines.len() - told, pulls, whole, all) {
        text.push('\n');
        text.push_str(&line);
    }
    text
}

/// `agentdocker brief`'s text: the brief, then GitHub's open pull requests,
/// within [`agentdocker_core::brief::COMMAND_BYTES`] in all unless `all`.
/// When both do not fit, the pull requests have the room the brief leaves,
/// and never less than a quarter of it; each part counts what it leaves.
pub fn brief_text(
    brief: &agentdocker_core::brief::Brief,
    pulls: &Result<Pulls>,
    now: chrono::DateTime<chrono::Utc>,
    all: bool,
) -> String {
    use agentdocker_core::brief::{COMMAND_BYTES, all_command, render};
    let command = all_command(brief);
    if all {
        return format!(
            "{}\n{}",
            render(brief, now, usize::MAX),
            pulls_text(pulls, &command, true, usize::MAX)
        );
    }
    let unbounded = render(brief, now, usize::MAX);
    let pulls_room = COMMAND_BYTES
        .saturating_sub(unbounded.len() + 1)
        .max(PULLS_SHARE);
    let pulls = pulls_text(pulls, &command, false, pulls_room);
    let brief = render(brief, now, COMMAND_BYTES.saturating_sub(pulls.len() + 1));
    match (brief.is_empty(), pulls.is_empty()) {
        (_, true) => brief,
        (true, false) => pulls,
        (false, false) => format!("{brief}\n{pulls}"),
    }
}

/// How long a hook's report may take: a commit waits for it.
const REPORT_TIMEOUT: Duration = Duration::from_secs(5);
/// A witness file is a few hundred bytes; past this it is not one.
const WITNESS_BYTES: u64 = 16 * 1024;

/// The report of the commit HEAD is at in the witness file's worktree:
/// HEAD and the full ref it names (none when detached), as git says now.
fn commit_report(witness: WitnessFile) -> Result<Request> {
    let git = |args: &[&str]| -> Result<agentdocker_host::command::Output> {
        let argv: Vec<String> = std::iter::once("git")
            .chain(args.iter().copied())
            .map(str::to_owned)
            .collect();
        agentdocker_host::command::run(&witness.checkout, &argv, TOOL_TIMEOUT)
            .context("cannot run git")
    };
    let head = git(&["rev-parse", "--verify", "--quiet", "HEAD^{commit}"])?;
    ensure!(head.success, "HEAD names no commit");
    let branch = git(&["symbolic-ref", "--quiet", "HEAD"])?;
    Ok(Request::ReportCommit {
        checkout: witness.checkout.clone(),
        head: head.stdout.trim().to_owned(),
        // `--quiet` makes a detached HEAD a failure that says nothing.
        branch: branch.success.then(|| branch.stdout.trim().to_owned()),
        token: witness.token,
        agent: None,
    })
}

/// `report-commit --witness <file>`: send the commit just made to the
/// daemon that created the worktree, at the socket the file names, never
/// starting one. Who made it is the daemon's to find.
pub async fn report_commit(witness: &Path) -> Result<Response> {
    use std::io::Read as _;
    let mut text = String::new();
    agentdocker_host::dirs::open_private(witness)
        .with_context(|| format!("cannot open {}", witness.display()))?
        .take(WITNESS_BYTES)
        .read_to_string(&mut text)?;
    let witness: WitnessFile =
        serde_json::from_str(&text).context("the witness file is not one")?;
    let client = Client::new(Some(witness.socket.clone())).with_start_timeout(None);
    let request = commit_report(witness)?;
    tokio::time::timeout(REPORT_TIMEOUT, client.call(&request))
        .await
        .context("the daemon did not answer in time")?
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::gate::Word;
    use agentdocker_core::{AgentId, ProjectId};

    /// The report names HEAD and its full ref, a tag of the branch's name
    /// notwithstanding, and no branch when HEAD is detached.
    #[test]
    fn a_report_names_head_and_its_full_ref() {
        let tmp = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(["-c", "user.email=t@example.invalid", "-c", "user.name=T"])
                .args(["-c", "commit.gpgsign=false"])
                .args(args)
                .current_dir(tmp.path())
                .output()
                .unwrap();
            assert!(output.status.success(), "git {args:?}: {output:?}");
            String::from_utf8_lossy(&output.stdout).trim().to_owned()
        };
        git(&["init", "-q", "-b", "feature/mine"]);
        git(&["commit", "-q", "--allow-empty", "-m", "first"]);
        git(&["tag", "feature/mine"]);
        let head = git(&["rev-parse", "HEAD"]);
        let witness = || WitnessFile {
            token: agentdocker_core::secret::SecretText::new("t".into()).unwrap(),
            socket: tmp.path().join("sock"),
            checkout: tmp.path().to_path_buf(),
        };
        let Request::ReportCommit {
            head: said,
            branch,
            agent,
            ..
        } = commit_report(witness()).unwrap()
        else {
            unreachable!()
        };
        assert_eq!(said, head);
        assert_eq!(branch.as_deref(), Some("refs/heads/feature/mine"));
        assert_eq!(agent, None, "who made it is the daemon's to find");
        git(&["checkout", "-q", "--detach"]);
        let Request::ReportCommit { branch, .. } = commit_report(witness()).unwrap() else {
            unreachable!()
        };
        assert_eq!(branch, None);
    }

    fn verdict(head: &str) -> Box<CodeVerdict> {
        Box::new(CodeVerdict {
            id: "1".into(),
            seq: 1,
            project: ProjectId::from("p"),
            branch: "b".into(),
            head: head.into(),
            from: None,
            by: AgentId::from("r"),
            by_name: "reviewer-1".into(),
            human: false,
            roles: vec!["reviewer".into()],
            runtime: "codex".into(),
            verdict: Word::Pass,
            findings: Findings {
                important: 0,
                nit: 1,
                pre_existing: 0,
            },
            note: String::new(),
            at: chrono::Utc::now(),
            findings_presence: agentdocker_core::evidence::FindingsPresence::Stated,
            evidence: None,
            superseded: Vec::new(),
        })
    }

    /// A page of receipts names the project and the full ref, says how each
    /// agent was named and who it was (escaped), and what the page cannot
    /// establish.
    #[test]
    fn receipts_say_who_wrote_them_and_what_they_cannot_establish() {
        use agentdocker_core::gate::*;
        let at = chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let receipt = |seq, basis, author| AuthorshipReceipt {
            seq,
            commit: "a".repeat(40),
            agent: AgentId::from("agent-1-id"),
            witness: "hook".into(),
            basis,
            author,
            at,
        };
        let author = AuthorSnapshot {
            name: "writ\u{202e}er".into(),
            runtime: "codex".into(),
            configured: Some(agentdocker_core::evidence::Configured {
                provider: Some("openai".into()),
                model: Some("model-a".into()),
            }),
            session: None,
        };
        let coverage = AuthorshipCoverage {
            retained: 20_000,
            capture_since: Some(at),
            pruned_through: Some(4),
            oldest_retained: Some(at),
            seed_skipped: 2,
            page_may_miss: true,
            unreceipted: Unreceipted::Unknown,
        };
        let said = receipts_said(
            &ProjectId::from("p"),
            "refs/heads/feature/x",
            &[
                receipt(5, Basis::HookSession, Some(author)),
                receipt(6, Basis::HookOwner, None),
            ],
            Some(6),
            &coverage,
        );
        for expected in [
            "refs/heads/feature/x in project p",
            "#5 aaaaaaaa by writ\\u{202e}er (codex, configured openai/model-a)",
            "reported from its own session",
            "reported unauthenticated; its worktree, not shown to be its commit",
            "agent agent-1-id (no snapshot kept)",
            "more after #6 (--after 6)",
            "do not by themselves identify a change's commits",
            "pruned receipts through #4",
            "2 rows were left out when receipts were first taken",
            "oldest retained receipt, on any ref, was kept at 2023-11-14 22:13:20 UTC",
            "this page may be missing receipts",
            "lack a receipt is not known",
        ] {
            assert!(said.contains(expected), "{expected:?} in {said}");
        }
        // Words a newer daemon wrote read as unknown and are said so; an
        // empty page past earlier receipts says it is this page that is.
        let newer: AuthorshipReceipt = serde_json::from_value(serde_json::json!({
            "seq": 9, "commit": "b".repeat(40), "agent": "agent-2", "witness": "hook",
            "basis": "attested", "at": at,
        }))
        .unwrap();
        let unknown: Unreceipted =
            serde_json::from_value(serde_json::json!({"state": "estimated", "count": 3})).unwrap();
        let later = AuthorshipCoverage {
            unreceipted: unknown,
            oldest_retained: None,
            ..coverage.clone()
        };
        let said = receipts_said(
            &ProjectId::from("p"),
            "refs/heads/feature/x",
            &[newer],
            None,
            &later,
        );
        assert!(said.contains("basis unknown"), "{said}");
        assert!(said.contains("lack a receipt is not known"), "{said}");
        assert!(said.contains("the project holds no receipt"), "{said}");
        let past = receipts_said(
            &ProjectId::from("p"),
            "refs/heads/feature/x",
            &[],
            None,
            &coverage,
        );
        assert!(past.contains("no receipts on this page"), "{past}");
    }

    /// A pull request number is read in the repository of the project read:
    /// the same number names another branch in another repository, and a
    /// project named by id must be this checkout's.
    #[cfg(unix)]
    #[test]
    fn a_pull_request_is_read_in_its_project_s_repository() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let gh = tmp.path().join("gh");
        std::fs::write(&gh, "#!/bin/sh\ncat \"$PWD/pr.json\"\n").unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        let gh = gh.to_str().unwrap();
        let mut seen = Vec::new();
        for (name, branch) in [("one", "feature/one"), ("two", "feature/two")] {
            let repo = tmp.path().join(name);
            std::fs::create_dir_all(&repo).unwrap();
            std::fs::write(
                repo.join("pr.json"),
                format!("{{\"headRefName\": \"{branch}\"}}"),
            )
            .unwrap();
            let (dir, project) = pr_repository(Some(repo.to_str().unwrap())).unwrap();
            assert_eq!(project, repo.to_str().unwrap());
            seen.push(pr_branch(gh, &dir, 7).unwrap());
        }
        assert_eq!(seen, vec!["feature/one", "feature/two"]);
        let error = pr_repository(Some("not-this-checkout"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("not not-this-checkout"), "{error}");
    }

    /// A start is a time or a duration ago; one too far back for a time
    /// to hold, or negative, is refused, never a panic.
    #[test]
    fn a_start_too_far_back_is_refused() {
        let ago = parse_since("90m").unwrap();
        assert!(ago < chrono::Utc::now());
        for far in [
            "1000000000d",
            "9223372036854775807s",
            "99999999999999999999d",
            "-5m",
        ] {
            assert!(parse_since(far).is_err(), "{far}");
        }
    }

    /// A round says which limitations apply, the usage attributed to the
    /// reviewer from other sessions, and the strings a source supplied
    /// with their direction marks made visible.
    #[test]
    fn a_round_says_what_its_evidence_can_and_cannot_establish() {
        use agentdocker_core::evidence::*;
        let mut v = *verdict(&"a".repeat(40));
        let at = chrono::Utc::now();
        v.evidence = Some(ModelEvidence {
            configured: Some(Configured {
                provider: Some("open\u{202e}ai".into()),
                model: Some("model-b".into()),
            }),
            session: Some("s1".into()),
            observed: Some(Observed {
                window: Some(Window {
                    from: at,
                    to: at,
                    source: WindowSource::PreviousVerdict,
                    previous: Some(Predecessor {
                        id: "v1".into(),
                        seq: 4,
                        boundary: at,
                    }),
                }),
                captured_at: at,
                retained_from: None,
                read_from: None,
                collector: Collector::default(),
                result: ObservedResult::NoneFound { other_sessions: 3 },
                incomplete: vec![Incomplete::BeforeRetention, Incomplete::CollectorOff],
            }),
        });
        v.by_name = "review\u{202e}er".into();
        let line = round(&v);
        assert!(line.contains("by review\\u{202e}er (codex"), "{line}");
        assert!(
            line.contains("configured open\\u{202e}ai/model-b"),
            "{line}"
        );
        assert!(line.contains("since its verdict #4 (inferred)"), "{line}");
        assert!(
            line.contains("none of its own session in retained data; 3 samples attributed to it from other sessions"),
            "{line}"
        );
        assert!(
            line.contains(
                "may be incomplete: the window starts before retention; collection was off"
            ),
            "{line}"
        );
    }

    #[test]
    fn a_verdict_names_a_branch_and_head_with_a_word_or_sets_one_aside() {
        #[derive(clap::Parser)]
        struct Cli {
            #[command(flatten)]
            args: VerdictArgs,
        }
        use clap::Parser as _;
        let parsed = |argv: &[&str]| {
            Cli::try_parse_from(std::iter::once("verdict").chain(argv.iter().copied()))
                .map(|cli| cli.args)
        };
        let changes = parsed(&["feature/x", "--head", "abc", "--changes", "fix the test"]).unwrap();
        assert_eq!(changes.branch.as_deref(), Some("feature/x"));
        assert!(changes.changes && changes.supersede.is_none());
        // The words, and the spellings kept for them.
        for (word, pass) in [
            ("--pass", true),
            ("--approve", true),
            ("--none", false),
            ("--comment", false),
        ] {
            let said = parsed(&["feature/x", "--head", "abc", word]).unwrap();
            assert_eq!((said.pass, said.none), (pass, !pass), "{word}");
        }
        let lifted = parsed(&["--supersede", "1", "--reason", "the reviewer is gone"]).unwrap();
        assert_eq!(lifted.supersede.as_deref(), Some("1"));
        assert_eq!(lifted.reason.as_deref(), Some("the reviewer is gone"));
        assert!(lifted.branch.is_none() && lifted.head.is_none() && lifted.note.is_none());
        assert!(parsed(&["--supersede", "1"]).unwrap().reason.is_none());
        for bad in [
            &["feature/x", "--head", "abc"][..],
            &["feature/x", "--head", "abc", "--approve", "--changes"],
            &["feature/x", "--changes"],
            &["--head", "abc", "--changes"],
            &["--supersede", "1", "--approve"],
            // A word after it would be taken for a branch, never a reason.
            &["--supersede", "1", "the reviewer is gone"],
            &["--supersede", "1", "--findings", "0,1,0"],
            &[
                "feature/x",
                "--head",
                "abc",
                "--changes",
                "--reason",
                "gone",
            ],
        ] {
            assert!(parsed(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn findings_are_three_counts() {
        assert_eq!(
            parse_findings("0, 2,1").unwrap(),
            Findings {
                important: 0,
                nit: 2,
                pre_existing: 1
            }
        );
        for bad in ["", "1,2", "1,2,3,4", "a,b,c", "-1,0,0"] {
            assert!(parse_findings(bad).is_err(), "{bad:?}");
        }
    }

    /// GitHub's list is read as `gh pr list --json` writes it; anything
    /// else is said not to parse.
    #[test]
    fn open_pull_requests_are_read_from_ghs_json() {
        let pulls = parse_pulls(
            r#"[{"number":540,"title":"A brief","headRefName":"claude/ad-87-brief","headRefOid":"f14b62b60a0b52af93c490f12a99eaa85578a7f5"}]"#,
        )
        .unwrap();
        assert_eq!(
            pulls,
            [Pull {
                number: 540,
                title: "A brief".into(),
                branch: "claude/ad-87-brief".into(),
                head: "f14b62b60a0b52af93c490f12a99eaa85578a7f5".into(),
            }]
        );
        assert!(parse_pulls("[]").unwrap().is_empty());
        assert!(parse_pulls("not json").is_err());
    }

    fn pulls_json(n: usize) -> String {
        let pulls: Vec<serde_json::Value> = (0..n)
            .map(|i| {
                serde_json::json!({
                    "number": i + 1,
                    "title": format!("change {i}"),
                    "headRefName": format!("b{i}"),
                    "headRefOid": "f".repeat(40),
                })
            })
            .collect();
        serde_json::to_string(&pulls).unwrap()
    }

    /// GitHub is asked for one more than is told: when it has more, the
    /// text says so and how to have the rest, never dropping them silently:
    /// the brief of the project it is of, and, past that, `gh` in the
    /// checkout it was asked from, quoted as a shell word.
    #[test]
    fn more_open_pull_requests_than_were_asked_for_are_said() {
        let (pulls, more) = first_pulls(&pulls_json(PULLS_ASKED + 1), PULLS_ASKED).unwrap();
        assert_eq!((pulls.len(), more), (PULLS_ASKED, true));
        let (pulls, more) = first_pulls(&pulls_json(60), PULLS_ASKED).unwrap();
        assert_eq!((pulls.len(), more), (60, false));

        let listed = |more: bool| -> Result<Pulls> {
            Ok(Pulls {
                lines: (0..3).map(|i| format!("- #{i} \"t\": approved")).collect(),
                asked: 3,
                more,
                repository: PathBuf::from("/work/the other's repo"),
            })
        };
        let whole = "agentdocker brief --all --project b0";
        let text = pulls_text(&listed(true), whole, false, usize::MAX);
        assert!(
            text.ends_with(
                "Open pull requests left out: GitHub has more than the 3 asked for; \
                 `agentdocker brief --all --project b0` lists up to 2000."
            ),
            "{text}"
        );
        let text = pulls_text(&listed(true), whole, true, usize::MAX);
        assert!(
            text.ends_with(
                "`cd '/work/the other'\\''s repo' && gh pr list --state open --limit <n>` \
                 lists the rest, without the gate's word."
            ),
            "{text}"
        );
        let text = pulls_text(&listed(false), whole, false, usize::MAX);
        assert_eq!(text.lines().count(), 4, "{text}");
        assert!(!text.contains("left out"), "{text}");
        let unasked: Result<Pulls> = Err(anyhow::anyhow!("gh failed:\nnot logged in"));
        assert_eq!(
            pulls_text(&unasked, whole, false, usize::MAX),
            "GitHub was not asked for pull requests: \"gh failed: not logged in\""
        );
    }

    /// The way past `--all` names the checkout as one exact shell word,
    /// however long and whatever shell syntax its name holds: run, it
    /// selects that very directory and expands nothing. A path no word can
    /// hold on one line (a line break) or a relative one gets no command,
    /// only the path, as data.
    #[cfg(unix)]
    #[test]
    fn the_way_past_all_is_an_exact_shell_word_or_no_command() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let odd = root.join("it's $(touch ran) `touch ran` $HOME");
        let long = odd.join("d".repeat(120)).join("e".repeat(120));
        std::fs::create_dir_all(&long).unwrap();
        assert!(long.as_os_str().len() > 200);
        for dir in [&odd, &long] {
            let word = shell_word(dir.to_str().unwrap()).unwrap();
            let out = std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("cd {word} && pwd -P"))
                .current_dir(&root)
                .output()
                .unwrap();
            assert!(out.status.success(), "{word}: {out:?}");
            assert_eq!(
                String::from_utf8_lossy(&out.stdout).trim_end_matches('\n'),
                dir.to_str().unwrap()
            );
        }
        assert!(!root.join("ran").exists(), "nothing was substituted");
        let line = |repository: &str| {
            let pulls = Pulls {
                lines: vec!["- #1 \"t\": approved".into()],
                asked: 1,
                more: true,
                repository: PathBuf::from(repository),
            };
            pulls_left_out(0, &pulls, "agentdocker brief --all", true).unwrap()
        };
        let long = long.to_str().unwrap();
        let told = line(long);
        assert!(
            told.contains(&format!("`cd {} && gh pr list", shell_word(long).unwrap())),
            "{told}"
        );
        let broken = line("/work/a\n$(touch ran)");
        assert!(!broken.contains("cd "), "{broken}");
        assert!(
            broken.contains(r#"run in the checkout at "/work/a\n$(touch ran)""#),
            "{broken}"
        );
        assert!(!line("work/relative").contains("cd "));
        assert_eq!(shell_word("/work/a\u{2028}b"), None);
    }

    /// The command's text, the brief and the pull requests together, stays
    /// within its bound however long the titles are in bytes, and each part
    /// counts what it leaves; `--all` tells every one.
    #[test]
    fn the_command_text_stays_within_its_bound() {
        use agentdocker_core::brief::{
            Brief, COMMAND_BYTES, CheckoutLine, PULL_REQUESTS, Reader, Standing,
        };
        let now = chrono::Utc::now();
        let checkouts = (0..200)
            .map(|i| CheckoutLine {
                path: PathBuf::from(format!("/work/checkout-{i}")),
                at: now,
                standing: Some(Standing {
                    branch: Some(format!("topic-{i}")),
                    head: Some("a".repeat(40)),
                    dirty: vec!["a.rs".into(), "b.rs".into()],
                    ..Standing::default()
                }),
                error: None,
                gate: None,
                yours: false,
            })
            .collect();
        let brief = Brief {
            project: "orchard".into(),
            reader: Reader::Person,
            groups: Vec::new(),
            held_live: Vec::new(),
            checkpoints: Vec::new(),
            questions: Vec::new(),
            leases: Vec::new(),
            checkouts,
            checkouts_clean: 0,
            checkouts_unread: 0,
            not_covered: vec![PULL_REQUESTS.into()],
            repository: None,
            project_id: Some("b0".into()),
        };
        let title: String = "\u{1F680}".repeat(80);
        let lines: Vec<String> = (0..50)
            .map(|i| {
                format!(
                    "- #{i} {}: unreviewed: nobody who counts has passed b{i}",
                    serde_json::to_string(&agentdocker_core::memory::headline(&title)).unwrap()
                )
            })
            .collect();
        let pulls = || -> Result<Pulls> {
            Ok(Pulls {
                lines: lines.clone(),
                asked: PULLS_ASKED,
                more: false,
                repository: PathBuf::from("/work/b"),
            })
        };
        assert!(pulls_text(&pulls(), "", false, usize::MAX).len() > COMMAND_BYTES);
        let text = brief_text(&brief, &pulls(), now, false);
        assert!(text.len() <= COMMAND_BYTES, "{}", text.len());
        assert!(
            text.contains("; `agentdocker brief --all --project b0` shows them."),
            "the brief counts, and names the project for the person"
        );
        let pulls_part = &text[text.find("Open pull requests on GitHub").unwrap()..];
        assert!(pulls_part.len() >= PULLS_SHARE / 2, "{}", pulls_part.len());
        let told = pulls_part.lines().filter(|l| l.starts_with("- #")).count();
        assert!(told > 0 && told < 50, "{told}");
        assert!(
            pulls_part.ends_with(&format!(
                "Open pull requests left out: {} not shown for room; \
                 `agentdocker brief --all --project b0` lists up to 2000.",
                50 - told
            )),
            "{pulls_part}"
        );
        let all = brief_text(&brief, &pulls(), now, true);
        assert_eq!(all.lines().filter(|l| l.starts_with("- #")).count(), 50);
        assert!(!all.contains("left out"), "{:?}", all.lines().last());
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
        // The status and the line carry the counts and the runtime.
        assert_eq!(
            status(&approved).1,
            "pass by reviewer-1 (codex; important 0, nit 1, pre-existing 0)"
        );
        assert_eq!(
            said(&approved, "b", &head),
            "approved: b at aaaaaaaa by reviewer-1 (codex; important 0, nit 1, pre-existing 0)"
        );
        let stale = Gate::Stale {
            verdict: verdict(&head),
        };
        assert_eq!(status(&stale).0, "pending");
        assert!(said(&stale, "b", &other).contains("aaaaaaaa..bbbbbbbb is unreviewed"));
        let mut changes = verdict(&head);
        changes.verdict = Word::Changes;
        let changes = Gate::Changes { verdict: changes };
        assert_eq!(status(&changes).0, "failure");
        assert!(
            status(&changes)
                .1
                .starts_with("changes by reviewer-1 (codex;")
        );
        assert_eq!(status(&Gate::Missing).0, "pending");
        assert!(status(&approved).1.chars().count() <= 140);
        // A verdict kept before runtimes were says so rather than nothing.
        let mut older = verdict(&head);
        older.runtime = String::new();
        assert!(
            status(&Gate::Approved { verdict: older })
                .1
                .contains("(unknown;")
        );
    }

    #[test]
    fn the_gate_says_which_verdict_was_set_aside_and_by_whom() {
        let head = "a".repeat(40);
        let mut lifted = *verdict(&head);
        lifted.verdict = Word::Changes;
        lifted.superseded = vec![agentdocker_core::gate::Supersession {
            by: AgentId::from("s"),
            by_name: "reviewer-2".into(),
            with: Some("2".into()),
            role: Some(GATE_ROLE.into()),
            reason: SupersedeReason::ReviewerEnded,
            note: String::new(),
            at: chrono::Utc::now(),
        }];
        assert_eq!(
            set_aside(std::slice::from_ref(&lifted), GATE_ROLE),
            [
                "superseded: changes by reviewer-1 at aaaaaaaa (verdict 1): reviewer-1 has left; reviewer-2 passed since"
            ]
        );
        // Said only at the gate it was lifted for.
        assert!(set_aside(std::slice::from_ref(&lifted), "tester").is_empty());
        // The person's word, at every gate.
        lifted.superseded = vec![agentdocker_core::gate::Supersession {
            by: AgentId::from("user"),
            by_name: "user".into(),
            with: None,
            role: None,
            reason: SupersedeReason::Person,
            note: "the reviewer is gone".into(),
            at: chrono::Utc::now(),
        }];
        assert_eq!(
            set_aside(&[lifted], "tester"),
            ["superseded: changes by reviewer-1 at aaaaaaaa (verdict 1): set aside by user"]
        );
    }
}
