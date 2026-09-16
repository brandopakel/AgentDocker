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
        if record
            .provider_availability
            .as_ref()
            .is_some_and(|p| p.issue.is_some())
        {
            return Err("resolve provider availability before reconciling these records");
        }
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
            // These adapters can independently register the same proven
            // session. Their provenance is retained in the repair archive;
            // it does not distinguish provider/process identity.
            let adapter_provenance = key == "via"
                && matches!(value.as_str(), "hook" | "mcp")
                && matches!(other.as_str(), "hook" | "mcp");
            if value != other
                && !adapter_provenance
                && !(key == "session_id" && (value.is_empty() || other.is_empty()))
            {
                return Err("the records carry conflicting identity labels");
            }
        }
    }
    Ok(())
}

/// The ended record a fresh registration is the return of: the same
/// provider session (a non-empty `session_id`, which only a hooks adapter
/// vouches for), the same runtime, project and physical checkout, ended
/// rather than live, neither side managed, remote, contained or in a pane
/// — a supervised process is its supervisor's to bring back — and not the
/// fresh record itself. Names prove nothing and are not consulted. A live
/// namesake is not a candidate: two live records for one session are the
/// registration rule's problem, not this one's. When several ended
/// records carry the session (a session resumed twice, each time into a
/// fresh record before this rule existed), the one that ended last is the
/// one that holds the queue; `finished_at` decides and the id after it,
/// so the answer does not depend on iteration order. Whether the prior
/// record's process is in fact gone is the host's to check.
pub fn resumed_session<'a>(
    records: impl IntoIterator<Item = &'a AgentRecord>,
    fresh: &AgentRecord,
) -> Option<&'a AgentRecord> {
    let session = session_id(fresh)?;
    if !resumable(fresh) || !matches!(fresh.spec.runtime.as_str(), "claude-code" | "codex") {
        return None;
    }
    let project = fresh.project.as_ref().map(ProjectRef::id)?;
    let checkout = fresh
        .spec
        .workdir
        .as_ref()
        .filter(|path| path.is_absolute())?;
    records
        .into_iter()
        .filter(|prior| prior.id != fresh.id && !prior.status.is_live())
        .filter(|prior| session_id(prior) == Some(session))
        .filter(|prior| prior.spec.runtime == fresh.spec.runtime)
        .filter(|prior| prior.project.as_ref().map(ProjectRef::id) == Some(project.clone()))
        .filter(|prior| prior.spec.workdir.as_ref() == Some(checkout))
        .filter(|prior| resumable(prior))
        .max_by(|a, b| a.finished_at.cmp(&b.finished_at).then(a.id.cmp(&b.id)))
}

/// A record whose process is nobody else's to run: not supervised,
/// restored, contained or paned, and on this host.
fn resumable(record: &AgentRecord) -> bool {
    record.host == "local"
        && !record.managed
        && record.container.is_none()
        && !record.spec.tty
        && !record.spec.in_pane
        && !record.spec.restore
        && record.spec.restart.is_no()
        && record.input_binding.is_none()
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
    fn repair_cannot_erase_an_unresolved_provider_limit() {
        let a = record("a", Some("session"));
        let mut b = record("b", Some("session"));
        b.provider_availability = Some(crate::ProviderAvailability {
            process_started_at: b.process_started_at.unwrap(),
            observed_at: b.created_at,
            issue: Some(crate::ProviderIssue::local(crate::ProviderIssueKind::Usage)),
            cleared_observation: None,
        });
        assert_eq!(
            repair_pair([&a, &b], &a.id, &b.id),
            Err("resolve provider availability before reconciling these records")
        );
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
    fn known_adapter_provenance_does_not_replace_or_conflict_with_identity_evidence() {
        let mut a = record("a", Some("session"));
        let mut b = record("b", None);
        a.spec.labels.insert("via".into(), "hook".into());
        b.spec.labels.insert("via".into(), "mcp".into());
        assert!(repair_pair([&a, &b], &a.id, &b.id).is_ok());
        assert!(repair_pair([&a, &b], &b.id, &a.id).is_ok());
        for (key, value) in [
            ("session_id", "other-session"),
            ("via", "unknown-adapter"),
            ("role", "writer"),
        ] {
            let mut altered = b.clone();
            altered.spec.labels.insert(key.into(), value.into());
            let mut a = a.clone();
            a.spec.labels.insert("role".into(), "reviewer".into());
            assert!(repair_pair([&a, &altered], &a.id, &altered.id).is_err());
        }
        b.process_started_at = None;
        assert!(repair_pair([&a, &b], &a.id, &b.id).is_err());
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

    /// A fresh registration of a session resumes the ended record that
    /// last held it: same session, runtime and project, ended, nobody's
    /// to supervise. A live namesake, another session, another project,
    /// a managed record or a record with an input binding is not it; the
    /// fresh record itself never is; of two ended, the later ended wins.
    #[test]
    fn a_fresh_registration_resumes_the_ended_record_of_its_session() {
        let ended = |id: &str, session: Option<&str>, minutes: i64| {
            let mut record = record(id, session);
            record.status = crate::AgentStatus::Exited { code: Some(0) };
            record.finished_at = Some(record.created_at + chrono::Duration::minutes(minutes));
            record
        };
        let mut fresh = record("fresh", Some("session"));
        fresh.pid = Some(456);
        let earlier = ended("earlier", Some("session"), 1);
        let later = ended("later", Some("session"), 2);
        let other = ended("other", Some("another"), 3);
        let unnamed = ended("unnamed", None, 3);
        let live = record("live", Some("session"));
        let mut managed = ended("managed", Some("session"), 4);
        managed.managed = true;
        let mut bound = ended("bound", Some("session"), 4);
        bound.input_binding = Some(crate::InputBinding {
            provider: crate::ProviderGeneration {
                process: crate::ProcessIdentity {
                    pid: 10,
                    started_at: bound.created_at,
                },
                session: "thread".into(),
                profile: "/profiles/default".into(),
            },
            controller: crate::ProcessIdentity {
                pid: 20,
                started_at: bound.created_at,
            },
            controller_since: bound.created_at,
            token_sha256: "digest".into(),
            bound_at: bound.created_at,
            controller_generations: 1,
            uncertain: Vec::new(),
            launch: None,
            restart: Default::default(),
        });
        let mut elsewhere = ended("elsewhere", Some("session"), 4);
        elsewhere.project = Some(ProjectRef::directory("/fixture/elsewhere"));
        let mut worktree = ended("worktree", Some("session"), 4);
        worktree.spec.workdir = Some("/fixture/worktree".into());
        let mut codex = ended("codex", Some("session"), 4);
        codex.spec.runtime = "codex".into();
        let all = [
            &fresh, &earlier, &later, &other, &unnamed, &live, &managed, &bound, &elsewhere,
            &worktree, &codex,
        ];
        assert_eq!(
            resumed_session(all, &fresh).map(|r| r.id.as_str()),
            Some("later")
        );
        // Order of the records does not pick the answer.
        let mut reversed = all;
        reversed.reverse();
        assert_eq!(
            resumed_session(reversed, &fresh).map(|r| r.id.as_str()),
            Some("later")
        );
        // Two that ended together are told apart by id.
        let mut twin = ended("aaa-twin", Some("session"), 2);
        twin.finished_at = later.finished_at;
        assert_eq!(
            resumed_session([&later, &twin], &fresh).map(|r| r.id.as_str()),
            Some("later")
        );
        // Nothing to resume for a session nobody vouched for, a managed
        // newcomer, or a runtime without session evidence.
        let mut nameless = fresh.clone();
        nameless.spec.labels.remove("session_id");
        assert!(resumed_session(all, &nameless).is_none());
        let mut supervised = fresh.clone();
        supervised.managed = true;
        assert!(resumed_session(all, &supervised).is_none());
        let mut unknown = fresh.clone();
        unknown.spec.runtime = "gemini-cli".into();
        assert!(resumed_session(all, &unknown).is_none());
    }
}
