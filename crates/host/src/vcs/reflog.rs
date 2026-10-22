//! What git recorded a checkout's HEAD doing, entry by entry: its own HEAD
//! reflog (`logs/HEAD` in the checkout's git directory, one per worktree),
//! replayed from where the last reading stopped.
//!
//! Two heads seen a moment apart cannot say what happened between them: a
//! commit followed by a switch back reads as no move at all, and a
//! fast-forward of one commit has the old head as its parent just as a
//! commit made there does. The reflog names each operation, in order.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Bytes of reflog read at most in one replay. A longer stretch of new
/// entries is read over several replays; a first look or a rewritten
/// reflog reads at most this much of its end.
const READ_LIMIT: u64 = 8 * 1024 * 1024;

/// How far a checkout's HEAD reflog has been read: where the next entry
/// starts, how many entries came before it, the last one read and the
/// branch HEAD was on after it. A reflog rewritten under it — expired,
/// pruned, cut short — is told by that last entry no longer ending where it
/// did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReflogCheckpoint {
    /// Bytes read: where the next entry starts.
    pub offset: u64,
    /// Entries read so far.
    pub entries: u64,
    /// The SHA-256 of the last entry read (its line, without the newline);
    /// empty before any.
    #[serde(default)]
    pub last: String,
    /// That line's length in bytes, newline included.
    #[serde(default)]
    pub last_len: u64,
    /// Its time, seconds since the epoch, as git wrote it.
    #[serde(default)]
    pub at: i64,
    /// The branch HEAD was on after it, or none when detached.
    #[serde(default)]
    pub branch: Option<String>,
}

/// A commit git recorded as made in the checkout with `git commit`, on the
/// branch HEAD was on when it was made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReflogCommit {
    pub commit: String,
    pub branch: String,
    /// When it was made, seconds since the epoch, as git wrote it.
    pub at: i64,
}

/// What one replay read: where reading stopped and the commits made since
/// the checkpoint it started from, oldest first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Replayed {
    pub checkpoint: ReflogCheckpoint,
    pub commits: Vec<ReflogCommit>,
}

/// Replay `dir`'s HEAD reflog from `from`. Without a checkpoint this is a
/// first look: it starts where the reflog ends now, on the branch HEAD is
/// on, and nothing before it is replayed. A directory that is no longer a
/// checkout, or a reflog git has not written (none yet, or turned off), has
/// nothing to replay. `Err` when the reflog is there and cannot be read:
/// the caller keeps its checkpoint and asks again, since a reading that
/// failed proves nothing.
pub fn replay(dir: &Path, from: Option<&ReflogCheckpoint>) -> Result<Replayed, String> {
    let Some((gitdir, common)) = super::git_dirs(dir) else {
        return Ok(unchanged(from));
    };
    let is_branch =
        |name: &str| super::resolve_ref(&common, &format!("refs/heads/{name}")).is_some();
    let path = gitdir.join("logs").join("HEAD");
    let file = match crate::files::open_regular(&path) {
        Ok(file) => Some(file),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let Some(mut file) = file else {
        // Never written, or removed: what comes next starts from nothing.
        let mut checkpoint = from.cloned().unwrap_or_else(|| ReflogCheckpoint {
            branch: super::state(dir).and_then(|s| s.branch),
            ..ReflogCheckpoint::default()
        });
        checkpoint.offset = 0;
        checkpoint.last = String::new();
        checkpoint.last_len = 0;
        return Ok(Replayed {
            checkpoint,
            commits: Vec::new(),
        });
    };
    let failed = |error: std::io::Error| format!("{}: {error}", path.display());
    let len = file.metadata().map_err(failed)?.len();
    let Some(from) = from else {
        // A first look: the end of what is there now.
        let start = len.saturating_sub(64 * 1024);
        let bytes = read_range(&mut file, start, len).map_err(failed)?;
        let mut checkpoint = ReflogCheckpoint {
            branch: super::state(dir).and_then(|s| s.branch),
            ..ReflogCheckpoint::default()
        };
        let lines = complete_lines(&bytes, start, start == 0);
        if let Some(last) = lines.last() {
            checkpoint.offset = last.end;
            checkpoint.last = digest(last.raw);
            checkpoint.last_len = last.end - last.start;
            checkpoint.at = parse(&last.text).map_or(0, |entry| entry.at);
        }
        checkpoint.entries = lines.len() as u64;
        return Ok(Replayed {
            checkpoint,
            commits: Vec::new(),
        });
    };
    // Where the last entry read should still be, unchanged.
    let start = from.offset.saturating_sub(from.last_len);
    let continues = len >= from.offset && {
        let bytes = read_range(&mut file, start, from.offset).map_err(failed)?;
        let lines = complete_lines(&bytes, start, true);
        (from.last.is_empty() && from.offset == 0)
            || (lines.len() == 1
                && lines[0].end == from.offset
                && digest(lines[0].raw) == from.last)
    };
    let (bytes, base, after) = if continues {
        let end = len.min(from.offset.saturating_add(READ_LIMIT));
        let bytes = read_range(&mut file, from.offset, end).map_err(failed)?;
        (bytes, from.offset, None)
    } else {
        // Rewritten under the checkpoint: read the end, and replay from just
        // after the last entry read if it is still there, else from the
        // entries written no earlier than it.
        let start = len.saturating_sub(READ_LIMIT);
        let bytes = read_range(&mut file, start, len).map_err(failed)?;
        (bytes, start, Some(from))
    };
    let mut lines = complete_lines(&bytes, base, continues || base == 0);
    if let Some(from) = after {
        match lines.iter().rposition(|line| digest(line.raw) == from.last) {
            Some(index) if !from.last.is_empty() => {
                lines.drain(..=index);
            }
            _ => lines.retain(|line| parse(&line.text).is_some_and(|entry| entry.at >= from.at)),
        }
    }
    let mut checkpoint = from.clone();
    let mut commits = Vec::new();
    for line in &lines {
        checkpoint.offset = line.end;
        checkpoint.last = digest(line.raw);
        checkpoint.last_len = line.end - line.start;
        checkpoint.entries += 1;
        let Some(entry) = parse(&line.text) else {
            continue;
        };
        checkpoint.at = entry.at;
        if made_by_commit(entry.message)
            && let Some(branch) = &checkpoint.branch
        {
            commits.push(ReflogCommit {
                commit: entry.new.to_owned(),
                branch: branch.clone(),
                at: entry.at,
            });
        }
        checkpoint.branch = branch_after(checkpoint.branch.take(), entry.message, &is_branch);
    }
    if !continues && lines.is_empty() {
        // Nothing new after a rewrite: the checkpoint is where the reflog
        // ends now.
        if let Some(last) = complete_lines(&bytes, base, base == 0).last() {
            checkpoint.offset = last.end;
            checkpoint.last = digest(last.raw);
            checkpoint.last_len = last.end - last.start;
        } else {
            checkpoint.offset = 0;
            checkpoint.last = String::new();
            checkpoint.last_len = 0;
        }
    }
    Ok(Replayed {
        checkpoint,
        commits,
    })
}

/// Nothing to replay: the checkpoint as it was, or an empty one.
fn unchanged(from: Option<&ReflogCheckpoint>) -> Replayed {
    Replayed {
        checkpoint: from.cloned().unwrap_or_default(),
        commits: Vec::new(),
    }
}

/// Whether an entry records a commit made with `git commit`: `commit: …`,
/// `commit (amend): …`, `commit (merge): …`, `commit (initial): …`. A pull,
/// a merge git concludes by itself, a reset, a cherry-pick or a rebase
/// writes something else.
fn made_by_commit(message: &str) -> bool {
    message.starts_with("commit:") || message.starts_with("commit (")
}

/// The branch HEAD is on after an entry, from the one it was on before:
/// `checkout: moving from X to Y` goes to Y when Y is a branch (else HEAD
/// is detached), a rebase detaches at its start and returns to its branch
/// at its finish, and `Branch: renamed` follows the branch HEAD is on.
fn branch_after(
    branch: Option<String>,
    message: &str,
    is_branch: &impl Fn(&str) -> bool,
) -> Option<String> {
    if let Some(moving) = message.strip_prefix("checkout: moving from ") {
        let to = moving.rsplit_once(" to ").map(|(_, to)| to.trim())?;
        return is_branch(to).then(|| to.to_owned());
    }
    if let Some((_, to)) = message.split_once("(finish): returning to refs/heads/") {
        return Some(to.trim().to_owned()).filter(|to| !to.is_empty());
    }
    if message
        .split_once(": ")
        .is_some_and(|(operation, _)| operation.ends_with("(start)"))
    {
        return None;
    }
    if let Some(renamed) = message.strip_prefix("Branch: renamed refs/heads/")
        && let Some((old, new)) = renamed.split_once(" to refs/heads/")
        && branch.as_deref() == Some(old.trim())
    {
        return Some(new.trim().to_owned());
    }
    branch
}

/// One reflog entry: `<old> <new> <name> <<email>> <time> <zone>\t<message>`.
struct Entry<'a> {
    new: &'a str,
    at: i64,
    message: &'a str,
}

fn parse(line: &str) -> Option<Entry<'_>> {
    let (head, message) = line.split_once('\t').unwrap_or((line, ""));
    let mut words = head.split(' ');
    let _old = words.next()?;
    let new = words.next()?;
    if !super::valid_oid(new) {
        return None;
    }
    let mut tail = head.rsplitn(3, ' ');
    let _zone = tail.next()?;
    let at = tail.next()?.parse().ok()?;
    Some(Entry { new, at, message })
}

/// A complete line of the reflog: its bytes, its text (a message git
/// wrote in another encoding is read as far as it is UTF-8), and where it
/// starts and ends in the file, the newline included.
struct Line<'a> {
    raw: &'a [u8],
    text: std::borrow::Cow<'a, str>,
    start: u64,
    end: u64,
}

/// The complete lines in `bytes`, read from `base` in the file. A line
/// still being written, with no newline yet, is left for the next replay.
/// Unless reading began where a line begins (`aligned`), the first line,
/// which the start may have cut, is skipped.
fn complete_lines(bytes: &[u8], base: u64, aligned: bool) -> Vec<Line<'_>> {
    let mut lines = Vec::new();
    let mut start = 0usize;
    let mut skip = !aligned;
    for (index, byte) in bytes.iter().enumerate() {
        if *byte != b'\n' {
            continue;
        }
        if !std::mem::take(&mut skip) {
            let raw = &bytes[start..index];
            lines.push(Line {
                raw,
                text: String::from_utf8_lossy(raw),
                start: base + start as u64,
                end: base + index as u64 + 1,
            });
        }
        start = index + 1;
    }
    lines
}

fn digest(raw: &[u8]) -> String {
    format!("{:x}", Sha256::digest(raw))
}

fn read_range(file: &mut std::fs::File, start: u64, end: u64) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    if end <= start {
        return Ok(bytes);
    }
    file.seek(SeekFrom::Start(start))?;
    file.take(end - start).read_to_end(&mut bytes)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_is_read_whatever_the_name_says() {
        let a = "a".repeat(40);
        let b = "b".repeat(40);
        let line = format!("{a} {b} A Name With  Spaces <a@b> 1792632891 -0700\tcommit: x");
        let entry = parse(&line).unwrap();
        assert_eq!(
            (entry.new, entry.at, entry.message),
            (b.as_str(), 1792632891, "commit: x")
        );
        // The entry `git worktree add` writes has no message.
        let line = format!("{a} {b} t <t@e> 1792632891 -0700");
        assert_eq!(parse(&line).unwrap().message, "");
        assert!(parse("not a reflog line").is_none());
    }

    #[test]
    fn the_branch_follows_switches_rebases_and_renames() {
        let branches = |name: &str| ["main", "feature/x", "side"].contains(&name);
        let on = |branch: &str| Some(branch.to_owned());
        assert_eq!(
            branch_after(
                on("main"),
                "checkout: moving from main to feature/x",
                &branches
            ),
            on("feature/x")
        );
        // Detached: a commit, a tag, `HEAD`.
        for to in ["4a91ed40ae204879f71821119224808a833a10c3", "v1.0", "HEAD"] {
            let message = format!("checkout: moving from main to {to}");
            assert_eq!(branch_after(on("main"), &message, &branches), None);
        }
        assert_eq!(
            branch_after(on("side"), "rebase (start): checkout main", &branches),
            None
        );
        assert_eq!(
            branch_after(
                None,
                "rebase (finish): returning to refs/heads/side",
                &branches
            ),
            on("side")
        );
        assert_eq!(
            branch_after(
                None,
                "pull --rebase (finish): returning to refs/heads/side",
                &branches
            ),
            on("side")
        );
        assert_eq!(
            branch_after(
                on("main"),
                "Branch: renamed refs/heads/main to refs/heads/trunk",
                &branches
            ),
            on("trunk")
        );
        assert_eq!(
            branch_after(
                on("side"),
                "Branch: renamed refs/heads/main to refs/heads/trunk",
                &branches
            ),
            on("side")
        );
        for unchanged in [
            "commit: x",
            "pull: Fast-forward",
            "reset: moving to HEAD~1",
            "",
        ] {
            assert_eq!(branch_after(on("main"), unchanged, &branches), on("main"));
        }
        assert!(made_by_commit("commit (amend): x") && made_by_commit("commit: x"));
        assert!(!made_by_commit("cherry-pick: x") && !made_by_commit("merge up: Fast-forward"));
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "init.defaultBranch=main",
            ])
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn have_git() -> bool {
        std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
    }

    fn repository() -> (tempfile::TempDir, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("repo");
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q"]);
        git(&dir, &["commit", "-q", "--allow-empty", "-m", "root"]);
        (tmp, dir)
    }

    fn commits(replayed: &Replayed) -> Vec<(&str, &str)> {
        replayed
            .commits
            .iter()
            .map(|c| (c.commit.as_str(), c.branch.as_str()))
            .collect()
    }

    /// A first look replays nothing; after it, every commit is found on
    /// the branch it was made on, even one followed by a switch back to
    /// where HEAD was, and a fast-forward is not one.
    #[test]
    fn every_commit_made_since_the_last_reading_is_found_on_its_branch() {
        if !have_git() {
            return;
        }
        let (_tmp, dir) = repository();
        let root = git(&dir, &["rev-parse", "HEAD"]);
        git(&dir, &["branch", "feature"]);
        git(&dir, &["branch", "review"]);
        let first = replay(&dir, None).unwrap();
        assert!(first.commits.is_empty(), "history is not replayed");
        assert_eq!(first.checkpoint.branch.as_deref(), Some("main"));

        // A commit on feature, then back to review at the same head as
        // before: no head moved, and the commit is still found.
        git(&dir, &["checkout", "-q", "feature"]);
        git(&dir, &["commit", "-q", "--allow-empty", "-m", "mine"]);
        let mine = git(&dir, &["rev-parse", "HEAD"]);
        git(&dir, &["checkout", "-q", "review"]);
        assert_eq!(git(&dir, &["rev-parse", "HEAD"]), root);
        let second = replay(&dir, Some(&first.checkpoint)).unwrap();
        assert_eq!(commits(&second), vec![(mine.as_str(), "feature")]);
        assert_eq!(second.checkpoint.branch.as_deref(), Some("review"));
        assert_eq!(second.checkpoint.entries, first.checkpoint.entries + 3);
        // Read once: nothing more from the same place.
        let again = replay(&dir, Some(&second.checkpoint)).unwrap();
        assert_eq!(
            again,
            Replayed {
                checkpoint: second.checkpoint.clone(),
                commits: Vec::new()
            }
        );

        // A commit, then a fast-forward onto somebody else's commit in the
        // same interval: the commit made here is the one found.
        git(&dir, &["checkout", "-q", "-b", "theirs"]);
        git(&dir, &["commit", "-q", "--allow-empty", "-m", "theirs"]);
        let theirs = git(&dir, &["rev-parse", "HEAD"]);
        git(&dir, &["checkout", "-q", "review"]);
        let third = replay(&dir, Some(&again.checkpoint)).unwrap();
        git(&dir, &["checkout", "-q", "-b", "local", &root]);
        git(&dir, &["commit", "-q", "--allow-empty", "-m", "b"]);
        let b = git(&dir, &["rev-parse", "HEAD"]);
        git(&dir, &["reset", "-q", "--hard", &root]);
        git(&dir, &["merge", "-q", "--ff-only", &theirs]);
        let fourth = replay(&dir, Some(&third.checkpoint)).unwrap();
        assert_eq!(commits(&fourth), vec![(b.as_str(), "local")]);
        assert_eq!(
            git(&dir, &["rev-parse", "HEAD"]),
            theirs,
            "the head is not what was made"
        );
    }

    /// A line still being written waits; an expired reflog is noticed, and
    /// what was written after the last entry read is still replayed.
    #[test]
    fn a_partial_line_waits_and_a_rewritten_reflog_is_followed() {
        if !have_git() {
            return;
        }
        let (_tmp, dir) = repository();
        let log = dir.join(".git/logs/HEAD");
        let first = replay(&dir, None).unwrap();
        let text = std::fs::read_to_string(&log).unwrap();
        let line = text.lines().last().unwrap().to_owned();
        let partial = line[..20].to_owned();
        std::fs::write(&log, format!("{text}{partial}")).unwrap();
        let waited = replay(&dir, Some(&first.checkpoint)).unwrap();
        assert_eq!(waited.checkpoint, first.checkpoint, "nothing complete yet");
        std::fs::write(&log, &text).unwrap();

        git(&dir, &["commit", "-q", "--allow-empty", "-m", "one"]);
        let one = replay(&dir, Some(&first.checkpoint)).unwrap();
        assert_eq!(one.commits.len(), 1);
        // Every entry expired, then a commit: the checkpoint's entry is
        // gone, and the commit made after it is still found.
        git(&dir, &["reflog", "expire", "--expire=all", "--all"]);
        git(&dir, &["commit", "-q", "--allow-empty", "-m", "two"]);
        let two = git(&dir, &["rev-parse", "HEAD"]);
        let after = replay(&dir, Some(&one.checkpoint)).unwrap();
        assert_eq!(commits(&after), vec![(two.as_str(), "main")]);
        let settled = replay(&dir, Some(&after.checkpoint)).unwrap();
        assert!(settled.commits.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_reflog_that_cannot_be_read_says_so_and_a_missing_one_has_nothing() {
        use std::os::unix::fs::PermissionsExt;
        if !have_git() {
            return;
        }
        let (tmp, dir) = repository();
        let first = replay(&dir, None).unwrap();
        let log = dir.join(".git/logs/HEAD");
        std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::File::open(&log).is_err() {
            assert!(replay(&dir, Some(&first.checkpoint)).is_err());
        }
        std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::remove_file(&log).unwrap();
        let gone = replay(&dir, Some(&first.checkpoint)).unwrap();
        assert!(gone.commits.is_empty());
        assert_eq!(gone.checkpoint.offset, 0);
        // Not a checkout at all: nothing to replay.
        let plain = tmp.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        assert_eq!(
            replay(&plain, None).unwrap().checkpoint,
            ReflogCheckpoint::default()
        );
    }
}
