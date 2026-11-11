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
    /// Findings by severity, as `important,nit,pre-existing` counts (`0,1,0`; all zero when absent).
    #[arg(long, value_parser = parse_findings, default_value = "0,0,0")]
    findings: Findings,
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
        conflicts_with_all = ["branch", "head", "from", "pass", "changes", "none", "findings", "note"]
    )]
    supersede: Option<String>,
    /// Why the verdict is set aside (with --supersede).
    // Its own conflicts too: clap waives a `requires` when anything the
    // required argument conflicts with is given.
    #[arg(
        long,
        requires = "supersede",
        conflicts_with_all = ["branch", "head", "from", "pass", "changes", "none", "findings", "note"]
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
/// The project a verdict or a gate names: the one given, else for the
/// person (by the `user` name or by their record's id, as
/// `AGENTDOCKER_AGENT_ID=$(agentdocker me)` sets it) this checkout's;
/// an agent's own the daemon knows.
async fn project(client: &Client, named: Option<String>, agent: &str) -> Result<Option<String>> {
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
fn who(v: &CodeVerdict) -> String {
    format!(
        "{} ({}; {})",
        v.by_name,
        v.runtime_said(),
        v.findings.said()
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
        findings: args.findings,
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
            superseded: Vec::new(),
        })
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
        assert!(
            parsed(&["feature/x", "--head", "abc", "--approve"])
                .unwrap()
                .approve
        );
        assert!(
            parsed(&["feature/x", "--head", "abc", "--comment"])
                .unwrap()
                .comment
        );
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
        lifted.verdict = Verdict::Changes;
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
