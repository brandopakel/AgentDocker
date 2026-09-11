//! Explicit maintenance for proven legacy duplicates. No daemon is started,
//! stopped or signalled by this command. Preview is a read-only snapshot; apply
//! requires its exact digest, exclusive database ownership and ended sessions.
use agentdocker_core::{AgentId, AgentRecord, HUMAN_RUNTIME};
use anyhow::{Context, Result, ensure};
use chrono::Utc;
use std::path::Path;

pub use crate::store::reconcile::RepairPreview;

pub fn repair(
    home: &Path,
    kept: &str,
    retired: &str,
    expected: Option<&str>,
) -> Result<RepairPreview> {
    ensure!(
        !kept.is_empty() && !retired.is_empty(),
        "supply both complete agent IDs"
    );
    if let Some(expected) = expected {
        ensure!(
            expected.len() == 64 && expected.bytes().all(|b| b.is_ascii_hexdigit()),
            "--apply requires the exact preview plan SHA-256"
        );
    }
    let home = home
        .canonicalize()
        .context("state home must already exist")?;
    agentdocker_host::dirs::check_private_dir(&home)?;
    let store = crate::store::Store::open_repair(&home.join("state.db"), expected.is_some())?;
    store.repair(
        &AgentId::from(kept),
        &AgentId::from(retired),
        expected,
        Utc::now(),
        quiescent,
    )
}

fn quiescent(records: &[AgentRecord]) -> Result<()> {
    use nix::{errno::Errno, sys::signal::kill, unistd::Pid};
    for record in records {
        if record.spec.runtime == HUMAN_RUNTIME {
            continue;
        }
        ensure!(
            !record.status.is_live(),
            "session {} is still recorded as active; finish sessions normally before repair",
            record.id
        );
        if let Some(pid) = record.pid {
            let raw = i32::try_from(pid)
                .ok()
                .filter(|pid| *pid > 0)
                .context("stored process ID is invalid")?;
            match kill(Pid::from_raw(raw), None) {
                Err(Errno::ESRCH) => (),
                Ok(()) | Err(Errno::EPERM) => {
                    let current = agentdocker_host::procinfo::start_time(pid);
                    ensure!(
                        matches!((record.process_started_at,current),(Some(old),Some(now)) if old!=now),
                        "session {} still has a live or unverifiable process; no process was signalled",
                        record.id
                    );
                }
                Err(error) => return Err(error.into()),
            }
        }
        ensure!(
            record.container.is_none(),
            "container state needs its own engine verification before identity maintenance"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::{AgentSpec, AgentStatus};

    #[test]
    fn finished_status_never_authorizes_repair_over_a_live_process() {
        let mut record = AgentRecord::new(AgentSpec::default(), false, Utc::now());
        record.status = AgentStatus::Exited { code: Some(0) };
        record.pid = Some(std::process::id());
        record.process_started_at = agentdocker_host::procinfo::start_time(std::process::id());
        assert!(
            quiescent(&[record.clone()])
                .unwrap_err()
                .to_string()
                .contains("live or unverifiable")
        );
        record.process_started_at = None;
        assert!(quiescent(&[record.clone()]).is_err());
        record.pid = None;
        assert!(quiescent(&[record.clone()]).is_ok());
        record.status = AgentStatus::Running;
        assert!(
            quiescent(&[record])
                .unwrap_err()
                .to_string()
                .contains("active")
        );
    }
}
