//! The set of agents the daemon knows about.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Utc};
use thiserror::Error;

use crate::agent::ROLE_PREFIX;
use crate::{AgentId, AgentRecord, AgentStatus, ProjectId, ProjectRef, VcsState};

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RegistryError {
    #[error("agent id `{0}` is reserved by a durable identity alias")]
    IdentityReserved(AgentId),
    #[error("an agent named `{0}` is already live; stop it or pick another name")]
    NameTaken(String),
    #[error("no agent matches `{0}`")]
    NotFound(String),
    #[error("`{0}` is ambiguous; use a longer id prefix")]
    Ambiguous(String),
    #[error("no agent works in a project matching `{0}`")]
    ProjectNotFound(String),
    #[error("`{0}` matches several projects; use a longer id prefix")]
    ProjectAmbiguous(String),
    #[error("no live agent holds the role `{0}`")]
    RoleNotFound(String),
    #[error("several live agents hold the role `{0}`; name one")]
    RoleAmbiguous(String),
    #[error(
        "`role:{0}` is also a live agent's name; the role cannot be addressed until it is renamed"
    )]
    RoleShadowed(String),
}

#[derive(Debug, Default)]
pub struct Registry {
    agents: HashMap<AgentId, AgentRecord>,
    aliases: BTreeMap<AgentId, AgentId>,
    retired_names: BTreeMap<AgentId, String>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a record. Names must be unique among live agents; finished agents
    /// keep their name so `logs` still works, but neither reserve it nor
    /// need it free.
    pub fn insert(&mut self, record: AgentRecord) -> Result<(), RegistryError> {
        if self.aliases.contains_key(&record.id) {
            return Err(RegistryError::IdentityReserved(record.id));
        }
        if record.status.is_live() && self.live().any(|a| a.spec.name == record.spec.name) {
            return Err(RegistryError::NameTaken(record.spec.name));
        }
        self.agents.insert(record.id.clone(), record);
        Ok(())
    }

    pub fn get(&self, id: &AgentId) -> Option<&AgentRecord> {
        self.agents.get(self.canonical_id(id))
    }

    pub fn get_mut(&mut self, id: &AgentId) -> Option<&mut AgentRecord> {
        let canonical = self.canonical_id(id).clone();
        self.agents.get_mut(&canonical)
    }

    /// Restore the complete flat alias set only after all checks succeed. Alias
    /// chains, missing targets and reused IDs fail startup rather than guessing.
    pub fn restore_aliases(
        &mut self,
        aliases: &[crate::identity::AgentAlias],
    ) -> Result<(), crate::identity::AliasError> {
        let mut checked = BTreeMap::new();
        for alias in aliases {
            let reason = if alias.retired == alias.canonical {
                Some("self reference")
            } else if self.agents.contains_key(&alias.retired) {
                Some("retired ID still owns a record")
            } else if !self.agents.contains_key(&alias.canonical) {
                Some("canonical record is missing")
            } else if checked
                .insert(alias.retired.clone(), alias.canonical.clone())
                .is_some()
            {
                Some("duplicate retired ID")
            } else {
                None
            };
            if let Some(reason) = reason {
                return Err(crate::identity::AliasError {
                    retired: alias.retired.clone(),
                    reason,
                });
            }
        }
        // A flat map is bounded to one lookup on every request. Because every
        // target is a real record and no key is one, chains cannot be admitted.
        self.retired_names = aliases
            .iter()
            .filter_map(|alias| {
                alias
                    .retired_name
                    .clone()
                    .map(|name| (alias.retired.clone(), name))
            })
            .collect();
        self.aliases = checked;
        Ok(())
    }

    /// Retire one record into another: the retired record leaves, and its
    /// id resolves to the canonical one from now on. Both must be records
    /// of their own; a chain or a self-reference is refused.
    pub fn retire_into(
        &mut self,
        retired: &AgentId,
        canonical: &AgentId,
    ) -> Result<AgentRecord, crate::identity::AliasError> {
        let reason = if retired == canonical {
            Some("self reference")
        } else if !self.agents.contains_key(canonical) {
            Some("canonical record is missing")
        } else if !self.agents.contains_key(retired) {
            Some("retired ID owns no record")
        } else if self.aliases.values().any(|target| target == retired) {
            Some("retired ID is already a canonical alias target")
        } else {
            None
        };
        if let Some(reason) = reason {
            return Err(crate::identity::AliasError {
                retired: retired.clone(),
                reason,
            });
        }
        let record = self.agents.remove(retired).expect("checked");
        self.retired_names
            .insert(retired.clone(), record.spec.name.clone());
        self.aliases.insert(retired.clone(), canonical.clone());
        Ok(record)
    }

    /// Retire several records into one at once, the way a session come
    /// back folds every life it left: each retired record leaves and its
    /// id resolves to the canonical one, and an alias that pointed at any
    /// of them is pointed at the canonical record instead, so the map
    /// stays flat. Checked whole before anything moves: the canonical
    /// record must exist and not be among the retired, and every retired
    /// id must own a record of its own. The retired records are returned
    /// in the order given.
    pub fn fold_into(
        &mut self,
        retired: &[AgentId],
        canonical: &AgentId,
    ) -> Result<Vec<AgentRecord>, crate::identity::AliasError> {
        if !self.agents.contains_key(canonical) {
            return Err(crate::identity::AliasError {
                retired: canonical.clone(),
                reason: "canonical record is missing",
            });
        }
        let mut seen = std::collections::BTreeSet::new();
        for id in retired {
            let reason = if id == canonical {
                Some("self reference")
            } else if !self.agents.contains_key(id) {
                Some("retired ID owns no record")
            } else if !seen.insert(id.clone()) {
                Some("duplicate retired ID")
            } else {
                None
            };
            if let Some(reason) = reason {
                return Err(crate::identity::AliasError {
                    retired: id.clone(),
                    reason,
                });
            }
        }
        for target in self.aliases.values_mut() {
            if retired.contains(target) {
                *target = canonical.clone();
            }
        }
        Ok(retired
            .iter()
            .map(|id| {
                let record = self.agents.remove(id).expect("checked");
                self.retired_names
                    .insert(id.clone(), record.spec.name.clone());
                self.aliases.insert(id.clone(), canonical.clone());
                record
            })
            .collect())
    }

    pub fn canonical_id<'a>(&'a self, id: &'a AgentId) -> &'a AgentId {
        self.aliases.get(id).unwrap_or(id)
    }

    pub fn aliases(&self) -> &BTreeMap<AgentId, AgentId> {
        &self.aliases
    }

    /// Historical queries can include former IDs without rewriting attribution.
    pub fn identity_ids(&self, id: &AgentId) -> Vec<AgentId> {
        let canonical = self.canonical_id(id);
        let mut ids = vec![canonical.clone()];
        ids.extend(
            self.aliases
                .iter()
                .filter(|(_, target)| *target == canonical)
                .map(|(old, _)| old.clone()),
        );
        ids
    }

    /// Current name plus names retained when identities were retired.
    /// Legacy aliases with no saved name contribute no guessed historical name.
    pub fn identity_names(&self, id: &AgentId) -> Vec<String> {
        let canonical = self.canonical_id(id);
        let mut names: Vec<_> = self
            .get(canonical)
            .map(|r| r.spec.name.clone())
            .into_iter()
            .collect();
        names.extend(
            self.aliases
                .iter()
                .filter(|(_, target)| *target == canonical)
                .filter_map(|(retired, _)| self.retired_names.get(retired).cloned()),
        );
        names.sort();
        names.dedup();
        names
    }

    /// Turn what a user typed into an id. Tries, in order: exact id, the name
    /// of a live agent, the name of the most recent finished agent, then a
    /// unique id prefix. `role:<name>` is the one live agent holding that
    /// role anywhere; [`Registry::resolve_role`] scopes it to a project.
    pub fn resolve(&self, reference: &str) -> Result<AgentId, RegistryError> {
        if reference.is_empty() {
            return Err(RegistryError::NotFound(reference.to_owned()));
        }
        if let Some(role) = reference.strip_prefix(ROLE_PREFIX) {
            return self.resolve_role(role, None);
        }
        let exact = AgentId::from(reference);
        if let Some(canonical) = self.aliases.get(&exact) {
            return Ok(canonical.clone());
        }
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

    /// The one live agent holding `role` — in `project` when one is
    /// given, anywhere otherwise. None is not found; several are
    /// ambiguous: a role is an address only while one agent answers to it.
    /// A live agent *named* `role:<name>` (a record from before names
    /// spelled that way were refused) is neither reached nor bypassed:
    /// the reference is refused as shadowed until the record is renamed.
    pub fn resolve_role(
        &self,
        role: &str,
        project: Option<&ProjectId>,
    ) -> Result<AgentId, RegistryError> {
        let spelled = format!("{ROLE_PREFIX}{role}");
        if self.live().any(|a| a.spec.name == spelled) {
            return Err(RegistryError::RoleShadowed(role.to_owned()));
        }
        let holders: Vec<&AgentRecord> = self
            .live()
            .filter(|a| a.role() == Some(role))
            .filter(|a| {
                project.is_none_or(|wanted| {
                    a.project.as_ref().is_some_and(|mine| mine.id() == *wanted)
                })
            })
            .collect();
        match holders.as_slice() {
            [one] => Ok(one.id.clone()),
            [] => Err(RegistryError::RoleNotFound(role.to_owned())),
            _ => Err(RegistryError::RoleAmbiguous(role.to_owned())),
        }
    }

    pub fn live(&self) -> impl Iterator<Item = &AgentRecord> {
        self.agents.values().filter(|a| a.status.is_live())
    }

    /// Every record, finished ones included, in no particular order.
    pub fn all(&self) -> impl Iterator<Item = &AgentRecord> {
        self.agents.values()
    }

    /// Every agent, grouped by project. `all = false` hides finished ones.
    pub fn list(&self, all: bool) -> Vec<AgentRecord> {
        self.matching(all, None, &BTreeMap::new())
    }

    /// Agents that pass every filter: in `project` when one is given, and
    /// carrying each of `labels`. Grouped by project (by name, then id) and
    /// ordered by creation time within a project; agents outside any
    /// project come last.
    pub fn matching(
        &self,
        all: bool,
        project: Option<&ProjectId>,
        labels: &BTreeMap<String, String>,
    ) -> Vec<AgentRecord> {
        let mut agents: Vec<AgentRecord> = self
            .agents
            .values()
            .filter(|a| all || a.status.is_live())
            .filter(|a| {
                project.is_none_or(|wanted| {
                    a.project.as_ref().is_some_and(|mine| mine.id() == *wanted)
                })
            })
            .filter(|a| labels.iter().all(|(k, v)| a.spec.labels.get(k) == Some(v)))
            .cloned()
            .collect();
        agents.sort_by_cached_key(|a| {
            (
                a.project.is_none(),
                a.project.as_ref().map(|p| (p.name(), p.id())),
                a.created_at,
                a.id.clone(),
            )
        });
        agents
    }

    /// Turn a project reference — a full id or a unique prefix — into the
    /// id of a project some agent (live or finished) works in.
    pub fn resolve_project(&self, reference: &str) -> Result<ProjectId, RegistryError> {
        if reference.is_empty() {
            return Err(RegistryError::ProjectNotFound(reference.to_owned()));
        }
        let mut ids: Vec<ProjectId> = self
            .agents
            .values()
            .filter_map(|a| a.project.as_ref().map(ProjectRef::id))
            .collect();
        ids.sort();
        ids.dedup();
        if ids.iter().any(|id| id.as_str() == reference) {
            return Ok(ProjectId::from(reference));
        }
        let by_prefix: Vec<&ProjectId> = ids
            .iter()
            .filter(|id| id.as_str().starts_with(reference))
            .collect();
        match by_prefix.as_slice() {
            [one] => Ok((*one).clone()),
            [] => Err(RegistryError::ProjectNotFound(reference.to_owned())),
            _ => Err(RegistryError::ProjectAmbiguous(reference.to_owned())),
        }
    }

    /// Update status and the derived timestamps. Returns the updated record.
    pub fn set_status(
        &mut self,
        id: &AgentId,
        status: AgentStatus,
        now: DateTime<Utc>,
    ) -> Option<AgentRecord> {
        let record = self.get_mut(id)?;
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

    /// Record what an agent's checkout looks like. Returns the record and
    /// whether the branch, head, or dirtiness changed (a fresh timestamp
    /// alone does not count, so callers persist and announce only real
    /// changes).
    pub fn set_vcs(&mut self, id: &AgentId, vcs: VcsState) -> Option<(AgentRecord, bool)> {
        let record = self.get_mut(id)?;
        let changed = !record.vcs.as_ref().is_some_and(|old| old.same_as(&vcs));
        record.vcs = Some(vcs);
        Some((record.clone(), changed))
    }

    /// Record that the agent is alive. Returns `false` if it is unknown.
    pub fn touch(&mut self, id: &AgentId, now: DateTime<Utc>) -> bool {
        match self.get_mut(id) {
            Some(record) => {
                record.last_seen = now;
                true
            }
            None => false,
        }
    }

    pub fn remove(&mut self, id: &AgentId) -> Option<AgentRecord> {
        let canonical = self.canonical_id(id).clone();
        self.aliases.retain(|_, target| *target != canonical);
        self.retired_names
            .retain(|id, _| self.aliases.contains_key(id));
        self.agents.remove(&canonical)
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
    use crate::agent::{ROLE_LABEL, check_role};
    use crate::{AgentSpec, ProjectRef};

    fn record(name: &str) -> AgentRecord {
        let spec = AgentSpec {
            name: name.to_owned(),
            ..AgentSpec::default()
        };
        AgentRecord::new(spec, true, Utc::now())
    }

    #[test]
    fn retiring_a_canonical_target_cannot_break_existing_aliases() {
        let mut registry = Registry::new();
        let old = record("old");
        let current = record("current");
        let later = record("later");
        for record in [&old, &current, &later] {
            registry.insert(record.clone()).unwrap();
        }
        registry.retire_into(&old.id, &current.id).unwrap();
        assert!(registry.retire_into(&current.id, &later.id).is_err());
        assert_eq!(registry.resolve(old.id.as_str()).unwrap(), current.id);
        assert_eq!(registry.get(&current.id).unwrap().id, current.id);
        assert_eq!(registry.get(&later.id).unwrap().id, later.id);
        assert_eq!(registry.aliases().len(), 1);
    }

    /// Folding several records into one leaves a flat map: an alias that
    /// pointed at a folded record points at the canonical one, every
    /// folded id resolves there, and nothing moves when the set is wrong.
    #[test]
    fn folding_several_records_into_one_keeps_the_alias_map_flat() {
        let mut registry = Registry::new();
        let oldest = record("oldest");
        let earlier = record("earlier");
        let last = record("last");
        let fresh = record("fresh");
        for record in [&oldest, &earlier, &last, &fresh] {
            registry.insert(record.clone()).unwrap();
        }
        // A life before this: oldest was retired into earlier.
        registry.retire_into(&oldest.id, &earlier.id).unwrap();
        // Refused whole: a retired id without a record, the canonical
        // among the retired, or a duplicate.
        for retired in [
            vec![fresh.id.clone(), AgentId::from("nobody")],
            vec![fresh.id.clone(), last.id.clone()],
            vec![fresh.id.clone(), fresh.id.clone()],
        ] {
            assert!(registry.fold_into(&retired, &last.id).is_err());
            assert_eq!(registry.len(), 3, "nothing moved");
            assert_eq!(registry.resolve(oldest.id.as_str()).unwrap(), earlier.id);
        }
        let folded = registry
            .fold_into(&[fresh.id.clone(), earlier.id.clone()], &last.id)
            .unwrap();
        assert_eq!(
            folded.iter().map(|r| r.id.clone()).collect::<Vec<_>>(),
            vec![fresh.id.clone(), earlier.id.clone()]
        );
        assert_eq!(registry.len(), 1);
        for id in [&oldest.id, &earlier.id, &fresh.id] {
            assert_eq!(registry.resolve(id.as_str()).unwrap(), last.id, "{id}");
            assert_eq!(registry.aliases()[id], last.id, "flat, not a chain");
        }
        // What the map holds now restores as it is.
        let aliases: Vec<_> = registry
            .aliases()
            .iter()
            .map(|(retired, canonical)| crate::identity::AgentAlias {
                retired: retired.clone(),
                canonical: canonical.clone(),
                retired_name: registry.retired_names.get(retired).cloned(),
                reconciled_at: Utc::now(),
            })
            .collect();
        let mut again = Registry::new();
        again.insert(last.clone()).unwrap();
        again.restore_aliases(&aliases).unwrap();
        assert_eq!(again.aliases().len(), 3);
        assert_eq!(
            registry.identity_names(&last.id),
            ["earlier", "fresh", "last", "oldest"]
        );
        assert_eq!(
            again.identity_names(&last.id),
            registry.identity_names(&last.id)
        );
    }

    #[test]
    fn durable_aliases_route_exact_ids_and_reject_partial_or_cyclic_restore() {
        use crate::identity::AgentAlias;
        let mut registry = Registry::new();
        let mut canonical = record("current");
        canonical.id = "canonical".into();
        registry.insert(canonical.clone()).unwrap();
        let old = AgentId::from("retired-id");
        let alias = AgentAlias {
            retired: old.clone(),
            canonical: canonical.id.clone(),
            retired_name: Some("early".into()),
            reconciled_at: canonical.created_at,
        };
        registry
            .restore_aliases(std::slice::from_ref(&alias))
            .unwrap();
        assert_eq!(registry.resolve(old.as_str()).unwrap(), canonical.id);
        assert_eq!(registry.get(&old).unwrap().id, canonical.id);
        assert_eq!(
            registry.identity_ids(&old),
            [canonical.id.clone(), old.clone()]
        );
        assert!(
            registry.resolve("retired-").is_err(),
            "aliases are exact, never guessed prefixes"
        );
        assert_eq!(registry.list(true).len(), 1);
        assert_eq!(registry.identity_names(&old), ["current", "early"]);
        let mut invalid = alias.clone();
        invalid.retired = "second-old".into();
        invalid.canonical = old.clone();
        assert!(registry.restore_aliases(&[alias.clone(), invalid]).is_err());
        assert_eq!(
            registry.resolve(old.as_str()).unwrap(),
            canonical.id,
            "failed restore leaves the previous routing intact"
        );
        assert_eq!(registry.identity_names(&old), ["current", "early"]);
        let mut legacy = serde_json::to_value(&alias).unwrap();
        legacy.as_object_mut().unwrap().remove("retired_name");
        let legacy = serde_json::from_value(legacy).unwrap();
        registry.restore_aliases(&[legacy]).unwrap();
        assert_eq!(registry.identity_names(&old), ["current"]);
        assert!(registry.restore_aliases(&[alias.clone(), alias]).is_err());
        let mut reused = record("reused");
        reused.id = old.clone();
        assert!(matches!(
            registry.insert(reused),
            Err(RegistryError::IdentityReserved(_))
        ));
        registry
            .set_status(
                &old,
                AgentStatus::Exited { code: Some(0) },
                canonical.created_at,
            )
            .unwrap();
        assert!(!registry.get(&old).unwrap().status.is_live());
        assert_eq!(registry.remove(&old).unwrap().id, canonical.id);
        assert!(registry.resolve(old.as_str()).is_err());
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

    /// A role names the one live agent holding it: in a project when the
    /// lookup is scoped, anywhere otherwise; a finished holder does not
    /// count, and two holders are an ambiguity, not a choice.
    #[test]
    fn a_role_names_the_one_live_holder_in_a_project() {
        let mut reg = Registry::new();
        let mut reviewer = record("rev");
        reviewer
            .spec
            .labels
            .insert(ROLE_LABEL.to_owned(), "reviewer".to_owned());
        reviewer.project = Some(ProjectRef::directory("/work/one"));
        let mut elsewhere = record("rev-2");
        elsewhere
            .spec
            .labels
            .insert(ROLE_LABEL.to_owned(), "reviewer".to_owned());
        elsewhere.project = Some(ProjectRef::directory("/work/two"));
        let one = reviewer.project.as_ref().unwrap().id();
        let two = elsewhere.project.as_ref().unwrap().id();
        let (reviewer_id, elsewhere_id) = (reviewer.id.clone(), elsewhere.id.clone());
        reg.insert(reviewer).unwrap();
        reg.insert(elsewhere).unwrap();
        assert_eq!(
            reg.resolve_role("reviewer", Some(&one)),
            Ok(reviewer_id.clone())
        );
        assert_eq!(
            reg.resolve_role("reviewer", Some(&two)),
            Ok(elsewhere_id.clone())
        );
        assert_eq!(
            reg.resolve("role:reviewer"),
            Err(RegistryError::RoleAmbiguous("reviewer".into())),
            "unscoped, two hold it"
        );
        assert_eq!(
            reg.resolve_role("implementer", Some(&one)),
            Err(RegistryError::RoleNotFound("implementer".into()))
        );
        reg.set_status(
            &elsewhere_id,
            AgentStatus::Exited { code: Some(0) },
            Utc::now(),
        );
        assert_eq!(
            reg.resolve("role:reviewer"),
            Ok(reviewer_id.clone()),
            "one live holder"
        );
        // A record named like a role — from before such names were
        // refused — neither takes the role's messages nor loses its own:
        // the reference is refused until it is renamed.
        let legacy = record("role:reviewer");
        let legacy_id = legacy.id.clone();
        reg.insert(legacy).unwrap();
        assert_eq!(
            reg.resolve("role:reviewer"),
            Err(RegistryError::RoleShadowed("reviewer".into()))
        );
        assert_eq!(
            reg.resolve_role("reviewer", Some(&one)),
            Err(RegistryError::RoleShadowed("reviewer".into()))
        );
        reg.set_status(&legacy_id, AgentStatus::Exited { code: None }, Utc::now());
        assert_eq!(reg.resolve("role:reviewer"), Ok(reviewer_id));
        assert!(check_role("reviewer").is_ok());
        assert!(check_role("code-reviewer-2").is_ok());
        for bad in ["", "Reviewer", "re viewer", "-rev", "rev-", &"r".repeat(41)] {
            assert!(check_role(bad).is_err(), "{bad:?}");
        }
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

    fn record_in(name: &str, root: &str) -> AgentRecord {
        let mut rec = record(name);
        rec.project = Some(ProjectRef::directory(root));
        rec
    }

    #[test]
    fn matching_groups_by_project_and_filters() {
        let mut reg = Registry::new();
        let mut alone = record("alone");
        alone.spec.labels.insert("team".into(), "x".into());
        reg.insert(alone).unwrap();
        reg.insert(record_in("b1", "/work/beta")).unwrap();
        let mut a1 = record_in("a1", "/work/alpha");
        a1.spec.labels.insert("team".into(), "x".into());
        reg.insert(a1).unwrap();
        reg.insert(record_in("a2", "/work/alpha")).unwrap();

        let names = |agents: Vec<AgentRecord>| -> Vec<String> {
            agents.into_iter().map(|a| a.spec.name).collect()
        };
        // Grouped by project name; the project-less agent last.
        let listed = names(reg.list(false));
        assert_eq!(listed[..2], ["a1".to_owned(), "a2".to_owned()]);
        assert_eq!(listed[2], "b1");
        assert_eq!(listed[3], "alone");

        let alpha = ProjectRef::directory("/work/alpha").id();
        assert_eq!(
            names(reg.matching(false, Some(&alpha), &BTreeMap::new())),
            ["a1", "a2"]
        );
        let team = BTreeMap::from([("team".to_owned(), "x".to_owned())]);
        assert_eq!(names(reg.matching(false, None, &team)), ["a1", "alone"]);
        assert_eq!(names(reg.matching(false, Some(&alpha), &team)), ["a1"]);
    }

    #[test]
    fn resolve_project_by_id_or_unique_prefix() {
        let mut reg = Registry::new();
        reg.insert(record_in("a", "/work/alpha")).unwrap();
        reg.insert(record_in("b", "/work/beta")).unwrap();
        let alpha = ProjectRef::directory("/work/alpha").id();
        assert_eq!(reg.resolve_project(alpha.as_str()), Ok(alpha.clone()));
        assert_eq!(
            reg.resolve_project(&alpha.as_str()[..10]),
            Ok(alpha.clone())
        );
        assert_eq!(
            reg.resolve_project("nope"),
            Err(RegistryError::ProjectNotFound("nope".into()))
        );
        assert_eq!(
            reg.resolve_project(""),
            Err(RegistryError::ProjectNotFound(String::new()))
        );
        // Every id is hex, so a one-character prefix is almost surely shared;
        // build the ambiguous case explicitly instead of hoping.
        let beta = ProjectRef::directory("/work/beta").id();
        let common = alpha
            .as_str()
            .chars()
            .zip(beta.as_str().chars())
            .take_while(|(x, y)| x == y)
            .count();
        if common > 0 {
            assert_eq!(
                reg.resolve_project(&alpha.as_str()[..common]),
                Err(RegistryError::ProjectAmbiguous(
                    alpha.as_str()[..common].to_owned()
                ))
            );
        }
    }

    #[test]
    fn set_vcs_reports_real_changes_only() {
        let mut reg = Registry::new();
        let rec = record("v");
        let id = rec.id.clone();
        reg.insert(rec).unwrap();
        let at = Utc::now();
        let main = VcsState {
            branch: Some("main".into()),
            head: Some("abc".into()),
            dirty: None,
            updated_at: at,
        };
        assert!(reg.set_vcs(&id, main.clone()).unwrap().1);
        let later = VcsState {
            updated_at: at + chrono::Duration::seconds(5),
            ..main.clone()
        };
        assert!(!reg.set_vcs(&id, later).unwrap().1);
        let feature = VcsState {
            branch: Some("feature".into()),
            ..main
        };
        let (rec, changed) = reg.set_vcs(&id, feature).unwrap();
        assert!(changed);
        assert_eq!(rec.vcs.unwrap().branch.as_deref(), Some("feature"));
        assert!(
            reg.set_vcs(
                &AgentId::from("nope"),
                VcsState {
                    branch: None,
                    head: None,
                    dirty: None,
                    updated_at: at,
                }
            )
            .is_none()
        );
    }
}
