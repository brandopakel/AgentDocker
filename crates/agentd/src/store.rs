//! SQLite-backed durable state.
//!
//! The in-memory structures in [`crate::daemon::Daemon`] are the source of
//! truth for reads; every mutation is written through here so a restart can
//! rebuild them. Records are stored as JSON blobs beside the few columns
//! needed for lookups, so adding a field to a core type never needs a
//! migration — only a changed meaning does.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};

use agentdocker_core::{AgentId, AgentRecord, Envelope, Event, Lease, LeaseId};
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};

const SCHEMA_VERSION: i64 = 3;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS agents (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    live       INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    json       TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS leases (
    id       TEXT PRIMARY KEY,
    holder   TEXT NOT NULL,
    resource TEXT NOT NULL,
    json     TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS inbox (
    seq        INTEGER PRIMARY KEY AUTOINCREMENT,
    agent      TEXT NOT NULL,
    message_id TEXT NOT NULL,
    json       TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS inbox_agent ON inbox (agent, seq);
CREATE TABLE IF NOT EXISTS events (
    seq  INTEGER PRIMARY KEY AUTOINCREMENT,
    at   TEXT NOT NULL,
    json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS projects (
    root        TEXT PRIMARY KEY,
    fingerprint TEXT NOT NULL,
    computed_at TEXT NOT NULL
);
";

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("cannot open state database {}", path.display()))?;
        Self::init(conn)
    }

    /// A throwaway database for tests.
    #[cfg(test)]
    pub fn in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update_and_check(None, "journal_mode", "WAL", |_| Ok(()))?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(SCHEMA)?;

        let version: Option<String> = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        match version.as_deref().map(str::parse::<i64>) {
            None => {
                conn.execute(
                    "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)",
                    params![SCHEMA_VERSION.to_string()],
                )?;
            }
            Some(Ok(found)) if found == SCHEMA_VERSION => {}
            Some(Ok(1 | 2)) => {
                // v2 adds stopping status and physical lease identities; v3
                // records dedicated process groups. Legacy groups default to
                // None. The daemon maps legacy file keys idempotently on load.
                conn.execute(
                    "UPDATE meta SET value = ?1 WHERE key = 'schema_version'",
                    params![SCHEMA_VERSION.to_string()],
                )?;
            }
            Some(other) => anyhow::bail!(
                "state database has schema version {other:?}; this build expects {SCHEMA_VERSION}"
            ),
        }
        Ok(Self { conn })
    }

    // ----- agents ---------------------------------------------------------

    pub fn upsert_agent(&self, record: &AgentRecord) -> Result<()> {
        self.conn.execute(
            "INSERT INTO agents (id, name, live, created_at, json)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET
                 name = excluded.name,
                 live = excluded.live,
                 created_at = excluded.created_at,
                 json = excluded.json",
            params![
                record.id.as_str(),
                record.spec.name,
                i64::from(record.status.is_live()),
                record.created_at.to_rfc3339(),
                serde_json::to_string(record)?,
            ],
        )?;
        Ok(())
    }

    /// Forget an agent and anything queued for it.
    pub fn delete_agent(&self, id: &AgentId) -> Result<()> {
        self.conn
            .execute("DELETE FROM inbox WHERE agent = ?1", params![id.as_str()])?;
        self.conn
            .execute("DELETE FROM agents WHERE id = ?1", params![id.as_str()])?;
        Ok(())
    }

    pub fn load_agents(&self) -> Result<Vec<AgentRecord>> {
        let mut stmt = self
            .conn
            .prepare("SELECT json FROM agents ORDER BY created_at, id")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    // ----- leases ---------------------------------------------------------

    pub fn upsert_lease(&self, lease: &Lease) -> Result<()> {
        self.conn.execute(
            "INSERT INTO leases (id, holder, resource, json) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET
                 holder = excluded.holder,
                 resource = excluded.resource,
                 json = excluded.json",
            params![
                lease.id.as_str(),
                lease.holder.as_str(),
                lease.resource.as_str(),
                serde_json::to_string(lease)?,
            ],
        )?;
        Ok(())
    }

    pub fn delete_lease(&self, id: &LeaseId) -> Result<()> {
        self.conn
            .execute("DELETE FROM leases WHERE id = ?1", params![id.as_str()])?;
        Ok(())
    }

    pub fn load_leases(&self) -> Result<Vec<Lease>> {
        let mut stmt = self.conn.prepare("SELECT json FROM leases")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    // ----- inboxes --------------------------------------------------------

    /// Queue a message for an agent, keeping only the newest `capacity`.
    pub fn enqueue(&self, agent: &AgentId, message: &Envelope, capacity: usize) -> Result<()> {
        self.conn.execute(
            "INSERT INTO inbox (agent, message_id, json) VALUES (?1, ?2, ?3)",
            params![
                agent.as_str(),
                message.id.as_str(),
                serde_json::to_string(message)?
            ],
        )?;
        self.conn.execute(
            "DELETE FROM inbox WHERE agent = ?1 AND seq NOT IN (
                 SELECT seq FROM inbox WHERE agent = ?1 ORDER BY seq DESC LIMIT ?2
             )",
            params![agent.as_str(), i64::try_from(capacity).unwrap_or(i64::MAX)],
        )?;
        Ok(())
    }

    pub fn clear_inbox(&self, agent: &AgentId) -> Result<()> {
        self.conn.execute(
            "DELETE FROM inbox WHERE agent = ?1",
            params![agent.as_str()],
        )?;
        Ok(())
    }

    pub fn load_inboxes(&self) -> Result<HashMap<AgentId, VecDeque<Envelope>>> {
        let mut stmt = self
            .conn
            .prepare("SELECT agent, json FROM inbox ORDER BY seq")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut inboxes: HashMap<AgentId, VecDeque<Envelope>> = HashMap::new();
        for row in rows {
            let (agent, json) = row?;
            let message: Envelope = serde_json::from_str(&json)?;
            inboxes
                .entry(AgentId::from(agent))
                .or_default()
                .push_back(message);
        }
        Ok(inboxes)
    }

    /// Acknowledge a delivered message without removing later arrivals.
    pub fn ack_inbox(
        &self,
        agent: &AgentId,
        messages: &[agentdocker_core::MessageId],
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for message in messages {
            tx.execute(
                "DELETE FROM inbox WHERE agent = ?1 AND message_id = ?2",
                params![agent.as_str(), message.as_str()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    // ----- projects -------------------------------------------------------

    /// Remember a repository's fingerprint so `git` walks its history once
    /// per host, not once per agent.
    pub fn upsert_project(&self, root: &Path, fingerprint: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO projects (root, fingerprint, computed_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(root) DO UPDATE SET
                 fingerprint = excluded.fingerprint,
                 computed_at = excluded.computed_at",
            params![
                root.to_string_lossy(),
                fingerprint,
                chrono::Utc::now().to_rfc3339()
            ],
        )?;
        Ok(())
    }

    pub fn load_projects(&self) -> Result<HashMap<PathBuf, String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT root, fingerprint FROM projects")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.map(|row| {
            let (root, fingerprint) = row?;
            Ok((PathBuf::from(root), fingerprint))
        })
        .collect()
    }

    // ----- events ---------------------------------------------------------

    /// Append an event under its `seq`; a `seq` of 0 lets SQLite pick the
    /// next one.
    pub fn append_event(&self, event: &Event) -> Result<()> {
        self.conn.execute(
            "INSERT INTO events (seq, at, json) VALUES (NULLIF(?1, 0), ?2, ?3)",
            params![
                i64::try_from(event.seq).unwrap_or(i64::MAX),
                event.at.to_rfc3339(),
                serde_json::to_string(event)?
            ],
        )?;
        Ok(())
    }

    /// The highest `seq` ever stored, or 0 when there are none.
    pub fn max_event_seq(&self) -> Result<u64> {
        let max: i64 =
            self.conn
                .query_row("SELECT COALESCE(MAX(seq), 0) FROM events", [], |row| {
                    row.get(0)
                })?;
        Ok(u64::try_from(max).unwrap_or(0))
    }

    /// The most recent `limit` events, oldest first.
    pub fn recent_events(&self, limit: usize) -> Result<Vec<Event>> {
        let mut stmt = self.conn.prepare(
            "SELECT json FROM (
                 SELECT seq, json FROM events ORDER BY seq DESC LIMIT ?1
             ) ORDER BY seq",
        )?;
        let rows = stmt.query_map(params![i64::try_from(limit).unwrap_or(i64::MAX)], |row| {
            row.get::<_, String>(0)
        })?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    /// Drop everything but the newest `keep` events. Returns how many went.
    pub fn prune_events(&self, keep: usize) -> Result<usize> {
        let removed = self.conn.execute(
            "DELETE FROM events WHERE seq NOT IN (
                 SELECT seq FROM events ORDER BY seq DESC LIMIT ?1
             )",
            params![i64::try_from(keep).unwrap_or(i64::MAX)],
        )?;
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::{
        AgentSpec, AgentStatus, Destination, EventKind, LeaseMode, ResourceKey,
    };
    use chrono::{Duration, Utc};

    fn record(name: &str) -> AgentRecord {
        let spec = AgentSpec {
            name: name.to_owned(),
            ..AgentSpec::default()
        };
        AgentRecord::new(spec, false, Utc::now())
    }

    fn envelope(text: &str) -> Envelope {
        Envelope::new(
            "user",
            Destination::Agent(AgentId::from("x")),
            "chat",
            serde_json::json!({ "text": text }),
            None,
            Utc::now(),
        )
    }

    #[test]
    fn agents_round_trip() {
        let store = Store::in_memory().unwrap();
        let mut a = record("a");
        store.upsert_agent(&a).unwrap();
        a.status = AgentStatus::Exited { code: Some(3) };
        store.upsert_agent(&a).unwrap();
        let loaded = store.load_agents().unwrap();
        assert_eq!(loaded, vec![a.clone()]);

        store.delete_agent(&a.id).unwrap();
        assert!(store.load_agents().unwrap().is_empty());
    }

    #[test]
    fn leases_round_trip() {
        let store = Store::in_memory().unwrap();
        let now = Utc::now();
        let lease = Lease {
            id: LeaseId::generate(),
            resource: ResourceKey::new("task:1"),
            holder: AgentId::from("a"),
            mode: LeaseMode::Shared,
            acquired_at: now,
            expires_at: now + Duration::seconds(30),
            note: Some("n".into()),
        };
        store.upsert_lease(&lease).unwrap();
        assert_eq!(store.load_leases().unwrap(), vec![lease.clone()]);
        store.delete_lease(&lease.id).unwrap();
        assert!(store.load_leases().unwrap().is_empty());
    }

    #[test]
    fn inbox_keeps_newest_up_to_capacity() {
        let store = Store::in_memory().unwrap();
        let agent = AgentId::from("a");
        for i in 0..5 {
            store.enqueue(&agent, &envelope(&i.to_string()), 3).unwrap();
        }
        let inboxes = store.load_inboxes().unwrap();
        let texts: Vec<String> = inboxes[&agent]
            .iter()
            .map(|m| m.payload["text"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(texts, vec!["2", "3", "4"]);

        store.clear_inbox(&agent).unwrap();
        assert!(store.load_inboxes().unwrap().is_empty());
    }

    #[test]
    fn events_replay_and_prune() {
        let store = Store::in_memory().unwrap();
        for i in 0..10u32 {
            let mut event = Event::new(
                EventKind::AgentRemoved {
                    agent: AgentId::from(i.to_string()),
                },
                Utc::now(),
            );
            event.seq = u64::from(i) + 1;
            store.append_event(&event).unwrap();
        }
        assert_eq!(store.max_event_seq().unwrap(), 10);
        let recent = store.recent_events(3).unwrap();
        let ids: Vec<String> = recent
            .iter()
            .map(|e| match &e.kind {
                EventKind::AgentRemoved { agent } => agent.to_string(),
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(ids, vec!["7", "8", "9"]);

        assert_eq!(store.prune_events(4).unwrap(), 6);
        assert_eq!(store.recent_events(100).unwrap().len(), 4);
    }

    #[test]
    fn rejects_unknown_schema_version() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        conn.execute(
            "INSERT INTO meta (key, value) VALUES ('schema_version', '999')",
            [],
        )
        .unwrap();
        assert!(Store::init(conn).is_err());
    }

    #[test]
    fn projects_round_trip_and_overwrite() {
        let store = Store::in_memory().unwrap();
        store.upsert_project(Path::new("/repo"), "aaa").unwrap();
        store.upsert_project(Path::new("/other"), "bbb").unwrap();
        store.upsert_project(Path::new("/repo"), "ccc").unwrap();
        let projects = store.load_projects().unwrap();
        assert_eq!(projects.len(), 2);
        assert_eq!(projects[Path::new("/repo")], "ccc");
        assert_eq!(projects[Path::new("/other")], "bbb");
    }
}
