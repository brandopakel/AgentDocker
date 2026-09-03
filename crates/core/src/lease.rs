//! Leases: time-limited claims on shared resources.
//!
//! This is the primitive that prevents two agents from clobbering the same
//! file, branch, or task. A lease always has a TTL so a crashed agent can
//! never wedge the system; the daemon also releases every lease an agent
//! holds the moment it exits.

use std::collections::HashMap;
use std::fmt;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::AgentId;

/// Something agents coordinate over. Keys are `kind:value`, for example
/// `path:/repo/src/main.rs`, `branch:feature/login`, `task:ISSUE-42`.
///
/// `path` keys overlap hierarchically: a lease on a directory covers every
/// file and directory beneath it. All other kinds only overlap when equal.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ResourceKey(String);

impl ResourceKey {
    pub fn new(key: impl Into<String>) -> Self {
        let mut key: String = key.into();
        if key.starts_with("path:") && key.len() > "path:/".len() {
            while key.ends_with('/') {
                key.pop();
            }
        }
        Self(key)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The part before the first `:`; `resource` when there is none.
    pub fn kind(&self) -> &str {
        self.0.split_once(':').map_or("resource", |(kind, _)| kind)
    }

    pub fn value(&self) -> &str {
        self.0.split_once(':').map_or(&self.0, |(_, value)| value)
    }

    pub fn overlaps(&self, other: &ResourceKey) -> bool {
        if self == other {
            return true;
        }
        if self.kind() != "path" || other.kind() != "path" {
            return false;
        }
        is_ancestor(self.value(), other.value()) || is_ancestor(other.value(), self.value())
    }
}

fn is_ancestor(parent: &str, child: &str) -> bool {
    let parent = parent.trim_end_matches('/');
    child.len() > parent.len()
        && child.starts_with(parent)
        && child.as_bytes()[parent.len()] == b'/'
}

impl fmt::Display for ResourceKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseMode {
    /// Nobody else may hold any lease on an overlapping resource.
    #[default]
    Exclusive,
    /// Others may also hold shared leases; blocks exclusive ones.
    Shared,
}

impl fmt::Display for LeaseMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exclusive => f.write_str("exclusive"),
            Self::Shared => f.write_str("shared"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LeaseId(String);

impl LeaseId {
    pub fn generate() -> Self {
        let raw = uuid::Uuid::new_v4().simple().to_string();
        Self(raw[..12].to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for LeaseId {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

impl fmt::Display for LeaseId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    pub id: LeaseId,
    pub resource: ResourceKey,
    pub holder: AgentId,
    pub mode: LeaseMode,
    pub acquired_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    /// Free text for humans and other agents: what the holder is doing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Lease {
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        self.expires_at <= now
    }

    /// Would this lease stop `requester` from taking `mode` on `resource`?
    fn blocks(&self, requester: &AgentId, mode: LeaseMode, resource: &ResourceKey) -> bool {
        self.holder != *requester
            && self.resource.overlaps(resource)
            && (self.mode == LeaseMode::Exclusive || mode == LeaseMode::Exclusive)
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum LeaseError {
    #[error("{resource} is held by {}", describe_holders(held_by))]
    Conflict {
        resource: ResourceKey,
        held_by: Vec<Lease>,
    },
    #[error("lease {0} not found")]
    NotFound(LeaseId),
    #[error("lease {lease} belongs to {holder}, not {requester}")]
    NotHolder {
        lease: LeaseId,
        holder: AgentId,
        requester: AgentId,
    },
}

fn describe_holders(leases: &[Lease]) -> String {
    leases
        .iter()
        .map(|l| format!("{} ({} on {})", l.holder.short(), l.mode, l.resource))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Result of a successful claim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Claimed {
    New(Lease),
    /// The holder already had this exact lease; its expiry was extended.
    Renewed(Lease),
}

impl Claimed {
    pub fn lease(&self) -> &Lease {
        match self {
            Self::New(l) | Self::Renewed(l) => l,
        }
    }

    pub fn into_lease(self) -> Lease {
        match self {
            Self::New(l) | Self::Renewed(l) => l,
        }
    }
}

#[derive(Debug, Default)]
pub struct LeaseTable {
    leases: HashMap<LeaseId, Lease>,
}

impl LeaseTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Take a lease, or learn who is in the way. Claiming a resource you
    /// already hold in the same mode renews it instead of failing.
    pub fn claim(
        &mut self,
        resource: ResourceKey,
        holder: AgentId,
        mode: LeaseMode,
        ttl: Duration,
        note: Option<String>,
        now: DateTime<Utc>,
    ) -> Result<Claimed, LeaseError> {
        self.expire(now);

        if let Some(existing) = self
            .leases
            .values_mut()
            .find(|l| l.holder == holder && l.resource == resource && l.mode == mode)
        {
            existing.expires_at = now + ttl;
            if note.is_some() {
                existing.note = note;
            }
            return Ok(Claimed::Renewed(existing.clone()));
        }

        let held_by: Vec<Lease> = self
            .leases
            .values()
            .filter(|l| l.blocks(&holder, mode, &resource))
            .cloned()
            .collect();
        if !held_by.is_empty() {
            return Err(LeaseError::Conflict { resource, held_by });
        }

        let lease = Lease {
            id: LeaseId::generate(),
            resource,
            holder,
            mode,
            acquired_at: now,
            expires_at: now + ttl,
            note,
        };
        self.leases.insert(lease.id.clone(), lease.clone());
        Ok(Claimed::New(lease))
    }

    pub fn renew(
        &mut self,
        id: &LeaseId,
        holder: &AgentId,
        ttl: Duration,
        now: DateTime<Utc>,
    ) -> Result<Lease, LeaseError> {
        self.expire(now);
        let lease = self.owned_mut(id, holder)?;
        lease.expires_at = now + ttl;
        Ok(lease.clone())
    }

    pub fn release(&mut self, id: &LeaseId, holder: &AgentId) -> Result<Lease, LeaseError> {
        self.owned_mut(id, holder)?;
        Ok(self.leases.remove(id).expect("checked above"))
    }

    /// Drop every lease held by `holder`; used when an agent exits.
    pub fn release_all(&mut self, holder: &AgentId) -> Vec<Lease> {
        let ids: Vec<LeaseId> = self
            .leases
            .values()
            .filter(|l| l.holder == *holder)
            .map(|l| l.id.clone())
            .collect();
        ids.iter().filter_map(|id| self.leases.remove(id)).collect()
    }

    /// Remove and return every lease whose TTL has passed.
    pub fn expire(&mut self, now: DateTime<Utc>) -> Vec<Lease> {
        let ids: Vec<LeaseId> = self
            .leases
            .values()
            .filter(|l| l.is_expired(now))
            .map(|l| l.id.clone())
            .collect();
        ids.iter().filter_map(|id| self.leases.remove(id)).collect()
    }

    pub fn get(&self, id: &LeaseId) -> Option<&Lease> {
        self.leases.get(id)
    }

    /// Every lease overlapping `resource`.
    pub fn holders_of(&self, resource: &ResourceKey) -> Vec<&Lease> {
        self.sorted(|l| l.resource.overlaps(resource))
    }

    pub fn by_holder(&self, holder: &AgentId) -> Vec<&Lease> {
        self.sorted(|l| l.holder == *holder)
    }

    pub fn all(&self) -> Vec<&Lease> {
        self.sorted(|_| true)
    }

    pub fn len(&self) -> usize {
        self.leases.len()
    }

    pub fn is_empty(&self) -> bool {
        self.leases.is_empty()
    }

    fn sorted(&self, keep: impl Fn(&Lease) -> bool) -> Vec<&Lease> {
        let mut leases: Vec<&Lease> = self.leases.values().filter(|l| keep(l)).collect();
        leases.sort_by(|a, b| {
            a.acquired_at
                .cmp(&b.acquired_at)
                .then_with(|| a.id.as_str().cmp(b.id.as_str()))
        });
        leases
    }

    fn owned_mut(&mut self, id: &LeaseId, holder: &AgentId) -> Result<&mut Lease, LeaseError> {
        let lease = self
            .leases
            .get_mut(id)
            .ok_or_else(|| LeaseError::NotFound(id.clone()))?;
        if lease.holder != *holder {
            return Err(LeaseError::NotHolder {
                lease: id.clone(),
                holder: lease.holder.clone(),
                requester: holder.clone(),
            });
        }
        Ok(lease)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(name: &str) -> AgentId {
        AgentId::from(name)
    }

    fn key(s: &str) -> ResourceKey {
        ResourceKey::new(s)
    }

    fn ttl() -> Duration {
        Duration::seconds(60)
    }

    #[test]
    fn resource_key_parts() {
        let k = key("path:/repo/src");
        assert_eq!(k.kind(), "path");
        assert_eq!(k.value(), "/repo/src");
        assert_eq!(key("ISSUE-1").kind(), "resource");
        assert_eq!(key("ISSUE-1").value(), "ISSUE-1");
        assert_eq!(key("path:/repo/src/").as_str(), "path:/repo/src");
        assert_eq!(key("path:/").as_str(), "path:/");
    }

    #[test]
    fn path_keys_overlap_hierarchically() {
        assert!(key("path:/repo").overlaps(&key("path:/repo/src/main.rs")));
        assert!(key("path:/repo/src/main.rs").overlaps(&key("path:/repo")));
        assert!(key("path:/").overlaps(&key("path:/anything")));
        assert!(!key("path:/repo").overlaps(&key("path:/repository")));
        assert!(!key("path:/repo/a").overlaps(&key("path:/repo/b")));
        assert!(!key("branch:main").overlaps(&key("branch:main/x")));
        assert!(key("branch:main").overlaps(&key("branch:main")));
        assert!(!key("path:/repo").overlaps(&key("branch:/repo")));
    }

    #[test]
    fn exclusive_blocks_everyone_else() {
        let mut t = LeaseTable::new();
        let now = Utc::now();
        t.claim(
            key("task:1"),
            agent("a"),
            LeaseMode::Exclusive,
            ttl(),
            None,
            now,
        )
        .unwrap();

        let err = t
            .claim(
                key("task:1"),
                agent("b"),
                LeaseMode::Exclusive,
                ttl(),
                None,
                now,
            )
            .unwrap_err();
        assert!(matches!(err, LeaseError::Conflict { ref held_by, .. } if held_by.len() == 1));

        let err = t
            .claim(
                key("task:1"),
                agent("b"),
                LeaseMode::Shared,
                ttl(),
                None,
                now,
            )
            .unwrap_err();
        assert!(matches!(err, LeaseError::Conflict { .. }));
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn shared_allows_shared_but_not_exclusive() {
        let mut t = LeaseTable::new();
        let now = Utc::now();
        t.claim(
            key("task:1"),
            agent("a"),
            LeaseMode::Shared,
            ttl(),
            None,
            now,
        )
        .unwrap();
        t.claim(
            key("task:1"),
            agent("b"),
            LeaseMode::Shared,
            ttl(),
            None,
            now,
        )
        .unwrap();
        assert_eq!(t.len(), 2);

        let err = t
            .claim(
                key("task:1"),
                agent("c"),
                LeaseMode::Exclusive,
                ttl(),
                None,
                now,
            )
            .unwrap_err();
        assert!(matches!(err, LeaseError::Conflict { ref held_by, .. } if held_by.len() == 2));
    }

    #[test]
    fn directory_lease_covers_files_within() {
        let mut t = LeaseTable::new();
        let now = Utc::now();
        t.claim(
            key("path:/repo/src"),
            agent("a"),
            LeaseMode::Exclusive,
            ttl(),
            None,
            now,
        )
        .unwrap();
        assert!(
            t.claim(
                key("path:/repo/src/lib.rs"),
                agent("b"),
                LeaseMode::Exclusive,
                ttl(),
                None,
                now
            )
            .is_err()
        );
        assert!(
            t.claim(
                key("path:/repo/docs"),
                agent("b"),
                LeaseMode::Exclusive,
                ttl(),
                None,
                now
            )
            .is_ok()
        );
        assert_eq!(t.holders_of(&key("path:/repo/src/lib.rs")).len(), 1);
        assert_eq!(t.holders_of(&key("path:/repo")).len(), 2);
    }

    #[test]
    fn reclaim_by_same_holder_renews() {
        let mut t = LeaseTable::new();
        let now = Utc::now();
        let first = t
            .claim(
                key("task:1"),
                agent("a"),
                LeaseMode::Exclusive,
                ttl(),
                None,
                now,
            )
            .unwrap();
        let later = now + Duration::seconds(30);
        let second = t
            .claim(
                key("task:1"),
                agent("a"),
                LeaseMode::Exclusive,
                ttl(),
                Some("still on it".into()),
                later,
            )
            .unwrap();
        let Claimed::Renewed(renewed) = second else {
            panic!("expected renewal")
        };
        assert_eq!(renewed.id, first.lease().id);
        assert_eq!(renewed.expires_at, later + ttl());
        assert_eq!(renewed.note.as_deref(), Some("still on it"));
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn leases_expire() {
        let mut t = LeaseTable::new();
        let now = Utc::now();
        t.claim(
            key("task:1"),
            agent("a"),
            LeaseMode::Exclusive,
            ttl(),
            None,
            now,
        )
        .unwrap();
        assert!(t.expire(now + Duration::seconds(59)).is_empty());
        let expired = t.expire(now + Duration::seconds(60));
        assert_eq!(expired.len(), 1);
        assert!(t.is_empty());

        t.claim(
            key("task:1"),
            agent("a"),
            LeaseMode::Exclusive,
            ttl(),
            None,
            now,
        )
        .unwrap();
        // A conflicting claim after expiry succeeds because claim() expires first.
        assert!(
            t.claim(
                key("task:1"),
                agent("b"),
                LeaseMode::Exclusive,
                ttl(),
                None,
                now + Duration::seconds(61)
            )
            .is_ok()
        );
    }

    #[test]
    fn only_the_holder_can_release_or_renew() {
        let mut t = LeaseTable::new();
        let now = Utc::now();
        let lease = t
            .claim(
                key("task:1"),
                agent("a"),
                LeaseMode::Exclusive,
                ttl(),
                None,
                now,
            )
            .unwrap()
            .into_lease();
        assert!(matches!(
            t.release(&lease.id, &agent("b")),
            Err(LeaseError::NotHolder { .. })
        ));
        assert!(matches!(
            t.renew(&lease.id, &agent("b"), ttl(), now),
            Err(LeaseError::NotHolder { .. })
        ));
        let renewed = t
            .renew(&lease.id, &agent("a"), ttl(), now + Duration::seconds(10))
            .unwrap();
        assert_eq!(renewed.expires_at, now + Duration::seconds(70));
        t.release(&lease.id, &agent("a")).unwrap();
        assert!(matches!(
            t.release(&lease.id, &agent("a")),
            Err(LeaseError::NotFound(_))
        ));
    }

    #[test]
    fn release_all_on_exit() {
        let mut t = LeaseTable::new();
        let now = Utc::now();
        t.claim(
            key("task:1"),
            agent("a"),
            LeaseMode::Exclusive,
            ttl(),
            None,
            now,
        )
        .unwrap();
        t.claim(
            key("task:2"),
            agent("a"),
            LeaseMode::Shared,
            ttl(),
            None,
            now,
        )
        .unwrap();
        t.claim(
            key("task:3"),
            agent("b"),
            LeaseMode::Exclusive,
            ttl(),
            None,
            now,
        )
        .unwrap();
        assert_eq!(t.release_all(&agent("a")).len(), 2);
        assert_eq!(t.by_holder(&agent("a")).len(), 0);
        assert_eq!(t.all().len(), 1);
    }
}
