//! Durable identity redirects and the evidence required before reconciling them.
//! No name, idle state or empty inbox proves that two records are one session.

use crate::{AgentId, AgentRecord, ProjectRef};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// An exact former ID remains addressable after a committed repair. Historical
/// events keep the ID they originally recorded; this supplies its current route.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentAlias {
    pub retired: AgentId,
    pub canonical: AgentId,
    pub reconciled_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("invalid agent alias {retired}: {reason}")]
pub struct AliasError {
    pub retired: AgentId,
    pub reason: &'static str,
}

/// Registration identity: exact known process birth, runtime and physical
/// checkout. An absent session ID can join one known session, but callers must
/// reject multiple candidates because this relation is intentionally not
/// transitive across a sessionless transport and two named provider sessions.
pub fn same_registration(a: &AgentRecord, b: &AgentRecord) -> bool {
    b.pid.is_some()
        && b.process_started_at.is_some()
        && a.pid == b.pid
        && a.process_started_at == b.process_started_at
        && a.spec.runtime == b.spec.runtime
        && a.project.as_ref().map(ProjectRef::id) == b.project.as_ref().map(ProjectRef::id)
        && matches!((&a.spec.workdir, &b.spec.workdir), (Some(one), Some(other)) if one == other)
        && match (session_id(a), session_id(b)) {
            (Some(one), Some(other)) => one == other,
            _ => true,
        }
}

fn session_id(record: &AgentRecord) -> Option<&str> {
    record
        .spec
        .labels
        .get("session_id")
        .map(String::as_str)
        .filter(|id| !id.is_empty())
}

/// Only an unambiguous pair of external local sessions is eligible for a
/// maintenance repair. The host must separately prove the daemon and provider
/// processes are quiescent, and the store must validate every affected object.
pub fn repair_pair<'a>(
    records: impl IntoIterator<Item = &'a AgentRecord>,
    kept: &AgentId,
    retired: &AgentId,
) -> Result<(), &'static str> {
    if kept == retired {
        return Err("choose two different records");
    }
    let records: Vec<_> = records.into_iter().collect();
    let a = records
        .iter()
        .copied()
        .find(|a| &a.id == kept)
        .ok_or("canonical record is missing")?;
    let b = records
        .iter()
        .copied()
        .find(|a| &a.id == retired)
        .ok_or("retired record is missing")?;
    if !same_registration(a, b) {
        return Err("process birth, runtime, session or physical checkout does not match");
    }
    if session_id(a).is_none() && session_id(b).is_none() {
        return Err("neither record identifies the provider session");
    }
    for record in [a, b] {
        if !record
            .pid
            .is_some_and(|pid| pid > 0 && pid <= i32::MAX as u32)
            || record.project.is_none()
            || !record
                .spec
                .workdir
                .as_ref()
                .is_some_and(|path| path.is_absolute())
        {
            return Err("physical process and checkout evidence is incomplete");
        }
        if record.host != "local"
            || record.managed
            || record.container.is_some()
            || record.spec.tty
            || record.spec.in_pane
            || record.spec.restore
            || !record.spec.restart.is_no()
            || record.process_group.is_some()
        {
            return Err("managed, remote or restorable ownership requires a separate transfer");
        }
        if !matches!(record.spec.runtime.as_str(), "claude-code" | "codex") {
            return Err("this provider does not have established session identity evidence");
        }
    }
    // Check the complete stored set, including history. A missing session label
    // must never become a wildcard that absorbs one of several real sessions.
    if records.iter().any(|candidate| {
        candidate.id != *kept
            && candidate.id != *retired
            && (same_registration(a, candidate) || same_registration(b, candidate))
    }) {
        return Err("another stored session makes the identity ambiguous");
    }
    for (key, value) in &a.spec.labels {
        if let Some(other) = b.spec.labels.get(key) {
            if value != other && !(key == "session_id" && (value.is_empty() || other.is_empty())) {
                return Err("the records carry conflicting identity labels");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AgentSpec;

    fn record(id: &str, session: Option<&str>) -> AgentRecord {
        let checkout = if cfg!(windows) {
            r"C:\fixture\checkout"
        } else {
            "/fixture/checkout"
        };
        let now = DateTime::parse_from_rfc3339("2026-09-11T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut record = AgentRecord::new(
            AgentSpec {
                runtime: "claude-code".into(),
                workdir: Some(checkout.into()),
                ..Default::default()
            },
            false,
            now,
        );
        record.id = id.into();
        record.pid = Some(123);
        record.process_started_at = Some(now);
        record.project = Some(ProjectRef::directory(checkout));
        if let Some(session) = session {
            record
                .spec
                .labels
                .insert("session_id".into(), session.into());
        }
        record
    }

    #[test]
    fn repair_refuses_the_sessionless_bridge_between_distinct_sessions() {
        let a = record("a", Some("first"));
        let b = record("b", None);
        let c = record("c", Some("second"));
        assert!(same_registration(&a, &b) && same_registration(&b, &c));
        assert!(!same_registration(&a, &c));
        assert!(repair_pair([&a, &b], &a.id, &b.id).is_ok());
        for records in [[&a, &b, &c], [&c, &b, &a]] {
            assert!(
                repair_pair(records, &a.id, &b.id)
                    .unwrap_err()
                    .contains("ambiguous")
            );
        }
    }

    #[test]
    fn matching_names_never_replace_birth_checkout_or_ownership_evidence() {
        let a = record("a", Some("session"));
        let b = record("b", Some("session"));
        for changed in 0..6 {
            let mut b = b.clone();
            match changed {
                0 => b.process_started_at = None,
                1 => b.pid = Some(124),
                2 => b.spec.workdir = Some("/fixture/another-checkout".into()),
                3 => b.managed = true,
                4 => b.host = "another-host".into(),
                _ => {
                    b.spec
                        .labels
                        .insert("session_id".into(), "different".into());
                }
            }
            assert!(
                repair_pair([&a, &b], &a.id, &b.id).is_err(),
                "case {changed}"
            );
        }
    }

    #[test]
    fn matching_relative_checkouts_are_not_physical_identity_evidence() {
        let mut a = record("a", Some("session"));
        let mut b = record("b", Some("session"));
        for record in [&mut a, &mut b] {
            record.spec.workdir = Some("relative/checkout".into());
            record.project = Some(ProjectRef::directory("relative/checkout"));
        }
        assert!(same_registration(&a, &b));
        assert_eq!(
            repair_pair([&a, &b], &a.id, &b.id),
            Err("physical process and checkout evidence is incomplete")
        );
    }
}
