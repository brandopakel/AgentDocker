//! The git pre-commit backstop.
//!
//! The hooks adapters refuse an edit over a path another agent holds, but
//! only where an adapter runs (Claude Code hooks, the OpenCode plugin,
//! Codex hooks). An agent with none — Gemini CLI, a shell script, a person
//! — can commit over a held path. `agentdocker setup --git-hook` puts a
//! marked block in the repository's `pre-commit` hook that pipes the staged
//! paths to `agentdocker hook pre-commit`, which refuses the commit when a
//! staged path is held exclusively by another live agent, with the
//! holder's name and note. The committer's own leases pass. A daemon that
//! cannot be reached lets the commit through and says so on stderr: a
//! backstop must never block the person when the daemon is down, and with
//! no daemon there are no leases to protect.
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use agentdocker_core::runtime::Wiring;
use agentdocker_core::{AgentId, AgentRecord, Lease, LeaseMode, Request, ResourceKey, Response};
use agentdocker_host::runtimes::shell::{BEGIN, END, current_block, without_block};
use anyhow::{Context, Result};

use crate::client::Backend;

/// What the block's first line says it is for; what tells our block from
/// the shell block's, which uses the same markers in a different file.
const PURPOSE: &str = "refuse a commit over paths other agents hold (agentdocker setup --git-hook)";

/// How long the hook waits for the daemon, all its questions together,
/// before letting the commit go.
const DAEMON_WAIT: Duration = Duration::from_secs(3);

/// Where this repository's hooks live: what `git rev-parse --git-path
/// hooks` says, which honours `core.hooksPath` and a linked worktree's
/// common directory, or the common git directory's `hooks` when git
/// itself cannot be asked.
pub fn hooks_dir(repo: &Path) -> Option<PathBuf> {
    let asked = std::process::Command::new("git")
        .args(["rev-parse", "--git-path", "hooks"])
        .current_dir(repo)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
        .map(|text| {
            let path = PathBuf::from(text);
            if path.is_absolute() {
                path
            } else {
                repo.join(path)
            }
        });
    asked.or_else(|| agentdocker_host::vcs::git_dirs(repo).map(|(_, common)| common.join("hooks")))
}

/// The `pre-commit` hook of the repository containing `repo`.
pub fn hook_path(repo: &Path) -> Option<PathBuf> {
    hooks_dir(repo).map(|dir| dir.join("pre-commit"))
}

/// The marked block: POSIX sh, so it runs under whatever `sh` git uses.
/// The executable that planned it is named, with `agentdocker` on `PATH`
/// as the fallback; neither there, the check is skipped and said so.
pub fn block(executable: &Path) -> String {
    let quoted = shlex::try_quote(&executable.to_string_lossy())
        .map(|quoted| quoted.into_owned())
        .unwrap_or_else(|_| "agentdocker".to_owned());
    format!(
        "{BEGIN} {PURPOSE}\n\
         AGENTDOCKER={quoted}\n\
         [ -x \"$AGENTDOCKER\" ] || AGENTDOCKER=\"$(command -v agentdocker 2>/dev/null || true)\"\n\
         if [ -n \"$AGENTDOCKER\" ]; then\n\
         \x20   git diff --cached --name-only -z --no-renames | \"$AGENTDOCKER\" hook pre-commit || exit $?\n\
         else\n\
         \x20   echo \"agentdocker: not found; the lease check was skipped and the commit goes through\" >&2\n\
         fi\n\
         {END}\n"
    )
}

/// The shells a hook's `#!` line may name for the block to be put in it.
const SHELLS: &[&str] = &["sh", "bash", "dash", "ash", "ksh", "mksh", "zsh"];

/// Whether a hook's first line runs it with a POSIX shell: `#!/bin/sh`,
/// `#!/usr/bin/env bash` and the like. A file with no `#!` line is one.
fn runs_in_a_shell(text: &str) -> bool {
    let Some(line) = text.lines().next().and_then(|line| line.strip_prefix("#!")) else {
        return true;
    };
    let mut words = line.split_whitespace();
    let program = words
        .next()
        .and_then(|path| path.rsplit('/').next())
        .unwrap_or_default();
    let program = if program == "env" {
        words
            .find(|word| !word.starts_with('-'))
            .unwrap_or_default()
    } else {
        program
    };
    SHELLS.contains(&program)
}

/// The hook file as the plan leaves it: our block replaced in place when
/// an older one is there; put first in an existing shell hook otherwise —
/// right after its `#!` line, so a hook that ends with `exec` or `exit`
/// still runs it, and taking it out leaves the hook as it was; and a new
/// `sh` script when there is no hook. `Err` names the first line of a hook
/// that is not a shell script, which the block cannot go into.
pub fn with_block(before: Option<&str>, executable: &Path) -> Result<String, String> {
    let block = block(executable);
    let Some(text) = before else {
        return Ok(format!("#!/bin/sh\n{block}"));
    };
    if let Some(found) = current_block(text) {
        return Ok(text.replacen(found, &block, 1));
    }
    if !runs_in_a_shell(text) {
        return Err(text.lines().next().unwrap_or_default().to_owned());
    }
    Ok(
        match text.strip_prefix("#!").and_then(|_| text.split_once('\n')) {
            Some((shebang, rest)) => format!("{shebang}\n{block}{rest}"),
            // A `#!` line with nothing after it, or no `#!` line at all.
            None if text.starts_with("#!") => format!("{text}\n{block}"),
            None => format!("{block}{text}"),
        },
    )
}

/// The hook with our block taken out; `None` when the file would be only
/// the shebang we wrote, which means the whole file was ours and goes.
pub fn without(text: &str) -> Option<Option<String>> {
    let rest = without_block(text)?;
    Some((rest.trim() != "#!/bin/sh").then_some(rest))
}

/// Whether a hook's block is ours: the shell block uses the same markers,
/// so the purpose line decides, not the markers alone.
pub fn has_block(text: &str) -> bool {
    current_block(text).is_some_and(|found| found.starts_with(&format!("{BEGIN} {PURPOSE}")))
}

/// Whether the repository's hook carries the block this executable plans,
/// in a file git will run: the right bytes in a hook without the execute
/// bit are skipped by git, so they are not wired.
pub fn wiring(path: &Path, executable: &Path) -> Wiring {
    match std::fs::read_to_string(path) {
        Ok(text) => match current_block(&text) {
            Some(found) if found == block(executable) && runnable(path) => Wiring::Wired,
            Some(_) if has_block(&text) => Wiring::Unverified,
            Some(_) | None => Wiring::Missing,
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Wiring::Missing,
        Err(_) => Wiring::Unverified,
    }
}

/// Whether git would run the file: on Unix, an execute bit; elsewhere git
/// decides by itself.
fn runnable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        true
    }
}

/// One staged path another agent holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// As git named it: relative to the working tree.
    pub path: PathBuf,
    pub lease: Lease,
    /// The holder's name, or its short id when it has none.
    pub holder: String,
}

/// What the check decided.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Nothing staged is held by anyone else: the commit goes through.
    Clear,
    /// These staged paths are held by other live agents: refuse.
    Refused(Vec<Refusal>),
    /// The daemon could not be asked: the commit goes through, and why.
    Skipped(String),
}

/// The staged paths that are held exclusively by a live agent other than
/// `me`. Leases on a directory cover the files beneath it; a lease held
/// by an agent that is no longer live is nobody's.
pub fn refusals(
    root: &Path,
    staged: &[PathBuf],
    leases: &[Lease],
    agents: &[AgentRecord],
    me: Option<&AgentId>,
) -> Vec<Refusal> {
    let mut refused = Vec::new();
    for path in staged {
        let absolute = agentdocker_host::project::canonical(&root.join(path));
        let key = ResourceKey::new(format!("path:{}", absolute.display()));
        for lease in leases {
            if lease.mode != LeaseMode::Exclusive
                || me.is_some_and(|me| *me == lease.holder)
                || !lease.resource.overlaps(&key)
            {
                continue;
            }
            let Some(holder) = agents.iter().find(|agent| agent.id == lease.holder) else {
                continue;
            };
            let name = if holder.spec.name.is_empty() {
                holder.id.short().to_owned()
            } else {
                holder.spec.name.clone()
            };
            refused.push(Refusal {
                path: path.clone(),
                lease: lease.clone(),
                holder: name,
            });
        }
    }
    refused
}

/// Ask the daemon what it holds under the repository and decide. `me` is
/// how the committer is known — an agent id, prefix or name — when it is.
pub async fn check<B: Backend>(
    backend: &B,
    root: &Path,
    staged: &[PathBuf],
    me: Option<&str>,
    deadline: tokio::time::Instant,
) -> Verdict {
    if staged.is_empty() {
        return Verdict::Clear;
    }
    let root = agentdocker_host::project::canonical(root);
    let leases = match tokio::time::timeout_at(
        deadline,
        backend.call(Request::Leases {
            agent: None,
            resource: Some(format!("path:{}", root.display())),
        }),
    )
    .await
    {
        Ok(Ok(Response::Leases { leases })) => leases,
        Ok(Ok(Response::Error { message, .. })) => {
            return Verdict::Skipped(format!("the daemon refused the lease query: {message}"));
        }
        Ok(Ok(other)) => {
            return Verdict::Skipped(format!("unexpected answer to the lease query: {other:?}"));
        }
        Ok(Err(error)) => {
            return Verdict::Skipped(format!("the daemon is not reachable: {error:#}"));
        }
        Err(_) => {
            return Verdict::Skipped(format!(
                "the daemon did not answer within {} s",
                DAEMON_WAIT.as_secs()
            ));
        }
    };
    if leases.is_empty() {
        return Verdict::Clear;
    }
    let agents = match tokio::time::timeout_at(
        deadline,
        backend.call(Request::List {
            all: false,
            project: None,
            labels: Default::default(),
        }),
    )
    .await
    {
        Ok(Ok(Response::Agents { agents, .. })) => agents,
        Ok(Ok(Response::Error { message, .. })) => {
            return Verdict::Skipped(format!("the daemon refused the agent list: {message}"));
        }
        Ok(Ok(other)) => {
            return Verdict::Skipped(format!("unexpected answer to the agent list: {other:?}"));
        }
        Ok(Err(error)) => {
            return Verdict::Skipped(format!("the daemon is not reachable: {error:#}"));
        }
        Err(_) => {
            return Verdict::Skipped(format!(
                "the daemon did not answer within {} s",
                DAEMON_WAIT.as_secs()
            ));
        }
    };
    let me = me.and_then(|reference| {
        agents
            .iter()
            .find(|agent| {
                agent.id.as_str() == reference
                    || agent.spec.name == reference
                    || (reference.len() >= 4 && agent.id.as_str().starts_with(reference))
            })
            .map(|agent| agent.id.clone())
    });
    let refused = refusals(&root, staged, &leases, &agents, me.as_ref());
    if refused.is_empty() {
        Verdict::Clear
    } else {
        Verdict::Refused(refused)
    }
}

/// What the committer reads on stderr, and the hook's exit status.
pub fn report(verdict: &Verdict, root: &Path) -> (String, i32) {
    match verdict {
        Verdict::Clear => (String::new(), 0),
        Verdict::Skipped(why) => (
            format!(
                "agentdocker: {why}; the lease check was skipped and the commit goes through\n"
            ),
            0,
        ),
        Verdict::Refused(refused) => {
            let mut text = format!(
                "agentdocker: commit refused: {} staged path{} held by another agent\n",
                refused.len(),
                if refused.len() == 1 { " is" } else { "s are" }
            );
            for refusal in refused {
                text.push_str(&format!(
                    "  {} — {} ({}), {} lease, ends {}",
                    // A name may hold a newline: shown escaped, on its line.
                    refusal.path.display().to_string().escape_debug(),
                    refusal.holder,
                    refusal.lease.holder.short(),
                    refusal.lease.mode,
                    crate::format::until(refusal.lease.expires_at)
                ));
                if let Some(note) = &refusal.lease.note {
                    text.push_str(&format!(": \"{note}\""));
                }
                text.push('\n');
            }
            let first = &refused[0];
            let resource = format!(
                "path:{}",
                agentdocker_host::project::canonical(&root.join(&first.path)).display()
            );
            let resource = shlex::try_quote(&resource)
                .map(|quoted| quoted.into_owned())
                .unwrap_or(resource);
            text.push_str(&format!(
                "Ask the holder to let go: agentdocker request-release {resource} --reason \"...\" \
                 (they answer with respond-release and you get a release_response message); or wait \
                 for the lease to expire. `git commit --no-verify` commits over it anyway.\n"
            ));
            (text, 1)
        }
    }
}

/// The staged paths as `git diff --name-only -z` writes them: each name
/// ends at a NUL, and every other byte, a newline included, is part of it.
/// On Unix the bytes are the name exactly, whatever their encoding.
pub fn staged_paths(bytes: &[u8]) -> Vec<PathBuf> {
    bytes
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
        .map(path_of)
        .collect()
}

#[cfg(unix)]
fn path_of(name: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    PathBuf::from(std::ffi::OsStr::from_bytes(name))
}

/// Git writes names as UTF-8 where the platform's are not bytes.
#[cfg(not(unix))]
fn path_of(name: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(name).into_owned())
}

/// `agentdocker hook pre-commit`: the staged paths come NUL-separated on
/// stdin (`git diff --cached --name-only -z`), the working tree is the
/// current directory (git runs the hook there), and the committer is the
/// session's agent when `AGENTDOCKER_AGENT_ID` or the process ancestry
/// says so. Never starts a daemon.
pub async fn run(client: crate::client::Client) -> Result<i32> {
    let client = client.with_start_timeout(None);
    let mut bytes = Vec::new();
    std::io::stdin()
        .read_to_end(&mut bytes)
        .context("cannot read the staged paths from stdin")?;
    let staged = staged_paths(&bytes);
    let root = std::env::current_dir().context("cannot read the working directory")?;
    // One deadline for everything the hook asks: the committer, the
    // leases, the agents.
    let deadline = tokio::time::Instant::now() + DAEMON_WAIT;
    let me = match std::env::var("AGENTDOCKER_AGENT_ID") {
        Ok(id) if !id.is_empty() => Some(id),
        _ => match tokio::time::timeout_at(deadline, crate::sender::resolve(&client, None)).await {
            Ok(Ok(found)) => found,
            // No session to speak of, or one that could not be named:
            // the committer's own leases then do not pass, which is the
            // safe side of unknown.
            Ok(Err(_)) | Err(_) => None,
        },
    };
    let verdict = check(&client, &root, &staged, me.as_deref(), deadline).await;
    let (text, status) = report(&verdict, &root);
    eprint!("{text}");
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::mock::Mock;
    use agentdocker_core::{AgentSpec, AgentStatus, LeaseId};
    use chrono::Utc;

    fn later() -> tokio::time::Instant {
        tokio::time::Instant::now() + DAEMON_WAIT
    }

    fn agent(name: &str) -> AgentRecord {
        let mut record = AgentRecord::new(
            AgentSpec {
                name: name.to_owned(),
                ..AgentSpec::default()
            },
            false,
            Utc::now(),
        );
        record.status = AgentStatus::Running;
        record
    }

    fn lease(holder: &AgentId, resource: &str, mode: LeaseMode, note: Option<&str>) -> Lease {
        Lease {
            id: LeaseId::generate(),
            resource: ResourceKey::new(resource),
            holder: holder.clone(),
            mode,
            acquired_at: Utc::now(),
            held_since: None,
            actor: None,
            change_seq: None,
            expires_at: Utc::now() + chrono::Duration::minutes(5),
            note: note.map(str::to_owned),
            amount: 0,
            automatic: false,
        }
    }

    /// A staged path under another live agent's exclusive lease is
    /// refused with its holder and note; the committer's own leases, a
    /// shared lease, a lease outside the staged paths and a lease of an
    /// agent no longer live all pass; a directory lease covers the files
    /// beneath it.
    #[tokio::test]
    async fn refuses_only_what_another_live_agent_holds_exclusively() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("src/deep")).unwrap();
        std::fs::write(root.join("src/lib.rs"), "").unwrap();
        std::fs::write(root.join("src/deep/mod.rs"), "").unwrap();
        std::fs::write(root.join("README.md"), "").unwrap();
        std::fs::write(root.join("mine.rs"), "").unwrap();
        std::fs::write(root.join("shared.rs"), "").unwrap();
        std::fs::write(root.join("gone.rs"), "").unwrap();
        let reviewer = agent("Claude reviewer");
        let me = agent("me");
        let departed = agent("departed");
        let path = |relative: &str| format!("path:{}", root.join(relative).display());
        let leases = vec![
            lease(
                &reviewer.id,
                &path("src"),
                LeaseMode::Exclusive,
                Some("refactoring the parser"),
            ),
            lease(&me.id, &path("mine.rs"), LeaseMode::Exclusive, None),
            lease(&reviewer.id, &path("shared.rs"), LeaseMode::Shared, None),
            lease(&departed.id, &path("gone.rs"), LeaseMode::Exclusive, None),
        ];
        let staged: Vec<PathBuf> = [
            "src/lib.rs",
            "src/deep/mod.rs",
            "README.md",
            "mine.rs",
            "shared.rs",
            "gone.rs",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect();
        let backend = Mock::with(vec![
            Response::Leases {
                leases: leases.clone(),
            },
            Response::Agents {
                agents: vec![reviewer.clone(), me.clone()],
                aliases: Default::default(),
            },
        ]);
        let verdict = check(
            &backend,
            &root,
            &staged,
            Some(&me.id.as_str()[..8]),
            later(),
        )
        .await;
        let Verdict::Refused(refused) = &verdict else {
            panic!("{verdict:?}");
        };
        assert_eq!(
            refused.iter().map(|r| r.path.as_path()).collect::<Vec<_>>(),
            [Path::new("src/lib.rs"), Path::new("src/deep/mod.rs")]
        );
        assert!(refused.iter().all(|r| r.holder == "Claude reviewer"));
        let requests = backend.requests();
        assert!(matches!(
            &requests[0],
            Request::Leases { agent: None, resource: Some(resource) } if *resource == path("")
                .trim_end_matches('/')
        ));
        let (text, status) = report(&verdict, &root);
        assert_eq!(status, 1);
        assert!(text.starts_with(
            "agentdocker: commit refused: 2 staged paths are held by another agent\n"
        ));
        assert!(text.contains("src/lib.rs — Claude reviewer ("), "{text}");
        assert!(text.contains("\"refactoring the parser\""), "{text}");
        assert!(
            text.contains(&format!(
                "agentdocker request-release {}",
                path("src/lib.rs")
            )),
            "{text}"
        );
        assert!(text.contains("--no-verify"));

        // The same leases, with the committer unknown: its own lease
        // counts against it, which is the safe side of not knowing.
        let backend = Mock::with(vec![
            Response::Leases { leases },
            Response::Agents {
                agents: vec![reviewer, me],
                aliases: Default::default(),
            },
        ]);
        let Verdict::Refused(refused) = check(&backend, &root, &staged, None, later()).await else {
            panic!("refused");
        };
        assert!(refused.iter().any(|r| r.path == Path::new("mine.rs")));
    }

    /// `-z` ends a name at a NUL and nowhere else: a newline is part of the
    /// name, so a file named across two lines is one path, and an exact
    /// lease on it is found; on Unix a name that is not UTF-8 keeps its
    /// bytes. The refusal shows such a name escaped, on one line.
    #[test]
    fn a_staged_name_ends_only_at_nul() {
        let staged = staged_paths(b"a.rs\0line\nbreak.txt\0dir/c d.rs\0\0");
        assert_eq!(
            staged,
            [
                PathBuf::from("a.rs"),
                PathBuf::from("line\nbreak.txt"),
                PathBuf::from("dir/c d.rs")
            ]
        );
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let raw = staged_paths(b"caf\xe9.txt\0");
            assert_eq!(raw[0].as_os_str().as_bytes(), b"caf\xe9.txt");
        }

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::write(root.join("line\nbreak.txt"), "").unwrap();
        let holder = agent("holder");
        let held = lease(
            &holder.id,
            &format!("path:{}", root.join("line\nbreak.txt").display()),
            LeaseMode::Exclusive,
            None,
        );
        let refused = refusals(&root, &staged, &[held], &[holder], None);
        assert_eq!(refused.len(), 1, "{refused:?}");
        assert_eq!(refused[0].path, PathBuf::from("line\nbreak.txt"));
        let (text, status) = report(&Verdict::Refused(refused), &root);
        assert_eq!(status, 1);
        assert!(
            text.lines()
                .nth(1)
                .is_some_and(|line| line.starts_with("  line\\nbreak.txt — holder (")),
            "the path is one line, escaped: {text}"
        );
    }

    /// Nothing held under the repository, or nothing staged, is clear
    /// without a second question; a daemon that cannot be reached is a
    /// skip that lets the commit through and says why, exit zero.
    #[tokio::test]
    async fn clear_when_nothing_is_held_and_open_when_the_daemon_is_away() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let backend = Mock::with(vec![Response::Leases { leases: vec![] }]);
        assert_eq!(
            check(&backend, &root, &[PathBuf::from("a.rs")], None, later()).await,
            Verdict::Clear
        );
        assert_eq!(backend.requests().len(), 1, "no agent list for nothing");
        let backend = Mock::default();
        assert_eq!(
            check(&backend, &root, &[], None, later()).await,
            Verdict::Clear
        );
        assert!(backend.requests().is_empty());

        struct Down;
        impl Backend for Down {
            async fn call(&self, _: Request) -> Result<Response> {
                Err(anyhow::anyhow!("connection refused"))
            }
        }
        let verdict = check(&Down, &root, &[PathBuf::from("a.rs")], None, later()).await;
        assert!(matches!(&verdict, Verdict::Skipped(why) if why.contains("connection refused")));
        let (text, status) = report(&verdict, &root);
        assert_eq!(status, 0);
        assert!(text.contains("the lease check was skipped and the commit goes through"));
        assert_eq!(report(&Verdict::Clear, &root), (String::new(), 0));
    }

    /// The block is a new `sh` hook when there is none, is appended after
    /// a blank line to a hook the person already has, replaces an older
    /// block in place, is recognised as wired only when it is this
    /// executable's, and is taken out leaving the person's hook — or
    /// nothing, when the file was ours.
    #[test]
    fn the_block_is_added_once_recognised_and_taken_out() {
        let exe = Path::new("/opt/agent docker/bin/agentdocker");
        let fresh = with_block(None, exe).unwrap();
        assert!(
            fresh.starts_with("#!/bin/sh\n# >>> agentdocker >>> refuse a commit"),
            "{fresh}"
        );
        assert!(fresh.contains("AGENTDOCKER='/opt/agent docker/bin/agentdocker'\n"));
        assert!(fresh.contains("| \"$AGENTDOCKER\" hook pre-commit || exit $?"));
        assert!(fresh.ends_with("# <<< agentdocker <<<\n"));
        assert!(
            fresh.lines().all(|line| !line.contains("bash")),
            "POSIX sh only"
        );
        assert!(has_block(&fresh));
        assert_eq!(without(&fresh), Some(None), "the whole file was ours");

        // First, after the `#!` line: a hook that ends in `exec` still runs it.
        let theirs = "#!/bin/sh\nnpm test\nexec git diff-index --check --cached HEAD --";
        let added = with_block(Some(theirs), exe).unwrap();
        assert!(
            added.starts_with("#!/bin/sh\n# >>> agentdocker >>>"),
            "{added}"
        );
        assert!(added.ends_with(
            "# <<< agentdocker <<<\nnpm test\nexec git diff-index --check --cached HEAD --"
        ));
        assert_eq!(with_block(Some(&added), exe).unwrap(), added, "added once");
        let newer = with_block(Some(&added), Path::new("/usr/local/bin/agentdocker")).unwrap();
        assert!(newer.contains("AGENTDOCKER=/usr/local/bin/agentdocker\n"));
        assert!(
            !newer.contains("agent docker"),
            "the older block is replaced"
        );
        assert_eq!(
            without(&added),
            Some(Some(theirs.to_owned())),
            "their hook stays, to the byte"
        );
        assert_eq!(without(theirs), None);
        // Any POSIX shell takes it, through `env` too; a hook with no `#!`
        // line is a shell script; one in another language is refused.
        for shell in [
            "#!/usr/bin/env bash",
            "#!/bin/zsh -e",
            "#!/usr/bin/env -S dash",
        ] {
            let hook = format!("{shell}\nmake lint\n");
            let added = with_block(Some(&hook), exe).unwrap();
            assert!(
                added.starts_with(&format!("{shell}\n# >>> agentdocker >>>")),
                "{added}"
            );
            assert_eq!(without(&added), Some(Some(hook)));
        }
        let bare = with_block(Some("make lint\n"), exe).unwrap();
        assert!(bare.starts_with("# >>> agentdocker >>>") && bare.ends_with("<<<\nmake lint\n"));
        assert_eq!(
            with_block(Some("#!/usr/bin/env python3\nprint('hi')\n"), exe),
            Err("#!/usr/bin/env python3".to_owned())
        );
        assert_eq!(
            with_block(Some("#!/usr/bin/env fish\necho hi\n"), exe),
            Err("#!/usr/bin/env fish".to_owned())
        );

        let shell_block = agentdocker_host::runtimes::shell::with_block(None, "zsh");
        assert!(!has_block(&shell_block), "the shell block is not ours");

        let tmp = tempfile::tempdir().unwrap();
        let hook = tmp.path().join("pre-commit");
        assert_eq!(wiring(&hook, exe), Wiring::Missing);
        let added = with_block(Some(theirs), exe).unwrap();
        std::fs::write(&hook, &added).unwrap();
        assert_eq!(
            wiring(&hook, exe),
            Wiring::Unverified,
            "the right bytes in a file git skips"
        );
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert_eq!(wiring(&hook, exe), Wiring::Wired);
        assert_eq!(
            wiring(&hook, Path::new("/usr/local/bin/agentdocker")),
            Wiring::Unverified,
            "another executable's block"
        );
        std::fs::write(&hook, theirs).unwrap();
        assert_eq!(wiring(&hook, exe), Wiring::Missing);
    }

    /// The hook path is the repository's hooks directory, which a linked
    /// worktree shares with its main checkout.
    #[test]
    fn the_hook_lives_in_the_repositorys_hooks_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(repo.join(".git/hooks")).unwrap();
        std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        let expected = repo.canonicalize().unwrap().join(".git/hooks/pre-commit");
        let found = hook_path(&repo).expect("a hook path");
        assert_eq!(
            found.canonicalize().unwrap_or(found.clone()),
            expected.canonicalize().unwrap_or(expected)
        );
        assert!(hook_path(tmp.path()).is_none(), "not a repository");
    }
}
