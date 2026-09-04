//! The set of agents the daemon knows about.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use thiserror::Error;

use crate::{AgentId, AgentRecord, AgentStatus};

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RegistryError {
    #[error("an agent named `{0}` is already live; stop it or pick another name")]
    NameTaken(String),
    #[error("no agent matches `{0}`")]
    NotFound(String),
    #[error("`{0}` is ambiguous; use a longer id prefix")]
    Ambiguous(String),
}

#[derive(Debug, Default)]
pub struct Registry {
    agents: HashMap<AgentId, AgentRecord>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a record. Names must be unique among live agents; finished agents
    /// keep their name so `logs` still works, but neither reserve it nor
    /// need it free.
    pub fn insert(&mut self, record: AgentRecord) -> Result<(), RegistryError> {
        if record.status.is_live() && self.live().any(|a| a.spec.name == record.spec.name) {
            return Err(RegistryError::NameTaken(record.spec.name));
        }
        self.agents.insert(record.id.clone(), record);
        Ok(())
    }

    pub fn get(&self, id: &AgentId) -> Option<&AgentRecord> {
        self.agents.get(id)
    }

    pub fn get_mut(&mut self, id: &AgentId) -> Option<&mut AgentRecord> {
        self.agents.get_mut(id)
    }

    /// Turn what a user typed into an id. Tries, in order: exact id, the name
    /// of a live agent, the name of the most recent finished agent, then a
    /// unique id prefix.
    pub fn resolve(&self, reference: &str) -> Result<AgentId, RegistryError> {
        if reference.is_empty() {
            return Err(RegistryError::NotFound(reference.to_owned()));
        }
        let exact = AgentId::from(reference);
        if self.agents.contains_key(&exact) {
            return Ok(exact);
        }

        let mut by_name: Vec<&AgentRecord> = self
            .agents
            .values()
            .filter(|a| a.spec.name == reference)
            .collect();
        let live_by_name: Vec<&AgentRecord> = by_name
            .iter()
            .copied()
            .filter(|a| a.status.is_live())
            .collect();
        match live_by_name.as_slice() {
            [one] => return Ok(one.id.clone()),
            [] => {}
            _ => return Err(RegistryError::Ambiguous(reference.to_owned())),
        }
        if !by_name.is_empty() {
            by_name.sort_by_key(|a| std::cmp::Reverse(a.created_at));
            return Ok(by_name[0].id.clone());
        }

        let by_prefix: Vec<&AgentId> = self
            .agents
            .keys()
            .filter(|id| id.as_str().starts_with(reference))
            .collect();
        match by_prefix.as_slice() {
            [one] => Ok((*one).clone()),
            [] => Err(RegistryError::NotFound(reference.to_owned())),
            _ => Err(RegistryError::Ambiguous(reference.to_owned())),
        }
    }

    pub fn live(&self) -> impl Iterator<Item = &AgentRecord> {
        self.agents.values().filter(|a| a.status.is_live())
    }

    /// Agents ordered by creation time. `all = false` hides finished ones.
    pub fn list(&self, all: bool) -> Vec<AgentRecord> {
        let mut agents: Vec<AgentRecord> = self
            .agents
            .values()
            .filter(|a| all || a.status.is_live())
            .cloned()
            .collect();
        agents.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        agents
    }

    /// Update status and the derived timestamps. Returns the updated record.
    pub fn set_status(
        &mut self,
        id: &AgentId,
        status: AgentStatus,
        now: DateTime<Utc>,
    ) -> Option<AgentRecord> {
        let record = self.agents.get_mut(id)?;
        if status == AgentStatus::Running && record.started_at.is_none() {
            record.started_at = Some(now);
        }
        if !status.is_live() && record.finished_at.is_none() {
            record.finished_at = Some(now);
        }
        record.status = status;
        record.last_seen = now;
        Some(record.clone())
    }

    /// Record that the agent is alive. Returns `false` if it is unknown.
    pub fn touch(&mut self, id: &AgentId, now: DateTime<Utc>) -> bool {
        match self.agents.get_mut(id) {
            Some(record) => {
                record.last_seen = now;
                true
            }
            None => false,
        }
    }

    pub fn remove(&mut self, id: &AgentId) -> Option<AgentRecord> {
        self.agents.remove(id)
    }

    pub fn len(&self) -> usize {
        self.agents.len()
    }

    pub fn is_empty(&self) -> bool {
        self.agents.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AgentSpec;

    fn record(name: &str) -> AgentRecord {
        let spec = AgentSpec {
            name: name.to_owned(),
            ..AgentSpec::default()
        };
        AgentRecord::new(spec, true, Utc::now())
    }

    #[test]
    fn names_are_unique_among_live_agents() {
        let mut reg = Registry::new();
        let first = record("worker");
        let first_id = first.id.clone();
        reg.insert(first).unwrap();
        assert_eq!(
            reg.insert(record("worker")),
            Err(RegistryError::NameTaken("worker".into()))
        );
        reg.set_status(&first_id, AgentStatus::Exited { code: Some(0) }, Utc::now());
        reg.insert(record("worker")).unwrap();
        assert_eq!(reg.len(), 2);

        // A finished record (as restored from storage) never needs the name.
        let mut finished = record("worker");
        finished.status = AgentStatus::Exited { code: None };
        reg.insert(finished).unwrap();
        assert_eq!(reg.len(), 3);
        assert_eq!(reg.live().count(), 1);
    }

    #[test]
    fn resolve_by_name_then_prefix() {
        let mut reg = Registry::new();
        let a = record("alpha");
        let a_id = a.id.clone();
        reg.insert(a).unwrap();

        assert_eq!(reg.resolve("alpha"), Ok(a_id.clone()));
        assert_eq!(reg.resolve(a_id.as_str()), Ok(a_id.clone()));
        assert_eq!(reg.resolve(a_id.short()), Ok(a_id.clone()));
        assert_eq!(
            reg.resolve("nope"),
            Err(RegistryError::NotFound("nope".into()))
        );
        assert_eq!(reg.resolve(""), Err(RegistryError::NotFound(String::new())));
    }

    #[test]
    fn resolve_prefers_live_then_newest_finished() {
        let mut reg = Registry::new();
        let old = record("w");
        let old_id = old.id.clone();
        reg.insert(old).unwrap();
        reg.set_status(&old_id, AgentStatus::Exited { code: Some(0) }, Utc::now());

        let mut newer = record("w");
        newer.created_at = Utc::now() + chrono::Duration::seconds(1);
        let newer_id = newer.id.clone();
        reg.insert(newer).unwrap();
        assert_eq!(reg.resolve("w"), Ok(newer_id.clone()));

        reg.set_status(&newer_id, AgentStatus::Exited { code: Some(0) }, Utc::now());
        assert_eq!(reg.resolve("w"), Ok(newer_id));
    }

    #[test]
    fn set_status_tracks_timestamps() {
        let mut reg = Registry::new();
        let rec = record("x");
        let id = rec.id.clone();
        reg.insert(rec).unwrap();
        let now = Utc::now();
        let running = reg.set_status(&id, AgentStatus::Running, now).unwrap();
        assert_eq!(running.started_at, Some(now));
        assert_eq!(running.finished_at, None);
        let later = now + chrono::Duration::seconds(5);
        let done = reg
            .set_status(&id, AgentStatus::Exited { code: Some(0) }, later)
            .unwrap();
        assert_eq!(done.finished_at, Some(later));
        assert_eq!(reg.list(false).len(), 0);
        assert_eq!(reg.list(true).len(), 1);
    }
}
