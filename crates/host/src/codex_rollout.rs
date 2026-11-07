//! A Codex thread's rollout file under its profile, read for what its first
//! record says of the thread: nothing but that record is read, and nothing
//! is written. Codex keeps one JSONL file per thread under
//! `<profile>/sessions/<year>/<month>/<day>/rollout-<time>-<thread>.jsonl`
//! and moves finished ones to `archived_sessions`; the thread id is the
//! file name's tail, so the file is found by name, newest days first, and
//! its `session_meta` is checked to be that thread's.

use std::fs;
use std::io::{self, BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use agentdocker_core::codex_thread::{self, ThreadMeta};

/// The directories a profile keeps rollouts in.
const ROLLOUT_DIRS: [&str; 2] = ["sessions", "archived_sessions"];
/// A rollout is at most this deep under one of them (year, month, day).
const MAX_DEPTH: usize = 4;
/// At most this many directory entries are looked at for one thread.
const MAX_ENTRIES: usize = 50_000;
/// The first record is read up to this many bytes; a longer one is not
/// `session_meta`.
const MAX_FIRST_RECORD: u64 = 256 * 1024;

/// Whether `thread` can be a Codex thread id in a file name: hexadecimal
/// digits and hyphens, as Codex writes them, never a path.
pub fn is_thread_id(thread: &str) -> bool {
    (8..=64).contains(&thread.len()) && thread.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

/// The rollout file of `thread` under `profile`, if one exists where Codex
/// keeps them. Newest days are looked at first. `None` when no file is
/// named for it within the bounds.
pub fn locate(profile: &Path, thread: &str) -> io::Result<Option<PathBuf>> {
    if !is_thread_id(thread) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a Codex thread id",
        ));
    }
    let suffix = format!("-{thread}.jsonl");
    let mut budget = MAX_ENTRIES;
    for dir in ROLLOUT_DIRS {
        let root = profile.join(dir);
        if !root.is_dir() {
            continue;
        }
        if let Some(found) = find(&root, &suffix, 0, &mut budget)? {
            return Ok(Some(found));
        }
    }
    Ok(None)
}

fn find(dir: &Path, suffix: &str, depth: usize, budget: &mut usize) -> io::Result<Option<PathBuf>> {
    let mut entries: Vec<fs::DirEntry> = match fs::read_dir(dir) {
        Ok(entries) => entries.collect::<io::Result<_>>()?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    // Year, month and day directories sort by name; the newest come first.
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.file_name()));
    for entry in entries {
        if *budget == 0 {
            return Ok(None);
        }
        *budget -= 1;
        let kind = entry.file_type()?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if kind.is_file() && name.starts_with("rollout-") && name.ends_with(suffix) {
            return Ok(Some(entry.path()));
        }
        if kind.is_dir() && depth + 1 < MAX_DEPTH {
            if let Some(found) = find(&entry.path(), suffix, depth + 1, budget)? {
                return Ok(Some(found));
            }
        }
    }
    Ok(None)
}

/// What a rollout's first record says of its thread; `None` when the file
/// does not begin with a `session_meta` record this build can read.
pub fn thread_meta(path: &Path) -> io::Result<Option<ThreadMeta>> {
    let file = crate::files::open_regular(path)?;
    let mut line = Vec::new();
    BufReader::new(file.take(MAX_FIRST_RECORD)).read_until(b'\n', &mut line)?;
    if line.last() != Some(&b'\n') {
        // Not yet a whole record, or too long to be the one wanted.
        return Ok(None);
    }
    let Ok(record) = serde_json::from_slice::<serde_json::Value>(&line) else {
        return Ok(None);
    };
    Ok(codex_thread::session_meta(&record))
}

/// What `thread`'s rollout under `profile` says of it, when the file exists
/// and its first record is that thread's.
pub fn read_thread(profile: &Path, thread: &str) -> io::Result<Option<ThreadMeta>> {
    let Some(path) = locate(profile, thread)? else {
        return Ok(None);
    };
    Ok(thread_meta(&path)?.filter(|meta| meta.thread == thread))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = "01a0a0a0-0000-7000-8000-00000000000a";
    const CHILD: &str = "01a0a0a0-0000-7000-8000-00000000000b";

    fn write(profile: &Path, dir: &str, day: &str, thread: &str, first: &str) -> PathBuf {
        let folder = profile.join(dir).join(day);
        fs::create_dir_all(&folder).unwrap();
        let path = folder.join(format!("rollout-2026-10-20T09-00-00-{thread}.jsonl"));
        fs::write(
            &path,
            format!("{first}\n{{\"type\":\"turn_context\",\"payload\":{{}}}}\n"),
        )
        .unwrap();
        path
    }

    #[test]
    fn a_threads_rollout_is_found_by_name_and_read_for_its_meta_alone() {
        let temp = tempfile::TempDir::new().unwrap();
        let profile = temp.path();
        write(
            profile,
            "sessions",
            "2026/10/19",
            ROOT,
            &format!(
                r#"{{"type":"session_meta","payload":{{"id":"{ROOT}","cli_version":"0.164.0","source":"cli"}}}}"#
            ),
        );
        write(
            profile,
            "archived_sessions",
            "2026/10/20",
            CHILD,
            &format!(
                r#"{{"type":"session_meta","payload":{{"id":"{CHILD}","cli_version":"0.164.0","source":{{"subagent":{{"thread_spawn":{{"parent_thread_id":"{ROOT}","depth":1,"agent_nickname":"Quill","agent_role":"reviewer"}}}}}}}}}}"#
            ),
        );
        let root = read_thread(profile, ROOT).unwrap().unwrap();
        assert_eq!(root.thread, ROOT);
        assert!(!root.is_child());
        let child = read_thread(profile, CHILD).unwrap().unwrap();
        assert_eq!(child.parent_thread.as_deref(), Some(ROOT));
        assert_eq!(child.role.as_deref(), Some("reviewer"));
        assert_eq!(
            read_thread(profile, "01a0a0a0-0000-7000-8000-00000000000c").unwrap(),
            None
        );
        assert!(read_thread(profile, "../etc/passwd").is_err());
        assert!(read_thread(profile, "not a thread").is_err());
    }

    #[test]
    fn a_file_named_for_another_thread_or_without_a_whole_first_record_says_nothing() {
        let temp = tempfile::TempDir::new().unwrap();
        let profile = temp.path();
        // Named for the child, but its meta is the root's.
        write(
            profile,
            "sessions",
            "2026/10/20",
            CHILD,
            &format!(
                r#"{{"type":"session_meta","payload":{{"id":"{ROOT}","cli_version":"0.164.0"}}}}"#
            ),
        );
        assert_eq!(read_thread(profile, CHILD).unwrap(), None);
        // A first record still being written.
        let folder = profile.join("sessions").join("2026/10/21");
        fs::create_dir_all(&folder).unwrap();
        fs::write(
            folder.join(format!("rollout-2026-10-21T09-00-00-{ROOT}.jsonl")),
            r#"{"type":"session_meta","payload":{"id":"#,
        )
        .unwrap();
        assert_eq!(read_thread(profile, ROOT).unwrap(), None);
        // No rollout directories at all.
        let empty = tempfile::TempDir::new().unwrap();
        assert_eq!(read_thread(empty.path(), ROOT).unwrap(), None);
    }
}
