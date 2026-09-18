//! Incremental recovery for a head whose proof has left the fast tail window.
//! Keep only source identity and an offset, never transcript/message contents.
use agentdocker_core::{AgentRecord, Envelope};
use agentdocker_host::{dirs, lock};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::MetadataExt,
    path::Path,
};

const WINDOW: u64 = super::TAIL_BYTES;
const OVERLAP: u64 = WINDOW / 2;

pub(super) fn tail(path: &Path) -> Result<Option<String>> {
    let mut source = dirs::read_private_file(path)?;
    let before = source.metadata()?;
    let text = window(
        &mut source,
        before.len().saturating_sub(WINDOW),
        before.len(),
    )?;
    unchanged(path, &before)?;
    Ok(text)
}

fn unchanged(path: &Path, before: &std::fs::Metadata) -> Result<()> {
    let after = std::fs::symlink_metadata(path)?;
    ensure!(
        after.is_file()
            && after.dev() == before.dev()
            && after.ino() == before.ino()
            && after.len() >= before.len(),
        "channel transcript changed during receipt recovery"
    );
    Ok(())
}

fn window(source: &mut std::fs::File, offset: u64, length: u64) -> Result<Option<String>> {
    let boundary = if offset == 0 {
        true
    } else {
        source.seek(SeekFrom::Start(offset - 1))?;
        let mut byte = [0];
        source.read_exact(&mut byte)?;
        byte[0] == b'\n'
    };
    source.seek(SeekFrom::Start(offset))?;
    let mut bytes = Vec::new();
    source
        .take(WINDOW.min(length.saturating_sub(offset)))
        .read_to_end(&mut bytes)?;
    let first = if boundary {
        Some(0)
    } else {
        bytes.iter().position(|b| *b == b'\n').map(|i| i + 1)
    };
    let last = bytes.iter().rposition(|b| *b == b'\n');
    Ok(match (first, last) {
        (Some(first), Some(last)) if first <= last => std::str::from_utf8(&bytes[first..last])
            .ok()
            .map(str::to_owned),
        _ => None,
    })
}

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Cursor {
    generation: Option<chrono::DateTime<chrono::Utc>>,
    session: Option<String>,
    head: String,
    path_hash: String,
    device: u64,
    inode: u64,
    observed_len: u64,
    offset: u64,
}

pub(super) fn find(
    home: &Path,
    transcript: &Path,
    agent: &AgentRecord,
    message: &Envelope,
    consumed: impl FnOnce(&str) -> bool,
) -> Result<bool> {
    let mut source = dirs::read_private_file(transcript)?;
    let before = source.metadata()?;
    let directory = home.join("channel-receipts");
    dirs::ensure_private_dir(&directory)?;
    let key = format!("{:x}", Sha256::digest(agent.id.as_str().as_bytes()));
    let lock_path = directory.join(format!("{key}.lock"));
    dirs::private_file(&lock_path, true, false)?;
    let Some(_guard) = lock::try_exclusive_existing(&lock_path)? else {
        return Ok(false);
    };
    let mut state = dirs::private_file(&directory.join(format!("{key}.json")), true, false)?;
    let mut bytes = Vec::new();
    (&mut state).take(16 * 1024).read_to_end(&mut bytes)?;
    let mut current = Cursor {
        generation: agent.process_started_at,
        session: agent.spec.labels.get("session_id").cloned(),
        head: message.id.to_string(),
        path_hash: format!(
            "{:x}",
            Sha256::digest(transcript.as_os_str().as_encoded_bytes())
        ),
        device: before.dev(),
        inode: before.ino(),
        observed_len: before.len(),
        offset: 0,
    };
    if let Ok(mut previous) = serde_json::from_slice::<Cursor>(&bytes) {
        let offset = previous.offset;
        let shortened = before.len() < previous.observed_len;
        previous.offset = 0;
        previous.observed_len = before.len();
        if !shortened && previous == current && offset <= before.len() {
            current.offset = offset;
        }
    }
    let limit = WINDOW.min(before.len().saturating_sub(current.offset));
    // A partial record at either boundary never becomes evidence. The
    // one-MiB overlap retains a nearby input/response pair for the next call.
    let found = window(&mut source, current.offset, before.len())?
        .as_deref()
        .is_some_and(consumed);
    unchanged(transcript, &before)?;
    // At the end, retain the final overlap and reread it on later calls: a
    // response may be appended after the preceding Stop hook returned.
    current.offset = current.offset.saturating_add(limit.saturating_sub(OVERLAP));
    state.seek(SeekFrom::Start(0))?;
    state.set_len(0)?;
    // This checkpoint is only a search hint. An interrupted write restarts
    // the search; it can never authorize an ACK without rereading the proof.
    state.write_all(&serde_json::to_vec(&current)?)?;
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::{AgentSpec, Destination};
    use chrono::Utc;
    use serde_json::json;
    fn fixture() -> (tempfile::TempDir, std::path::PathBuf, AgentRecord, Envelope) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("session.jsonl");
        let agent = AgentRecord::new(
            AgentSpec {
                runtime: "claude-code".into(),
                ..AgentSpec::default()
            },
            false,
            Utc::now(),
        );
        let message = Envelope::new(
            "human",
            Destination::Agent(agent.id.clone()),
            "message",
            json!({"text":"pause"}),
            None,
            Utc::now(),
        );
        (root, path, agent, message)
    }
    #[test]
    fn older_head_is_found_over_bounded_windows_and_only_offsets_are_kept() {
        let (root, path, agent, message) = fixture();
        let padding = format!("{}\n", "x".repeat(1023));
        let proof = format!("PROOF-{}", message.id);
        std::fs::write(
            &path,
            format!(
                "{}{}\n{}",
                padding.repeat(3072),
                proof,
                padding.repeat(4096)
            ),
        )
        .unwrap();
        let mut windows = 0;
        let found = (0..8).any(|_| {
            find(root.path(), &path, &agent, &message, |window| {
                windows += 1;
                assert!(window.len() <= WINDOW as usize);
                window.contains(&proof)
            })
            .unwrap()
        });
        assert!(found && windows > 1 && windows < 8);
        for entry in std::fs::read_dir(root.path().join("channel-receipts")).unwrap() {
            let data = std::fs::read(entry.unwrap().path()).unwrap();
            assert!(data.len() < 1024);
            assert!(!String::from_utf8_lossy(&data).contains("PROOF-"));
        }
    }
    #[test]
    fn head_generation_inode_and_truncation_changes_restart_without_reusing_proof() {
        for change in 0..5 {
            let (root, path, mut agent, mut message) = fixture();
            let original = format!("START\n{}", format!("{}\n", "x".repeat(1023)).repeat(4096));
            std::fs::write(&path, &original).unwrap();
            assert!(!find(root.path(), &path, &agent, &message, |_| false).unwrap());
            match change {
                0 => message.id = agentdocker_core::MessageId::generate(),
                1 => agent.process_started_at = Some(Utc::now()),
                2 => {
                    std::fs::rename(&path, root.path().join("old")).unwrap();
                    std::fs::write(&path, &original).unwrap();
                }
                3 => std::fs::write(&path, "START\n").unwrap(),
                _ => std::fs::write(
                    &path,
                    format!("START\n{}\n", "x".repeat(WINDOW as usize + 1024)),
                )
                .unwrap(),
            }
            // Another test's fork can briefly inherit our just-dropped
            // descriptor until exec closes it. Production skips busy locks;
            // allow that same retry here without accepting a later offset.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                if find(root.path(), &path, &agent, &message, |window| {
                    window.starts_with("START\n") || window == "START"
                })
                .unwrap()
                {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "source change {change} must restart at the beginning"
                );
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
    }
    #[test]
    fn concurrent_hooks_and_replaced_or_unfinished_sources_do_not_prove_a_receipt() {
        let (root, path, agent, message) = fixture();
        std::fs::write(&path, "unfinished").unwrap();
        assert!(
            !find(root.path(), &path, &agent, &message, |_| panic!(
                "no complete line"
            ))
            .unwrap()
        );
        std::fs::write(&path, "record\n").unwrap();
        assert!(
            find(root.path(), &path, &agent, &message, |_| {
                std::fs::rename(&path, root.path().join("old")).unwrap();
                std::fs::write(&path, "replacement\n").unwrap();
                true
            })
            .is_err()
        );
        let key = format!("{:x}", Sha256::digest(agent.id.as_str().as_bytes()));
        let _guard = lock::try_exclusive_existing(
            &root
                .path()
                .join("channel-receipts")
                .join(format!("{key}.lock")),
        )
        .unwrap()
        .unwrap();
        assert!(
            !find(root.path(), &path, &agent, &message, |_| panic!(
                "concurrent scan"
            ))
            .unwrap()
        );
    }
}
